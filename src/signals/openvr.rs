use crate::event::{Kind, SignalEvent, Source, TrackState, now_ms};
use openvr::system::Event;
use openvr::{ApplicationType, TrackedDeviceClass, TrackedDeviceIndex, TrackingUniverseOrigin, MAX_TRACKED_DEVICE_COUNT};
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct FrameSample {
    pub idx: usize,
    pub t_ms: u64,
    pub pos: [f32; 3],
    pub vel: [f32; 3],
    pub rot: [[f32; 4]; 3],
    pub ang_speed: f32,
    pub state: TrackState,
    pub valid: bool,
}

#[derive(Clone, Debug)]
pub struct DeviceMeta {
    pub idx: usize,
    pub serial: String,
    pub model: String,
    pub class: String,
    pub dongle: String,
    pub battery_pct: Option<f32>,
    pub is_lighthouse: bool,
}

pub enum VrMsg {
    Event(SignalEvent),
    Frame(FrameSample),
    Device(DeviceMeta),
    Status(String),
}

struct DevState {
    connected: bool,
    valid: bool,
    state: TrackState,
    serial: Option<String>,
    interesting: bool,
}

impl Default for DevState {
    fn default() -> Self {
        Self { connected: false, valid: false, state: TrackState::Uninitialized, serial: None, interesting: false }
    }
}

pub fn run(tx: std::sync::mpsc::Sender<VrMsg>) {
    let ctx = loop {
        match unsafe { openvr::init(ApplicationType::Background) } {
            Ok(c) => break c,
            Err(e) => {
                let _ = tx.send(VrMsg::Status(format!("waiting for SteamVR: {e:?}")));
                std::thread::sleep(Duration::from_secs(5));
            }
        }
    };
    let Ok(system) = ctx.system() else {
        let _ = tx.send(VrMsg::Status("IVRSystem unavailable".into()));
        return;
    };
    let _ = tx.send(VrMsg::Status("connected to SteamVR".into()));

    let mut devs: Vec<DevState> = (0..MAX_TRACKED_DEVICE_COUNT).map(|_| DevState::default()).collect();
    for (i, dev) in devs.iter_mut().enumerate() {
        if system.is_tracked_device_connected(TrackedDeviceIndex(i as u32)) {
            announce(&system, i, dev, &tx);
        }
    }

    loop {
        while let Some(info) = system.poll_next_event() {
            let i = info.tracked_device_index.0 as usize;
            let serial = devs.get(i).and_then(|d| d.serial.clone());
            let kind = match info.event {
                Event::TrackedDeviceActivated => {
                    if i < MAX_TRACKED_DEVICE_COUNT {
                        announce(&system, i, &mut devs[i], &tx);
                    }
                    Some(Kind::DeviceActivated)
                }
                Event::TrackedDeviceDeactivated => Some(Kind::DeviceDeactivated),
                Event::WirelessDisconnect => Some(Kind::WirelessDisconnect),
                Event::WirelessReconnect => Some(Kind::WirelessReconnect),
                Event::EnterStandbyMode => Some(Kind::StandbyStart),
                Event::LeaveStandbyMode => Some(Kind::StandbyEnd),
                Event::Quit(_) | Event::ProcessQuit(_) | Event::DriverRequestedQuit => {
                    let _ = tx.send(VrMsg::Status("SteamVR is shutting down".into()));
                    system.acknowledge_quit_exiting();
                    return;
                }
                _ => None,
            };
            if let Some(kind) = kind {
                let serial = devs.get(i).and_then(|d| d.serial.clone()).or(serial);
                let detail = format!("device {i} {:?}", kind);
                let _ = tx.send(VrMsg::Event(SignalEvent::new(Source::Api, serial, kind, detail)));
            }
        }

        let poses = system.device_to_absolute_tracking_pose(TrackingUniverseOrigin::Standing, 0.0);
        let t = now_ms();
        for i in 0..MAX_TRACKED_DEVICE_COUNT {
            let d = &mut devs[i];
            if !d.interesting {
                continue;
            }
            let p = &poses[i];
            let raw = &p.0;
            let connected = p.device_is_connected();
            let valid = p.pose_is_valid();
            let state = TrackState::from_raw(raw.eTrackingResult);
            if connected != d.connected {
                let kind = if connected { Kind::DeviceActivated } else { Kind::DeviceDeactivated };
                let _ = tx.send(VrMsg::Event(SignalEvent::new(Source::Api, d.serial.clone(), kind, format!("device {i} connected={connected}"))));
                d.connected = connected;
            }
            if valid != d.valid {
                let _ = tx.send(VrMsg::Event(SignalEvent::new(Source::Api, d.serial.clone(), Kind::PoseValid(valid), format!("device {i} pose_valid={valid}"))));
                d.valid = valid;
            }
            if state != d.state {
                let _ = tx.send(VrMsg::Event(SignalEvent::new(
                    Source::Api,
                    d.serial.clone(),
                    Kind::TrackingState { from: d.state.label(), to: state.label() },
                    format!("device {i} {} -> {}", d.state.label(), state.label()),
                )));
                d.state = state;
            }
            if connected {
                let m = raw.mDeviceToAbsoluteTracking.m;
                let av = raw.vAngularVelocity.v;
                let _ = tx.send(VrMsg::Frame(FrameSample {
                    idx: i,
                    t_ms: t,
                    pos: [m[0][3], m[1][3], m[2][3]],
                    vel: raw.vVelocity.v,
                    rot: m,
                    ang_speed: (av[0] * av[0] + av[1] * av[1] + av[2] * av[2]).sqrt(),
                    state,
                    valid,
                }));
            }
        }
        std::thread::sleep(Duration::from_millis(11));
    }
}

fn prop(system: &openvr::System, i: usize, p: openvr::TrackedDeviceProperty) -> String {
    system
        .string_tracked_device_property(TrackedDeviceIndex(i as u32), p)
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn announce(system: &openvr::System, i: usize, d: &mut DevState, tx: &std::sync::mpsc::Sender<VrMsg>) {
    let class = system.tracked_device_class(TrackedDeviceIndex(i as u32));
    let tracking_system = prop(system, i, openvr::property::TrackingSystemName_String);
    let serial = prop(system, i, openvr::property::SerialNumber_String);
    let is_lighthouse = tracking_system.contains("lighthouse");
    d.serial = if serial.is_empty() { None } else { Some(serial.clone()) };
    d.interesting = matches!(
        class,
        TrackedDeviceClass::HMD | TrackedDeviceClass::Controller | TrackedDeviceClass::GenericTracker | TrackedDeviceClass::TrackingReference
    );
    if !d.interesting {
        return;
    }
    let battery = system
        .get_tracked_device_property_f32(TrackedDeviceIndex(i as u32), openvr::property::DeviceBatteryPercentage_Float.0)
        .ok();
    let _ = tx.send(VrMsg::Device(DeviceMeta {
        idx: i,
        serial,
        model: prop(system, i, openvr::property::ModelNumber_String),
        class: format!("{class:?}"),
        dongle: prop(system, i, openvr::property::ConnectedWirelessDongle_String),
        battery_pct: battery.map(|b| b * 100.0),
        is_lighthouse,
    }));
}

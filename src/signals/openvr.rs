use super::packets::Gate;
use crate::engine::Msg;
use crate::event::{DeviceClass, Kind, SignalEvent, Source, TrackState, now_ms};
use openvr::chaperone::ChaperoneCalibrationState;
use openvr::system::Event;
use openvr::{
    ApplicationType, MAX_TRACKED_DEVICE_COUNT, TrackedDeviceClass, TrackedDeviceIndex,
    TrackingUniverseOrigin,
};
use std::sync::Arc;
use std::sync::mpsc::Sender;
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
    pub class: DeviceClass,
    pub dongle: String,
    pub battery_pct: Option<f32>,
    pub is_lighthouse: bool,
}

struct DevState {
    connected: bool,
    valid: bool,
    state: TrackState,
    serial: Option<String>,
    battery_pct: Option<f32>,
    interesting: bool,
    base: bool,
    update: Option<bool>,
}

impl Default for DevState {
    fn default() -> Self {
        Self {
            connected: false,
            valid: false,
            state: TrackState::Uninitialized,
            serial: None,
            battery_pct: None,
            interesting: false,
            base: false,
            update: None,
        }
    }
}

const BATTERY_POLL_TICKS: u32 = 5400;
const BASE_FRAME_TICKS: u32 = 90;
const ROOM_TICKS: u32 = 450;

pub fn run(tx: Sender<Msg>, gate: Arc<Gate>) {
    loop {
        let ctx = loop {
            match unsafe { openvr::init(ApplicationType::Background) } {
                Ok(c) => break c,
                Err(e) => {
                    let status = match e {
                        openvr::InitError::Init_NoServerForBackgroundApp => {
                            "waiting for SteamVR:".to_string()
                        }
                        e => format!("waiting for SteamVR: {e}"),
                    };
                    if tx.send(Msg::Status(status)).is_err() {
                        return;
                    }
                    std::thread::sleep(Duration::from_secs(5));
                }
            }
        };
        crate::dlog!("connected to SteamVR");
        gate.open();
        session(&ctx, &tx);
        drop(ctx);
        gate.close(Duration::from_secs(3));
        crate::dlog!("SteamVR went away");
        if tx.send(Msg::SteamVrExited).is_err()
            || tx.send(Msg::Status("waiting for SteamVR:".into())).is_err()
        {
            return;
        }
        std::thread::sleep(Duration::from_secs(5));
    }
}

fn session(ctx: &openvr::Context, tx: &Sender<Msg>) {
    let Ok(system) = ctx.system() else {
        let _ = tx.send(Msg::Status(
            "waiting for SteamVR: SteamVR started but its tracking system isn't ready".into(),
        ));
        return;
    };
    let _ = tx.send(Msg::Status("connected to SteamVR".into()));

    let mut devs: Vec<DevState> = (0..MAX_TRACKED_DEVICE_COUNT)
        .map(|_| DevState::default())
        .collect();
    for (i, dev) in devs.iter_mut().enumerate() {
        if system.is_tracked_device_connected(TrackedDeviceIndex(i as u32)) {
            announce(&system, i, dev, tx);
        }
    }

    let chaperone = ctx.chaperone().ok();
    let mut room: Option<u32> = None;
    let mut tick: u32 = 0;
    loop {
        while let Some(info) = system.poll_next_event() {
            let i = info.tracked_device_index.0 as usize;
            crate::dlog!(
                "openvr event {:?} on device {i} ({})",
                info.event,
                devs.get(i).and_then(|d| d.serial.as_deref()).unwrap_or("-")
            );
            let kind = match info.event {
                Event::TrackedDeviceActivated => {
                    if i < MAX_TRACKED_DEVICE_COUNT {
                        announce(&system, i, &mut devs[i], tx);
                    }
                    Some(Kind::DeviceActivated)
                }
                Event::TrackedDeviceDeactivated => Some(Kind::DeviceDeactivated),
                Event::WirelessDisconnect => Some(Kind::WirelessDisconnect),
                Event::WirelessReconnect => Some(Kind::WirelessReconnect),
                Event::EnterStandbyMode => Some(Kind::StandbyStart),
                Event::LeaveStandbyMode => Some(Kind::StandbyEnd),
                Event::PropertyChanged(p)
                    if i < MAX_TRACKED_DEVICE_COUNT
                        && (p.property == openvr::property::Firmware_UpdateAvailable_Bool
                            || p.property == openvr::property::Firmware_ManualUpdate_Bool) =>
                {
                    check_update(&system, i, &mut devs[i], tx);
                    None
                }
                Event::Quit(_) => {
                    system.acknowledge_quit_exiting();
                    return;
                }
                _ => None,
            };
            if let Some(kind) = kind {
                let serial = devs.get(i).and_then(|d| d.serial.clone());
                let detail = kind.to_string();
                let _ = tx.send(Msg::Event(SignalEvent::new(
                    Source::Api,
                    serial,
                    kind,
                    detail,
                )));
            }
        }

        let poses = system.device_to_absolute_tracking_pose(TrackingUniverseOrigin::Standing, 0.0);
        let t = now_ms();
        tick = tick.wrapping_add(1);
        if tick.is_multiple_of(ROOM_TICKS)
            && let Some(c) = &chaperone
        {
            let code = room_code(c.get_calibration_state());
            if room != Some(code) {
                crate::dlog!("room setup check {code}");
                if room.is_some() || code != 1 {
                    let kind = Kind::RoomCheck { code };
                    let _ = tx.send(Msg::Event(SignalEvent::at(
                        t,
                        Source::Api,
                        None,
                        kind.clone(),
                        kind.to_string(),
                    )));
                }
                room = Some(code);
            }
        }
        let poll_battery = tick.is_multiple_of(BATTERY_POLL_TICKS);
        let base_frame = tick.is_multiple_of(BASE_FRAME_TICKS);
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
                let kind = if connected {
                    Kind::DeviceActivated
                } else {
                    Kind::DeviceDeactivated
                };
                let _ = tx.send(Msg::Event(SignalEvent::at(
                    t,
                    Source::Api,
                    d.serial.clone(),
                    kind.clone(),
                    kind.to_string(),
                )));
                d.connected = connected;
            }
            if valid != d.valid {
                let _ = tx.send(Msg::Event(SignalEvent::at(
                    t,
                    Source::Api,
                    d.serial.clone(),
                    Kind::PoseValid(valid),
                    Kind::PoseValid(valid).to_string(),
                )));
                d.valid = valid;
            }
            if state != d.state {
                let kind = Kind::TrackingState {
                    from: d.state,
                    to: state,
                };
                let _ = tx.send(Msg::Event(SignalEvent::at(
                    t,
                    Source::Api,
                    d.serial.clone(),
                    kind.clone(),
                    kind.to_string(),
                )));
                d.state = state;
            }
            if poll_battery && connected {
                let pct = system
                    .get_tracked_device_property_f32(
                        TrackedDeviceIndex(i as u32),
                        openvr::property::DeviceBatteryPercentage_Float.0,
                    )
                    .ok()
                    .map(|b| b * 100.0);
                if let Some(pct) = pct {
                    let changed = d
                        .battery_pct
                        .map(|old| (old - pct).abs() >= 5.0)
                        .unwrap_or(true);
                    if changed {
                        d.battery_pct = Some(pct);
                        let _ = tx.send(Msg::Battery { idx: i, pct });
                        let _ = tx.send(Msg::Event(SignalEvent::new(
                            Source::Api,
                            d.serial.clone(),
                            Kind::BatteryLevel { pct },
                            format!("battery {pct:.0}%"),
                        )));
                    }
                }
            }
            if connected && (!d.base || base_frame) {
                let m = raw.mDeviceToAbsoluteTracking.m;
                let av = raw.vAngularVelocity.v;
                let _ = tx.send(Msg::Frame(FrameSample {
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

fn room_code(state: ChaperoneCalibrationState) -> u32 {
    use openvr::chaperone::{
        ChaperoneCalibrationErrorState as E, ChaperoneCalibrationWarningState as W,
    };
    match state {
        ChaperoneCalibrationState::Ok => 1,
        ChaperoneCalibrationState::Warning(w) => match w {
            W::Warning => 100,
            W::BaseStationMayHaveMoved => 101,
            W::BaseStationRemoved => 102,
            W::SeatedBoundsInvalid => 103,
            W::Unknown(n) => n,
        },
        ChaperoneCalibrationState::Error(e) => match e {
            E::Error => 200,
            E::BaseStationUninitialized => 201,
            E::BaseStationConflict => 202,
            E::PlayAreaInvalid | E::CollisionBoundsInvalid => 203,
            E::Unknown(n) => n,
        },
        ChaperoneCalibrationState::Unknown(n) => n,
    }
}

fn check_update(system: &openvr::System, i: usize, d: &mut DevState, tx: &Sender<Msg>) {
    if d.base || !d.interesting {
        return;
    }
    let idx = TrackedDeviceIndex(i as u32);
    let available = system
        .bool_tracked_device_property(idx, openvr::property::Firmware_UpdateAvailable_Bool)
        .unwrap_or(false);
    let manual = available
        && system
            .bool_tracked_device_property(idx, openvr::property::Firmware_ManualUpdate_Bool)
            .unwrap_or(false);
    let now = available.then_some(manual);
    if now == d.update {
        return;
    }
    d.update = now;
    let kind = Kind::UpdateAvailable {
        manual: now.unwrap_or(false),
        waiting: now.is_some(),
    };
    let _ = tx.send(Msg::Event(SignalEvent::new(
        Source::Api,
        d.serial.clone(),
        kind.clone(),
        kind.to_string(),
    )));
}

fn dump(system: &openvr::System, i: usize, serial: &str) {
    use openvr::property as p;
    let idx = TrackedDeviceIndex(i as u32);
    let mut out: Vec<String> = Vec::new();
    for (name, key) in [
        ("manufacturer", p::ManufacturerName_String),
        ("tracking firmware", p::TrackingFirmwareVersion_String),
        ("hardware revision", p::HardwareRevision_String),
        ("dongle", p::ConnectedWirelessDongle_String),
        ("programming target", p::Firmware_ProgrammingTarget_String),
        ("mode label", p::ModeLabel_String),
        ("render model", p::RenderModelName_String),
        ("manual update url", p::Firmware_ManualUpdateURL_String),
    ] {
        let v = prop(system, i, key);
        if !v.is_empty() {
            out.push(format!("{name}={v}"));
        }
    }
    for (name, key) in [
        ("firmware", p::FirmwareVersion_Uint64),
        ("hardware", p::HardwareRevision_Uint64),
        ("fpga", p::FPGAVersion_Uint64),
        ("vrc", p::VRCVersion_Uint64),
        ("radio", p::RadioVersion_Uint64),
        ("dongle version", p::DongleVersion_Uint64),
    ] {
        if let Ok(v) = system.uint64_tracked_device_property(idx, key) {
            out.push(format!("{name}={v}"));
        }
    }
    for (name, key) in [
        ("wireless", p::DeviceIsWireless_Bool),
        ("charging", p::DeviceIsCharging_Bool),
        ("battery reported", p::DeviceProvidesBatteryStatus_Bool),
        ("update available", p::Firmware_UpdateAvailable_Bool),
        ("manual update", p::Firmware_ManualUpdate_Bool),
        ("update required", p::Firmware_ForceUpdateRequired_Bool),
        ("can power off", p::DeviceCanPowerOff_Bool),
    ] {
        if let Ok(v) = system.bool_tracked_device_property(idx, key) {
            out.push(format!("{name}={v}"));
        }
    }
    crate::dlog!("device {i} {serial}: {}", out.join(", "));
}

fn prop(system: &openvr::System, i: usize, p: openvr::TrackedDeviceProperty) -> String {
    system
        .string_tracked_device_property(TrackedDeviceIndex(i as u32), p)
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn announce(system: &openvr::System, i: usize, d: &mut DevState, tx: &Sender<Msg>) {
    let class = match system.tracked_device_class(TrackedDeviceIndex(i as u32)) {
        TrackedDeviceClass::HMD => DeviceClass::Hmd,
        TrackedDeviceClass::Controller => DeviceClass::Controller,
        TrackedDeviceClass::GenericTracker => DeviceClass::GenericTracker,
        TrackedDeviceClass::TrackingReference => DeviceClass::TrackingReference,
        _ => DeviceClass::Other,
    };
    d.interesting = class != DeviceClass::Other;
    d.base = class == DeviceClass::TrackingReference;
    if !d.interesting {
        return;
    }
    let tracking_system = prop(system, i, openvr::property::TrackingSystemName_String);
    let mut serial = prop(system, i, openvr::property::SerialNumber_String);
    if serial.is_empty() {
        serial = format!("dev-{i}");
    }
    d.serial = Some(serial.clone());
    let battery = system
        .get_tracked_device_property_f32(
            TrackedDeviceIndex(i as u32),
            openvr::property::DeviceBatteryPercentage_Float.0,
        )
        .ok()
        .map(|b| b * 100.0);
    d.battery_pct = battery;
    if crate::DEV {
        dump(system, i, &serial);
    }
    let _ = tx.send(Msg::Device(DeviceMeta {
        idx: i,
        serial,
        model: prop(system, i, openvr::property::ModelNumber_String),
        class,
        dongle: prop(system, i, openvr::property::ConnectedWirelessDongle_String),
        battery_pct: battery,
        is_lighthouse: tracking_system.contains("lighthouse"),
    }));
    check_update(system, i, d, tx);
}

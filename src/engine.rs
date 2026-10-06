use crate::correlate::{Correlator, Verdict};
use crate::detect::{DevDetect, Frame, Thresholds, beyond_radius};
use crate::event::{DeviceClass, Kind, SignalEvent, Source, TrackState};
use crate::names::Names;
use crate::report::SessionWriter;
use crate::signals::openvr::{DeviceMeta, FrameSample};
use crate::signals::usb::{UsbDongle, crowding};
use crate::summary::{self, DeviceRecord, RADIO_OFF_MS, RadioStats, Summary};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

pub enum Msg {
    Event(SignalEvent),
    Frame(FrameSample),
    Device(DeviceMeta),
    Battery { idx: usize, pct: f32 },
    Usb(Vec<UsbDongle>),
    Status(String),
    SteamVrExited,
}

#[derive(Clone, Copy)]
pub struct DevLive {
    pub state: TrackState,
    pub valid: bool,
    pub connected: bool,
    pub parked: bool,
    pub pos: [f32; 3],
}

pub const SNAPSHOT_MS: u64 = 30_000;

pub struct Engine {
    pub thresholds: Thresholds,
    pub correlator: Correlator,
    pub detectors: HashMap<usize, DevDetect>,
    pub meta: HashMap<usize, DeviceMeta>,
    pub session: SessionWriter,
    pub live: HashMap<usize, DevLive>,
    pub devices: Vec<DeviceRecord>,
    pub usb: Vec<UsbDongle>,
    pub radio: BTreeMap<String, RadioStats>,
    pub names: Names,
    pub notices: Vec<String>,
    incidents: HashMap<String, usize>,
    dirty: bool,
    last_snapshot: u64,
    last_flaps: u64,
}

impl Engine {
    pub fn new(session: SessionWriter, names: Names) -> Self {
        Self {
            thresholds: Thresholds::default(),
            correlator: Correlator::default(),
            detectors: HashMap::new(),
            meta: HashMap::new(),
            session,
            live: HashMap::new(),
            devices: Vec::new(),
            usb: Vec::new(),
            radio: BTreeMap::new(),
            names,
            notices: Vec::new(),
            incidents: HashMap::new(),
            dirty: false,
            last_snapshot: 0,
            last_flaps: 0,
        }
    }

    fn device_for_dongle(&self, dongle: &str) -> Option<String> {
        self.devices
            .iter()
            .find(|m| !m.dongle.is_empty() && m.dongle == dongle)
            .map(|m| m.serial.clone())
    }

    fn attribute_dongle(&self, ev: &mut SignalEvent) {
        if ev.device.is_some() {
            return;
        }
        let dongle = match &ev.kind {
            Kind::UsbRemove {
                dongle: Some(d), ..
            }
            | Kind::UsbAttach {
                dongle: Some(d), ..
            } => d.clone(),
            Kind::RadioGap { dongle, .. } => dongle.clone(),
            _ => return,
        };
        if let Some(serial) = self.device_for_dongle(&dongle) {
            if matches!(ev.kind, Kind::UsbRemove { .. } | Kind::UsbAttach { .. }) {
                ev.detail = format!("{} (dongle of {serial})", ev.detail);
            }
            ev.device = Some(serial);
        }
    }

    pub fn set_devices(&mut self, devices: Vec<DeviceRecord>) {
        if let Some(h) = devices.iter().find(|d| d.class == DeviceClass::Hmd) {
            self.correlator.set_hmd(h.serial.clone());
        }
        self.devices = devices;
    }

    fn ingest(&mut self, ev: SignalEvent, emitted: &mut Vec<SignalEvent>) {
        if let Kind::RadioGap { dongle, ms } = &ev.kind {
            let r = self.radio.entry(dongle.clone()).or_default();
            if *ms < RADIO_OFF_MS {
                r.count += 1;
                r.max_ms = r.max_ms.max(*ms);
            }
        }
        self.session.event(&ev);
        self.correlator.ingest(&ev);
        emitted.push(ev);
    }

    pub fn handle(&mut self, msg: Msg) -> Vec<SignalEvent> {
        let mut emitted = Vec::new();
        match msg {
            Msg::Event(mut ev) => {
                self.attribute_dongle(&mut ev);
                if ev.kind == Kind::DeviceDeactivated
                    && let Some((idx, _)) = self
                        .meta
                        .iter()
                        .find(|(_, m)| Some(m.serial.as_str()) == ev.device.as_deref())
                    && let Some(l) = self.live.get_mut(idx)
                {
                    l.connected = false;
                }
                self.ingest(ev, &mut emitted);
            }
            Msg::Frame(f) => {
                let Some(meta) = self.meta.get(&f.idx) else {
                    return emitted;
                };
                self.session.pose(&meta.serial, &f);
                let parked = meta.class != DeviceClass::TrackingReference
                    && beyond_radius(f.pos, &self.thresholds);
                let was_parked = self.live.get(&f.idx).is_some_and(|l| l.parked);
                let serial = meta.serial.clone();
                let class = meta.class;
                let lighthouse = meta.is_lighthouse;
                self.live.insert(
                    f.idx,
                    DevLive {
                        state: f.state,
                        valid: f.valid,
                        connected: true,
                        parked,
                        pos: f.pos,
                    },
                );
                if parked != was_parked {
                    let kind = Kind::Parked(parked);
                    let detail = kind.to_string();
                    let ev =
                        SignalEvent::at(f.t_ms, Source::Api, Some(serial.clone()), kind, detail);
                    self.ingest(ev, &mut emitted);
                }
                let run_detectors = match class {
                    DeviceClass::Controller | DeviceClass::GenericTracker => lighthouse,
                    DeviceClass::Hmd => true,
                    _ => false,
                };
                if !run_detectors {
                    return emitted;
                }
                let frame = Frame {
                    t_ms: f.t_ms,
                    pos: f.pos,
                    vel: f.vel,
                    rot: f.rot,
                    ang_speed: f.ang_speed,
                    state: f.state,
                    valid: f.valid,
                };
                let is_hmd = class == DeviceClass::Hmd;
                let kinds = self
                    .detectors
                    .entry(f.idx)
                    .or_default()
                    .step(&frame, &self.thresholds);
                for kind in kinds {
                    if is_hmd && !matches!(kind, Kind::Jump { .. }) {
                        continue;
                    }
                    let detail = kind.to_string();
                    let ev =
                        SignalEvent::at(f.t_ms, Source::Api, Some(serial.clone()), kind, detail);
                    self.ingest(ev, &mut emitted);
                }
            }
            Msg::Device(meta) => {
                self.detectors
                    .entry(meta.idx)
                    .or_default()
                    .on_activated(crate::event::now_ms());
                let rec = DeviceRecord {
                    serial: meta.serial.clone(),
                    model: meta.model.clone(),
                    class: meta.class,
                    dongle: meta.dongle.clone(),
                    lighthouse: meta.is_lighthouse,
                };
                let mut devices = self.devices.clone();
                match devices.iter_mut().find(|d| d.serial == rec.serial) {
                    Some(d) if *d == rec => {}
                    Some(d) => *d = rec,
                    None => devices.push(rec),
                }
                if devices != self.devices {
                    self.session.set_devices(&devices);
                    self.set_devices(devices);
                    self.dirty = true;
                }
                self.meta.insert(meta.idx, meta);
            }
            Msg::Battery { idx, pct } => {
                if let Some(m) = self.meta.get_mut(&idx) {
                    m.battery_pct = Some(pct);
                }
            }
            Msg::Usb(dongles) => {
                if dongles != self.usb {
                    let before = crowding(&self.usb);
                    self.notices.extend(
                        crowding(&dongles)
                            .into_iter()
                            .filter(|w| !before.contains(w)),
                    );
                    self.session.set_usb(&dongles);
                    self.usb = dongles;
                    self.dirty = true;
                }
            }
            Msg::Status(_) | Msg::SteamVrExited => {}
        }
        emitted
    }

    pub fn tick(&mut self, now_ms: u64) -> Vec<Verdict> {
        let verdicts = self.correlator.tick(now_ms);
        for v in &verdicts {
            *self.incidents.entry(v.device.clone()).or_default() += 1;
            self.session.verdict(v);
            self.dirty = true;
        }
        verdicts
    }

    pub fn incident_count(&self, serial: &str) -> usize {
        self.incidents.get(serial).copied().unwrap_or(0)
    }

    pub fn label(&self, serial: &str) -> String {
        summary::label(serial, &self.devices, &self.names)
    }

    pub fn summary(&self) -> Summary {
        summary::build(
            self.session.verdicts(),
            &self.correlator.flap_summary(),
            &self.devices,
            self.correlator.parked(),
            &self.names,
        )
    }

    pub fn report_text(&self) -> String {
        crate::report::compose(self, &self.summary())
    }

    pub fn snapshot(&mut self, now_ms: u64) {
        if now_ms.saturating_sub(self.last_snapshot) < SNAPSHOT_MS {
            return;
        }
        let flaps = self.correlator.flap_summary();
        let total: u64 = flaps.devices.values().map(|f| f.count).sum();
        if !self.dirty && total == self.last_flaps {
            return;
        }
        self.last_snapshot = now_ms;
        self.last_flaps = total;
        self.dirty = false;
        let text = self.report_text();
        let _ = self.session.write_report(&text, &flaps);
    }

    pub fn finish(&mut self) -> anyhow::Result<PathBuf> {
        let text = self.report_text();
        self.session
            .write_report(&text, &self.correlator.flap_summary())
    }
}

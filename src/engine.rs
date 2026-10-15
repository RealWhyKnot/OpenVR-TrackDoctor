use crate::bases::{self, BaseMoves, BaseRecord};
use crate::correlate::{Correlator, Verdict};
use crate::detect::{DevDetect, Frame, Thresholds, beyond_radius};
use crate::diagnose::Diagnosis;
use crate::event::{DeviceClass, Kind, SignalEvent, Source, TrackState};
use crate::names::Names;
use crate::report::SessionWriter;
use crate::signals::openvr::{DeviceMeta, FrameSample};
use crate::signals::packets::{self, Batch, Totals};
use crate::signals::usb::{UsbDongle, crowding};
use crate::summary::{self, DeviceRecord, RADIO_OFF_MS, RadioStats, Summary};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;

pub enum Msg {
    Event(SignalEvent),
    Frame(FrameSample),
    Device(DeviceMeta),
    Battery { idx: usize, pct: f32 },
    Usb(Vec<UsbDongle>),
    BaseDb(Vec<BaseRecord>),
    Status(String),
    Packets(Batch),
    RawLog(Vec<String>),
    SteamVrExited,
}

#[derive(Clone, Copy)]
pub struct DevLive {
    pub state: TrackState,
    pub valid: bool,
    pub connected: bool,
    pub parked: bool,
    pub pos: [f32; 3],
    pub rot: [[f32; 4]; 3],
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub struct BasePose {
    pub serial: String,
    pub model: String,
    pub pos: [f32; 3],
    pub rot: [[f32; 4]; 3],
}

const TRAIL_EVERY_MS: u64 = 250;
const TRAIL_KEEP_MS: u64 = 600_000;

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
    pub packets: BTreeMap<String, Totals>,
    pub names: Names,
    pub notices: Vec<String>,
    pub base_moves: BaseMoves,
    pub base_db: Vec<BaseRecord>,
    pub diagnosis: Diagnosis,
    incidents: HashMap<String, usize>,
    trail: HashMap<String, VecDeque<(u64, [f32; 3])>>,
    dirty: bool,
    last_snapshot: u64,
    last_flaps: u64,
    packets_dirty: bool,
    packets_saved: u64,
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
            packets: BTreeMap::new(),
            names,
            notices: Vec::new(),
            base_moves: BaseMoves::default(),
            base_db: Vec::new(),
            diagnosis: Diagnosis::default(),
            incidents: HashMap::new(),
            trail: HashMap::new(),
            dirty: false,
            last_snapshot: 0,
            last_flaps: 0,
            packets_dirty: false,
            packets_saved: 0,
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
            Kind::RadioGap { dongle, .. } | Kind::BoardUnreachable { dongle } => dongle.clone(),
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
        if let Some(m) = self.base_moves.ingest(&ev) {
            self.notices.push(bases::notice(&m));
        }
        if matches!(ev.kind, Kind::BaseMoved { .. } | Kind::BaseMovedForTracking) {
            self.dirty = true;
        }
        if self.diagnosis.ingest(&ev) {
            let tag = |s: &str| self.tag(s);
            let found = self.diagnosis.findings(&tag);
            let fresh = self.diagnosis.fresh(found);
            self.notices.extend(fresh.iter().map(|f| f.line()));
            self.dirty = true;
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
                self.session.pose(&meta.serial, meta.class, &f);
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
                        rot: f.rot,
                    },
                );
                if f.valid && f.state == TrackState::RunningOk && !parked {
                    let trail = self.trail.entry(serial.clone()).or_default();
                    if trail
                        .back()
                        .is_none_or(|(t, _)| f.t_ms >= t + TRAIL_EVERY_MS)
                    {
                        trail.push_back((f.t_ms, f.pos));
                        while trail
                            .front()
                            .is_some_and(|(t, _)| f.t_ms.saturating_sub(*t) > TRAIL_KEEP_MS)
                        {
                            trail.pop_front();
                        }
                    }
                }
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
            Msg::BaseDb(db) => {
                if db != self.base_db {
                    self.session.set_bases(&db);
                    self.base_db = db;
                    self.dirty = true;
                }
            }
            Msg::Packets(batch) => {
                self.session.packets(&batch.lines);
                for l in &batch.lines {
                    let d = packets::dongle_of(l);
                    self.packets
                        .entry(d.to_string())
                        .or_insert_with(|| Totals {
                            dongle: d.to_string(),
                            ..Totals::default()
                        })
                        .add(l);
                }
                self.packets_dirty = true;
            }
            Msg::RawLog(lines) => self.session.raw_log(&lines),
            Msg::Status(_) | Msg::SteamVrExited => {}
        }
        emitted
    }

    pub fn position_at(&self, serial: &str, t_ms: u64) -> Option<[f32; 3]> {
        self.trail
            .get(serial)?
            .iter()
            .rev()
            .find(|(t, _)| *t <= t_ms)
            .map(|(_, p)| *p)
    }

    pub fn tick(&mut self, now_ms: u64) -> Vec<Verdict> {
        let mut verdicts = self.correlator.tick(now_ms);
        for v in &mut verdicts {
            if v.pos.is_none() {
                v.pos = self.position_at(&v.device, v.t_start_ms);
            }
        }
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

    pub fn tag(&self, serial: &str) -> String {
        match self.label(serial) {
            l if l.is_empty() => serial.to_string(),
            l => format!("{serial} ({l})"),
        }
    }

    pub fn summary(&self) -> Summary {
        let mut s = summary::build(
            self.session.verdicts(),
            &self.correlator.flap_summary(),
            &self.devices,
            self.correlator.parked(),
            &self.names,
        );
        s.base_warnings = bases::warnings(&self.base_moves, &self.base_db);
        s.diagnosis = self.diagnosis.findings(&|d| self.tag(d));
        s.setup = self.diagnosis.setup_notes();
        s
    }

    pub fn base_poses(&self) -> Vec<BasePose> {
        let mut out: Vec<BasePose> = self
            .meta
            .iter()
            .filter(|(_, m)| m.class == DeviceClass::TrackingReference)
            .filter_map(|(idx, m)| {
                let l = self.live.get(idx)?;
                Some(BasePose {
                    serial: m.serial.clone(),
                    model: m.model.clone(),
                    pos: l.pos,
                    rot: l.rot,
                })
            })
            .collect();
        out.sort_by(|a, b| a.serial.cmp(&b.serial));
        out
    }

    fn save_room(&self) {
        let poses = self.base_poses();
        if !poses.is_empty() {
            self.session.set_room(&poses);
        }
    }

    pub fn report_text(&self) -> String {
        crate::report::compose(self, &self.summary())
    }

    fn save_packets(&mut self) {
        let mut all: Vec<Totals> = self.packets.values().cloned().collect();
        for t in &mut all {
            t.device = self.device_for_dongle(&t.dongle);
        }
        self.session.set_packets(&all);
        self.packets_dirty = false;
    }

    pub fn snapshot(&mut self, now_ms: u64) {
        if self.packets_dirty && now_ms.saturating_sub(self.packets_saved) >= SNAPSHOT_MS {
            self.packets_saved = now_ms;
            self.save_packets();
        }
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
        if let Err(e) = self.session.write_report(&text, &flaps) {
            crate::dlog!("couldn't write the report: {e}");
        }
        self.save_room();
    }

    pub fn finish(&mut self) -> anyhow::Result<PathBuf> {
        self.save_room();
        if !self.packets.is_empty() {
            self.save_packets();
        }
        let text = self.report_text();
        self.session
            .write_report(&text, &self.correlator.flap_summary())
    }
}

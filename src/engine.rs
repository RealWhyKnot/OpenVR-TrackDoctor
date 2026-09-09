use crate::correlate::{Correlator, Verdict};
use crate::detect::{DevDetect, Frame, Thresholds};
use crate::event::{DeviceClass, Kind, SignalEvent, Source, TrackState};
use crate::report::SessionWriter;
use crate::signals::openvr::{DeviceMeta, FrameSample};
use std::collections::HashMap;

pub enum Msg {
    Event(SignalEvent),
    Frame(FrameSample),
    Device(DeviceMeta),
    Battery { idx: usize, pct: f32 },
    Status(String),
}

#[derive(Clone, Copy)]
pub struct DevLive {
    pub state: TrackState,
    pub valid: bool,
    pub connected: bool,
}

pub struct Engine {
    pub thresholds: Thresholds,
    pub correlator: Correlator,
    pub detectors: HashMap<usize, DevDetect>,
    pub meta: HashMap<usize, DeviceMeta>,
    pub session: SessionWriter,
    pub live: HashMap<usize, DevLive>,
}

impl Engine {
    pub fn new(session: SessionWriter) -> Self {
        Self {
            thresholds: Thresholds::default(),
            correlator: Correlator::default(),
            detectors: HashMap::new(),
            meta: HashMap::new(),
            session,
            live: HashMap::new(),
        }
    }

    fn attribute_usb(&self, ev: &mut SignalEvent) {
        let dongle = match &ev.kind {
            Kind::UsbRemove {
                dongle: Some(d), ..
            }
            | Kind::UsbAttach {
                dongle: Some(d), ..
            } => d.clone(),
            _ => return,
        };
        if let Some(m) = self
            .meta
            .values()
            .find(|m| !m.dongle.is_empty() && m.dongle == dongle)
        {
            ev.device = Some(m.serial.clone());
            ev.detail = format!("{} (dongle of {})", ev.detail, m.serial);
        }
    }

    pub fn handle(&mut self, msg: Msg) -> Vec<SignalEvent> {
        let mut emitted = Vec::new();
        match msg {
            Msg::Event(mut ev) => {
                self.attribute_usb(&mut ev);
                if ev.kind == Kind::DeviceDeactivated
                    && let Some((idx, _)) = self
                        .meta
                        .iter()
                        .find(|(_, m)| Some(m.serial.as_str()) == ev.device.as_deref())
                    && let Some(l) = self.live.get_mut(idx)
                {
                    l.connected = false;
                }
                self.session.event(&ev);
                self.correlator.ingest(&ev);
                emitted.push(ev);
            }
            Msg::Frame(f) => {
                let Some(meta) = self.meta.get(&f.idx) else {
                    return emitted;
                };
                self.live.insert(
                    f.idx,
                    DevLive {
                        state: f.state,
                        valid: f.valid,
                        connected: true,
                    },
                );
                let run_detectors = match meta.class {
                    DeviceClass::Controller | DeviceClass::GenericTracker => meta.is_lighthouse,
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
                let is_hmd = meta.class == DeviceClass::Hmd;
                let serial = meta.serial.clone();
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
                    let ev = SignalEvent::new(Source::Api, Some(serial.clone()), kind, detail);
                    self.session.event(&ev);
                    self.correlator.ingest(&ev);
                    emitted.push(ev);
                }
            }
            Msg::Device(meta) => {
                if meta.class == DeviceClass::Hmd {
                    self.correlator.set_hmd(meta.serial.clone());
                }
                self.detectors
                    .entry(meta.idx)
                    .or_default()
                    .on_activated(crate::event::now_ms());
                self.session.roster_line(&format!(
                    "{} {} class={:?} dongle={} lighthouse={}",
                    meta.serial, meta.model, meta.class, meta.dongle, meta.is_lighthouse
                ));
                self.meta.insert(meta.idx, meta);
            }
            Msg::Battery { idx, pct } => {
                if let Some(m) = self.meta.get_mut(&idx) {
                    m.battery_pct = Some(pct);
                }
            }
            Msg::Status(_) => {}
        }
        emitted
    }

    pub fn tick(&mut self, now_ms: u64) -> Vec<Verdict> {
        let verdicts = self.correlator.tick(now_ms);
        for v in &verdicts {
            self.session.verdict(v);
        }
        verdicts
    }
}

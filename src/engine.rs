use crate::correlate::{Correlator, Verdict};
use crate::detect::{DevDetect, Frame, Thresholds};
use crate::event::{Kind, SignalEvent, Source};
use crate::report::SessionWriter;
use crate::signals::openvr::{DeviceMeta, FrameSample};
use std::collections::HashMap;

pub enum Msg {
    Event(SignalEvent),
    Frame(FrameSample),
    Device(DeviceMeta),
    Status(String),
}

pub struct Engine {
    pub thresholds: Thresholds,
    pub correlator: Correlator,
    pub detectors: HashMap<usize, DevDetect>,
    pub meta: HashMap<usize, DeviceMeta>,
    pub session: SessionWriter,
    pub last_state: HashMap<usize, (String, bool)>,
}

impl Engine {
    pub fn new(session: SessionWriter) -> Self {
        Self {
            thresholds: Thresholds::default(),
            correlator: Correlator::default(),
            detectors: HashMap::new(),
            meta: HashMap::new(),
            session,
            last_state: HashMap::new(),
        }
    }

    pub fn handle(&mut self, msg: Msg) -> Vec<SignalEvent> {
        let mut emitted = Vec::new();
        match msg {
            Msg::Event(ev) => {
                if ev.kind == Kind::DeviceActivated
                    && let Some((idx, _)) = self.meta.iter().find(|(_, m)| Some(m.serial.as_str()) == ev.device.as_deref()) {
                        self.detectors.entry(*idx).or_default().on_activated(ev.t_ms);
                    }
                self.session.event(&ev);
                self.correlator.ingest(&ev);
                emitted.push(ev);
            }
            Msg::Frame(f) => {
                let Some(meta) = self.meta.get(&f.idx) else { return emitted };
                self.last_state.insert(f.idx, (format!("{:?}", f.state), f.valid));
                if !meta.is_lighthouse || !(meta.class == "Controller" || meta.class == "GenericTracker") {
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
                let serial = meta.serial.clone();
                let kinds = self.detectors.entry(f.idx).or_default().step(&frame, &self.thresholds);
                for kind in kinds {
                    let detail = match &kind {
                        Kind::Jump { meters, accel_mps2 } => format!("pose jump {meters:.3}m (implied accel {accel_mps2:.0} m/s^2)"),
                        Kind::OrientationJump { rad_per_s } => format!("orientation jump {rad_per_s:.0} rad/s"),
                        Kind::Drift { meters, secs } => format!("dead-reckoning drift {meters:.3}m over {secs:.1}s"),
                        Kind::SnapBack { meters } => format!("snap-back correction {meters:.3}m"),
                        k => format!("{k:?}"),
                    };
                    let ev = SignalEvent::new(Source::Api, Some(serial.clone()), kind, detail);
                    self.session.event(&ev);
                    self.correlator.ingest(&ev);
                    emitted.push(ev);
                }
            }
            Msg::Device(meta) => {
                self.detectors.entry(meta.idx).or_default().on_activated(crate::event::now_ms());
                self.meta.insert(meta.idx, meta);
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

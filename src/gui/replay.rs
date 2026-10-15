use super::present::{self, BAD, DIM, FAINT, GOOD, NORMAL, WARN};
use super::ui;
use crate::correlate::Verdict;
use crate::engine::{BasePose, Engine};
use crate::event::{DeviceClass, Kind, SignalEvent, Source, TrackState};
use crate::report::fmt_clock;
use crate::room::{self, BaseView, Bounds, DeviceView, DropView, Scene, Shape, Tone, V3};
use crate::summary::Summary;
use crate::track::Track;

pub const TRAIL_MS: u32 = 4_000;
pub const LEAD_MS: u32 = 3_000;
const FAR_M: f32 = 50.0;

#[derive(Debug)]
struct Who {
    shape: Shape,
    tone: Tone,
    label: String,
}

#[derive(Debug)]
struct Base {
    serial: String,
    view: BaseView,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Mark {
    pub t: u32,
    pub tone: i32,
}

#[derive(Debug)]
pub struct Replay {
    track: Track,
    pub length: u32,
    who: Vec<Option<Who>>,
    bases: Vec<Base>,
    events: Vec<(u32, ui::Line)>,
    pub marks: Vec<Mark>,
    drops: Vec<(u32, DropView)>,
    extent: Bounds,
}

fn rel(t_ms: u64, t0: u64) -> u32 {
    t_ms.saturating_sub(t0).min(u32::MAX as u64) as u32
}

fn far(p: V3) -> bool {
    (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt() > FAR_M
}

fn rot(q: [f32; 4]) -> room::Rot {
    let n = q.iter().map(|v| v * v).sum::<f32>().sqrt().max(1e-6);
    room::rot_from_quat(q[0] / n, q[1] / n, q[2] / n, q[3] / n)
}

pub fn event_text(ev: &SignalEvent) -> String {
    match (&ev.source, &ev.kind) {
        (Source::Log, _) => {
            let line = ev
                .detail
                .split_once("] - ")
                .map_or(ev.detail.as_str(), |(_, r)| r);
            format!("SteamVR log: {line}")
        }
        (_, Kind::UsbAttach { dongle, .. }) => {
            format!("dongle {} plugged in", dongle.as_deref().unwrap_or("?"))
        }
        (_, Kind::UsbRemove { dongle, .. }) => {
            format!("dongle {} unplugged", dongle.as_deref().unwrap_or("?"))
        }
        (_, Kind::OvrRecenter) => "the play space was recentered".into(),
        (Source::Api, k) => k.to_string(),
        _ => ev.detail.clone(),
    }
}

pub fn event_tone(ev: &SignalEvent) -> i32 {
    match &ev.kind {
        Kind::TrackingState { to, .. } if *to == TrackState::RunningOk => GOOD,
        Kind::TrackingState { .. }
        | Kind::PoseValid(false)
        | Kind::DeviceDeactivated
        | Kind::UsbRemove { .. } => WARN,
        Kind::Jump { .. }
        | Kind::OrientationJump { .. }
        | Kind::Drift { .. }
        | Kind::SnapBack { .. }
        | Kind::PoseFrozen { .. }
        | Kind::RadioGap { .. } => BAD,
        Kind::PoseValid(true) | Kind::DeviceActivated | Kind::UsbAttach { .. } => GOOD,
        _ if ev.source == Source::Log => DIM,
        _ => NORMAL,
    }
}

pub fn event_line(ev: &SignalEvent, label: &str, t0: u64) -> ui::Line {
    let who = match (ev.device.as_deref(), label) {
        (Some(s), "") => format!("{s}: "),
        (Some(s), l) => format!("{s} {l}: "),
        (None, _) => String::new(),
    };
    present::line(
        format!(
            "{} {who}{}",
            fmt_clock(ev.t_ms.saturating_sub(t0)),
            event_text(ev)
        ),
        event_tone(ev),
    )
}

impl Replay {
    pub fn build(
        track: Track,
        engine: &Engine,
        s: &Summary,
        events: &[SignalEvent],
        verdicts: &[Verdict],
        poses: &[BasePose],
    ) -> Replay {
        let t0 = track.t0;
        let who = track
            .devices
            .iter()
            .map(|d| {
                let shape = match d.class {
                    DeviceClass::Hmd => Shape::Headset,
                    DeviceClass::Controller => Shape::Controller,
                    DeviceClass::GenericTracker => Shape::Tracker,
                    _ => return None,
                };
                let name = engine.label(&d.serial);
                let shown = if name.is_empty() {
                    d.serial.clone()
                } else {
                    name
                };
                let (tone, label) = if shape == Shape::Headset {
                    (Tone::Headset, "Headset".to_string())
                } else {
                    let tone = present::device_tone(s, &d.serial);
                    let label = match s.rank(&d.serial) {
                        Some(r) if tone != Tone::Unknown => format!("#{r} {shown}"),
                        _ => shown,
                    };
                    (tone, label)
                };
                Some(Who { shape, tone, label })
            })
            .collect();
        let bases = present::base_views(poses)
            .into_iter()
            .zip(poses)
            .map(|(view, p)| Base {
                serial: p.serial.clone(),
                view,
            })
            .collect::<Vec<_>>();
        let mut lines = events
            .iter()
            .map(|e| {
                let label = e
                    .device
                    .as_deref()
                    .map(|d| engine.label(d))
                    .unwrap_or_default();
                (rel(e.t_ms, t0), event_line(e, &label, t0))
            })
            .collect::<Vec<_>>();
        lines.sort_by_key(|(t, _)| *t);
        let mut verdicts = verdicts.to_vec();
        verdicts.sort_by_key(|v| v.t_start_ms);
        let verdicts = verdicts.as_slice();
        let marks = verdicts
            .iter()
            .map(|v| Mark {
                t: rel(v.t_start_ms, t0),
                tone: match (present::quiet(v), v.pose_valid) {
                    (true, _) => FAINT,
                    (false, Some(false)) => BAD,
                    _ => WARN,
                },
            })
            .collect();
        let shown: Vec<&Verdict> = verdicts.iter().filter(|v| !present::quiet(v)).collect();
        let drops = shown
            .iter()
            .filter_map(|v| {
                let d = present::drops(std::slice::from_ref(*v)).pop()?;
                Some((rel(v.t_start_ms, t0), d))
            })
            .collect::<Vec<(u32, DropView)>>();
        let mut pts: Vec<V3> = bases.iter().map(|b| b.view.pos).collect();
        pts.extend(drops.iter().map(|(_, d)| d.pos));
        for d in &track.devices {
            if d.class == DeviceClass::TrackingReference {
                continue;
            }
            pts.extend(
                d.samples
                    .iter()
                    .filter(|s| s.tracking() && !far(s.pos))
                    .map(|s| s.pos),
            );
        }
        let extent = Bounds::around(&pts);
        let end = lines.last().map_or(0, |(t, _)| *t);
        Replay {
            length: track.end().max(end).max(1),
            track,
            who,
            bases,
            events: lines,
            marks,
            drops,
            extent,
        }
    }

    pub fn scene(&self, t: u32) -> Scene {
        let bases = self
            .bases
            .iter()
            .map(|b| {
                let mut view = b.view.clone();
                if let Some(s) = self.track.device(&b.serial).and_then(|d| d.at(t)) {
                    view.pos = s.pos;
                    view.rot = rot(s.q);
                }
                view
            })
            .collect();
        let mut devices = Vec::new();
        for (d, who) in self.track.devices.iter().zip(&self.who) {
            let Some(who) = who else {
                continue;
            };
            let Some(s) = d.at(t) else {
                continue;
            };
            if far(s.pos) {
                continue;
            }
            let tracking = s.tracking();
            let mut label = who.label.clone();
            if !tracking {
                label.push_str(" ?");
            }
            devices.push(DeviceView {
                pos: s.pos,
                rot: rot(s.q),
                shape: who.shape,
                tone: who.tone,
                label,
                tracking,
                trail: d.path(t.saturating_sub(TRAIL_MS), t),
            });
        }
        let drops = self
            .drops
            .iter()
            .take_while(|(at, _)| *at <= t)
            .map(|(_, d)| d.clone())
            .collect();
        Scene {
            bases,
            devices,
            drops,
            fixed: Some(self.extent),
        }
    }

    pub fn near(&self, t: u32, before: usize, after: usize) -> Vec<ui::Line> {
        let i = self.events.partition_point(|(at, _)| *at <= t);
        let from = i.saturating_sub(before);
        let to = (i + after).min(self.events.len());
        self.events[from..to]
            .iter()
            .enumerate()
            .map(|(k, (_, l))| {
                let mut l = l.clone();
                if from + k >= i {
                    l.tone = DIM;
                }
                l
            })
            .collect()
    }

    pub fn seek(&self, fraction: f32, snap: f32) -> u32 {
        let t = (fraction.clamp(0.0, 1.0) * self.length as f32) as u32;
        let reach = (snap.max(0.0) * self.length as f32) as u32;
        let near = self
            .marks
            .iter()
            .map(|m| m.t)
            .filter(|m| m.abs_diff(t) <= reach)
            .min_by_key(|m| m.abs_diff(t));
        match near {
            Some(m) => m.saturating_sub(LEAD_MS),
            None => t,
        }
    }

    pub fn clock(&self, t: u32) -> String {
        format!(
            "{} / {}",
            fmt_clock(t as u64),
            fmt_clock(self.length as u64).trim_start_matches('+')
        )
    }
}

use super::ui;
use crate::correlate::{Cause, Confidence, Verdict};
use crate::engine::{BasePose, Engine};
use crate::event::{DeviceClass, Kind, SignalEvent, TrackState};
use crate::report::{fmt_clock, incident};
use crate::room::{self, BaseView, DeviceView, DropView, Scene, Shape, Tone};
use crate::signals::usb::crowding;
use crate::summary::{self, Level, Summary, TreeCtx, fmt_dur};

pub const NORMAL: i32 = 0;
pub const GOOD: i32 = 1;
pub const WARN: i32 = 2;
pub const BAD: i32 = 3;
pub const FAINT: i32 = 4;
pub const BASE: i32 = 5;
pub const DIM: i32 = 6;

pub fn level_tone(l: Level) -> i32 {
    match l {
        Level::Head => BASE,
        Level::Plain => NORMAL,
        Level::Good => GOOD,
        Level::Warn => WARN,
        Level::Bad => BAD,
        Level::Dim => DIM,
    }
}

pub fn room_tone(t: Tone) -> i32 {
    match t {
        Tone::Base => BASE,
        Tone::Headset => NORMAL,
        Tone::Clean => GOOD,
        Tone::Some => WARN,
        Tone::Worst => BAD,
        Tone::Unknown => DIM,
    }
}

pub fn line(text: impl Into<slint::SharedString>, tone: i32) -> ui::Line {
    ui::Line {
        text: text.into(),
        tone,
    }
}

fn who(label: &str, serial: &str) -> String {
    if label.is_empty() {
        serial.to_string()
    } else {
        format!("{serial} {label}")
    }
}

pub fn quiet(v: &Verdict) -> bool {
    v.cause == Cause::Unknown && v.confidence == Confidence::Low
}

pub fn verdict_lines(v: &Verdict, label: &str, t0: u64) -> Vec<ui::Line> {
    let quiet = quiet(v);
    let mut out = vec![line(
        format!(
            "{} {}: {}{}",
            fmt_clock(v.t_start_ms.saturating_sub(t0)),
            who(label, &v.device),
            crate::report::cause_heading(v),
            incident(v)
        ),
        if quiet { NORMAL } else { WARN },
    )];
    if !quiet {
        out.push(line(format!("    {}", v.cause.describe()), DIM));
        for e in v.evidence.iter().take(3) {
            out.push(line(format!("      {e}"), DIM));
        }
    }
    out
}

pub fn anomaly_line(ev: &SignalEvent, label: &str, t0: u64) -> Option<ui::Line> {
    let anomaly = matches!(
        ev.kind,
        Kind::Jump { .. }
            | Kind::OrientationJump { .. }
            | Kind::Drift { .. }
            | Kind::SnapBack { .. }
            | Kind::PoseFrozen { .. }
            | Kind::Parked(_)
    );
    if !anomaly {
        return None;
    }
    let serial = ev.device.as_deref().unwrap_or("?");
    Some(line(
        format!(
            "{} {}: {}",
            fmt_clock(ev.t_ms.saturating_sub(t0)),
            who(label, serial),
            ev.detail
        ),
        DIM,
    ))
}

pub fn devices(engine: &Engine) -> Vec<ui::Device> {
    let mut metas: Vec<_> = engine.meta.iter().collect();
    metas.sort_by_key(|(idx, m)| (m.class == DeviceClass::TrackingReference, **idx));
    metas
        .into_iter()
        .map(|(idx, m)| {
            let live = engine.live.get(idx).copied();
            let base = m.class == DeviceClass::TrackingReference;
            let (state, tone) = match live {
                None => ("-".to_string(), DIM),
                Some(l) if !l.connected => ("off".to_string(), DIM),
                Some(l) if l.parked => ("parked, ignored".to_string(), DIM),
                Some(_) if base => ("on".to_string(), DIM),
                Some(l) => (
                    l.state.plain(),
                    match l.state {
                        TrackState::RunningOk => GOOD,
                        TrackState::Uninitialized => DIM,
                        _ => BAD,
                    },
                ),
            };
            let pose = match live {
                Some(l) if !base && l.connected => {
                    if l.valid {
                        "ok"
                    } else {
                        "lost"
                    }
                }
                _ => "",
            };
            let counted = !(base || m.class == DeviceClass::Hmd);
            ui::Device {
                serial: m.serial.clone().into(),
                name: engine.label(&m.serial).into(),
                state: state.into(),
                tone,
                pose: pose.into(),
                battery: m
                    .battery_pct
                    .filter(|_| !base)
                    .map(|b| format!("{b:.0}%"))
                    .unwrap_or_default()
                    .into(),
                flaps: if counted {
                    engine.correlator.flap_count(&m.serial).to_string()
                } else {
                    String::new()
                }
                .into(),
                incidents: if counted {
                    engine.incident_count(&m.serial).to_string()
                } else {
                    String::new()
                }
                .into(),
                dongle: m.dongle.clone().into(),
            }
        })
        .collect()
}

#[derive(Clone, Debug, Default)]
pub struct ReportView {
    pub findings: Vec<CardView>,
    pub heading: String,
    pub ranks: Vec<ui::Rank>,
    pub notes: Vec<ui::Line>,
    pub bases: Vec<ui::Line>,
    pub empty: String,
}

fn rank_tone(s: &Summary, i: usize) -> i32 {
    match s.rows.get(i) {
        Some(r) if !r.trouble() => GOOD,
        Some(r) if i < 3 && r.lost_ms > 0 => BAD,
        Some(_) => WARN,
        None => NORMAL,
    }
}

pub fn base_report(text: &str) -> Vec<ui::Line> {
    text.lines()
        .map(|l| {
            let tone = if !l.starts_with(' ') {
                DIM
            } else if l.trim_start().starts_with("base ") {
                FAINT
            } else if l.contains("loose") || l.contains("changed") {
                WARN
            } else {
                NORMAL
            };
            line(l.to_string(), tone)
        })
        .collect()
}

pub fn report(engine: &Engine, s: &Summary, finished: bool) -> ReportView {
    let span = s.span_ms.max(1);
    let ranks = s
        .rows
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let trouble = r.trouble();
            ui::Rank {
                rank: if trouble {
                    (i + 1).to_string()
                } else {
                    String::new()
                }
                .into(),
                serial: r.serial.clone().into(),
                name: r.label.clone().into(),
                lost: if r.lost_ms > 0 {
                    fmt_dur(r.lost_ms)
                } else if trouble {
                    "0s".into()
                } else {
                    "clean".into()
                }
                .into(),
                share: (r.lost_ms as f64 / span as f64).clamp(0.0, 1.0) as f32,
                incidents: if trouble {
                    r.incidents.to_string()
                } else {
                    String::new()
                }
                .into(),
                flaps: if trouble {
                    format!("{:.1}", r.flaps_per_h)
                } else {
                    String::new()
                }
                .into(),
                main: r.main.clone().into(),
                tone: rank_tone(s, i),
            }
        })
        .collect();
    ReportView {
        findings: cards(s),
        heading: format!(
            "{} {}. Worst first, ranked by time spent not tracking.",
            if finished {
                "Session length"
            } else {
                "So far:"
            },
            fmt_dur(s.span_ms)
        ),
        ranks,
        notes: summary::notes(s)
            .into_iter()
            .map(|n| line(n, DIM))
            .collect(),
        bases: base_report(&crate::report::base_section(engine)),
        empty: if finished {
            "No trackers or controllers were seen.".into()
        } else {
            "No trackers or controllers yet.".into()
        },
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CardView {
    pub title: String,
    pub what: String,
    pub steps: Vec<String>,
    pub tone: i32,
    pub reset: bool,
}

impl CardView {
    pub fn to_ui(&self) -> ui::Finding {
        ui::Finding {
            title: self.title.clone().into(),
            what: self.what.clone().into(),
            steps: slint::ModelRc::new(slint::VecModel::from(
                self.steps
                    .iter()
                    .map(|s| slint::SharedString::from(s.as_str()))
                    .collect::<Vec<_>>(),
            )),
            tone: self.tone,
            reset: self.reset,
        }
    }
}

fn cards(s: &Summary) -> Vec<CardView> {
    summary::cards(s)
        .into_iter()
        .map(|c| CardView {
            title: c.title,
            what: c.what,
            steps: c.steps,
            tone: level_tone(c.level),
            reset: c.reset,
        })
        .collect()
}

#[derive(Clone, Debug, Default)]
pub struct UsbView {
    pub tree: Vec<ui::Line>,
    pub warnings: Vec<ui::Line>,
}

pub fn usb(engine: &Engine, s: &Summary) -> UsbView {
    let ctx = TreeCtx {
        devices: &engine.devices,
        summary: Some(s),
        radio: &engine.radio,
        names: &engine.names,
    };
    UsbView {
        tree: summary::usb_tree(&engine.usb, &ctx)
            .into_iter()
            .map(|l| line(l.text, level_tone(l.level)))
            .collect(),
        warnings: crowding(&engine.usb)
            .into_iter()
            .map(|w| line(w, WARN))
            .collect(),
    }
}

#[derive(Clone, Debug, Default)]
pub struct RoomView {
    pub scene: Scene,
    pub legend: Vec<ui::RoomKey>,
    pub caption: String,
}

fn key(mark: &str, serial: &str, name: &str, detail: String, tone: i32) -> ui::RoomKey {
    ui::RoomKey {
        mark: mark.into(),
        serial: serial.into(),
        name: name.into(),
        detail: detail.into(),
        tone,
    }
}

pub fn base_views(poses: &[BasePose]) -> Vec<BaseView> {
    bases(poses).0
}

fn bases(poses: &[BasePose]) -> (Vec<BaseView>, Vec<ui::RoomKey>) {
    let mut views = Vec::new();
    let mut keys = Vec::new();
    for (i, b) in poses.iter().enumerate() {
        let mark = format!("B{}", i + 1);
        let fov = room::fov_for_model(&b.model);
        views.push(BaseView {
            pos: b.pos,
            rot: room::rot_from_matrix(&b.rot),
            fov,
            label: mark.clone(),
        });
        let generation = if fov.h_deg > 130.0 { "2.0" } else { "1.0" };
        keys.push(key(
            &mark,
            &b.serial,
            "",
            format!("base {generation}, {:.1} m up", b.pos[1]),
            BASE,
        ));
    }
    (views, keys)
}

pub fn device_tone(s: &Summary, serial: &str) -> Tone {
    let pos = s.rows.iter().position(|r| r.serial == serial);
    match pos.map(|p| (p, &s.rows[p])) {
        Some((_, r)) if !r.trouble() => Tone::Clean,
        Some((p, r)) if p < 3 && r.lost_ms > 0 => Tone::Worst,
        Some(_) => Tone::Some,
        None => Tone::Unknown,
    }
}

fn trouble_detail(s: &Summary, serial: &str) -> String {
    match s.row(serial) {
        Some(r) if r.trouble() => format!(
            "{} lost, {}",
            fmt_dur(r.lost_ms),
            summary::plural(r.incidents, "incident")
        ),
        Some(_) => "clean".into(),
        None => "no data yet".into(),
    }
}

pub fn replay_keys(
    engine: &Engine,
    s: &Summary,
    devices: &[(String, DeviceClass)],
) -> Vec<ui::RoomKey> {
    let mut keys: Vec<(bool, usize, ui::RoomKey)> = devices
        .iter()
        .filter_map(|(serial, class)| match class {
            DeviceClass::Hmd => Some((
                true,
                0,
                key("H", serial, "", "headset".into(), room_tone(Tone::Headset)),
            )),
            DeviceClass::Controller | DeviceClass::GenericTracker => {
                let tone = device_tone(s, serial);
                let mark = match (tone, s.rank(serial)) {
                    (Tone::Unknown, _) => "-".to_string(),
                    (_, Some(r)) => r.to_string(),
                    (_, None) => "ok".to_string(),
                };
                Some((
                    false,
                    s.rank(serial).unwrap_or(usize::MAX),
                    key(
                        &mark,
                        serial,
                        &engine.label(serial),
                        trouble_detail(s, serial),
                        room_tone(tone),
                    ),
                ))
            }
            _ => None,
        })
        .collect();
    keys.sort_by_key(|(headset, rank, _)| (*headset, *rank));
    keys.into_iter().map(|(_, _, k)| k).collect()
}

pub fn drops(verdicts: &[Verdict]) -> Vec<DropView> {
    verdicts
        .iter()
        .filter(|v| !quiet(v))
        .filter_map(|v| {
            Some(DropView {
                pos: v.pos?,
                tone: if v.pose_valid == Some(false) {
                    Tone::Worst
                } else {
                    Tone::Some
                },
            })
        })
        .collect()
}

pub fn live_room(engine: &Engine, s: &Summary) -> RoomView {
    let (base_views, mut legend) = bases(&engine.base_poses());
    let mut devices = Vec::new();
    let mut device_keys = Vec::new();
    let mut metas: Vec<_> = engine.meta.iter().collect();
    metas.sort_by_key(|(idx, _)| **idx);
    for (idx, m) in metas {
        if m.class == DeviceClass::TrackingReference {
            continue;
        }
        let Some(l) = engine.live.get(idx) else {
            continue;
        };
        if !l.connected || l.parked {
            continue;
        }
        let tracking = l.valid && l.state == TrackState::RunningOk;
        let name = engine.label(&m.serial);
        let (shape, tone, mark, detail) = match m.class {
            DeviceClass::Hmd => (
                Shape::Headset,
                Tone::Headset,
                "H".to_string(),
                String::new(),
            ),
            class => {
                let tone = device_tone(s, &m.serial);
                let mut mark = s
                    .rank(&m.serial)
                    .map_or("ok".to_string(), |r| r.to_string());
                if tone == Tone::Unknown {
                    mark = "-".into();
                }
                let mut detail = trouble_detail(s, &m.serial);
                if !tracking {
                    mark.push('?');
                    detail = format!("not tracking now; {detail}");
                }
                let shape = if class == DeviceClass::Controller {
                    Shape::Controller
                } else {
                    Shape::Tracker
                };
                (shape, tone, mark, detail)
            }
        };
        let shown = if name.is_empty() {
            m.serial.clone()
        } else {
            name.clone()
        };
        let label = match shape {
            Shape::Headset => "Headset".to_string(),
            _ => {
                let ranked = mark.trim_end_matches('?');
                let mut t = if ranked.chars().all(|c| c.is_ascii_digit()) {
                    format!("#{ranked} {shown}")
                } else {
                    shown
                };
                if !tracking {
                    t.push_str(" ?");
                }
                t
            }
        };
        devices.push(DeviceView {
            pos: l.pos,
            rot: room::rot_from_matrix(&l.rot),
            shape,
            tone,
            label,
            tracking,
            trail: Vec::new(),
        });
        let detail = if shape == Shape::Headset {
            if tracking {
                "tracking".into()
            } else {
                "not tracking now".into()
            }
        } else {
            detail
        };
        device_keys.push((
            shape == Shape::Headset,
            s.rank(&m.serial).unwrap_or(usize::MAX),
            key(&mark, &m.serial, &name, detail, room_tone(tone)),
        ));
    }
    device_keys.sort_by_key(|(headset, rank, _)| (*headset, *rank));
    let mut keys: Vec<ui::RoomKey> = device_keys.into_iter().map(|(_, _, k)| k).collect();
    keys.append(&mut legend);
    let caption = if devices.is_empty() && base_views.is_empty() {
        "Waiting for SteamVR to report positions.".to_string()
    } else {
        String::new()
    };
    RoomView {
        scene: Scene {
            bases: base_views,
            devices,
            drops: drops(engine.session.verdicts()),
            fixed: None,
        },
        legend: keys,
        caption,
    }
}

pub fn past_room(poses: &[BasePose], verdicts: &[Verdict]) -> RoomView {
    let (base_views, legend) = bases(poses);
    let drops = drops(verdicts);
    let caption = match (base_views.is_empty(), drops.is_empty()) {
        (true, true) => "This session has no saved positions. Sessions recorded before this version only have the Summary.".into(),
        (true, false) => "Dropout spots from this session. Its base station positions weren't saved.".into(),
        _ => "Base stations and dropout spots from this session. Devices only show while recording.".into(),
    };
    RoomView {
        scene: Scene {
            bases: base_views,
            devices: Vec::new(),
            drops,
            fixed: None,
        },
        legend,
        caption,
    }
}

pub fn name_rows(engine: &Engine) -> Vec<ui::NameRow> {
    name_rows_for(&engine.devices, &engine.names)
}

pub fn name_rows_for(
    devices: &[summary::DeviceRecord],
    names: &crate::names::Names,
) -> Vec<ui::NameRow> {
    let mut rows: Vec<ui::NameRow> = devices
        .iter()
        .filter(|d| d.class != DeviceClass::TrackingReference)
        .map(|d| ui::NameRow {
            serial: d.serial.clone().into(),
            kind: format!("{} ({})", summary::kind_label(&d.model, d.class), d.model).into(),
            name: names.get(&d.serial).unwrap_or("").into(),
        })
        .collect();
    rows.sort_by(|a, b| a.serial.cmp(&b.serial));
    rows.dedup_by(|a, b| a.serial == b.serial);
    rows
}

pub fn bases_lines(text: &str) -> Vec<ui::Line> {
    text.lines()
        .map(|l| {
            let tone = if l.contains('*') && !l.starts_with("SteamVR base moves") {
                WARN
            } else if !l.starts_with(' ') {
                BASE
            } else {
                NORMAL
            };
            line(l.to_string(), tone)
        })
        .collect()
}

pub fn past_devices(engine: &Engine, s: &Summary) -> Vec<ui::Device> {
    let mut devices: Vec<_> = engine.devices.iter().collect();
    devices.sort_by_key(|d| {
        (
            d.class == DeviceClass::TrackingReference,
            d.class == DeviceClass::Hmd,
            d.serial.clone(),
        )
    });
    devices
        .into_iter()
        .map(|d| {
            let base = d.class == DeviceClass::TrackingReference;
            let counted = !(base || d.class == DeviceClass::Hmd);
            let (state, tone) = if !counted {
                ("seen".to_string(), DIM)
            } else {
                let i = s.rows.iter().position(|r| r.serial == d.serial);
                match i.map(|i| (i, &s.rows[i])) {
                    Some((_, r)) if !r.trouble() => ("clean".to_string(), GOOD),
                    Some((i, r)) => (
                        format!("{} not tracking", fmt_dur(r.lost_ms)),
                        rank_tone(s, i),
                    ),
                    None => ("seen".to_string(), DIM),
                }
            };
            ui::Device {
                serial: d.serial.clone().into(),
                name: engine.label(&d.serial).into(),
                state: state.into(),
                tone,
                pose: "".into(),
                battery: "".into(),
                flaps: if counted {
                    engine.correlator.flap_count(&d.serial).to_string()
                } else {
                    String::new()
                }
                .into(),
                incidents: if counted {
                    engine.incident_count(&d.serial).to_string()
                } else {
                    String::new()
                }
                .into(),
                dongle: d.dongle.clone().into(),
            }
        })
        .collect()
}

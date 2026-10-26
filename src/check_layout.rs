use crate::bases::short_serial;
use crate::diagnose::{Finding, Rule};
use crate::event::{DeviceClass, Kind, SignalEvent, Source, TrackState};
use crate::room::{self, Rot, V3};
use crate::signals::openvr::{DeviceMeta, FrameSample};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

pub const STEP_M: f32 = 0.1;
pub const AUDIT_SAMPLES: usize = 3000;
pub const HEAD_EVERY_MS: u64 = 250;
pub const FRESH_MS: u64 = 500;
pub const NEAR_HEAD_M: f32 = 1.0;
pub const OVERHEAD_M: f32 = 0.5;
pub const MIN_SAMPLES: usize = 300;
pub const MAX_V1: usize = 2;
pub const APART_M: f32 = 5.0;
pub const ONE_SIDE_DEG: f32 = 90.0;
pub const SEEN_MIN: f32 = 0.5;
pub const REACH_MIN: f32 = 0.5;
pub const LOW_SHARE: f32 = 0.5;
pub const MAP_M: f32 = 0.25;
pub const MAP_DEG: f32 = 10.0;
pub const TRUSTED_MIN: f32 = 0.5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Generation {
    V1,
    V2,
}

pub fn generation(model: &str) -> Option<Generation> {
    let m = model.to_ascii_lowercase();
    if m.contains("htc v2") {
        Some(Generation::V1)
    } else if m.contains("valve sr") {
        Some(Generation::V2)
    } else {
        None
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct LayoutBase {
    pub serial: String,
    pub model: String,
    pub pos: Option<V3>,
    pub samples: usize,
    pub seen: f32,
    pub reach: f32,
    pub az_deg: f32,
    pub el_deg: f32,
    pub head_samples: usize,
    pub below_head: f32,
    pub trusted: f32,
}

#[derive(Clone, Debug, Default)]
struct BaseTrack {
    model: String,
    pose: Option<(V3, Rot)>,
    samples: usize,
    seen: usize,
    reach: usize,
    az: Vec<f32>,
    el: Vec<f32>,
    head: usize,
    below: usize,
    ok: bool,
    last_ok: Option<V3>,
}

#[derive(Clone, Debug, Default)]
pub struct Layout {
    bases: BTreeMap<String, BaseTrack>,
    kept: HashMap<String, V3>,
    hands: HashMap<String, (u64, V3)>,
    head_at: u64,
    arcs: Vec<f32>,
    samples: usize,
    sent: bool,
    audit: Option<(Vec<LayoutBase>, Option<f32>)>,
    map: HashMap<(String, String), (f32, f32)>,
}

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn len(a: V3) -> f32 {
    (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt()
}

fn local(pos: V3, rot: &Rot, p: V3) -> V3 {
    let d = sub(p, pos);
    [
        rot[0][0] * d[0] + rot[1][0] * d[1] + rot[2][0] * d[2],
        rot[0][1] * d[0] + rot[1][1] * d[1] + rot[2][1] * d[2],
        rot[0][2] * d[0] + rot[1][2] * d[1] + rot[2][2] * d[2],
    ]
}

fn median(mut v: Vec<f32>) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f32::total_cmp);
    v[v.len() / 2]
}

pub fn arc_deg(from: V3, bases: &[V3]) -> Option<f32> {
    let mut az: Vec<f32> = bases
        .iter()
        .map(|b| sub(*b, from))
        .filter(|d| d[0].hypot(d[2]) >= OVERHEAD_M)
        .map(|d| d[0].atan2(d[2]).to_degrees().rem_euclid(360.0))
        .collect();
    if az.len() < 2 {
        return None;
    }
    az.sort_by(f32::total_cmp);
    let wrap = az[0] + 360.0 - az[az.len() - 1];
    let gap = az.windows(2).map(|w| w[1] - w[0]).fold(wrap, f32::max);
    Some(360.0 - gap)
}

fn pair(a: &str, b: &str) -> (String, String) {
    let (a, b) = (a.to_ascii_uppercase(), b.to_ascii_uppercase());
    if a <= b { (a, b) } else { (b, a) }
}

fn turn_deg(a: &Rot, b: &Rot) -> f32 {
    let tr: f32 = (0..3)
        .flat_map(|i| (0..3).map(move |k| a[k][i] * b[k][i]))
        .sum();
    ((tr - 1.0) / 2.0).clamp(-1.0, 1.0).acos().to_degrees()
}

fn tracking(f: &FrameSample) -> bool {
    f.valid && f.state == TrackState::RunningOk
}

impl Layout {
    pub fn frame(
        &mut self,
        meta: &DeviceMeta,
        parked: bool,
        f: &FrameSample,
    ) -> Option<SignalEvent> {
        if self.sent {
            return None;
        }
        match meta.class {
            DeviceClass::TrackingReference if meta.is_lighthouse => {
                let b = self.bases.entry(meta.serial.clone()).or_default();
                b.model.clone_from(&meta.model);
                if tracking(f) {
                    b.pose = Some((f.pos, room::rot_from_matrix(&f.rot)));
                }
                let ok = self.matches_map(&meta.serial);
                if let Some(b) = self.bases.get_mut(&meta.serial) {
                    b.ok = ok;
                    if ok {
                        b.last_ok = b.pose.map(|(p, _)| p);
                    }
                }
                None
            }
            DeviceClass::Hmd => {
                if tracking(f) && !parked {
                    self.head(meta.is_lighthouse, f);
                    if meta.is_lighthouse {
                        return self.play(&meta.serial, f.pos, f.t_ms);
                    }
                }
                None
            }
            DeviceClass::Controller | DeviceClass::GenericTracker
                if meta.is_lighthouse && tracking(f) && !parked =>
            {
                self.hands.insert(meta.serial.clone(), (f.t_ms, f.pos));
                self.play(&meta.serial, f.pos, f.t_ms)
            }
            _ => None,
        }
    }

    pub fn ingest(&mut self, ev: &SignalEvent) {
        if let Kind::BaseRelation { a, b, mm, deg } = &ev.kind {
            self.map.insert(pair(a, b), (*mm as f32 / 1000.0, *deg));
        }
    }

    fn matches_map(&self, serial: &str) -> bool {
        let Some((pos, rot)) = self.bases.get(serial).and_then(|b| b.pose) else {
            return false;
        };
        let (mut agree, mut differ) = (0, 0);
        for (other, b) in &self.bases {
            let Some((p, r)) = b.pose.filter(|_| other.as_str() != serial) else {
                continue;
            };
            let Some(&(m, deg)) = self.map.get(&pair(&name(serial), &name(other))) else {
                continue;
            };
            if (len(sub(pos, p)) - m).abs() <= MAP_M && (turn_deg(&rot, &r) - deg).abs() <= MAP_DEG
            {
                agree += 1;
            } else {
                differ += 1;
            }
        }
        agree > differ
    }

    fn head(&mut self, lighthouse: bool, f: &FrameSample) {
        if f.t_ms.saturating_sub(self.head_at) < HEAD_EVERY_MS {
            return;
        }
        self.head_at = f.t_ms;
        let near = lighthouse
            || self.hands.values().any(|(t, p)| {
                f.t_ms.saturating_sub(*t) <= FRESH_MS && len(sub(*p, f.pos)) <= NEAR_HEAD_M
            });
        if !near {
            return;
        }
        for b in self.bases.values_mut().filter(|b| b.ok) {
            if let Some((pos, _)) = b.pose {
                b.head += 1;
                if pos[1] < f.pos[1] {
                    b.below += 1;
                }
            }
        }
    }

    fn play(&mut self, serial: &str, p: V3, t_ms: u64) -> Option<SignalEvent> {
        if self
            .kept
            .get(serial)
            .is_some_and(|k| len(sub(*k, p)) < STEP_M)
        {
            return None;
        }
        self.kept.insert(serial.to_string(), p);
        if self.bases.is_empty() {
            return None;
        }
        let all_ok = self.bases.values().all(|b| b.ok);
        let mut posed: Vec<V3> = Vec::new();
        for b in self.bases.values_mut().filter(|b| b.ok) {
            let Some((pos, rot)) = b.pose else {
                continue;
            };
            posed.push(pos);
            let fov = room::fov_for_model(&b.model);
            b.samples += 1;
            if len(sub(p, pos)) <= fov.range_m {
                b.reach += 1;
            }
            if room::base_sees(pos, &rot, fov, p) {
                b.seen += 1;
            }
            let l = local(pos, &rot, p);
            let ahead = -l[2];
            b.az.push(l[0].atan2(ahead).to_degrees());
            b.el.push(l[1].atan2(l[0].hypot(ahead)).to_degrees());
        }
        if all_ok && let Some(a) = arc_deg(p, &posed) {
            self.arcs.push(a);
        }
        self.samples += 1;
        if self.samples < AUDIT_SAMPLES {
            return None;
        }
        self.sent = true;
        let kind = self.summarize();
        let detail = match &kind {
            Kind::BaseLayout { bases, .. } if bases.len() > 1 => format!(
                "Checked where your {} base stations are and where they point, against where you played.",
                bases.len()
            ),
            _ => {
                "Checked where your base station is and where it points, against where you played."
                    .to_string()
            }
        };
        Some(SignalEvent::at(t_ms, Source::Probe, None, kind, detail))
    }

    pub fn summarize(&self) -> Kind {
        let bases = self
            .bases
            .iter()
            .map(|(serial, b)| {
                let share = |n: usize, of: usize| if of == 0 { 0.0 } else { n as f32 / of as f32 };
                LayoutBase {
                    serial: serial.clone(),
                    model: b.model.clone(),
                    pos: b.last_ok,
                    samples: b.samples,
                    seen: share(b.seen, b.samples),
                    reach: share(b.reach, b.samples),
                    az_deg: median(b.az.clone()),
                    el_deg: median(b.el.clone()),
                    head_samples: b.head,
                    below_head: share(b.below, b.head),
                    trusted: share(b.samples, self.samples),
                }
            })
            .collect();
        Kind::BaseLayout {
            bases,
            arc_deg: (self.arcs.len() >= MIN_SAMPLES
                && self.arcs.len() as f32 >= TRUSTED_MIN * self.samples as f32)
                .then(|| median(self.arcs.clone())),
            samples: self.samples,
        }
    }

    pub fn audited(&mut self, kind: &Kind) {
        if let Kind::BaseLayout { bases, arc_deg, .. } = kind {
            self.sent = true;
            self.audit = Some((bases.clone(), *arc_deg));
            crate::dlog!(
                "base layout: arc {arc_deg:?}, bases {bases:?}, problems {:?}",
                problems(bases, *arc_deg)
            );
        }
    }

    pub fn problems(&self) -> Vec<Problem> {
        match &self.audit {
            Some((bases, arc)) => problems(bases, *arc),
            None => Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Problem {
    Mixed {
        v1: Vec<String>,
        v2: Vec<String>,
    },
    TooManyV1 {
        bases: Vec<String>,
    },
    Single {
        base: String,
    },
    Apart {
        a: String,
        b: String,
        m: f32,
    },
    OneSide {
        n: usize,
        arc: f32,
    },
    Aim {
        base: String,
        seen: f32,
        az: f32,
        el: f32,
        fov: room::Fov,
    },
    Far {
        base: String,
        reach: f32,
        range: f32,
    },
    Low {
        base: String,
        share: f32,
    },
}

fn name(serial: &str) -> String {
    short_serial(serial)
}

pub fn problems(bases: &[LayoutBase], arc: Option<f32>) -> Vec<Problem> {
    let mut out = Vec::new();
    let of = |g: Generation| -> Vec<String> {
        bases
            .iter()
            .filter(|b| generation(&b.model) == Some(g))
            .map(|b| name(&b.serial))
            .collect()
    };
    let (v1, v2) = (of(Generation::V1), of(Generation::V2));
    if !v1.is_empty() && !v2.is_empty() {
        out.push(Problem::Mixed {
            v1: v1.clone(),
            v2: v2.clone(),
        });
    } else if v1.len() > MAX_V1 {
        out.push(Problem::TooManyV1 { bases: v1.clone() });
    }
    let posed: Vec<&LayoutBase> = bases
        .iter()
        .filter(|b| b.pos.is_some() && b.samples >= MIN_SAMPLES && b.trusted >= TRUSTED_MIN)
        .collect();
    if let [one] = bases {
        out.push(Problem::Single {
            base: name(&one.serial),
        });
    }
    match posed.as_slice() {
        [a, b] if bases.len() == 2 && v1.len() == 2 => {
            if let (Some(pa), Some(pb)) = (a.pos, b.pos) {
                let m = len(sub(pa, pb));
                if m > APART_M {
                    out.push(Problem::Apart {
                        a: name(&a.serial),
                        b: name(&b.serial),
                        m,
                    });
                }
            }
        }
        _ => {}
    }
    if let Some(arc) = arc
        && bases.len() >= 2
        && arc < ONE_SIDE_DEG
    {
        out.push(Problem::OneSide {
            n: bases.len(),
            arc,
        });
    }
    for b in &posed {
        let range = room::fov_for_model(&b.model).range_m;
        if b.reach < REACH_MIN {
            out.push(Problem::Far {
                base: name(&b.serial),
                reach: b.reach,
                range,
            });
        } else if b.seen < SEEN_MIN {
            out.push(Problem::Aim {
                base: name(&b.serial),
                seen: b.seen,
                az: b.az_deg,
                el: b.el_deg,
                fov: room::fov_for_model(&b.model),
            });
        }
        if b.head_samples >= MIN_SAMPLES && b.below_head > LOW_SHARE {
            out.push(Problem::Low {
                base: name(&b.serial),
                share: b.below_head,
            });
        }
    }
    out
}

fn pct(share: f32) -> String {
    format!("{:.0}%", share * 100.0)
}

fn about(deg: f32) -> u32 {
    ((deg.abs() / 5.0).round() as u32 * 5).max(5)
}

fn list(names: &[String]) -> String {
    crate::diagnose::join(&names.iter().map(|n| format!("LHB-{n}")).collect::<Vec<_>>())
}

const ROOM_SETUP: &str = "Run Room Setup again in SteamVR afterwards.";

pub fn explain(layout: &Layout) -> Vec<Finding> {
    layout.problems().into_iter().map(finding).collect()
}

fn finding(p: Problem) -> Finding {
    let make = |key: String, title: String, what: String, steps: Vec<String>| Finding {
        rule: Rule::BaseLayout,
        key,
        title,
        what,
        steps,
    };
    match p {
        Problem::Mixed { v1, v2 } => make(
            "mixed".into(),
            "You have 1.0 and 2.0 base stations together".into(),
            format!(
                "SteamVR found 1.0 base stations ({}) and 2.0 base stations ({}) in the same setup. Valve says the two kinds don't work together.",
                list(&v1),
                list(&v2)
            ),
            vec![
                "Use only one kind. Unplug either the 1.0 ones or the 2.0 ones.".into(),
                ROOM_SETUP.into(),
            ],
        ),
        Problem::TooManyV1 { bases } => make(
            "v1count".into(),
            "More than two 1.0 base stations".into(),
            format!(
                "SteamVR found {} 1.0 base stations ({}). 1.0 base stations only work as a pair: one main base and one that follows it.",
                bases.len(),
                list(&bases)
            ),
            vec![
                "Unplug all but two of them.".into(),
                "Set one to channel b and the other to c. If you use the sync cable, set them to A and b.".into(),
            ],
        ),
        Problem::Single { base } => make(
            "single".into(),
            "Only one base station".into(),
            format!(
                "SteamVR only saw one base station this session ({}). Whenever your body is between it and a device, that device loses tracking.",
                list(&[base])
            ),
            vec![
                "Add a second base station on the opposite side of where you play.".into(),
                ROOM_SETUP.into(),
            ],
        ),
        Problem::Apart { a, b, m } => make(
            "apart".into(),
            "Your two base stations are far apart".into(),
            format!(
                "Your two 1.0 base stations ({}) are {m:.1} m apart. HTC suggests keeping them no more than 5 m apart.",
                list(&[a, b])
            ),
            vec![
                "Move them closer together, on opposite corners of where you play.".into(),
                ROOM_SETUP.into(),
            ],
        ),
        Problem::OneSide { n, arc } => make(
            "oneside".into(),
            "Your base stations are all on one side".into(),
            format!(
                "Seen from where you play, your {n} base stations are all within {arc:.0}° of each other. When you turn your back to them, your body blocks their view of your controllers and trackers."
            ),
            vec![
                "Move one of them to the other side of where you play, so they face each other across it.".into(),
                ROOM_SETUP.into(),
            ],
        ),
        Problem::Aim {
            base,
            seen,
            az,
            el,
            fov,
        } => {
            let step = if el.abs() >= az.abs() {
                format!(
                    "Tilt it {} about {}°, so it points at the middle of where you play.",
                    if el < 0.0 { "down" } else { "up" },
                    about(el)
                )
            } else {
                format!(
                    "Turn it about {}° toward the middle of where you play.",
                    about(az)
                )
            };
            let side = if el < -fov.v_deg / 2.0 {
                " Most of the time they were below its view."
            } else if el > fov.v_deg / 2.0 {
                " Most of the time they were above its view."
            } else if az.abs() > fov.h_deg / 2.0 {
                " Most of the time they were off to the side of its view."
            } else {
                ""
            };
            make(
                format!("aim-{base}"),
                format!("Base {base} doesn't point at where you play"),
                format!(
                    "Base {base} could only see your controllers and trackers {} of the time you moved around.{side}",
                    pct(seen)
                ),
                vec![step, ROOM_SETUP.into()],
            )
        }
        Problem::Far { base, reach, range } => make(
            format!("far-{base}"),
            format!("Base {base} is too far from where you play"),
            format!(
                "Your controllers and trackers were more than {range:.0} m from base {base} {} of the time you moved around. That's past its rated range.",
                pct(1.0 - reach)
            ),
            vec![
                format!("Move it within {range:.0} m of where you play."),
                ROOM_SETUP.into(),
            ],
        ),
        Problem::Low { base, share } => make(
            format!("low-{base}"),
            format!("Base {base} is mounted below head height"),
            format!(
                "Base {base} sat lower than your headset {} of the time you played, so your body gets between it and your devices more often.",
                pct(share)
            ),
            vec![
                "Mount it higher, above your head, and tilt it down toward the middle of where you play.".into(),
                ROOM_SETUP.into(),
            ],
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnose::Diagnosis;

    const V2: &str = "Valve SR Imp";
    const V1: &str = "HTC V2-XD/XE";
    const MIDDLE: V3 = [-0.29, 0.0, 0.62];

    type Base = (&'static str, &'static str, V3, Rot);

    fn home() -> Vec<Base> {
        vec![
            (
                "LHB-9417E0B3",
                V2,
                [0.768, 2.072, -0.925],
                [
                    [-0.70523834, -0.3725825, 0.6031757],
                    [-0.082573175, 0.88815314, 0.45206815],
                    [-0.7041451, 0.2690097, -0.657125],
                ],
            ),
            (
                "LHB-9D8DEA7B",
                V2,
                [1.495, 2.026, 1.418],
                [
                    [0.1849912, -0.42846382, 0.88441896],
                    [0.04958941, 0.90287536, 0.4270327],
                    [-0.9814882, -0.03513946, 0.18827127],
                ],
            ),
            (
                "LHB-B394A63C",
                V2,
                [-0.821, 2.065, 1.813],
                [
                    [0.5912016, 0.33110625, -0.7354246],
                    [0.11238669, 0.86912775, 0.4816494],
                    [0.79865503, -0.36740384, 0.47661778],
                ],
            ),
            (
                "LHB-C538EFC9",
                V2,
                [-2.599, 2.092, 0.161],
                [
                    [-0.10416898, 0.42453024, -0.8994014],
                    [0.024795894, 0.90514874, 0.42437118],
                    [0.9942505, 0.021904856, -0.10481503],
                ],
            ),
        ]
    }

    fn cross(a: V3, b: V3) -> V3 {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    }

    fn unit(a: V3) -> V3 {
        let l = len(a);
        [a[0] / l, a[1] / l, a[2] / l]
    }

    fn aimed(pos: V3, target: V3) -> Rot {
        let f = unit(sub(target, pos));
        let right = unit(cross(f, [0.0, 1.0, 0.0]));
        let up = cross(right, f);
        [
            [right[0], up[0], -f[0]],
            [right[1], up[1], -f[1]],
            [right[2], up[2], -f[2]],
        ]
    }

    fn base(serial: &'static str, model: &'static str, pos: V3) -> Base {
        (serial, model, pos, aimed(pos, [MIDDLE[0], 1.0, MIDDLE[2]]))
    }

    fn meta(idx: usize, serial: &str, model: &str, class: DeviceClass, lh: bool) -> DeviceMeta {
        DeviceMeta {
            idx,
            serial: serial.into(),
            model: model.into(),
            class,
            dongle: String::new(),
            battery_pct: None,
            is_lighthouse: lh,
        }
    }

    fn sample(idx: usize, t_ms: u64, pos: V3, rot: &Rot) -> FrameSample {
        let mut m = [[0.0; 4]; 3];
        for (row, r) in m.iter_mut().zip(rot) {
            row[..3].copy_from_slice(r);
        }
        for (row, p) in m.iter_mut().zip(pos) {
            row[3] = p;
        }
        FrameSample {
            idx,
            t_ms,
            pos,
            vel: [0.0; 3],
            rot: m,
            ang_speed: 0.0,
            state: TrackState::RunningOk,
            valid: true,
        }
    }

    fn hand() -> DeviceMeta {
        meta(
            20,
            "LHR-A43A0229",
            "Knuckles Right",
            DeviceClass::Controller,
            true,
        )
    }

    fn relations(truth: &[Base]) -> Vec<SignalEvent> {
        let mut out = Vec::new();
        for (i, a) in truth.iter().enumerate() {
            for b in &truth[i + 1..] {
                let kind = Kind::BaseRelation {
                    a: name(a.0),
                    b: name(b.0),
                    mm: (len(sub(a.2, b.2)) * 1000.0).round() as u32,
                    deg: turn_deg(&a.3, &b.3),
                };
                out.push(SignalEvent::at(1, Source::Log, None, kind, ""));
            }
        }
        out
    }

    fn play(bases: &[Base], head: Option<(bool, f32)>) -> (Option<SignalEvent>, Layout) {
        play_with(bases, bases, head)
    }

    fn play_with(
        bases: &[Base],
        truth: &[Base],
        head: Option<(bool, f32)>,
    ) -> (Option<SignalEvent>, Layout) {
        let mut layout = Layout::default();
        for ev in relations(truth) {
            layout.ingest(&ev);
        }
        let event = drive(&mut layout, bases, head);
        if let Some(ev) = &event {
            layout.audited(&ev.kind);
        }
        (event, layout)
    }

    fn drive(
        layout: &mut Layout,
        bases: &[Base],
        head: Option<(bool, f32)>,
    ) -> Option<SignalEvent> {
        let mut event = None;
        let metas: Vec<DeviceMeta> = bases
            .iter()
            .enumerate()
            .map(|(i, b)| meta(i, b.0, b.1, DeviceClass::TrackingReference, true))
            .collect();
        let hand = hand();
        let hmd = meta(
            21,
            "1PASH5D1P17365",
            "Meta Quest Pro",
            DeviceClass::Hmd,
            head.is_some_and(|h| h.0),
        );
        for i in 0..AUDIT_SAMPLES + 50 {
            let t = 1_000_000 + i as u64 * 100;
            for (m, b) in metas.iter().zip(bases) {
                assert!(
                    layout
                        .frame(m, false, &sample(m.idx, t, b.2, &b.3))
                        .is_none()
                );
            }
            let k = i as f32;
            let p = [
                MIDDLE[0] + ((k * 0.37) % 2.0 - 1.0),
                0.2 + (k * 0.13) % 1.6,
                MIDDLE[2] + ((k * 0.53) % 2.0 - 1.0),
            ];
            if let Some(ev) = layout.frame(&hand, false, &sample(20, t, p, &room::IDENTITY)) {
                event = Some(ev);
            }
            if let Some((_, y)) = head {
                let h = [p[0] + 0.2, y, p[2] - 0.2];
                if let Some(ev) = layout.frame(&hmd, false, &sample(21, t, h, &room::IDENTITY)) {
                    event = Some(ev);
                }
            }
        }
        event
    }

    #[test]
    fn measured_home_layout_has_no_problems() {
        let (event, layout) = play(&home(), Some((true, 1.75)));
        let ev = event.expect("the audit runs once enough play is seen");
        assert_eq!(ev.source, Source::Probe);
        let Kind::BaseLayout {
            bases,
            arc_deg,
            samples,
        } = &ev.kind
        else {
            panic!("not a layout event");
        };
        assert_eq!(*samples, AUDIT_SAMPLES);
        assert_eq!(bases.len(), 4);
        for b in bases {
            assert!(b.seen > 0.9, "{} sees {}", b.serial, b.seen);
            assert_eq!(b.reach, 1.0, "{} reach", b.serial);
            assert!(b.head_samples >= MIN_SAMPLES, "{}", b.head_samples);
            assert_eq!(b.below_head, 0.0, "{} sits above the headset", b.serial);
        }
        assert!(arc_deg.is_some_and(|a| a > 180.0), "arc {arc_deg:?}");
        assert_eq!(layout.problems(), Vec::new());
        assert!(explain(&layout).is_empty());
    }

    #[test]
    fn pose_that_disagrees_with_steamvr_map_is_left_out() {
        let truth = home();
        let mut seen_by_api = home();
        let p = truth[0].2;
        seen_by_api[2].2 = [p[0] - 0.3, p[1], p[2] - 0.2];
        seen_by_api[2].3 = truth[0].3;
        let (event, layout) = play_with(&seen_by_api, &truth, Some((true, 1.75)));
        let Some(Kind::BaseLayout { bases, arc_deg, .. }) = event.map(|e| e.kind) else {
            panic!("no audit");
        };
        assert_eq!(arc_deg, None);
        let b394 = bases.iter().find(|b| b.serial == "LHB-B394A63C").unwrap();
        assert_eq!((b394.pos, b394.samples, b394.head_samples), (None, 0, 0));
        for b in bases.iter().filter(|b| b.serial != "LHB-B394A63C") {
            assert!(b.pos.is_some(), "{}", b.serial);
            assert!(b.samples + 3 >= AUDIT_SAMPLES, "{} {}", b.serial, b.samples);
        }
        assert_eq!(layout.problems(), Vec::new());
    }

    #[test]
    fn without_steamvr_map_only_models_are_judged() {
        let mut bases = home();
        bases[3].1 = V1;
        let pos = bases[1].2;
        bases[1].3 = aimed(pos, [pos[0] + 3.0, 1.0, pos[2] + 1.0]);
        let (event, layout) = play_with(&bases, &[], None);
        assert!(event.is_some());
        let found = layout.problems();
        assert!(
            matches!(found.as_slice(), [Problem::Mixed { .. }]),
            "{found:?}"
        );
    }

    #[test]
    fn audit_happens_once() {
        let (event, mut layout) = play(&home(), None);
        assert!(event.is_some());
        let hand = hand();
        for i in 0..10u64 {
            let p = [i as f32, 1.0, 0.0];
            let f = sample(20, 9_000_000 + i * 100, p, &room::IDENTITY);
            assert!(layout.frame(&hand, false, &f).is_none());
        }
    }

    #[test]
    fn no_audit_without_lighthouse_play() {
        let mut layout = Layout::default();
        let b = &home()[0];
        let m = meta(0, b.0, b.1, DeviceClass::TrackingReference, true);
        let quest = meta(1, "QUEST", "Quest Touch", DeviceClass::Controller, false);
        for i in 0..AUDIT_SAMPLES as u64 * 2 {
            let t = i * 100;
            assert!(layout.frame(&m, false, &sample(0, t, b.2, &b.3)).is_none());
            let p = [(i % 20) as f32 * 0.15, 1.0, 0.0];
            assert!(
                layout
                    .frame(&quest, false, &sample(1, t, p, &room::IDENTITY))
                    .is_none()
            );
            assert!(
                layout
                    .frame(&hand(), true, &sample(20, t, p, &room::IDENTITY))
                    .is_none()
            );
        }
    }

    #[test]
    fn two_bases_on_one_wall_are_all_on_one_side() {
        let bases = [
            base("LHB-AAAA0001", V2, [-0.8, 2.1, 3.0]),
            base("LHB-AAAA0002", V2, [0.2, 2.1, 3.0]),
        ];
        let (_, layout) = play(&bases, Some((true, 1.75)));
        let found = layout.problems();
        assert!(
            matches!(found.as_slice(), [Problem::OneSide { n: 2, arc }] if *arc < 45.0),
            "{found:?}"
        );
        assert_eq!(explain(&layout)[0].key, "oneside");
    }

    #[test]
    fn opposite_corners_are_fine() {
        let bases = [
            base("LHB-AAAA0001", V2, [-1.9, 2.1, -1.1]),
            base("LHB-AAAA0002", V2, [1.3, 2.1, 2.3]),
        ];
        let (_, layout) = play(&bases, Some((true, 1.75)));
        assert_eq!(layout.problems(), Vec::new());
    }

    #[test]
    fn base_facing_the_wall_is_told_to_turn() {
        let mut bases = home();
        let pos = bases[1].2;
        bases[1].3 = aimed(pos, [pos[0] + 3.0, 1.0, pos[2] + 1.0]);
        let (_, layout) = play(&bases, Some((true, 1.75)));
        let found = explain(&layout);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].key, "aim-9D8DEA7B");
        assert!(
            found[0].steps[0].starts_with("Turn it about"),
            "{:?}",
            found[0].steps
        );
        assert!(
            found[0].what.contains("off to the side"),
            "{}",
            found[0].what
        );
    }

    #[test]
    fn base_aimed_at_the_ceiling_is_told_to_tilt_down() {
        let mut bases = home();
        let pos = bases[0].2;
        bases[0].3 = aimed(pos, [MIDDLE[0], 9.0, MIDDLE[2]]);
        let (_, layout) = play(&bases, Some((true, 1.75)));
        let found = explain(&layout);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].key, "aim-9417E0B3");
        assert!(
            found[0].steps[0].starts_with("Tilt it down"),
            "{:?}",
            found[0].steps
        );
        assert!(
            found[0].what.contains("below its view"),
            "{}",
            found[0].what
        );
    }

    #[test]
    fn mixed_generations_are_flagged() {
        let mut bases = home();
        bases[3].1 = V1;
        let (_, layout) = play(&bases, None);
        assert_eq!(
            layout.problems(),
            vec![Problem::Mixed {
                v1: vec!["C538EFC9".into()],
                v2: vec!["9417E0B3".into(), "9D8DEA7B".into(), "B394A63C".into()],
            }]
        );
        let found = explain(&layout);
        assert!(
            found[0].what.contains("(LHB-C538EFC9)"),
            "{}",
            found[0].what
        );
    }

    #[test]
    fn three_v1_bases_are_too_many_and_two_are_fine() {
        let three = [
            base("LHB-11110001", V1, [-1.9, 2.1, -1.1]),
            base("LHB-11110002", V1, [1.3, 2.1, 2.3]),
            base("LHB-11110003", V1, [1.3, 2.1, -1.1]),
        ];
        let (_, layout) = play(&three, None);
        let found = layout.problems();
        assert!(
            matches!(found.as_slice(), [Problem::TooManyV1 { bases }] if bases.len() == 3),
            "{found:?}"
        );
        let (_, layout) = play(&three[..2], None);
        assert_eq!(layout.problems(), Vec::new());
    }

    #[test]
    fn low_base_is_flagged_when_the_headset_lines_up() {
        let mut bases = home();
        let low = [bases[2].2[0], 1.1, bases[2].2[2]];
        bases[2] = base("LHB-B394A63C", V2, low);
        for lh in [true, false] {
            let (_, layout) = play(&bases, Some((lh, 1.75)));
            let found = layout.problems();
            assert!(
                matches!(found.as_slice(), [Problem::Low { base, share }] if base == "B394A63C" && *share == 1.0),
                "{found:?}"
            );
        }
        let (_, layout) = play(&bases, Some((false, 4.0)));
        assert_eq!(layout.problems(), Vec::new());
        let (_, layout) = play(&home(), Some((false, 1.75)));
        assert_eq!(layout.problems(), Vec::new());
    }

    #[test]
    fn two_bases_six_and_a_half_metres_apart() {
        let bases = [
            base("LHB-AAAA0001", V1, [-2.6, 2.1, -2.0]),
            base("LHB-AAAA0002", V1, [2.0, 2.1, 2.6]),
        ];
        let (_, layout) = play(&bases, None);
        let found = explain(&layout);
        assert!(found.iter().any(|f| f.key == "apart"), "{found:?}");
        let apart = found.iter().find(|f| f.key == "apart").unwrap();
        assert!(apart.what.contains("6.5 m apart"), "{}", apart.what);
        let bases = [
            base("LHB-AAAA0001", V2, [-2.6, 2.1, -2.0]),
            base("LHB-AAAA0002", V2, [2.0, 2.1, 2.6]),
        ];
        let (_, layout) = play(&bases, None);
        assert!(explain(&layout).is_empty(), "{:?}", explain(&layout));
    }

    #[test]
    fn base_past_its_range_is_far() {
        let mut bases = home();
        bases.push(base("LHB-AAAA0005", V2, [MIDDLE[0] + 9.0, 2.1, MIDDLE[2]]));
        let (_, layout) = play(&bases, None);
        let found = explain(&layout);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].key, "far-AAAA0005");
        assert!(found[0].what.contains("more than 7 m"), "{}", found[0].what);
    }

    #[test]
    fn one_base_alone() {
        let bases = [base("LHB-AAAA0001", V2, [-1.9, 2.1, -1.1])];
        let (_, layout) = play(&bases, None);
        assert_eq!(
            layout.problems(),
            vec![Problem::Single {
                base: "AAAA0001".into()
            }]
        );
    }

    #[test]
    fn arc_is_the_spread_of_bases_around_a_point() {
        let at = [0.0, 1.0, 0.0];
        let opposite = [[3.0, 2.0, 3.0], [-3.0, 2.0, -3.0]];
        assert!((arc_deg(at, &opposite).unwrap() - 180.0).abs() < 0.01);
        let wall = [[-0.5, 2.0, 3.0], [0.5, 2.0, 3.0]];
        assert!((arc_deg(at, &wall).unwrap() - 18.92).abs() < 0.05);
        let overhead = [[0.1, 2.0, 0.1], [3.0, 2.0, 3.0]];
        assert_eq!(arc_deg(at, &overhead), None);
    }

    #[test]
    fn diagnosis_reports_the_layout_finding_once() {
        let mut bases = home();
        bases[3].1 = V1;
        let pos = bases[1].2;
        bases[1].3 = aimed(pos, [pos[0] + 3.0, 1.0, pos[2] + 1.0]);
        let mut d = Diagnosis::default();
        for ev in relations(&bases) {
            d.ingest(&ev);
        }
        let ev = drive(&mut d.layout, &bases, None).expect("audit event");
        assert!(d.ingest(&ev));
        let label = |s: &str| s.to_string();
        let found = d.findings(&label);
        let keys: Vec<&str> = found.iter().map(|f| f.key.as_str()).collect();
        assert_eq!(keys, ["mixed", "aim-9D8DEA7B"]);
        assert!(found.iter().all(|f| f.rule == Rule::BaseLayout));
        assert_eq!(d.fresh(found.clone()).len(), 2);
        assert!(d.fresh(found).is_empty());
    }
}

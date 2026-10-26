use crate::diagnose::{Finding, Rule, join};
use crate::event::{DeviceClass, TrackState};
use crate::signals::openvr::FrameSample;
use std::collections::BTreeMap;

pub const BIN_MS: u64 = 1_000;
pub const GAP_MS: u64 = 2_500;
pub const MERGE_MS: u64 = 1_000;
pub const SECTORS: usize = 8;
pub const WINDOW: usize = 2;
pub const LEVEL: f32 = 0.7;
pub const MIN_SPAN_MS: u64 = 20 * 60_000;
pub const MIN_FACING_MS: u64 = 3 * 60_000;
pub const MIN_SHARE: f64 = 0.05;
pub const MIN_DROPS: usize = 20;
pub const MIN_LEFT: usize = 10;
pub const MIN_OCCASIONS: usize = 10;
pub const MIN_RATIO: f64 = 3.0;
pub const SHIFTS: usize = 199;
pub const VISIT_GAP_BINS: usize = 60;
pub const EVAL_MS: u64 = 60_000;
pub const AHEAD_DEG: f32 = 60.0;
const NONE: u8 = u8::MAX;

#[derive(Clone, Copy, Debug, Default)]
struct Bin {
    ms: u16,
    drops: u8,
}

#[derive(Clone, Copy, Debug)]
struct Last {
    t_ms: u64,
    tracking: bool,
}

#[derive(Clone, Debug)]
struct Dev {
    class: DeviceClass,
    last: Option<Last>,
    recovered: Option<u64>,
    drops: usize,
    bins: Vec<Bin>,
}

#[derive(Clone, Copy, Debug)]
struct Head {
    t_ms: u64,
    pos: [f32; 3],
    sector: u8,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Facing {
    pub serial: String,
    pub class: DeviceClass,
    pub start: usize,
    pub in_ms: u64,
    pub in_drops: usize,
    pub out_ms: u64,
    pub out_drops: usize,
    pub ratio: f64,
    pub shifted_max: f64,
    pub left_ratio: f64,
    pub left_drops: usize,
    pub visits: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Window {
    start: usize,
    in_ms: u64,
    in_drops: usize,
    out_ms: u64,
    out_drops: usize,
    ratio: f64,
}

#[derive(Clone, Debug, Default)]
pub struct Spots {
    t0: Option<u64>,
    latest: u64,
    checked: u64,
    sectors: Vec<u8>,
    devs: BTreeMap<String, Dev>,
    head: Option<Head>,
    centre: ([f64; 2], f64),
    bases: BTreeMap<String, [f32; 3]>,
    found: Vec<Facing>,
    noted: BTreeMap<String, String>,
}

pub fn yaw_deg(rot: &[[f32; 4]; 3]) -> Option<f32> {
    let f = [-rot[0][2], -rot[1][2], -rot[2][2]];
    if f[1].abs() > LEVEL {
        return None;
    }
    Some(f[0].atan2(-f[2]).to_degrees())
}

pub fn sector_of(yaw_deg: f32) -> usize {
    let s = ((yaw_deg + 180.0) / 360.0 * SECTORS as f32).floor() as i64;
    s.rem_euclid(SECTORS as i64) as usize
}

pub fn window_centre(start: usize) -> f32 {
    -180.0 + 360.0 / SECTORS as f32 * (start as f32 + WINDOW as f32 / 2.0)
}

fn wrap(deg: f32) -> f32 {
    (deg + 180.0).rem_euclid(360.0) - 180.0
}

fn in_window(sector: u8, start: usize) -> bool {
    sector != NONE && (sector as usize + SECTORS - start) % SECTORS < WINDOW
}

fn tally(
    bins: &[Bin],
    sectors: &[u8],
    shift: usize,
    n: usize,
) -> ([u64; SECTORS], [usize; SECTORS]) {
    let mut ms = [0u64; SECTORS];
    let mut drops = [0usize; SECTORS];
    for (b, bin) in bins.iter().enumerate() {
        if bin.ms == 0 && bin.drops == 0 {
            continue;
        }
        let s = sectors.get((b + shift) % n).copied().unwrap_or(NONE);
        if s == NONE {
            continue;
        }
        ms[s as usize] += u64::from(bin.ms);
        drops[s as usize] += usize::from(bin.drops);
    }
    (ms, drops)
}

fn best(ms: &[u64; SECTORS], drops: &[usize; SECTORS]) -> Option<Window> {
    let total: u64 = ms.iter().sum();
    let dropped: usize = drops.iter().sum();
    if total == 0 || dropped == 0 {
        return None;
    }
    let floor = (MIN_SHARE * total as f64).max(MIN_FACING_MS as f64);
    let mut out: Option<Window> = None;
    for start in 0..SECTORS {
        let in_ms: u64 = (0..WINDOW).map(|i| ms[(start + i) % SECTORS]).sum();
        let in_drops: usize = (0..WINDOW).map(|i| drops[(start + i) % SECTORS]).sum();
        let out_ms = total - in_ms;
        let out_drops = dropped - in_drops;
        if (in_ms as f64) < floor || out_ms == 0 {
            continue;
        }
        let ratio = rate(in_drops, in_ms) / (out_drops as f64).max(0.5) * out_ms as f64;
        if out.is_none_or(|o| ratio > o.ratio) {
            out = Some(Window {
                start,
                in_ms,
                in_drops,
                out_ms,
                out_drops,
                ratio,
            });
        }
    }
    out
}

fn rate(drops: usize, ms: u64) -> f64 {
    if ms == 0 {
        0.0
    } else {
        drops as f64 / ms as f64
    }
}

fn visits(bins: &[Bin], sectors: &[u8], start: usize) -> Vec<(u64, usize)> {
    let mut out: Vec<(u64, usize)> = Vec::new();
    let mut last: Option<usize> = None;
    for (b, s) in sectors.iter().enumerate() {
        if !in_window(*s, start) {
            continue;
        }
        if last.is_none_or(|l| b - l > VISIT_GAP_BINS) {
            out.push((0, 0));
        }
        last = Some(b);
        if let (Some(bin), Some(v)) = (bins.get(b), out.last_mut()) {
            v.0 += u64::from(bin.ms);
            v.1 += usize::from(bin.drops);
        }
    }
    out.retain(|v| v.0 > 0);
    out
}

impl Spots {
    fn bin(&mut self, t_ms: u64) -> usize {
        let t0 = *self.t0.get_or_insert(t_ms);
        self.latest = self.latest.max(t_ms);
        (t_ms.saturating_sub(t0) / BIN_MS) as usize
    }

    pub fn frame(
        &mut self,
        serial: &str,
        class: DeviceClass,
        lighthouse: bool,
        parked: bool,
        f: &FrameSample,
    ) {
        match class {
            DeviceClass::TrackingReference => {
                if f.valid {
                    self.bases.insert(serial.to_string(), f.pos);
                }
            }
            DeviceClass::Hmd => self.head(parked, f),
            DeviceClass::Controller | DeviceClass::GenericTracker if lighthouse => {
                self.device(serial, class, parked, f);
            }
            _ => {}
        }
    }

    fn head(&mut self, parked: bool, f: &FrameSample) {
        if !f.valid || parked {
            return;
        }
        let b = self.bin(f.t_ms);
        let sector = yaw_deg(&f.rot).map_or(NONE, |y| sector_of(y) as u8);
        if let Some(h) = self.head
            && f.t_ms >= h.t_ms
        {
            let dt = (f.t_ms - h.t_ms).min(GAP_MS) as f64;
            self.centre.0[0] += f64::from(h.pos[0]) * dt;
            self.centre.0[1] += f64::from(h.pos[2]) * dt;
            self.centre.1 += dt;
            if f.t_ms - h.t_ms <= GAP_MS {
                let from = self.bin(h.t_ms) + 1;
                if self.sectors.len() < b {
                    self.sectors.resize(b, NONE);
                }
                for s in self.sectors.iter_mut().take(b).skip(from) {
                    *s = h.sector;
                }
            }
        }
        if self.sectors.len() <= b {
            self.sectors.resize(b + 1, NONE);
        }
        self.sectors[b] = sector;
        self.head = Some(Head {
            t_ms: f.t_ms,
            pos: f.pos,
            sector,
        });
    }

    fn device(&mut self, serial: &str, class: DeviceClass, parked: bool, f: &FrameSample) {
        self.bin(f.t_ms);
        let t0 = self.t0.unwrap_or(f.t_ms);
        let d = self.devs.entry(serial.to_string()).or_insert_with(|| Dev {
            class,
            last: None,
            recovered: None,
            drops: 0,
            bins: Vec::new(),
        });
        if parked {
            d.last = None;
            return;
        }
        let tracking = f.valid && f.state == TrackState::RunningOk;
        if let Some(prev) = d.last
            && f.t_ms >= prev.t_ms
        {
            let gap = f.t_ms - prev.t_ms;
            if prev.tracking {
                let at = (prev.t_ms.saturating_sub(t0) / BIN_MS) as usize;
                if d.bins.len() <= at {
                    d.bins.resize(at + 1, Bin::default());
                }
                let bin = &mut d.bins[at];
                bin.ms = bin.ms.saturating_add(gap.min(GAP_MS) as u16);
                let fresh = d
                    .recovered
                    .is_none_or(|r| f.t_ms.saturating_sub(r) > MERGE_MS);
                if !tracking && gap <= GAP_MS && fresh {
                    bin.drops = bin.drops.saturating_add(1);
                    d.drops += 1;
                }
            } else if tracking {
                d.recovered = Some(f.t_ms);
            }
        }
        d.last = Some(Last {
            t_ms: f.t_ms,
            tracking,
        });
    }

    fn evaluate(&mut self) -> Vec<Facing> {
        let n = self
            .devs
            .values()
            .map(|d| d.bins.len())
            .chain(std::iter::once(self.sectors.len()))
            .max()
            .unwrap_or(0);
        let known = self.sectors.iter().filter(|s| **s != NONE).count() as u64 * BIN_MS;
        if known < MIN_SPAN_MS {
            return Vec::new();
        }
        let lo = n / 10;
        let mut out = Vec::new();
        let mut notes: Vec<(String, String)> = Vec::new();
        for (serial, d) in &self.devs {
            if d.drops < MIN_DROPS {
                continue;
            }
            let (ms, drops) = tally(&d.bins, &self.sectors, 0, n);
            let Some(w) = best(&ms, &drops) else {
                continue;
            };
            if w.in_drops < MIN_DROPS || w.ratio < MIN_RATIO {
                continue;
            }
            let mut shifted_max: f64 = 0.0;
            for i in 0..SHIFTS {
                let shift = lo + (n - 2 * lo) * i / SHIFTS;
                let (ms, drops) = tally(&d.bins, &self.sectors, shift, n);
                let r = best(&ms, &drops).map_or(0.0, |w| w.ratio);
                shifted_max = shifted_max.max(r);
                if r >= w.ratio {
                    break;
                }
            }
            if shifted_max >= w.ratio {
                notes.push((
                    serial.clone(),
                    format!(
                        "facing {}: {:.1}x ({} drops in {:.0} min) but shifting the timeline gave {shifted_max:.1}x",
                        window_centre(w.start),
                        w.ratio,
                        w.in_drops,
                        w.in_ms as f64 / 60_000.0
                    ),
                ));
                continue;
            }
            let mut v = visits(&d.bins, &self.sectors, w.start);
            v.sort_by(|a, b| b.1.cmp(&a.1).then(b.0.cmp(&a.0)));
            let (top_ms, top_drops) = v.first().copied().unwrap_or((0, 0));
            let left_drops = w.in_drops.saturating_sub(top_drops);
            let left_ms = w.in_ms.saturating_sub(top_ms);
            let left_ratio =
                rate(left_drops, left_ms) / (w.out_drops as f64).max(0.5) * w.out_ms as f64;
            let with = v.iter().filter(|x| x.1 > 0).count();
            if with < MIN_OCCASIONS || left_drops < MIN_LEFT || left_ratio < MIN_RATIO {
                notes.push((
                    serial.clone(),
                    format!(
                        "facing {}: {:.1}x but only {with} turns had drops, {top_drops} of {} in one stretch, {left_drops} left at {left_ratio:.1}x",
                        window_centre(w.start),
                        w.ratio,
                        w.in_drops
                    ),
                ));
                continue;
            }
            notes.push((
                serial.clone(),
                format!(
                    "facing {}: {:.1}x ({} drops in {:.0} min, {} in {:.0} min elsewhere), shifted max {shifted_max:.1}x, {left_ratio:.1}x without the worst stretch, drops in {with} of {} turns",
                    window_centre(w.start),
                    w.ratio,
                    w.in_drops,
                    w.in_ms as f64 / 60_000.0,
                    w.out_drops,
                    w.out_ms as f64 / 60_000.0,
                    v.len()
                ),
            ));
            out.push(Facing {
                serial: serial.clone(),
                class: d.class,
                start: w.start,
                in_ms: w.in_ms,
                in_drops: w.in_drops,
                out_ms: w.out_ms,
                out_drops: w.out_drops,
                ratio: w.ratio,
                shifted_max,
                left_ratio,
                left_drops,
                visits: with,
            });
        }
        for (serial, note) in notes {
            let verdict = note.split(':').next().unwrap_or_default().to_string()
                + if out.iter().any(|f| f.serial == serial) {
                    " flagged"
                } else {
                    " passed over"
                };
            if self.noted.get(&serial) != Some(&verdict) {
                crate::dlog!("spots: {serial} {note}");
                self.noted.insert(serial, verdict);
            }
        }
        out
    }

    pub fn check(&mut self) -> bool {
        if self.latest < self.checked + EVAL_MS {
            return false;
        }
        self.checked = self.latest;
        let found = self.evaluate();
        let keys = |v: &[Facing]| -> Vec<(String, usize)> {
            v.iter().map(|f| (f.serial.clone(), f.start)).collect()
        };
        let changed = keys(&found) != keys(&self.found);
        self.found = found;
        changed
    }

    pub fn found(&self) -> &[Facing] {
        &self.found
    }

    fn centre(&self) -> Option<[f32; 2]> {
        let (sum, w) = self.centre;
        (w > 0.0).then(|| [(sum[0] / w) as f32, (sum[1] / w) as f32])
    }

    pub fn around(&self, start: usize) -> (Vec<String>, Vec<String>) {
        let mut ahead = Vec::new();
        let mut behind = Vec::new();
        let Some(c) = self.centre() else {
            return (ahead, behind);
        };
        let facing = window_centre(start);
        for (serial, p) in &self.bases {
            let (dx, dz) = (p[0] - c[0], p[2] - c[1]);
            if dx.hypot(dz) < 0.3 {
                continue;
            }
            let off = wrap(dx.atan2(-dz).to_degrees() - facing).abs();
            if off <= AHEAD_DEG {
                ahead.push(serial.clone());
            } else if off >= 180.0 - AHEAD_DEG {
                behind.push(serial.clone());
            }
        }
        (ahead, behind)
    }

    pub fn explain(&self, label: &dyn Fn(&str) -> String) -> Vec<Finding> {
        self.found
            .iter()
            .filter_map(|f| self.finding(f, label))
            .collect()
    }

    fn finding(&self, f: &Facing, label: &dyn Fn(&str) -> String) -> Option<Finding> {
        let (ahead, behind) = self.around(f.start);
        let bases = |list: &[String]| {
            if list.len() == 1 {
                format!("base {}", list[0])
            } else {
                format!("bases {}", join(list))
            }
        };
        let way = match (ahead.is_empty(), behind.is_empty()) {
            (false, false) => format!(
                "toward {}, with {} behind you",
                bases(&ahead),
                bases(&behind)
            ),
            (false, true) => format!("toward {}", bases(&ahead)),
            (true, false) => format!("with your back to {}", bases(&behind)),
            (true, true) => return None,
        };
        let name = label(&f.serial);
        let mins = |ms: u64| (ms as f64 / 60_000.0).round().max(1.0) as u64;
        let mut steps = vec![format!(
            "Stand facing that way and look for anything between {name} and the bases, including your own body or furniture."
        )];
        if f.class == DeviceClass::GenericTracker {
            steps.push("If you wear it on your back, chest or hip, move it a little to the side so your body blocks it less.".to_string());
        }
        steps.push("If you often face that way, add a base or move one so the side behind you is covered too.".to_string());
        Some(Finding {
            rule: Rule::Facing,
            key: f.serial.clone(),
            title: format!("{name} loses tracking when you face one way"),
            what: format!(
                "When you faced {way}, {name} lost tracking {:.1} times as often as when you faced elsewhere: {} times in {} minutes, compared with {} times in {} minutes. The drops came on {} separate occasions you turned that way.",
                f.ratio,
                f.in_drops,
                mins(f.in_ms),
                f.out_drops,
                mins(f.out_ms),
                f.visits
            ),
            steps,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnose::Diagnosis;
    use crate::event::{Kind, SignalEvent, Source};

    const T0: u64 = 1_000_000_000;
    const STEP_MS: u64 = 100;
    const TRACKER: &str = "LHR-905BD201";

    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.0 >> 33
        }

        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    fn sample(t_ms: u64, pos: [f32; 3], yaw: f32, tracking: bool) -> FrameSample {
        let (s, c) = yaw.to_radians().sin_cos();
        FrameSample {
            idx: 0,
            t_ms,
            pos,
            vel: [0.0; 3],
            rot: [
                [c, 0.0, -s, pos[0]],
                [0.0, 1.0, 0.0, pos[1]],
                [s, 0.0, c, pos[2]],
            ],
            ang_speed: 0.0,
            state: if tracking {
                TrackState::RunningOk
            } else {
                TrackState::RunningOutOfRange
            },
            valid: tracking,
        }
    }

    fn headings(rng: &mut Rng, secs: u64) -> Vec<f32> {
        let mut out = Vec::new();
        while (out.len() as u64) < secs {
            let yaw = rng.below(360) as f32 - 180.0;
            let dwell = 5 + rng.below(26);
            out.extend(std::iter::repeat_n(yaw, dwell as usize));
        }
        out.truncate(secs as usize);
        out
    }

    fn play(
        spots: &mut Spots,
        yaws: &[f32],
        mut drop_at: impl FnMut(usize, f32) -> bool,
        mut each_second: impl FnMut(&mut Spots, u64),
    ) {
        spots.frame(
            "LHB-AHEAD",
            DeviceClass::TrackingReference,
            false,
            false,
            &sample(T0, [3.0, 2.2, 0.0], -90.0, true),
        );
        spots.frame(
            "LHB-BEHIND",
            DeviceClass::TrackingReference,
            false,
            false,
            &sample(T0, [-3.0, 2.2, 0.0], 90.0, true),
        );
        let mut down_until = 0;
        for (sec, yaw) in yaws.iter().enumerate() {
            if drop_at(sec, *yaw) {
                down_until = T0 + sec as u64 * 1_000 + 400;
            }
            for k in 0..1_000 / STEP_MS {
                let t = T0 + sec as u64 * 1_000 + k * STEP_MS;
                spots.frame(
                    "HMD",
                    DeviceClass::Hmd,
                    false,
                    false,
                    &sample(t, [0.0, 1.7, 0.0], *yaw, true),
                );
                spots.frame(
                    TRACKER,
                    DeviceClass::GenericTracker,
                    true,
                    false,
                    &sample(t, [0.0, 1.0, 0.1], *yaw, t >= down_until),
                );
            }
            each_second(spots, T0 + sec as u64 * 1_000);
        }
    }

    fn facing_east(yaw: f32) -> bool {
        (45.0..135.0).contains(&yaw)
    }

    fn blocked(rng: &mut Rng) -> impl FnMut(usize, f32) -> bool + '_ {
        move |_, yaw| {
            if facing_east(yaw) {
                rng.below(15) == 0
            } else {
                rng.below(180) == 0
            }
        }
    }

    fn label(s: &str) -> String {
        s.to_string()
    }

    #[test]
    fn drops_that_follow_one_heading_over_many_turns_are_flagged() {
        let yaws = headings(&mut Rng(7), 60 * 60);
        let mut rng = Rng(11);
        let mut spots = Spots::default();
        play(&mut spots, &yaws, blocked(&mut rng), |s, _| {
            s.check();
        });
        let found = spots.found();
        assert_eq!(found.len(), 1, "{found:?}");
        let f = &found[0];
        assert_eq!(f.serial, TRACKER);
        assert_eq!(window_centre(f.start), 90.0);
        assert!(f.ratio >= MIN_RATIO, "{f:?}");
        assert!(f.shifted_max < f.ratio, "{f:?}");
        assert!(f.visits >= 10, "{f:?}");
        let text = spots.explain(&label);
        assert_eq!(text.len(), 1);
        assert_eq!(text[0].rule, Rule::Facing);
        assert_eq!(text[0].key, TRACKER);
        assert!(
            text[0]
                .what
                .contains("toward base LHB-AHEAD, with base LHB-BEHIND behind you"),
            "{}",
            text[0].what
        );
        assert!(
            text[0].what.contains(&format!("{} times in", f.in_drops)),
            "{}",
            text[0].what
        );
        assert_eq!(text[0].steps.len(), 3);
    }

    #[test]
    fn drops_spread_over_every_heading_are_not_flagged() {
        let yaws = headings(&mut Rng(7), 60 * 60);
        let mut rng = Rng(11);
        let mut spots = Spots::default();
        play(
            &mut spots,
            &yaws,
            |_, _| rng.below(40) == 0,
            |s, _| {
                s.check();
            },
        );
        let d = &spots.devs[TRACKER];
        assert!(d.drops >= 60, "only {} drops", d.drops);
        assert!(spots.found().is_empty(), "{:?}", spots.found());
        assert!(spots.explain(&label).is_empty());
    }

    #[test]
    fn one_bad_stretch_facing_one_way_is_not_flagged() {
        let mut yaws = headings(&mut Rng(7), 60 * 60);
        for y in &mut yaws[30 * 60..35 * 60] {
            *y = 90.0;
        }
        let mut rng = Rng(11);
        let mut spots = Spots::default();
        play(
            &mut spots,
            &yaws,
            |sec, _| {
                if (30 * 60..35 * 60).contains(&sec) {
                    sec % 10 == 0
                } else {
                    rng.below(180) == 0
                }
            },
            |s, _| {
                s.check();
            },
        );
        assert!(spots.devs[TRACKER].drops >= MIN_DROPS);
        assert!(spots.found().is_empty(), "{:?}", spots.found());
    }

    #[test]
    fn short_sessions_are_not_judged() {
        let yaws = headings(&mut Rng(7), 15 * 60);
        let mut rng = Rng(11);
        let mut spots = Spots::default();
        play(
            &mut spots,
            &yaws,
            |_, yaw| rng.below(if facing_east(yaw) { 4 } else { 180 }) == 0,
            |s, _| {
                s.check();
            },
        );
        assert!(spots.devs[TRACKER].drops >= MIN_DROPS);
        assert!(spots.found().is_empty(), "{:?}", spots.found());
    }

    #[test]
    fn diagnosis_reports_it_on_a_tracking_change() {
        let yaws = headings(&mut Rng(7), 60 * 60);
        let mut rng = Rng(11);
        let mut d = Diagnosis::default();
        let mut raised = false;
        let mut spots = std::mem::take(&mut d.spots);
        play(&mut spots, &yaws, blocked(&mut rng), |s, t| {
            std::mem::swap(&mut d.spots, s);
            let e = SignalEvent::at(
                t,
                Source::Api,
                Some(TRACKER.to_string()),
                Kind::TrackingState {
                    from: TrackState::RunningOk,
                    to: TrackState::RunningOutOfRange,
                },
                "",
            );
            raised |= d.ingest(&e);
            std::mem::swap(&mut d.spots, s);
        });
        d.spots = spots;
        assert!(raised);
        let found = d.findings(&label);
        assert!(
            found
                .iter()
                .any(|f| f.rule == Rule::Facing && f.key == TRACKER),
            "{found:?}"
        );
    }

    #[test]
    fn headings_map_to_the_right_sector() {
        assert_eq!(
            yaw_deg(&sample(0, [0.0; 3], 90.0, true).rot).map(sector_of),
            Some(6)
        );
        assert_eq!(
            yaw_deg(&sample(0, [0.0; 3], -179.0, true).rot).map(sector_of),
            Some(0)
        );
        assert_eq!(
            yaw_deg(&sample(0, [0.0; 3], 179.0, true).rot).map(sector_of),
            Some(7)
        );
        let mut down = sample(0, [0.0; 3], 0.0, true).rot;
        down[1][2] = 0.9;
        down[2][2] = 0.3;
        assert_eq!(yaw_deg(&down), None);
        assert!(in_window(5, 5) && in_window(6, 5) && !in_window(7, 5));
        assert!(in_window(0, 7) && !in_window(NONE, 7));
    }
}

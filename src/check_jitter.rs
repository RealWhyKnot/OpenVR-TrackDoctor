use crate::diagnose::{Finding, Rule, join};
use crate::event::{DeviceClass, Kind, SignalEvent, Source, TrackState};
use crate::signals::openvr::FrameSample;
use std::collections::BTreeMap;

pub const BLOCK_MS: u64 = 1_000;
pub const BLOCK_FRAMES: usize = 40;
pub const SETTLE_MS: u64 = 3_000;
pub const GAP_MS: u64 = 500;
pub const STILL_SPIN_RAD_S: f32 = 0.035;
pub const STILL_TURN_DEG: f64 = 0.3;
pub const MOVING_MM: f64 = 20.0;
pub const STRETCH_MIN_MS: u64 = 5_000;
pub const STRETCH_MAX_MS: u64 = 120_000;
pub const JUDGE_SECS: f32 = 30.0;
pub const SHAKY_MM: f32 = 1.0;
pub const ONE_BASE_RATIO: f32 = 2.0;
pub const ONE_BASE_SHARE: f32 = 0.5;
pub const COMPARE_SECS: f32 = 10.0;
pub const FAR_M: f32 = 5.0;
pub const LOG_EVERY_MS: u64 = 30_000;

#[derive(Clone, Debug, Default)]
struct Stretch {
    start: u64,
    end: u64,
    sq: f64,
    n: u64,
    bases: Option<u32>,
    pos: [f32; 3],
}

#[derive(Clone, Debug, Default)]
struct Live {
    since: Option<u64>,
    last: u64,
    start: u64,
    pts: Vec<(u64, [f32; 3])>,
    first: [[f32; 4]; 3],
    turn: f64,
    spin: f32,
    stretch: Option<Stretch>,
    logged: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Still {
    pub mm: f32,
    pub secs: f32,
    pub bases: Option<u32>,
    pub base_m: Option<f32>,
}

#[derive(Clone, Debug, Default)]
pub struct Jitter {
    live: BTreeMap<String, Live>,
    base_pos: BTreeMap<String, [f32; 3]>,
    seeing: BTreeMap<String, u32>,
    still: BTreeMap<String, Vec<Still>>,
    shown: Vec<(String, Cause)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    OneBase,
    Far,
    Mount,
    Device,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DevStill {
    pub serial: String,
    pub secs: f32,
    pub mm: f32,
    pub one: Option<(f32, f32)>,
    pub many: Option<(f32, f32)>,
    pub one_share: f32,
    pub base_m: Option<f32>,
}

impl DevStill {
    fn shaky(&self) -> bool {
        self.secs >= JUDGE_SECS && self.mm > SHAKY_MM
    }

    fn one_base(&self) -> bool {
        match (self.one, self.many) {
            (Some((a, one)), Some((b, many))) if a >= COMPARE_SECS && b >= COMPARE_SECS => {
                one >= many * ONE_BASE_RATIO || (self.one_share >= ONE_BASE_SHARE && one > SHAKY_MM)
            }
            (Some((_, one)), _) => self.one_share >= ONE_BASE_SHARE && one > SHAKY_MM,
            _ => false,
        }
    }

    fn cause(&self) -> Cause {
        if self.one_base() {
            Cause::OneBase
        } else if self.base_m.is_some_and(|m| m >= FAR_M) {
            Cause::Far
        } else {
            Cause::Device
        }
    }
}

fn pooled(items: &[&Still]) -> Option<(f32, f32)> {
    let secs: f32 = items.iter().map(|s| s.secs).sum();
    if secs <= 0.0 {
        return None;
    }
    let sq: f32 = items.iter().map(|s| s.secs * s.mm * s.mm).sum();
    Some((secs, (sq / secs).sqrt()))
}

fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

fn turn_deg(a: &[[f32; 4]; 3], b: &[[f32; 4]; 3]) -> f64 {
    let trace: f64 = a
        .iter()
        .zip(b)
        .flat_map(|(ra, rb)| ra[..3].iter().zip(&rb[..3]))
        .map(|(x, y)| *x as f64 * *y as f64)
        .sum();
    ((trace - 1.0) / 2.0).clamp(-1.0, 1.0).acos().to_degrees()
}

fn solve3(m: [[f64; 3]; 3], v: [f64; 3]) -> Option<[f64; 3]> {
    let det = |m: [[f64; 3]; 3]| {
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
            - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    };
    let d = det(m);
    if d.abs() < 1e-12 {
        return None;
    }
    let mut out = [0.0; 3];
    for (k, o) in out.iter_mut().enumerate() {
        let mut mk = m;
        for (row, vi) in mk.iter_mut().zip(v) {
            row[k] = vi;
        }
        *o = det(mk) / d;
    }
    Some(out)
}

fn block_spread(pts: &[(u64, [f32; 3])]) -> Option<([f32; 3], f64, f64)> {
    let (&(t_first, p0), &(t_last, _)) = (pts.first()?, pts.last()?);
    let mid = (t_first + t_last) as f64 / 2.0;
    let span = (t_last - t_first) as f64 / 2000.0;
    let tau = |t: u64| (t as f64 - mid) / 1000.0;
    let mut s = [0.0f64; 5];
    for (t, _) in pts {
        let x = tau(*t);
        for (k, sk) in s.iter_mut().enumerate() {
            *sk += x.powi(k as i32);
        }
    }
    let m = [[s[0], s[1], s[2]], [s[1], s[2], s[3]], [s[2], s[3], s[4]]];
    let mut mean = [0.0f32; 3];
    let mut sq = 0.0;
    let mut moved = 0.0;
    for axis in 0..3 {
        let y = |p: &[f32; 3]| p[axis] as f64 - p0[axis] as f64;
        let mut v = [0.0f64; 3];
        for (t, p) in pts {
            let x = tau(*t);
            v[0] += y(p);
            v[1] += x * y(p);
            v[2] += x * x * y(p);
        }
        let [a, b, c] = solve3(m, v)?;
        let fit = |x: f64| a + b * x + c * x * x;
        sq += pts
            .iter()
            .map(|(t, p)| (y(p) - fit(tau(*t))).powi(2))
            .sum::<f64>();
        moved += (fit(span) - fit(-span)).powi(2);
        mean[axis] = (p0[axis] as f64 + fit(0.0)) as f32;
    }
    Some((mean, sq, moved.sqrt() * 1000.0))
}

fn fmt_secs(secs: f32) -> String {
    let s = secs.round() as u64;
    if s < 120 {
        format!("{s} seconds")
    } else if s < 7200 {
        format!("{} minutes", s / 60)
    } else {
        format!("{:.1} hours", s as f32 / 3600.0)
    }
}

fn bases_word(n: Option<u32>) -> String {
    match n {
        None => "an unknown number of bases".into(),
        Some(1) => "1 base".into(),
        Some(n) => format!("{n} bases"),
    }
}

pub fn still_text(s: &Still) -> String {
    let near = s
        .base_m
        .map_or_else(String::new, |m| format!(", {m:.1} m from the nearest one"));
    format!(
        "sat still for {} and wobbled {:.2} mm, seeing {}{near}",
        fmt_secs(s.secs),
        s.mm,
        bases_word(s.bases)
    )
}

impl Jitter {
    fn nearest(&self, pos: [f32; 3]) -> Option<f32> {
        self.base_pos
            .values()
            .map(|b| dist(*b, pos))
            .min_by(|a, b| a.total_cmp(b))
    }

    fn close(&mut self, serial: &str) -> Option<SignalEvent> {
        let st = self.live.get_mut(serial)?.stretch.take()?;
        let span = st.end.saturating_sub(st.start);
        if span < STRETCH_MIN_MS || st.n == 0 {
            return None;
        }
        let still = Still {
            mm: ((st.sq / st.n as f64).sqrt() * 1000.0) as f32,
            secs: span as f32 / 1000.0,
            bases: st.bases,
            base_m: self.nearest(st.pos),
        };
        let live = self.live.get_mut(serial)?;
        if st.end.saturating_sub(live.logged) >= LOG_EVERY_MS {
            live.logged = st.end;
            crate::dlog!(
                "jitter: {serial} still {:.0}s, {:.3} mm rms, bases {:?}, nearest base {:?} m",
                still.secs,
                still.mm,
                still.bases,
                still.base_m
            );
        }
        let kind = Kind::StillJitter {
            mm: still.mm,
            secs: still.secs,
            bases: still.bases,
            base_m: still.base_m,
        };
        Some(SignalEvent::at(
            st.end,
            Source::Api,
            Some(serial.to_string()),
            kind,
            still_text(&still),
        ))
    }

    pub fn frame(
        &mut self,
        serial: &str,
        class: DeviceClass,
        lighthouse: bool,
        parked: bool,
        f: &FrameSample,
    ) -> Option<SignalEvent> {
        if class == DeviceClass::TrackingReference {
            if f.valid {
                self.base_pos.insert(serial.to_string(), f.pos);
            }
            return None;
        }
        if !lighthouse
            || !matches!(
                class,
                DeviceClass::Hmd | DeviceClass::Controller | DeviceClass::GenericTracker
            )
        {
            return None;
        }
        let live = self.live.entry(serial.to_string()).or_default();
        let gap = f.t_ms.saturating_sub(live.last) > GAP_MS;
        live.last = f.t_ms;
        if !f.valid || f.state != TrackState::RunningOk || parked || gap {
            live.since = None;
            live.pts.clear();
            return self.close(serial);
        }
        let since = *live.since.get_or_insert(f.t_ms);
        if f.t_ms.saturating_sub(since) < SETTLE_MS {
            return None;
        }
        if live.pts.is_empty() {
            live.start = f.t_ms;
            live.first = f.rot;
            live.turn = 0.0;
            live.spin = 0.0;
        }
        live.pts.push((f.t_ms, f.pos));
        live.turn = live.turn.max(turn_deg(&live.first, &f.rot));
        live.spin = live.spin.max(f.ang_speed);
        if f.t_ms.saturating_sub(live.start) < BLOCK_MS {
            return None;
        }
        let pts = std::mem::take(&mut live.pts);
        let (start, turn, spin) = (live.start, live.turn, live.spin);
        let bases = self.seeing.get(serial).copied();
        if pts.len() < BLOCK_FRAMES {
            return self.close(serial);
        }
        let Some((mean, sq, moved_mm)) = block_spread(&pts) else {
            return self.close(serial);
        };
        let rms_mm = (sq / pts.len() as f64).sqrt() * 1000.0;
        let still = spin < STILL_SPIN_RAD_S
            && turn < STILL_TURN_DEG
            && rms_mm < MOVING_MM
            && moved_mm < MOVING_MM;
        if !still {
            return self.close(serial);
        }
        let live = self.live.get_mut(serial)?;
        let same = live.stretch.as_ref().is_some_and(|s| s.bases == bases);
        let mut out = None;
        if !same {
            out = self.close(serial);
        }
        let live = self.live.get_mut(serial)?;
        let st = live.stretch.get_or_insert(Stretch {
            start,
            end: start,
            bases,
            ..Stretch::default()
        });
        st.end = f.t_ms;
        st.sq += sq;
        st.n += pts.len().saturating_sub(3) as u64;
        st.pos = mean;
        if out.is_none() && st.end.saturating_sub(st.start) >= STRETCH_MAX_MS {
            out = self.close(serial);
        }
        out
    }

    pub fn ingest(&mut self, ev: &SignalEvent) -> bool {
        match &ev.kind {
            Kind::BaseSight { seeing, .. } => {
                if let Some(d) = &ev.device {
                    self.seeing.insert(d.clone(), seeing.len() as u32);
                }
                false
            }
            Kind::LogRotated => {
                self.seeing.clear();
                false
            }
            Kind::DeviceDeactivated
            | Kind::DevicePowerOff
            | Kind::WirelessDisconnect
            | Kind::StandbyStart => {
                if let Some(d) = &ev.device {
                    self.seeing.remove(d);
                }
                false
            }
            Kind::StillJitter {
                mm,
                secs,
                bases,
                base_m,
            } => {
                let Some(d) = &ev.device else {
                    return false;
                };
                self.still.entry(d.clone()).or_default().push(Still {
                    mm: *mm,
                    secs: *secs,
                    bases: *bases,
                    base_m: *base_m,
                });
                let now = self.verdict();
                if now == self.shown {
                    return false;
                }
                crate::dlog!("jitter: findings now {now:?}");
                self.shown = now;
                true
            }
            _ => false,
        }
    }

    pub fn devices(&self) -> Vec<DevStill> {
        self.still
            .iter()
            .filter_map(|(serial, all)| {
                let refs: Vec<&Still> = all.iter().collect();
                let (secs, mm) = pooled(&refs)?;
                let one: Vec<&Still> = all
                    .iter()
                    .filter(|s| s.bases.is_some_and(|b| b <= 1))
                    .collect();
                let many: Vec<&Still> = all
                    .iter()
                    .filter(|s| s.bases.is_some_and(|b| b >= 2))
                    .collect();
                let known: f32 = one.iter().chain(&many).map(|s| s.secs).sum();
                let one = pooled(&one);
                let m: Vec<(f32, f32)> = all
                    .iter()
                    .filter_map(|s| s.base_m.map(|m| (s.secs, m)))
                    .collect();
                let msecs: f32 = m.iter().map(|x| x.0).sum();
                Some(DevStill {
                    serial: serial.clone(),
                    secs,
                    mm,
                    one_share: if known > 0.0 {
                        one.map_or(0.0, |o| o.0) / known
                    } else {
                        0.0
                    },
                    one,
                    many: pooled(&many),
                    base_m: (msecs > 0.0).then(|| m.iter().map(|x| x.0 * x.1).sum::<f32>() / msecs),
                })
            })
            .collect()
    }

    fn setup(&self) -> Option<(Vec<DevStill>, Vec<DevStill>, Cause)> {
        let judged: Vec<DevStill> = self
            .devices()
            .into_iter()
            .filter(|d| d.secs >= JUDGE_SECS)
            .collect();
        let shaky: Vec<DevStill> = judged.iter().filter(|d| d.shaky()).cloned().collect();
        if shaky.len() < 2 || shaky.len() * 2 <= judged.len() {
            return None;
        }
        let ones = shaky.iter().filter(|d| d.cause() == Cause::OneBase).count();
        let far = shaky
            .iter()
            .filter(|d| d.base_m.is_some_and(|m| m >= FAR_M))
            .count();
        let cause = if ones * 2 > shaky.len() {
            Cause::OneBase
        } else if far * 2 > shaky.len() {
            Cause::Far
        } else {
            Cause::Mount
        };
        Some((judged, shaky, cause))
    }

    fn verdict(&self) -> Vec<(String, Cause)> {
        if let Some((_, _, cause)) = self.setup() {
            return vec![("all".into(), cause)];
        }
        self.devices()
            .into_iter()
            .filter(DevStill::shaky)
            .map(|d| {
                let c = d.cause();
                (d.serial, c)
            })
            .collect()
    }

    pub fn active(&self) -> bool {
        !self.verdict().is_empty()
    }

    pub fn explain(&self, label: &dyn Fn(&str) -> String) -> Vec<Finding> {
        const NORMAL: &str = "Base station tracking usually keeps a device that's sitting still steady to well under 1 mm.";
        let finding = |key: &str, title: String, what: String, steps: &[&str]| Finding {
            rule: Rule::Shaky,
            key: key.to_string(),
            title,
            what,
            steps: steps.iter().map(|s| s.to_string()).collect(),
        };
        if let Some((judged, shaky, cause)) = self.setup() {
            let pool: Vec<Still> = shaky
                .iter()
                .map(|d| Still {
                    mm: d.mm,
                    secs: d.secs,
                    bases: None,
                    base_m: None,
                })
                .collect();
            let refs: Vec<&Still> = pool.iter().collect();
            let mm = pooled(&refs).map_or(0.0, |p| p.1);
            let names: Vec<String> = shaky.iter().map(|d| label(&d.serial)).collect();
            let who = if shaky.len() == judged.len() {
                format!(
                    "{} all wobbled by about {mm:.1} mm while sitting still.",
                    join(&names)
                )
            } else {
                format!(
                    "{} of the {} devices that sat still wobbled by about {mm:.1} mm: {}.",
                    shaky.len(),
                    judged.len(),
                    join(&names)
                )
            };
            let many: f32 = shaky.iter().filter_map(|d| d.many.map(|m| m.0)).sum();
            let total: f32 = shaky.iter().map(|d| d.secs).sum();
            let (why, steps): (String, &[&str]) = match cause {
                Cause::OneBase => (
                    "Most of that time they could see only one base.".into(),
                    &[
                        "Check that at least two bases can see where you play and that nothing blocks them.",
                    ],
                ),
                Cause::Far => (
                    format!(
                        "They sat about {:.1} m from the nearest base on average, and tracking gets shakier with distance.",
                        shaky.iter().filter_map(|d| d.base_m).sum::<f32>()
                            / shaky.iter().filter(|d| d.base_m.is_some()).count().max(1) as f32
                    ),
                    &[
                        "Move the bases closer to where you play, or bring the play space closer to them.",
                    ],
                ),
                _ => (
                    if many * 2.0 > total {
                        "They could see two or more bases most of that time, so a base that moves or vibrates is the likely cause.".into()
                    } else {
                        "A base that moves or vibrates would do this, since everything it tracks moves with it.".into()
                    },
                    &[
                        "Check that every base is mounted firmly. A base on a light stand or shelf that shakes when someone walks or music plays shakes everything it tracks.",
                        "Tighten loose mounts, or move the base onto something solid.",
                    ],
                ),
            };
            return vec![finding(
                "all",
                "Your devices shake when they sit still".into(),
                format!("{who} {NORMAL} {why}"),
                steps,
            )];
        }
        let all = self.devices();
        all.iter()
            .filter(|d| d.shaky())
            .map(|d| {
                let name = label(&d.serial);
                let steady: Vec<&DevStill> = all
                    .iter()
                    .filter(|o| o.serial != d.serial && o.secs >= JUDGE_SECS && !o.shaky())
                    .collect();
                let others = match steady.iter().map(|o| o.mm).reduce(f32::max) {
                    Some(top) if steady.len() == 1 => {
                        format!(" {} stayed within {top:.1} mm.", label(&steady[0].serial))
                    }
                    Some(top) => format!(" Your other devices stayed within {top:.1} mm."),
                    None => String::new(),
                };
                let (why, steps): (String, &[&str]) = match d.cause() {
                    Cause::OneBase => (
                        match (d.one, d.many) {
                            (Some((_, one)), Some((s, many))) if s >= COMPARE_SECS => format!(
                                " It wobbled {one:.1} mm when it could see only one base, against {many:.1} mm when it saw two or more."
                            ),
                            _ => format!(
                                " It could see only one base for {:.0}% of that time.",
                                d.one_share * 100.0
                            ),
                        },
                        &[
                            "Put it where at least two bases can see it.",
                            "If a base can't see that spot, aim it there or clear what's in the way.",
                        ],
                    ),
                    Cause::Far => (
                        format!(
                            " It sat about {:.1} m from the nearest base, and tracking gets shakier with distance.",
                            d.base_m.unwrap_or(0.0)
                        ),
                        &["Use it closer to the bases, or move a base closer to where you use it."],
                    ),
                    _ => (
                        String::new(),
                        &[
                            "If it sat on something that shakes, like a desk with a PC or speakers on it, move it somewhere solid and check again.",
                            "Swap it with another device in the same spot. If the shaking follows the device, the device itself may be faulty.",
                        ],
                    ),
                };
                finding(
                    &d.serial,
                    format!("{name} shakes when it sits still"),
                    format!(
                        "While {name} sat still for {}, its position wobbled by about {:.1} mm. {NORMAL}{why}{others}",
                        fmt_secs(d.secs),
                        d.mm
                    ),
                    steps,
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: u64 = 1_800_000_000_000;
    const IDENT: [[f32; 4]; 3] = [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
    ];

    struct Noise(u32);

    impl Noise {
        fn next(&mut self) -> f32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 17;
            self.0 ^= self.0 << 5;
            self.0 as f32 / u32::MAX as f32 * 2.0 - 1.0
        }

        fn gauss(&mut self) -> f32 {
            (0..6).map(|_| self.next()).sum::<f32>() / 2.0_f32.sqrt()
        }
    }

    fn frame(t_ms: u64, pos: [f32; 3]) -> FrameSample {
        FrameSample {
            idx: 1,
            t_ms,
            pos,
            vel: [0.0; 3],
            rot: IDENT,
            ang_speed: 0.004,
            state: TrackState::RunningOk,
            valid: true,
        }
    }

    fn sight(t_ms: u64, dev: &str, n: u32) -> SignalEvent {
        SignalEvent::at(
            t_ms,
            Source::Log,
            Some(dev.into()),
            Kind::BaseSight {
                ch: 1,
                gained: true,
                seeing: (1..=n).collect(),
                ids: Vec::new(),
            },
            "",
        )
    }

    fn sit(
        j: &mut Jitter,
        dev: &str,
        from_ms: u64,
        secs: u64,
        sigma_m: f32,
        seed: u32,
    ) -> Vec<SignalEvent> {
        let mut noise = Noise(seed);
        let at = [0.4, 0.9, -0.2];
        let mut out = Vec::new();
        for i in 0..secs * 1000 / 11 {
            let p = [
                at[0] + noise.gauss() * sigma_m,
                at[1] + noise.gauss() * sigma_m,
                at[2] + noise.gauss() * sigma_m,
            ];
            let f = frame(from_ms + i * 11, p);
            out.extend(j.frame(dev, DeviceClass::GenericTracker, true, false, &f));
        }
        out
    }

    fn feed(j: &mut Jitter, evs: &[SignalEvent]) {
        for e in evs {
            j.ingest(e);
        }
    }

    fn base(j: &mut Jitter, serial: &str, pos: [f32; 3]) {
        let f = frame(T0, pos);
        j.frame(serial, DeviceClass::TrackingReference, true, false, &f);
    }

    #[test]
    fn noise_measures_close_to_what_went_in() {
        let mut j = Jitter::default();
        base(&mut j, "LHB-1", [0.4, 2.4, 2.2]);
        let evs = sit(&mut j, "LHR-A", T0, 200, 0.0003, 7);
        assert_eq!(evs.len(), 1, "{evs:?}");
        let Kind::StillJitter {
            mm, secs, base_m, ..
        } = evs[0].kind
        else {
            panic!("{:?}", evs[0].kind);
        };
        let want = 0.3 * 3f32.sqrt();
        assert!((mm - want).abs() < 0.05, "{mm} vs {want}");
        assert!((secs - 120.0).abs() < 1.5, "{secs}");
        assert!((base_m.unwrap() - 2.83).abs() < 0.01, "{base_m:?}");
    }

    #[test]
    fn steady_devices_stay_quiet() {
        let mut j = Jitter::default();
        for (dev, seed) in [("LHR-A", 3), ("LHR-B", 11), ("LHR-C", 29)] {
            feed(&mut j, &[sight(T0, dev, 2)]);
            let evs = sit(&mut j, dev, T0 + 10, 90, 0.0002, seed);
            feed(&mut j, &evs);
            let evs = sit(&mut j, dev, T0 + 200_000, 2, 0.0002, seed);
            feed(&mut j, &evs);
        }
        assert_eq!(j.devices().len(), 3);
        assert!(
            j.devices().iter().all(|d| d.mm < 0.5 && d.secs > 80.0),
            "{:?}",
            j.devices()
        );
        assert!(!j.active());
        assert!(j.explain(&|s| s.to_string()).is_empty());
    }

    #[test]
    fn one_shaky_tracker_seeing_one_base_is_named() {
        let mut j = Jitter::default();
        base(&mut j, "LHB-1", [0.4, 2.4, 2.2]);
        feed(&mut j, &[sight(T0, "LHR-A", 2), sight(T0, "LHR-B", 2)]);
        let mut evs = sit(&mut j, "LHR-A", T0, 40, 0.0002, 5);
        evs.extend(sit(&mut j, "LHR-B", T0, 40, 0.0002, 9));
        feed(&mut j, &evs);
        let t1 = T0 + 50_000;
        feed(&mut j, &[sight(t1, "LHR-A", 1)]);
        let mut evs = sit(&mut j, "LHR-A", t1, 60, 0.0015, 13);
        evs.extend(sit(&mut j, "LHR-A", t1 + 70_000, 2, 0.0, 1));
        evs.extend(sit(&mut j, "LHR-B", t1, 60, 0.0002, 17));
        evs.extend(sit(&mut j, "LHR-B", t1 + 70_000, 2, 0.0, 1));
        let mut changed = false;
        for e in &evs {
            changed |= j.ingest(e);
        }
        assert!(changed);
        assert!(j.active());
        let found = j.explain(&|s| s.to_string());
        assert_eq!(found.len(), 1, "{found:?}");
        let f = &found[0];
        assert_eq!(f.rule, Rule::Shaky);
        assert_eq!(f.key, "LHR-A");
        assert_eq!(f.title, "LHR-A shakes when it sits still");
        assert!(f.what.contains("only one base"), "{}", f.what);
        assert!(f.what.contains("LHR-B stayed within 0.3 mm"), "{}", f.what);
        assert!(f.steps[0].contains("two bases"), "{:?}", f.steps);
        let a = j
            .devices()
            .into_iter()
            .find(|d| d.serial == "LHR-A")
            .unwrap();
        let (one, many) = (a.one.unwrap().1, a.many.unwrap().1);
        assert!(one > 2.0 && many < 0.5, "{one} {many}");
    }

    #[test]
    fn whole_setup_shaking_with_two_bases_points_at_the_mounts() {
        let mut j = Jitter::default();
        base(&mut j, "LHB-1", [0.4, 2.4, 2.2]);
        let mut evs = Vec::new();
        for (dev, seed) in [("LHR-A", 3), ("LHR-B", 11), ("LHR-C", 29)] {
            feed(&mut j, &[sight(T0, dev, 2)]);
            evs.extend(sit(&mut j, dev, T0, 50, 0.0012, seed));
            evs.extend(sit(&mut j, dev, T0 + 60_000, 2, 0.0, 1));
        }
        feed(&mut j, &evs);
        let found = j.explain(&|s| s.to_string());
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].key, "all");
        assert!(
            found[0]
                .what
                .starts_with("LHR-A, LHR-B and LHR-C all wobbled"),
            "{}",
            found[0].what
        );
        assert!(
            found[0]
                .what
                .contains("two or more bases most of that time"),
            "{}",
            found[0].what
        );
        assert!(
            found[0].steps[0].contains("mounted firmly"),
            "{:?}",
            found[0].steps
        );
    }

    #[test]
    fn breathing_under_a_worn_tracker_is_not_counted_as_shaking() {
        let mut j = Jitter::default();
        let mut noise = Noise(41);
        let mut evs = Vec::new();
        for i in 0..90_000 / 11u64 {
            let t = i as f32 * 0.011;
            let breath = (t * std::f32::consts::TAU * 0.25).sin() * 0.004;
            let f = frame(
                T0 + i * 11,
                [
                    0.1 + noise.gauss() * 0.0002,
                    1.1 + breath + noise.gauss() * 0.0002,
                    0.3 + breath * 0.5 + noise.gauss() * 0.0002,
                ],
            );
            evs.extend(j.frame("LHR-A", DeviceClass::GenericTracker, true, false, &f));
        }
        evs.extend(sit(&mut j, "LHR-A", T0 + 100_000, 2, 0.0, 1));
        feed(&mut j, &evs);
        let d = &j.devices()[0];
        assert!(d.secs > 80.0, "{d:?}");
        assert!(d.mm < 0.45, "{d:?}");
        assert!(!j.active());
    }

    #[test]
    fn a_device_being_carried_or_turned_is_not_measured() {
        let mut j = Jitter::default();
        let mut noise = Noise(21);
        let mut evs = Vec::new();
        for i in 0..60_000 / 11u64 {
            let t = i as f32 * 0.011;
            let mut f = frame(
                T0 + i * 11,
                [
                    0.4 + (t * 0.5).sin() * 0.003,
                    1.0 + noise.gauss() * 0.002,
                    0.0,
                ],
            );
            f.ang_speed = 0.2 + noise.next().abs() * 0.3;
            evs.extend(j.frame("LHR-A", DeviceClass::Controller, true, false, &f));
        }
        assert!(evs.is_empty(), "{evs:?}");
        let mut evs = Vec::new();
        for i in 0..60_000 / 11u64 {
            let mut f = frame(T0 + 100_000 + i * 11, [0.4, 1.0, 0.0]);
            f.valid = i % 400 > 50;
            evs.extend(j.frame("LHR-A", DeviceClass::Controller, true, false, &f));
        }
        assert!(evs.is_empty(), "{evs:?}");
    }

    #[test]
    fn devices_on_other_tracking_systems_are_skipped() {
        let mut j = Jitter::default();
        let mut out = Vec::new();
        for i in 0..20_000 / 11u64 {
            let f = frame(T0 + i * 11, [0.0, 1.6, 0.0]);
            out.extend(j.frame("QUEST", DeviceClass::Hmd, false, false, &f));
        }
        let f = frame(T0 + 30_000, [0.0, 1.6, 0.0]);
        out.extend(j.frame("QUEST", DeviceClass::Hmd, false, true, &f));
        assert!(out.is_empty());
        assert!(j.devices().is_empty());
    }
}

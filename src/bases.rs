use crate::event::{Kind, SignalEvent, Source};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const BIG_MOVE_MM: u32 = 50;
pub const BIG_MOVE_DEG: f32 = 1.0;
pub const STEADY_MM: u32 = 20;
pub const LOOSE_TILT_DEG: f64 = 1.5;
pub const SETTLE_MS: u64 = 10 * 60_000;
const POP_WINDOW_MS: u64 = 100;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MoveStats {
    pub moves: usize,
    pub big: usize,
    pub early: usize,
    pub max_mm: u32,
    pub max_deg: f32,
    pub pops: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Steadiness {
    Steady,
    Small,
    Settling,
    Shifting,
}

impl MoveStats {
    pub fn steadiness(&self) -> Steadiness {
        if self.big > 0 {
            Steadiness::Shifting
        } else if self.early > 0 {
            Steadiness::Settling
        } else if self.max_mm >= STEADY_MM {
            Steadiness::Small
        } else {
            Steadiness::Steady
        }
    }
}

pub fn is_big(mm: u32, deg: f32) -> bool {
    mm >= BIG_MOVE_MM || deg >= BIG_MOVE_DEG
}

pub fn short_serial(serial: &str) -> String {
    serial
        .strip_prefix("LHB-")
        .unwrap_or(serial)
        .to_ascii_uppercase()
}

pub const TURN_DEG: f64 = 3.0;
pub const SHIFT_MM: f64 = 50.0;
pub const HELD_DEG: f64 = 1.5;
pub const HELD_MM: f64 = 30.0;
const REL_WINDOW_MS: u64 = 5 * 60_000;
const REL_SPAN_MS: u64 = 10 * 60_000;
const REL_MIN: usize = 3;
const REL_LATE_MIN: usize = 2;
const REL_PAIRS_MIN: usize = 2;

type Sample = (u64, u32, f32);

#[derive(Clone, Debug, Default)]
pub struct Relations {
    run: usize,
    pairs: BTreeMap<(usize, String, String), Vec<Sample>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Turned {
    pub run: usize,
    pub base: String,
    pub deg: f64,
    pub mm: f64,
    pub against: Vec<String>,
}

#[derive(Clone, Copy, PartialEq)]
enum PairChange {
    Shifted(f64, f64),
    Held,
    Unclear,
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

fn change(seen: &[Sample]) -> PairChange {
    let (Some(first), Some(last)) = (
        seen.iter().map(|s| s.0).min(),
        seen.iter().map(|s| s.0).max(),
    ) else {
        return PairChange::Unclear;
    };
    let early: Vec<_> = seen
        .iter()
        .filter(|s| s.0 <= first + REL_WINDOW_MS)
        .collect();
    let late: Vec<_> = seen
        .iter()
        .filter(|s| s.0 + REL_WINDOW_MS >= last && s.0 >= first + REL_SPAN_MS)
        .collect();
    if late.len() < REL_LATE_MIN || early.len() + late.len() < REL_MIN {
        return PairChange::Unclear;
    }
    let mid = |v: &[&Sample]| {
        (
            median(v.iter().map(|s| f64::from(s.1)).collect()),
            median(v.iter().map(|s| f64::from(s.2)).collect()),
        )
    };
    let ((mm0, deg0), (mm1, deg1)) = (mid(&early), mid(&late));
    let mm = (mm1 - mm0).abs();
    let d = (deg1 - deg0).abs() % 360.0;
    let deg = d.min(360.0 - d);
    if deg >= TURN_DEG || mm >= SHIFT_MM {
        PairChange::Shifted(deg, mm)
    } else if deg < HELD_DEG && mm < HELD_MM {
        PairChange::Held
    } else {
        PairChange::Unclear
    }
}

impl Relations {
    pub fn ingest(&mut self, ev: &SignalEvent) {
        match &ev.kind {
            Kind::LogRotated => self.run += 1,
            Kind::BaseRelation { a, b, mm, deg } => {
                let (a, b) = (short_serial(a), short_serial(b));
                let (a, b) = if a <= b { (a, b) } else { (b, a) };
                self.pairs
                    .entry((self.run, a, b))
                    .or_default()
                    .push((ev.t_ms, *mm, *deg));
            }
            _ => {}
        }
    }

    pub fn turned(&self) -> Vec<Turned> {
        let mut runs: BTreeMap<usize, Vec<(&str, &str, PairChange)>> = BTreeMap::new();
        for ((run, a, b), seen) in &self.pairs {
            runs.entry(*run)
                .or_default()
                .push((a.as_str(), b.as_str(), change(seen)));
        }
        let mut out = Vec::new();
        for (run, pairs) in runs {
            let shifted: Vec<_> = pairs
                .iter()
                .filter_map(|(a, b, c)| match c {
                    PairChange::Shifted(deg, mm) => Some((*a, *b, *deg, *mm)),
                    _ => None,
                })
                .collect();
            let held: Vec<_> = pairs
                .iter()
                .filter(|p| p.2 == PairChange::Held)
                .map(|p| (p.0, p.1))
                .collect();
            let Some(&(a0, b0, _, _)) = shifted.first() else {
                continue;
            };
            if shifted.len() < REL_PAIRS_MIN {
                continue;
            }
            let found: Vec<&str> = [a0, b0]
                .into_iter()
                .filter(|x| shifted.iter().all(|s| s.0 == *x || s.1 == *x))
                .filter(|x| held.iter().any(|h| h.0 != *x && h.1 != *x))
                .collect();
            let [base] = found[..] else {
                continue;
            };
            let turns: Vec<f64> = shifted
                .iter()
                .map(|s| s.2)
                .filter(|d| *d >= TURN_DEG)
                .collect();
            out.push(Turned {
                run,
                base: base.to_string(),
                deg: if turns.is_empty() {
                    0.0
                } else {
                    turns.iter().sum::<f64>() / turns.len() as f64
                },
                mm: shifted
                    .iter()
                    .map(|s| s.3)
                    .filter(|m| *m >= SHIFT_MM)
                    .fold(0.0, f64::max),
                against: shifted
                    .iter()
                    .map(|s| if s.0 == base { s.1 } else { s.0 }.to_string())
                    .collect(),
            });
        }
        out
    }

    pub fn turned_bases(&self) -> Vec<Turned> {
        let mut out: Vec<Turned> = Vec::new();
        for t in self.turned() {
            match out.iter_mut().find(|o| o.base == t.base) {
                Some(o) if o.size() >= t.size() => {}
                Some(o) => *o = t,
                None => out.push(t),
            }
        }
        out
    }
}

impl Turned {
    fn size(&self) -> f64 {
        self.deg.max(self.mm / 10.0)
    }

    pub fn amount(&self) -> String {
        let mut parts = Vec::new();
        if self.deg > 0.0 {
            parts.push(format!("turned about {:.0} deg", self.deg));
        }
        if self.mm > 0.0 {
            parts.push(format!("shifted about {:.0} mm", self.mm));
        }
        parts.join(" and ")
    }

    pub fn what(&self) -> String {
        format!(
            "Base {} {} while SteamVR was running: its angle or distance to {} changed, while pairs without it held still.",
            self.base,
            self.amount(),
            names(&self.against)
        )
    }
}

#[derive(Clone, Debug, Default)]
pub struct BaseMoves {
    pub bases: BTreeMap<String, MoveStats>,
    pub relations: Relations,
    last_move: Option<(u64, String)>,
    run_start: Option<u64>,
}

pub struct FirstBigMove {
    pub base: String,
    pub mm: u32,
    pub deg: f32,
}

impl BaseMoves {
    pub fn ingest(&mut self, ev: &SignalEvent) -> Option<FirstBigMove> {
        self.relations.ingest(ev);
        if ev.source == Source::Log {
            if ev.kind == Kind::LogRotated {
                self.run_start = None;
                return None;
            }
            self.run_start.get_or_insert(ev.t_ms);
        }
        let t_ms = ev.t_ms;
        let settled = self
            .run_start
            .is_some_and(|s| t_ms.saturating_sub(s) >= SETTLE_MS);
        match &ev.kind {
            Kind::BaseMoved { base, mm, deg } => {
                let base = short_serial(base);
                let s = self.bases.entry(base.clone()).or_default();
                s.moves += 1;
                s.max_mm = s.max_mm.max(*mm);
                s.max_deg = s.max_deg.max(*deg);
                self.last_move = Some((t_ms, base.clone()));
                if !is_big(*mm, *deg) {
                    return None;
                }
                if !settled {
                    s.early += 1;
                    return None;
                }
                s.big += 1;
                (s.big == 1).then_some(FirstBigMove {
                    base,
                    mm: *mm,
                    deg: *deg,
                })
            }
            Kind::BaseMovedForTracking => {
                if let Some((t, base)) = &self.last_move
                    && t_ms.saturating_sub(*t) <= POP_WINDOW_MS
                    && let Some(s) = self.bases.get_mut(base)
                {
                    s.pops += 1;
                }
                None
            }
            _ => None,
        }
    }

    pub fn total(&self) -> usize {
        self.bases.values().map(|s| s.moves).sum()
    }

    fn shifting(&self) -> Vec<(&String, &MoveStats)> {
        let mut v: Vec<_> = self
            .bases
            .iter()
            .filter(|(_, s)| s.steadiness() == Steadiness::Shifting)
            .collect();
        v.sort_by(|a, b| b.1.max_mm.cmp(&a.1.max_mm).then(a.0.cmp(b.0)));
        v
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct BaseRun {
    pub seen_s: u64,
    pub gravity: [f64; 3],
    pub faults: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct BaseRecord {
    pub serial: String,
    pub origin: bool,
    pub runs: Vec<BaseRun>,
}

fn angle_deg(a: &[f64; 3], b: &[f64; 3]) -> Option<f64> {
    let na = a.iter().map(|x| x * x).sum::<f64>().sqrt();
    let nb = b.iter().map(|x| x * x).sum::<f64>().sqrt();
    if na < 1e-6 || nb < 1e-6 {
        return None;
    }
    let dot = a.iter().zip(b).map(|(x, y)| x * y).sum::<f64>() / (na * nb);
    Some(dot.clamp(-1.0, 1.0).acos().to_degrees())
}

impl BaseRecord {
    pub fn tilt_spread_deg(&self) -> f64 {
        let mut max: f64 = 0.0;
        for (i, a) in self.runs.iter().enumerate() {
            for b in &self.runs[i + 1..] {
                if let Some(d) = angle_deg(&a.gravity, &b.gravity) {
                    max = max.max(d);
                }
            }
        }
        max
    }

    pub fn loose(&self) -> bool {
        self.runs.len() >= 2 && self.tilt_spread_deg() >= LOOSE_TILT_DEG
    }

    pub fn latest_faults(&self) -> u64 {
        self.runs
            .iter()
            .max_by_key(|r| r.seen_s)
            .map_or(0, |r| r.faults)
    }
}

fn is_identity(pose: &serde_json::Value) -> bool {
    let Some(p) = pose.as_array() else {
        return false;
    };
    let want = [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];
    p.len() == want.len()
        && p.iter()
            .zip(want)
            .all(|(v, w)| v.as_f64().is_some_and(|x| (x - w).abs() < 1e-9))
}

pub fn parse_db(text: &str) -> Vec<BaseRecord> {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    let serial_of = |v: &serde_json::Value| v.as_u64().map(|n| format!("{n:08X}"));
    let mut out: Vec<BaseRecord> = json["base_stations"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|b| {
            let serial = serial_of(&b["config"]["serialNumber"])?;
            let runs = b["dynamic_states"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|d| {
                    let g = d["dynamic_state"]["gravity_vector"].as_array()?;
                    let gravity = [
                        g.first()?.as_f64()?,
                        g.get(1)?.as_f64()?,
                        g.get(2)?.as_f64()?,
                    ];
                    let seen_s = match &d["time_last_seen"] {
                        serde_json::Value::String(s) => s.parse().ok()?,
                        v => v.as_u64()?,
                    };
                    Some(BaseRun {
                        seen_s,
                        gravity,
                        faults: d["dynamic_state"]["faults"].as_u64().unwrap_or(0),
                    })
                })
                .collect();
            Some(BaseRecord {
                serial,
                origin: false,
                runs,
            })
        })
        .collect();

    let last_seen: BTreeMap<String, u64> = out
        .iter()
        .map(|b| {
            (
                b.serial.clone(),
                b.runs.iter().map(|r| r.seen_s).max().unwrap_or(0),
            )
        })
        .collect();
    let universe = json["known_universes"]
        .as_array()
        .into_iter()
        .flatten()
        .max_by_key(|u| {
            let bases = u["base_stations"].as_array().map_or(&[][..], |v| v);
            let newest = bases
                .iter()
                .filter_map(|b| serial_of(&b["base_serial_number"]))
                .filter_map(|s| last_seen.get(&s).copied())
                .max()
                .unwrap_or(0);
            (newest, bases.len())
        });
    if let Some(u) = universe {
        for b in u["base_stations"].as_array().into_iter().flatten() {
            if is_identity(&b["target_pose"]["pose"])
                && let Some(serial) = serial_of(&b["base_serial_number"])
                && let Some(rec) = out.iter_mut().find(|r| r.serial == serial)
            {
                rec.origin = true;
            }
        }
    }
    out.sort_by(|a, b| a.serial.cmp(&b.serial));
    out
}

pub fn read_db(path: &Path) -> Vec<BaseRecord> {
    std::fs::read_to_string(path)
        .map(|t| parse_db(&t))
        .unwrap_or_default()
}

pub fn read_live_db() -> Vec<BaseRecord> {
    crate::signals::logtail::lighthouse_db_path()
        .map(|p| read_db(&p))
        .unwrap_or_default()
}

fn names(list: &[String]) -> String {
    match list {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

pub fn warnings(moves: &BaseMoves, db: &[BaseRecord]) -> Vec<String> {
    let mut out = Vec::new();
    let shifting = moves.shifting();
    if !shifting.is_empty() {
        let list: Vec<String> = shifting
            .iter()
            .map(|(b, s)| {
                format!(
                    "{b} ({}, up to {} mm)",
                    crate::summary::plural(s.moves, "move"),
                    s.max_mm
                )
            })
            .collect();
        out.push(format!(
            "Base stations: SteamVR made big moves to {} in its map. A loose or bumped base shifts every tracker and throws off calibration.",
            names(&list)
        ));
    }
    let loose: Vec<String> = db
        .iter()
        .filter(|b| b.loose())
        .map(|b| format!("{} ({:.1} deg)", b.serial, b.tilt_spread_deg()))
        .collect();
    if !loose.is_empty() {
        out.push(format!(
            "Base stations: the tilt sensor in {} changed between SteamVR runs. Its mount is probably loose or sagging.",
            names(&loose)
        ));
    }
    let faulty: Vec<String> = db
        .iter()
        .filter(|b| b.latest_faults() != 0)
        .map(|b| format!("{} (code {})", b.serial, b.latest_faults()))
        .collect();
    if !faulty.is_empty() {
        out.push(format!(
            "Base stations: SteamVR saw a fault flag on {} the last time it ran.",
            names(&faulty)
        ));
    }
    out
}

fn verdict(s: &MoveStats, origin: bool) -> &'static str {
    match (s.steadiness(), origin) {
        (Steadiness::Shifting, _) if s.big > 1 => "kept moving in the map",
        (Steadiness::Shifting, _) => "one big move in the map",
        (Steadiness::Settling, _) => "moved while SteamVR was starting, then held",
        (Steadiness::Small, _) => "small corrections",
        (Steadiness::Steady, true) => "steady (SteamVR's origin, never moved)",
        (Steadiness::Steady, false) => "steady",
    }
}

pub fn section(moves: &BaseMoves, db: &[BaseRecord], seen: &[String]) -> String {
    let mut serials: Vec<String> = seen
        .iter()
        .map(|s| short_serial(s))
        .chain(moves.bases.keys().cloned())
        .chain(db.iter().map(|b| b.serial.clone()))
        .collect();
    serials.sort();
    serials.dedup();
    if serials.is_empty() {
        return "No base stations recorded for this session.\n".into();
    }
    let empty = MoveStats::default();
    let stats = |s: &str| moves.bases.get(s).unwrap_or(&empty);
    serials.sort_by(|a, b| {
        let (x, y) = (stats(a), stats(b));
        y.big
            .cmp(&x.big)
            .then(y.max_mm.cmp(&x.max_mm))
            .then(a.cmp(b))
    });
    let origin = |s: &str| db.iter().any(|b| b.serial == s && b.origin);

    let mut out = format!(
        "SteamVR moved bases in its map {} this session. It does that when trackers disagree about where a base is; on a solid setup it's rare and small.\n",
        crate::summary::plural(moves.total(), "time")
    );
    if moves.bases.values().any(|s| s.pops > 0) {
        out.push_str("Pops are moves SteamVR also applied to live tracking, which can make everything jump at once.\n");
    }
    out.push_str(&format!(
        " {:<9} {:>5}  {:<15} {:>4}  {}\n",
        "base", "moves", "largest", "pops", "verdict"
    ));
    for s in &serials {
        let st = stats(s);
        let largest = if st.moves == 0 {
            String::new()
        } else {
            format!("{} mm, {:.1} deg", st.max_mm, st.max_deg)
        };
        out.push_str(&format!(
            " {s:<9} {:>5}  {largest:<15} {:>4}  {}\n",
            st.moves,
            st.pops,
            verdict(st, origin(s))
        ));
    }
    if !moves.shifting().is_empty() {
        out.push_str("SteamVR always moves the base that sits farther from the origin base, so the base it moves isn't always the one that shifted.\n");
    }
    for t in moves.relations.turned_bases() {
        out.push_str(&format!("{} Check its mount.\n", t.what()));
    }

    let with_runs: Vec<&BaseRecord> = db.iter().filter(|b| b.runs.len() >= 2).collect();
    if with_runs.is_empty() {
        return out;
    }
    out.push_str(&format!(
        "Tilt sensor across SteamVR's last {} runs (under 1 deg is sensor noise):\n",
        with_runs.iter().map(|b| b.runs.len()).max().unwrap_or(0)
    ));
    for b in with_runs {
        let spread = b.tilt_spread_deg();
        let note = if b.loose() {
            "changed: loose or sagging mount"
        } else {
            "steady"
        };
        out.push_str(&format!(" {:<9} {spread:>4.1} deg  {note}\n", b.serial));
    }
    out
}

pub fn notice(m: &FirstBigMove) -> String {
    format!(
        "SteamVR moved base {} by {} mm and {:.1} deg in its map. It moves whichever base sits farther from the origin base, so another base may be the one that shifted. If this keeps happening, check every base's mount.",
        m.base, m.mm, m.deg
    )
}

pub struct SessionMoves {
    pub dir: PathBuf,
    pub start_ms: u64,
    pub offset_ms: Option<i64>,
    pub moves: BaseMoves,
}

pub fn scan_session(dir: &Path) -> Option<SessionMoves> {
    let start_ms = dir
        .file_name()?
        .to_str()?
        .strip_prefix("session-")?
        .parse()
        .ok()?;
    let text = std::fs::read_to_string(dir.join("events.jsonl")).ok()?;
    let mut moves = BaseMoves::default();
    let mut offset_ms = None;
    for line in text.lines() {
        let wanted = line.contains("BaseMoved")
            || line.contains("BaseRelation")
            || line.contains("LogRotated");
        if !wanted && offset_ms.is_some() {
            continue;
        }
        if !wanted && !line.contains("\"Log\"") {
            continue;
        }
        let Ok(ev) = serde_json::from_str::<SignalEvent>(line) else {
            continue;
        };
        if offset_ms.is_none()
            && ev.source == Source::Log
            && let Some(naive) = crate::signals::logtail::naive_ms(&ev.detail)
        {
            offset_ms = Some(-crate::signals::logtail::utc_offset(ev.t_ms, naive));
        }
        moves.ingest(&ev);
    }
    Some(SessionMoves {
        dir: dir.to_path_buf(),
        start_ms,
        offset_ms,
        moves,
    })
}

pub fn history(sessions: &Path, limit: usize) -> Vec<SessionMoves> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(sessions)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    let skip = dirs.len().saturating_sub(limit);
    dirs.into_iter()
        .skip(skip)
        .filter_map(|d| scan_session(&d))
        .collect()
}

fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

pub fn fmt_stamp(local_ms: i64, with_time: bool) -> String {
    let days = local_ms.div_euclid(86_400_000);
    let rem = local_ms.rem_euclid(86_400_000);
    let (_, m, d) = civil(days);
    let month = MONTHS[(m - 1) as usize];
    if with_time {
        format!(
            "{month} {d:>2} {:02}:{:02}",
            rem / 3_600_000,
            (rem / 60_000) % 60
        )
    } else {
        format!("{month} {d}")
    }
}

pub fn render_history(history: &[SessionMoves], db: &[BaseRecord]) -> String {
    let mut serials: Vec<String> = history
        .iter()
        .flat_map(|h| h.moves.bases.keys().cloned())
        .chain(db.iter().map(|b| b.serial.clone()))
        .collect();
    serials.sort();
    serials.dedup();
    let fallback = history.iter().rev().find_map(|h| h.offset_ms);
    let mut out = String::new();
    if history.is_empty() || serials.is_empty() {
        out.push_str("No recorded sessions with base station data yet.\n");
    } else {
        out.push_str(&format!(
            "SteamVR base moves per session, oldest first (moves / largest; * means at least one move of {BIG_MOVE_MM} mm or {BIG_MOVE_DEG:.0} deg after SteamVR's first {} minutes):\n", SETTLE_MS / 60_000
        ));
        out.push_str(&format!(" {:<13}", "session"));
        for s in &serials {
            out.push_str(&format!(" {s:>13}"));
        }
        out.push('\n');
        for h in history {
            let offset = h.offset_ms.or(fallback).unwrap_or(0);
            out.push_str(&format!(
                " {:<13}",
                fmt_stamp(h.start_ms as i64 + offset, true)
            ));
            for s in &serials {
                let cell = match h.moves.bases.get(s) {
                    Some(st) if st.moves > 0 => format!(
                        "{} / {}mm{}",
                        st.moves,
                        st.max_mm,
                        if st.big > 0 { "*" } else { " " }
                    ),
                    _ => "- ".into(),
                };
                out.push_str(&format!(" {cell:>13}"));
            }
            out.push('\n');
        }
        for h in history {
            let offset = h.offset_ms.or(fallback).unwrap_or(0);
            for t in h.moves.relations.turned_bases() {
                out.push_str(&format!(
                    " {}: {}\n",
                    fmt_stamp(h.start_ms as i64 + offset, true),
                    t.what()
                ));
            }
        }
    }
    let with_runs: Vec<&BaseRecord> = db.iter().filter(|b| b.runs.len() >= 2).collect();
    if with_runs.is_empty() {
        out.push_str("\nSteamVR's base station file wasn't found or has no tilt history yet.\n");
        return out;
    }
    let seen = with_runs.iter().flat_map(|b| &b.runs).map(|r| r.seen_s);
    let span = match (fallback, seen.clone().min(), seen.max()) {
        (Some(o), Some(a), Some(b)) => format!(
            ", {} to {}",
            fmt_stamp((a * 1000) as i64 + o, false),
            fmt_stamp((b * 1000) as i64 + o, false)
        ),
        _ => String::new(),
    };
    out.push_str(&format!(
        "\nTilt sensor per SteamVR run, oldest first{span}, in deg away from the oldest run (under 1 deg is sensor noise):\n"
    ));
    for b in with_runs {
        let mut runs = b.runs.clone();
        runs.sort_by_key(|r| r.seen_s);
        let first = runs[0].gravity;
        let steps: Vec<String> = runs
            .iter()
            .map(|r| format!("{:>3.1}", angle_deg(&first, &r.gravity).unwrap_or(0.0)))
            .collect();
        let note = if b.loose() {
            format!(
                "up to {:.1} deg apart between runs: loose or sagging mount",
                b.tilt_spread_deg()
            )
        } else {
            "steady".into()
        };
        let name = if b.origin {
            format!("{} (origin)", b.serial)
        } else {
            b.serial.clone()
        };
        out.push_str(&format!(" {name:<18} {}  {note}\n", steps.join(" ")));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn moved(base: &str, mm: u32, deg: f32) -> Kind {
        Kind::BaseMoved {
            base: base.into(),
            mm,
            deg,
        }
    }

    fn log(m: &mut BaseMoves, t: u64, kind: Kind) -> Option<FirstBigMove> {
        m.ingest(&SignalEvent::at(t, Source::Log, None, kind, ""))
    }

    fn settled() -> BaseMoves {
        let mut m = BaseMoves::default();
        log(&mut m, 0, Kind::BaseMovedForTracking);
        m
    }

    const S: u64 = SETTLE_MS;

    #[test]
    fn moves_are_counted_per_base_and_pops_attach_to_the_last_move() {
        let mut m = settled();
        assert!(log(&mut m, S + 1000, moved("9417e0b3", 30, 0.4)).is_none());
        let first = log(&mut m, S + 2000, moved("9417E0B3", 138, 2.2)).expect("first big move");
        assert_eq!((first.base.as_str(), first.mm), ("9417E0B3", 138));
        log(&mut m, S + 2000, Kind::BaseMovedForTracking);
        assert!(log(&mut m, S + 3000, moved("9417E0B3", 60, 0.8)).is_none());
        log(&mut m, S + 9000, Kind::BaseMovedForTracking);
        log(&mut m, S + 9500, moved("B394A63C", 7, 0.1));

        let s = &m.bases["9417E0B3"];
        assert_eq!((s.moves, s.big, s.max_mm, s.pops), (3, 2, 138, 1));
        assert_eq!(s.steadiness(), Steadiness::Shifting);
        assert_eq!(m.bases["B394A63C"].steadiness(), Steadiness::Steady);
        assert_eq!(m.total(), 4);
    }

    #[test]
    fn big_moves_while_steamvr_settles_give_no_notice() {
        let mut m = BaseMoves::default();
        assert!(log(&mut m, 5_000, moved("9417E0B3", 138, 2.2)).is_none());
        assert!(log(&mut m, S - 1, moved("9417E0B3", 90, 1.0)).is_none());
        let s = &m.bases["9417E0B3"];
        assert_eq!((s.moves, s.early, s.big), (2, 2, 0));
        assert_eq!(s.steadiness(), Steadiness::Settling);
        assert!(warnings(&m, &[]).is_empty());
        assert!(log(&mut m, S + 5_000, moved("9417E0B3", 60, 0.8)).is_some());
        assert_eq!(m.bases["9417E0B3"].steadiness(), Steadiness::Shifting);
    }

    #[test]
    fn a_new_log_starts_the_settle_window_again() {
        let mut m = settled();
        log(&mut m, S + 1, Kind::LogRotated);
        assert!(log(&mut m, S + 2, moved("9417E0B3", 138, 2.2)).is_none());
        assert_eq!(m.bases["9417E0B3"].early, 1);
        let api = SignalEvent::at(3 * S, Source::Api, None, Kind::DeviceActivated, "");
        m.ingest(&api);
        assert!(log(&mut m, 2 * S + 3, moved("9417E0B3", 138, 2.2)).is_some());
    }

    #[test]
    fn a_large_angle_alone_counts_as_a_big_move() {
        assert!(is_big(10, 1.2));
        assert!(!is_big(49, 0.9));
        let mut s = MoveStats {
            moves: 1,
            max_mm: 25,
            ..Default::default()
        };
        assert_eq!(s.steadiness(), Steadiness::Small);
        s.max_mm = 19;
        assert_eq!(s.steadiness(), Steadiness::Steady);
    }

    const DB: &str = r#"{
      "base_stations": [
        {"config": {"serialNumber": 2484592819}, "dynamic_states": [
          {"dynamic_state": {"gravity_vector": [0.115488, 0.916687, 0.382554], "faults": 0}, "time_last_seen": "1790534096"},
          {"dynamic_state": {"gravity_vector": [0.143822, 0.913267, 0.381127], "faults": 0}, "time_last_seen": "1790715109"},
          {"dynamic_state": {"gravity_vector": [0.173, 0.914, 0.367], "faults": 0}, "time_last_seen": "1791931000"}
        ]},
        {"config": {"serialNumber": 3308842953}, "dynamic_states": [
          {"dynamic_state": {"gravity_vector": [0.131, 0.877, 0.463], "faults": 0}, "time_last_seen": "1790534096"},
          {"dynamic_state": {"gravity_vector": [0.132, 0.880, 0.457], "faults": 4}, "time_last_seen": "1791931000"}
        ]}
      ],
      "known_universes": [
        {"id": "1", "base_stations": [
          {"base_serial_number": 3308842953, "target_pose": {"pose": [0, 0, 0, 1, 0, 0, 0]}},
          {"base_serial_number": 2484592819, "target_pose": {"pose": [0.03, 0.74, 0.39, 0.54, 2.29, 0.96, -2.28]}}
        ]}
      ]
    }"#;

    #[test]
    fn db_tilt_spread_flags_the_base_whose_gravity_wanders() {
        let db = parse_db(DB);
        assert_eq!(db.len(), 2);
        let wobbly = db.iter().find(|b| b.serial == "9417E0B3").unwrap();
        let solid = db.iter().find(|b| b.serial == "C538EFC9").unwrap();
        assert!(
            (wobbly.tilt_spread_deg() - 3.4).abs() < 0.2,
            "{}",
            wobbly.tilt_spread_deg()
        );
        assert!(wobbly.loose() && !wobbly.origin);
        assert!(!solid.loose() && solid.origin);
        assert_eq!(solid.latest_faults(), 4);

        let w = warnings(&BaseMoves::default(), &db);
        assert_eq!(w.len(), 2);
        assert!(w[0].contains("9417E0B3 (3.4 deg)"), "{w:?}");
        assert!(w[1].contains("C538EFC9 (code 4)"), "{w:?}");
    }

    fn rel(a: &str, b: &str, mm: u32, deg: f32) -> Kind {
        Kind::BaseRelation {
            a: a.into(),
            b: b.into(),
            mm,
            deg,
        }
    }

    fn measure(m: &mut BaseMoves, t: u64, turn: f32) {
        log(m, t, rel("C538EFC9", "9417e0b3", 4270, 121.7 - turn));
        log(m, t + 1, rel("9417E0B3", "9D8DEA7B", 3100, 62.8 + turn));
        log(m, t + 2, rel("C538EFC9", "9d8dea7b", 2900, 58.3));
        log(m, t + 3, rel("C538EFC9", "b394a63c", 2400, 58.3));
        log(m, t + 4, rel("9D8DEA7B", "B394A63C", 3500, 95.0));
    }

    #[test]
    fn the_base_in_every_changed_pair_is_the_one_that_turned() {
        let mut m = settled();
        for i in 0..3 {
            measure(&mut m, i * 60_000, 0.0);
        }
        assert!(m.relations.turned().is_empty());
        for i in 0..3 {
            measure(&mut m, 20 * 60_000 + i * 60_000, 5.0);
        }
        let t = m.relations.turned();
        assert_eq!(t.len(), 1, "{t:?}");
        assert_eq!(t[0].base, "9417E0B3");
        assert!((t[0].deg - 5.0).abs() < 0.01, "{t:?}");
        assert_eq!(t[0].mm, 0.0);
        assert_eq!(t[0].against, vec!["9D8DEA7B", "C538EFC9"]);
        assert!(
            t[0].what()
                .starts_with("Base 9417E0B3 turned about 5 deg while SteamVR was running: its angle or distance to 9D8DEA7B and C538EFC9 changed"),
            "{}",
            t[0].what()
        );
    }

    #[test]
    fn a_new_run_or_a_lone_changed_pair_names_no_base() {
        let mut m = settled();
        for i in 0..3 {
            measure(&mut m, i * 60_000, 0.0);
        }
        log(&mut m, 15 * 60_000, Kind::LogRotated);
        for i in 0..3 {
            measure(&mut m, 20 * 60_000 + i * 60_000, 5.0);
        }
        assert!(m.relations.turned().is_empty());

        let mut m = settled();
        for (t, d) in [
            (0, 0.0),
            (60_000, 0.0),
            (20 * 60_000, 6.0),
            (21 * 60_000, 6.0),
        ] {
            log(&mut m, t, rel("C538EFC9", "9417E0B3", 4270, 121.7 + d));
            log(&mut m, t + 1, rel("9D8DEA7B", "B394A63C", 3500, 95.0));
        }
        assert!(m.relations.turned().is_empty());
    }

    #[test]
    fn unreadable_db_gives_nothing() {
        assert!(parse_db("not json").is_empty());
        assert!(parse_db("{}").is_empty());
    }

    #[test]
    fn section_ranks_shifting_bases_first_and_marks_the_origin() {
        let mut m = settled();
        log(&mut m, S, moved("9D8DEA7B", 92, 1.2));
        log(&mut m, S + 10, moved("9417E0B3", 138, 2.2));
        log(&mut m, S + 10, Kind::BaseMovedForTracking);
        log(&mut m, S + 20, moved("B394A63C", 40, 0.5));
        let db = parse_db(DB);
        let seen = vec!["LHB-C538EFC9".to_string(), "LHB-B394A63C".to_string()];
        let text = section(&m, &db, &seen);
        let rows: Vec<&str> = text.lines().filter(|l| l.starts_with(' ')).collect();
        assert!(rows[1].starts_with(" 9417E0B3"), "{text}");
        assert!(
            rows[1].contains("138 mm, 2.2 deg") && rows[1].contains("one big move"),
            "{text}"
        );
        assert!(rows[2].starts_with(" 9D8DEA7B"), "{text}");
        assert!(rows[3].contains("small corrections"), "{text}");
        assert!(
            rows[4].contains("C538EFC9") && rows[4].contains("origin"),
            "{text}"
        );
        assert!(text.contains("isn't always the one that shifted"), "{text}");
        assert!(text.contains("9417E0B3   3.4 deg  changed"), "{text}");

        let w = warnings(&m, &[]);
        assert_eq!(w.len(), 1);
        assert!(
            w[0].contains("9417E0B3 (1 move, up to 138 mm) and 9D8DEA7B (1 move, up to 92 mm)"),
            "{w:?}"
        );
    }

    #[test]
    fn section_without_any_bases_says_so() {
        assert!(section(&BaseMoves::default(), &[], &[]).starts_with("No base stations"));
    }

    #[test]
    fn civil_dates_round_trip() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(
            fmt_stamp(1_791_866_242_776 - 5 * 3_600_000, true),
            "Oct 12 23:37"
        );
        assert_eq!(fmt_stamp(951_782_400_000, false), "Feb 29");
    }

    #[test]
    fn scan_reads_moves_and_the_log_clock_offset() {
        let dir = std::env::temp_dir()
            .join(format!("td-bases-{}", std::process::id()))
            .join("session-1791866242776");
        std::fs::create_dir_all(&dir).unwrap();
        let evs = [
            SignalEvent::at(
                1_791_866_243_000,
                Source::Api,
                Some("LHR-1".into()),
                Kind::DeviceActivated,
                "x",
            ),
            SignalEvent::at(
                1_791_866_245_419,
                Source::Log,
                None,
                moved("9417E0B3", 94, 1.2),
                "Mon Oct 12 2026 23:37:25.419 [Info] - lighthouse: Moving base 9417E0B3 94mm and 1.2 deg",
            ),
            SignalEvent::at(
                1_791_866_245_419 + SETTLE_MS + 60_000,
                Source::Log,
                None,
                moved("9417E0B3", 60, 0.8),
                "Mon Oct 12 2026 23:48:25.419 [Info] - lighthouse: Moving base 9417E0B3 60mm and 0.8 deg",
            ),
        ];
        let text: Vec<String> = evs
            .iter()
            .map(|e| serde_json::to_string(e).unwrap())
            .collect();
        std::fs::write(dir.join("events.jsonl"), text.join("\n")).unwrap();
        let s = scan_session(&dir).unwrap();
        assert_eq!(s.start_ms, 1_791_866_242_776);
        assert_eq!(s.offset_ms, Some(-5 * 3_600_000));
        let b = &s.moves.bases["9417E0B3"];
        assert_eq!((b.moves, b.early, b.big), (2, 1, 1));
        let h = render_history(&[s], &[]);
        assert!(h.contains("Oct 12 23:37") && h.contains("2 / 94mm*"), "{h}");
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }
}

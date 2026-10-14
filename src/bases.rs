use crate::event::{Kind, SignalEvent, Source};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const BIG_MOVE_MM: u32 = 50;
pub const BIG_MOVE_DEG: f32 = 1.0;
pub const STEADY_MM: u32 = 20;
pub const LOOSE_TILT_DEG: f64 = 1.5;
const POP_WINDOW_MS: u64 = 100;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MoveStats {
    pub moves: usize,
    pub big: usize,
    pub max_mm: u32,
    pub max_deg: f32,
    pub pops: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Steadiness {
    Steady,
    Small,
    Shifting,
}

impl MoveStats {
    pub fn steadiness(&self) -> Steadiness {
        if self.big > 0 {
            Steadiness::Shifting
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

#[derive(Clone, Debug, Default)]
pub struct BaseMoves {
    pub bases: BTreeMap<String, MoveStats>,
    last_move: Option<(u64, String)>,
}

pub struct FirstBigMove {
    pub base: String,
    pub mm: u32,
    pub deg: f32,
}

impl BaseMoves {
    pub fn ingest(&mut self, t_ms: u64, kind: &Kind) -> Option<FirstBigMove> {
        match kind {
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
        (Steadiness::Shifting, _) if s.big > 1 => "kept moving: check its mount and power plug",
        (Steadiness::Shifting, _) => "one big move: check its mount if it happens again",
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
    let shifting = moves.shifting().len();
    if let Some(o) = serials.iter().find(|s| origin(s))
        && shifting >= 2
        && shifting * 2 > serials.len() - 1
    {
        out.push_str(&format!(
            "SteamVR never moves {o}, its origin base. When most of the others keep moving, {o} may be the loose one.\n"
        ));
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
        "SteamVR moved base {} by {} mm and {:.1} deg in its map. If this keeps happening, check its mount and power plug.",
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
        let wanted = line.contains("BaseMoved");
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
        moves.ingest(ev.t_ms, &ev.kind);
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
            "SteamVR base moves per session, oldest first (moves / largest; * means at least one move of {BIG_MOVE_MM} mm or {BIG_MOVE_DEG:.0} deg):\n"
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

    #[test]
    fn moves_are_counted_per_base_and_pops_attach_to_the_last_move() {
        let mut m = BaseMoves::default();
        assert!(m.ingest(1000, &moved("9417e0b3", 30, 0.4)).is_none());
        let first = m
            .ingest(2000, &moved("9417E0B3", 138, 2.2))
            .expect("first big move");
        assert_eq!((first.base.as_str(), first.mm), ("9417E0B3", 138));
        m.ingest(2000, &Kind::BaseMovedForTracking);
        assert!(m.ingest(3000, &moved("9417E0B3", 60, 0.8)).is_none());
        m.ingest(9000, &Kind::BaseMovedForTracking);
        m.ingest(9500, &moved("B394A63C", 7, 0.1));

        let s = &m.bases["9417E0B3"];
        assert_eq!((s.moves, s.big, s.max_mm, s.pops), (3, 2, 138, 1));
        assert_eq!(s.steadiness(), Steadiness::Shifting);
        assert_eq!(m.bases["B394A63C"].steadiness(), Steadiness::Steady);
        assert_eq!(m.total(), 4);
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

    #[test]
    fn unreadable_db_gives_nothing() {
        assert!(parse_db("not json").is_empty());
        assert!(parse_db("{}").is_empty());
    }

    #[test]
    fn section_ranks_shifting_bases_first_and_points_at_the_origin() {
        let mut m = BaseMoves::default();
        m.ingest(0, &moved("9D8DEA7B", 92, 1.2));
        m.ingest(10, &moved("9417E0B3", 138, 2.2));
        m.ingest(10, &Kind::BaseMovedForTracking);
        m.ingest(20, &moved("B394A63C", 40, 0.5));
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
        assert!(text.contains("C538EFC9 may be the loose one"), "{text}");
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
        ];
        let text: Vec<String> = evs
            .iter()
            .map(|e| serde_json::to_string(e).unwrap())
            .collect();
        std::fs::write(dir.join("events.jsonl"), text.join("\n")).unwrap();
        let s = scan_session(&dir).unwrap();
        assert_eq!(s.start_ms, 1_791_866_242_776);
        assert_eq!(s.offset_ms, Some(-5 * 3_600_000));
        assert_eq!(s.moves.bases["9417E0B3"].big, 1);
        let h = render_history(&[s], &[]);
        assert!(h.contains("Oct 12 23:37") && h.contains("1 / 94mm*"), "{h}");
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }
}

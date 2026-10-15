use super::present::{self, BASE, GOOD, NORMAL, ReportView, RoomView, UsbView, WARN};
use super::replay::{self, Replay};
use super::ui;
use crate::correlate::Verdict;
use crate::engine::BasePose;
use crate::event::SignalEvent;
use crate::names::Names;
use crate::report;
use crate::store;
use crate::summary::{self, fmt_dur};
use std::io::{BufRead, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone, Debug, Default)]
pub struct Shown {
    pub devices: Vec<ui::Device>,
    pub feed: Vec<ui::Line>,
    pub report: ReportView,
    pub usb: UsbView,
    pub room: RoomView,
    pub log: Vec<ui::Line>,
    pub replay: Option<Arc<Replay>>,
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub dir: PathBuf,
    pub title: String,
    pub detail: String,
    pub tone: i32,
    pub size: u64,
    pub track: bool,
    pub live: bool,
}

pub fn start_ms(dir: &Path) -> Option<u64> {
    dir.file_name()?
        .to_str()?
        .strip_prefix("session-")?
        .parse()
        .ok()
}

pub fn title(dir: &Path) -> String {
    match start_ms(dir).and_then(crate::win::local_time) {
        Some((y, mo, d, h, mi)) => format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}"),
        None => dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
    }
}

fn event_ms(line: &str) -> Option<u64> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    v.get("t_ms")?.as_u64()
}

fn plain_span(events: &Path) -> Option<(u64, u64)> {
    let mut f = std::fs::File::open(events).ok()?;
    let mut first = String::new();
    std::io::BufReader::new(&f).read_line(&mut first).ok()?;
    let start = event_ms(first.trim())?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(64 * 1024)))
        .ok()?;
    let mut tail = Vec::new();
    f.read_to_end(&mut tail).ok()?;
    let tail = String::from_utf8_lossy(&tail);
    let end = tail
        .lines()
        .rev()
        .find_map(|l| event_ms(l.trim()))
        .unwrap_or(start);
    Some((start, end.max(start)))
}

fn span(dir: &Path) -> Option<(u64, u64)> {
    let plain = dir.join("events.jsonl");
    if plain.is_file() {
        return plain_span(&plain);
    }
    let text = store::read_text(dir, "events.jsonl")?;
    let start = text.lines().find_map(|l| event_ms(l.trim()))?;
    let end = text
        .lines()
        .rev()
        .find_map(|l| event_ms(l.trim()))
        .unwrap_or(start);
    Some((start, end.max(start)))
}

pub fn read_verdicts(dir: &Path) -> Vec<Verdict> {
    store::read_text(dir, "verdicts.jsonl")
        .map(|t| {
            t.lines()
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect()
        })
        .unwrap_or_default()
}

fn has_events(dir: &Path) -> bool {
    std::fs::metadata(dir.join("events.jsonl")).is_ok_and(|m| m.len() > 0)
        || dir.join("events.jsonl.gz").is_file()
}

pub fn pack_finished(root: &Path, recording: Option<&Path>, before_ms: u64) -> usize {
    store::session_dirs(root)
        .into_iter()
        .filter(|d| recording != Some(d.as_path()))
        .filter(|d| start_ms(d).is_some_and(|t| t < before_ms))
        .filter(|d| has_events(d))
        .filter(|d| store::needs_pack(d))
        .filter(|d| store::pack(d).is_ok())
        .count()
}

pub fn prune(root: &Path, keep_days: u32, recording: Option<&Path>, now_ms: u64) -> Vec<PathBuf> {
    if keep_days == 0 {
        return Vec::new();
    }
    let cutoff = now_ms.saturating_sub(keep_days as u64 * 86_400_000);
    store::session_dirs(root)
        .into_iter()
        .filter(|d| recording != Some(d.as_path()))
        .filter(|d| start_ms(d).is_some_and(|t| t < cutoff))
        .filter(|d| store::delete(d).is_ok())
        .collect()
}

pub fn list(recording: Option<&Path>) -> Vec<Entry> {
    list_in(&report::sessions_dir(), recording)
}

pub fn list_in(root: &Path, recording: Option<&Path>) -> Vec<Entry> {
    let mut dirs: Vec<PathBuf> = store::session_dirs(root)
        .into_iter()
        .filter(|d| has_events(d))
        .collect();
    dirs.sort_by_key(|d| std::cmp::Reverse(start_ms(d).unwrap_or(0)));
    dirs.into_iter().map(|dir| entry(dir, recording)).collect()
}

fn entry(dir: PathBuf, recording: Option<&Path>) -> Entry {
    let title = title(&dir);
    let size = store::size(&dir);
    if recording == Some(dir.as_path()) {
        return Entry {
            dir,
            title,
            detail: "recording now".into(),
            tone: BASE,
            size,
            track: false,
            live: true,
        };
    }
    let incidents = read_verdicts(&dir)
        .iter()
        .filter(|v| !present::quiet(v))
        .count();
    let track = store::has(&dir, crate::track::RAW_FILE);
    let moved =
        if track { store::read_track(&dir) } else { None }.map(|t| (t.t0, t.t0 + t.end() as u64));
    let length = match (span(&dir), moved) {
        (Some((a, b)), Some((c, d))) => Some((a.min(c), b.max(d))),
        (s, m) => s.or(m),
    }
    .map(|(a, b)| fmt_dur(b - a))
    .unwrap_or_else(|| "?".into());
    let movement = if track { "" } else { ", no movement saved" };
    Entry {
        detail: format!(
            "{length}, {}, {}{movement}",
            summary::plural(incidents, "incident"),
            store::fmt_size(size)
        ),
        tone: if incidents > 0 { WARN } else { GOOD },
        dir,
        title,
        size,
        track,
        live: false,
    }
}

pub fn totals(entries: &[Entry]) -> String {
    if entries.is_empty() {
        return String::new();
    }
    format!(
        "{} saved, using {} on disk.",
        summary::plural(entries.len(), "session"),
        store::fmt_size(entries.iter().map(|e| e.size).sum())
    )
}

pub fn items(entries: &[Entry]) -> Vec<ui::SessionItem> {
    entries
        .iter()
        .map(|e| ui::SessionItem {
            title: e.title.clone().into(),
            detail: e.detail.clone().into(),
            tone: e.tone,
            replay: e.track,
            live: e.live,
        })
        .collect()
}

pub fn past(dir: &Path, names: Names) -> anyhow::Result<Shown> {
    let replay = report::replay(dir, names, None)?;
    let engine = &replay.engine;
    let s = &replay.summary;
    let t0 = start_ms(dir)
        .or_else(|| engine.session.verdicts().first().map(|v| v.t_start_ms))
        .unwrap_or(0);
    let mut feed: Vec<ui::Line> = engine
        .session
        .verdicts()
        .iter()
        .rev()
        .flat_map(|v| present::verdict_lines(v, &engine.label(&v.device), t0))
        .collect();
    if feed.is_empty() {
        feed.push(present::line(
            "No tracking incidents in this session.",
            NORMAL,
        ));
    }
    let poses: Vec<BasePose> = report::read_json(&dir.join("room.json")).unwrap_or_default();
    let events = read_events(dir);
    let verdicts = read_verdicts(dir);
    let log = events
        .iter()
        .map(|e| {
            let label = e
                .device
                .as_deref()
                .map(|d| engine.label(d))
                .unwrap_or_default();
            replay::event_line(e, &label, t0)
        })
        .collect();
    let mut room = present::past_room(&poses, &verdicts);
    let moves = store::read_track(dir).filter(|t| !t.devices.is_empty()).map(|t| {
        let who: Vec<_> = t.devices.iter().map(|d| (d.serial.clone(), d.class)).collect();
        let mut legend = present::replay_keys(engine, s, &who);
        legend.append(&mut room.legend);
        room.legend = legend;
        room.caption = "Press play to watch the session. Each device trails where it went in the last few seconds, and dropout spots appear as the replay reaches them.".into();
        Arc::new(Replay::build(t, engine, s, &events, &verdicts, &poses))
    });
    Ok(Shown {
        devices: present::past_devices(engine, s),
        feed,
        report: present::report(engine, s, true),
        usb: present::usb(engine, s),
        room,
        log,
        replay: moves,
    })
}

pub fn read_events(dir: &Path) -> Vec<SignalEvent> {
    let mut events: Vec<SignalEvent> = store::read_text(dir, "events.jsonl")
        .map(|t| {
            t.lines()
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect()
        })
        .unwrap_or_default();
    events.sort_by_key(|e| e.t_ms);
    events
}

pub fn bases_lines() -> Vec<ui::Line> {
    let history = crate::bases::history(&report::sessions_dir(), 15);
    present::bases_lines(&crate::bases::render_history(
        &history,
        &crate::bases::read_live_db(),
    ))
}

pub fn last_roster() -> Vec<crate::summary::DeviceRecord> {
    report::latest_session()
        .and_then(|d| report::read_json(&d.join("devices.json")))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Kind, Source};

    const DAY: u64 = 86_400_000;
    const NOW: u64 = 1_791_000_000_000;

    fn scratch(tag: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("trackdoctor-sessions-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn old_session(root: &Path, start: u64, length_ms: u64) -> PathBuf {
        let dir = root.join(format!("session-{start}"));
        std::fs::create_dir_all(&dir).unwrap();
        let events = [
            SignalEvent::at(
                start,
                Source::Api,
                Some("LHR-A".into()),
                Kind::DeviceActivated,
                "",
            ),
            SignalEvent::at(
                start + length_ms,
                Source::Api,
                Some("LHR-A".into()),
                Kind::DeviceDeactivated,
                "",
            ),
        ];
        let text: String = events
            .iter()
            .map(|e| serde_json::to_string(e).unwrap() + "\n")
            .collect();
        std::fs::write(dir.join("events.jsonl"), text).unwrap();
        std::fs::write(dir.join("verdicts.jsonl"), "").unwrap();
        std::fs::write(dir.join("report.txt"), "report").unwrap();
        dir
    }

    #[test]
    fn pruning_spares_the_recording_session_and_newer_ones() {
        let root = scratch("prune");
        let old = old_session(&root, NOW - 40 * DAY, 60_000);
        let recording = old_session(&root, NOW - 45 * DAY, 60_000);
        let recent = old_session(&root, NOW - 10 * DAY, 60_000);
        assert!(prune(&root, 0, Some(&recording), NOW).is_empty());
        assert_eq!(prune(&root, 30, Some(&recording), NOW), vec![old.clone()]);
        assert!(!old.exists());
        assert!(recording.is_dir());
        assert!(recent.is_dir());
        for d in [recording, recent] {
            store::delete(&d).unwrap();
        }
        std::fs::remove_dir(&root).unwrap();
    }

    #[test]
    fn sessions_saved_before_packing_are_listed_read_and_packed() {
        let root = scratch("plain");
        let old = old_session(&root, NOW - 2 * DAY, 90_000);
        let recording = old_session(&root, NOW - DAY, 30_000);
        let empty = root.join(format!("session-{}", NOW - 3 * DAY));
        std::fs::create_dir_all(&empty).unwrap();
        std::fs::write(empty.join("events.jsonl"), "").unwrap();
        std::fs::write(empty.join("verdicts.jsonl"), "").unwrap();
        let entries = list_in(&root, Some(&recording));
        assert_eq!(entries.len(), 2);
        assert!(entries[0].live);
        let before = entries[1].clone();
        assert!(!before.track);
        assert!(
            before.detail.starts_with("1m 30s, 0 incidents, "),
            "{}",
            before.detail
        );
        assert!(
            before.detail.ends_with(", no movement saved"),
            "{}",
            before.detail
        );
        assert_eq!(read_events(&old).len(), 2);
        assert_eq!(pack_finished(&root, Some(&recording), NOW), 1);
        assert!(old.join("events.jsonl.gz").is_file());
        assert!(!old.join("events.jsonl").exists());
        assert!(recording.join("events.jsonl").is_file());
        assert!(!empty.join("events.jsonl.gz").exists());
        let after = list_in(&root, Some(&recording));
        assert_eq!(after.len(), 2);
        assert_eq!(after[1].detail.split(", ").next(), Some("1m 30s"));
        assert_eq!(read_events(&old).len(), 2);
        assert!(store::read_text(&old, "report.txt").is_some());
        for d in [old, recording, empty] {
            store::delete(&d).unwrap();
        }
        std::fs::remove_dir(&root).unwrap();
    }
}

use crate::correlate::{FlapSummary, Verdict};
use crate::engine::{Engine, Msg};
use crate::event::{DeviceClass, SignalEvent, Source};
use crate::names::{Names, data_dir};
use crate::signals::openvr::FrameSample;
use crate::signals::usb::{UsbDongle, crowding};
use crate::summary::{self, DeviceRecord, Summary, TreeCtx, fmt_dur};
use anyhow::{Context, anyhow};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const POSE_INTERVAL_MS: u64 = 100;
const POSE_HEADER: &str = "t_ms,serial,x,y,z,qw,qx,qy,qz,state,valid";

struct Files {
    events: std::fs::File,
    verdicts: std::fs::File,
    poses: Option<std::fs::File>,
}

pub struct SessionWriter {
    dir: PathBuf,
    files: Option<Files>,
    pose_last: HashMap<String, u64>,
    bytes: u64,
    all_verdicts: Vec<Verdict>,
}

fn write_line(file: &mut std::fs::File, bytes: &mut u64, line: &str) {
    if writeln!(file, "{line}").is_ok() {
        *bytes += line.len() as u64 + 1;
    }
}

pub fn quat(m: &[[f32; 4]; 3]) -> [f32; 4] {
    let tr = m[0][0] + m[1][1] + m[2][2];
    if tr > 0.0 {
        let s = (tr + 1.0).sqrt() * 2.0;
        [
            0.25 * s,
            (m[2][1] - m[1][2]) / s,
            (m[0][2] - m[2][0]) / s,
            (m[1][0] - m[0][1]) / s,
        ]
    } else if m[0][0] > m[1][1] && m[0][0] > m[2][2] {
        let s = (1.0 + m[0][0] - m[1][1] - m[2][2]).sqrt() * 2.0;
        [
            (m[2][1] - m[1][2]) / s,
            0.25 * s,
            (m[0][1] + m[1][0]) / s,
            (m[0][2] + m[2][0]) / s,
        ]
    } else if m[1][1] > m[2][2] {
        let s = (1.0 + m[1][1] - m[0][0] - m[2][2]).sqrt() * 2.0;
        [
            (m[0][2] - m[2][0]) / s,
            (m[0][1] + m[1][0]) / s,
            0.25 * s,
            (m[1][2] + m[2][1]) / s,
        ]
    } else {
        let s = (1.0 + m[2][2] - m[0][0] - m[1][1]).sqrt() * 2.0;
        [
            (m[1][0] - m[0][1]) / s,
            (m[0][2] + m[2][0]) / s,
            (m[1][2] + m[2][1]) / s,
            0.25 * s,
        ]
    }
}

pub fn sessions_dir() -> PathBuf {
    data_dir().join("sessions")
}

impl SessionWriter {
    pub fn new(poses: bool) -> anyhow::Result<Self> {
        let stamp = crate::event::now_ms();
        Self::in_dir(sessions_dir().join(format!("session-{stamp}")), poses)
    }

    pub fn in_dir(dir: PathBuf, poses: bool) -> anyhow::Result<Self> {
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        let mut bytes = 0;
        let poses = if poses {
            let mut f = std::fs::File::create(dir.join("poses.csv"))?;
            write_line(&mut f, &mut bytes, POSE_HEADER);
            Some(f)
        } else {
            None
        };
        Ok(Self {
            files: Some(Files {
                events: std::fs::File::create(dir.join("events.jsonl"))?,
                verdicts: std::fs::File::create(dir.join("verdicts.jsonl"))?,
                poses,
            }),
            pose_last: HashMap::new(),
            bytes,
            all_verdicts: Vec::new(),
            dir,
        })
    }

    pub fn memory(dir: PathBuf) -> Self {
        Self {
            dir,
            files: None,
            pose_last: HashMap::new(),
            bytes: 0,
            all_verdicts: Vec::new(),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn bytes_written(&self) -> u64 {
        self.bytes
    }

    pub fn verdicts(&self) -> &[Verdict] {
        &self.all_verdicts
    }

    pub fn event(&mut self, ev: &SignalEvent) {
        let Some(f) = self.files.as_mut() else {
            return;
        };
        if let Ok(line) = serde_json::to_string(ev) {
            write_line(&mut f.events, &mut self.bytes, &line);
        }
    }

    pub fn pose(&mut self, serial: &str, f: &FrameSample) {
        let Some(file) = self.files.as_mut().and_then(|x| x.poses.as_mut()) else {
            return;
        };
        if self
            .pose_last
            .get(serial)
            .is_some_and(|t| f.t_ms.saturating_sub(*t) < POSE_INTERVAL_MS)
        {
            return;
        }
        self.pose_last.insert(serial.to_string(), f.t_ms);
        let q = quat(&f.rot);
        let line = format!(
            "{},{serial},{:.4},{:.4},{:.4},{:.5},{:.5},{:.5},{:.5},{:?},{}",
            f.t_ms, f.pos[0], f.pos[1], f.pos[2], q[0], q[1], q[2], q[3], f.state, f.valid as u8
        );
        write_line(file, &mut self.bytes, &line);
    }

    pub fn verdict(&mut self, v: &Verdict) {
        if let Some(f) = self.files.as_mut()
            && let Ok(line) = serde_json::to_string(v)
        {
            write_line(&mut f.verdicts, &mut self.bytes, &line);
        }
        self.all_verdicts.push(v.clone());
    }

    fn write_json<T: serde::Serialize>(&self, name: &str, value: &T) {
        if self.files.is_some()
            && let Ok(text) = serde_json::to_string_pretty(value)
        {
            let _ = std::fs::write(self.dir.join(name), text);
        }
    }

    pub fn set_devices(&self, devices: &[DeviceRecord]) {
        self.write_json("devices.json", &devices);
    }

    pub fn set_usb(&self, dongles: &[UsbDongle]) {
        self.write_json("usb.json", &dongles);
    }

    pub fn write_report(&self, text: &str, flaps: &FlapSummary) -> anyhow::Result<PathBuf> {
        let path = self.dir.join("report.txt");
        if self.files.is_some() {
            std::fs::write(&path, text)?;
            std::fs::write(
                self.dir.join("flaps.json"),
                serde_json::to_string_pretty(flaps)?,
            )?;
        }
        Ok(path)
    }

    pub fn discard_if_empty(&mut self) -> bool {
        if self.files.is_none() || self.bytes > 0 {
            return false;
        }
        self.files = None;
        for f in SESSION_FILES {
            let _ = std::fs::remove_file(self.dir.join(f));
        }
        std::fs::remove_dir(&self.dir).is_ok()
    }
}

const SESSION_FILES: [&str; 7] = [
    "events.jsonl",
    "verdicts.jsonl",
    "poses.csv",
    "devices.json",
    "usb.json",
    "report.txt",
    "flaps.json",
];

pub fn fmt_clock(ms: u64) -> String {
    let s = ms / 1000;
    format!("+{}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
}

pub fn incident(v: &Verdict) -> String {
    let mut parts = Vec::new();
    if let Some(d) = v.duration_ms {
        parts.push(format!("outage {}", fmt_dur(d)));
    }
    match v.pose_valid {
        Some(true) => parts.push("pose stayed valid".to_string()),
        Some(false) => parts.push("pose lost".to_string()),
        None => {}
    }
    if !v.tags.is_empty() {
        let tags: Vec<String> = v.tags.iter().map(|t| format!("{t:?}")).collect();
        parts.push(format!("tags {}", tags.join("+")));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" | {}", parts.join(", "))
    }
}

pub fn incidents_text(verdicts: &[Verdict], t0: u64, label: &dyn Fn(&str) -> String) -> String {
    if verdicts.is_empty() {
        return "No tracking incidents recorded.\n".into();
    }
    let mut out = String::new();
    for v in verdicts {
        let who = match label(&v.device) {
            l if l.is_empty() => v.device.clone(),
            l => format!("{} ({l})", v.device),
        };
        out.push_str(&format!(
            "{} {who} | {} ({}){}\n    {}\n",
            fmt_clock(v.t_start_ms.saturating_sub(t0)),
            summary::plain_cause(&v.cause),
            summary::confidence_word(v.confidence),
            incident(v),
            v.cause.describe()
        ));
        for e in &v.evidence {
            out.push_str(&format!("      {e}\n"));
        }
        if !v.alternates.is_empty() {
            let alts: Vec<String> = v.alternates.iter().map(summary::plain_cause).collect();
            out.push_str(&format!("      also possible: {}\n", alts.join(", ")));
        }
    }
    out
}

fn flaps_text(f: &FlapSummary) -> String {
    if f.devices.is_empty() {
        return "None.\n".into();
    }
    let mut out = format!(
        "Tracking left RunningOk for under {} ms with a valid pose. Counted, not listed as incidents.\n",
        f.threshold_ms
    );
    let minutes = f.span_ms as f64 / 60_000.0;
    for (dev, s) in &f.devices {
        let rate = if minutes > 0.0 {
            format!(", {:.1}/min", s.count as f64 / minutes)
        } else {
            String::new()
        };
        out.push_str(&format!(
            "  {dev}: {} flaps{rate}, mean {} ms, max {} ms\n",
            s.count,
            s.total_ms / s.count.max(1),
            s.max_ms
        ));
    }
    out
}

pub fn compose(engine: &Engine, s: &Summary) -> String {
    let name = engine
        .session
        .dir()
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let flaps = engine.correlator.flap_summary();
    let mut out = format!(
        "TrackDoctor {} report, {name}\n\n== Summary ==\n",
        crate::VERSION
    );
    out.push_str(&summary::render(s));

    out.push_str("\n== USB connections ==\n");
    let ctx = TreeCtx {
        devices: &engine.devices,
        summary: Some(s),
        radio: &engine.radio,
        names: &engine.names,
    };
    if engine.usb.is_empty() {
        out.push_str("Not recorded for this session.\n");
    } else {
        out.push_str(&summary::render_tree(&summary::usb_tree(&engine.usb, &ctx)));
        for w in crowding(&engine.usb) {
            out.push_str(&format!("{w}\n"));
        }
    }

    out.push_str("\n== Devices seen ==\n");
    if engine.devices.is_empty() {
        out.push_str("Not recorded for this session.\n");
    }
    for d in &engine.devices {
        let dongle = if d.dongle.is_empty() {
            String::new()
        } else {
            format!(", dongle {}", d.dongle)
        };
        out.push_str(&format!(
            "  {} {} ({}{dongle})\n",
            d.serial,
            engine.label(&d.serial),
            d.model
        ));
    }

    out.push_str("\n== Incidents (time since session start) ==\n");
    let label = |serial: &str| engine.label(serial);
    out.push_str(&incidents_text(
        engine.session.verdicts(),
        flaps.start_ms,
        &label,
    ));
    out.push_str("\n== Flaps ==\n");
    out.push_str(&flaps_text(&flaps));
    out
}

pub fn latest_session() -> Option<PathBuf> {
    std::fs::read_dir(sessions_dir())
        .ok()?
        .filter_map(Result::ok)
        .filter(|e| std::fs::metadata(e.path().join("events.jsonl")).is_ok_and(|m| m.len() > 0))
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
        .map(|e| e.path())
}

fn old_roster(dir: &Path) -> Vec<DeviceRecord> {
    let Ok(text) = std::fs::read_to_string(dir.join("report.txt")) else {
        return Vec::new();
    };
    text.lines()
        .take_while(|l| !l.trim().is_empty() || l.starts_with("devices seen"))
        .filter_map(|l| {
            let l = l.trim();
            let (head, rest) = l.split_once(" class=")?;
            let (serial, model) = head.split_once(' ')?;
            let class = match rest.split(' ').next()? {
                "Hmd" => DeviceClass::Hmd,
                "Controller" => DeviceClass::Controller,
                "GenericTracker" => DeviceClass::GenericTracker,
                "TrackingReference" => DeviceClass::TrackingReference,
                _ => DeviceClass::Other,
            };
            let field = |k: &str| {
                rest.split(' ')
                    .find_map(|t| t.strip_prefix(k))
                    .unwrap_or("")
                    .to_string()
            };
            Some(DeviceRecord {
                serial: serial.into(),
                model: model.into(),
                class,
                dongle: field("dongle="),
                lighthouse: field("lighthouse=") == "true",
            })
        })
        .collect()
}

type ModelLookup<'a> = &'a dyn Fn(&str) -> Option<(String, String)>;

fn inferred_roster(events: &[SignalEvent], lookup: ModelLookup) -> Vec<DeviceRecord> {
    let mut out: Vec<DeviceRecord> = Vec::new();
    for e in events {
        let Some(serial) = e.device.as_deref() else {
            continue;
        };
        if out.iter().any(|d| d.serial == serial) {
            continue;
        }
        let mut model = String::new();
        let class = if e.source == Source::Api && e.detail.starts_with("device 0 ") {
            DeviceClass::Hmd
        } else if serial.starts_with("LHB-") {
            DeviceClass::TrackingReference
        } else if let Some((m, class)) = lookup(serial) {
            model = m;
            match class.as_str() {
                "generic_tracker" => DeviceClass::GenericTracker,
                "controller" => DeviceClass::Controller,
                _ => DeviceClass::Other,
            }
        } else {
            continue;
        };
        out.push(DeviceRecord {
            serial: serial.into(),
            model,
            class,
            dongle: String::new(),
            lighthouse: class != DeviceClass::Hmd,
        });
    }
    out
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

pub struct Replay {
    pub engine: Engine,
    pub summary: Summary,
    pub events: usize,
    pub skipped: usize,
    pub log_events: Option<usize>,
}

pub fn merge_log(events: Vec<SignalEvent>, log: &str) -> anyhow::Result<(Vec<SignalEvent>, usize)> {
    use crate::signals::logtail::{naive_ms, parse_line, utc_offset};
    let offset = events
        .iter()
        .filter(|e| e.source == Source::Log)
        .find_map(|e| Some(utc_offset(e.t_ms, naive_ms(&e.detail)?)))
        .ok_or_else(|| {
            anyhow!("this session has no SteamVR log lines to line the log's clock up with")
        })?;
    let lo = events.iter().map(|e| e.t_ms).min().unwrap_or(0) as i64;
    let hi = events.iter().map(|e| e.t_ms).max().unwrap_or(0) as i64;
    let fresh: Vec<SignalEvent> = log
        .lines()
        .filter_map(|line| {
            let utc = naive_ms(line)? + offset;
            if utc < lo || utc > hi {
                return None;
            }
            parse_line(line.trim_end(), utc as u64)
        })
        .collect();
    let n = fresh.len();
    let mut merged: Vec<SignalEvent> = events
        .into_iter()
        .filter(|e| e.source != Source::Log)
        .chain(fresh)
        .collect();
    merged.sort_by_key(|e| e.t_ms);
    Ok((merged, n))
}

pub fn replay(dir: &Path, names: Names, log: Option<&Path>) -> anyhow::Result<Replay> {
    replay_with(dir, names, log, &crate::signals::logtail::lighthouse_model)
}

fn replay_with(
    dir: &Path,
    names: Names,
    log: Option<&Path>,
    lookup: ModelLookup,
) -> anyhow::Result<Replay> {
    let text = std::fs::read_to_string(dir.join("events.jsonl"))
        .with_context(|| format!("no events.jsonl in {}", dir.display()))?;
    let mut skipped = 0;
    let mut events: Vec<SignalEvent> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| {
            let ev = serde_json::from_str(l).ok();
            if ev.is_none() {
                skipped += 1;
            }
            ev
        })
        .collect();
    let mut log_events = None;
    if let Some(path) = log {
        let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
        let (merged, n) = merge_log(events, &String::from_utf8_lossy(&bytes))?;
        events = merged;
        log_events = Some(n);
    }
    let devices = read_json::<Vec<DeviceRecord>>(&dir.join("devices.json"))
        .or_else(|| Some(old_roster(dir)).filter(|r| !r.is_empty()))
        .unwrap_or_else(|| inferred_roster(&events, lookup));

    let mut engine = Engine::new(SessionWriter::memory(dir.to_path_buf()), names);
    engine.set_devices(devices);
    if let Some(usb) = read_json::<Vec<UsbDongle>>(&dir.join("usb.json")) {
        engine.usb = usb;
    }
    const STEP_MS: u64 = 250;
    let mut now = events.first().map_or(0, |e| e.t_ms);
    let n = events.len();
    for ev in events {
        while now + STEP_MS <= ev.t_ms {
            now += STEP_MS;
            engine.tick(now);
        }
        now = now.max(ev.t_ms);
        engine.handle(Msg::Event(ev));
    }
    let end = now + 130_000;
    while now < end {
        now += STEP_MS;
        engine.tick(now);
    }
    let summary = engine.summary();
    Ok(Replay {
        engine,
        summary,
        events: n,
        skipped,
        log_events,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::correlate::{Cause, Confidence, FlapStats, Tag};
    use crate::event::{Kind, TrackState};

    fn verdict(duration_ms: Option<u64>, pose_valid: Option<bool>, tags: Vec<Tag>) -> Verdict {
        Verdict {
            device: "LHR-A".into(),
            t_start_ms: 61_000,
            t_end_ms: 63_000,
            cause: Cause::Unknown,
            confidence: Confidence::Low,
            duration_ms,
            pose_valid,
            tags,
            evidence: vec![],
            alternates: vec![],
        }
    }

    fn sample(t_ms: u64) -> FrameSample {
        FrameSample {
            idx: 3,
            t_ms,
            pos: [0.5, 1.25, -2.0],
            vel: [0.0; 3],
            rot: [
                [1.0, 0.0, 0.0, 0.5],
                [0.0, 1.0, 0.0, 1.25],
                [0.0, 0.0, 1.0, -2.0],
            ],
            ang_speed: 0.0,
            state: TrackState::RunningOk,
            valid: true,
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("trackdoctor-{name}-{}", std::process::id()))
    }

    #[test]
    fn incidents_show_clock_label_and_outage() {
        let text = incidents_text(
            &[
                verdict(Some(74_358), Some(false), vec![Tag::Standby]),
                verdict(Some(600), Some(true), vec![]),
            ],
            1_000,
            &|s: &str| {
                if s == "LHR-A" {
                    "left foot".into()
                } else {
                    String::new()
                }
            },
        );
        assert!(
            text.starts_with(
                "+0:01:00 LHR-A (left foot) | dropout, cause unclear (unsure) | outage 1m 14s, pose lost, tags Standby\n"
            ),
            "{text}"
        );
        assert!(
            text.contains("| outage 0.6s, pose stayed valid\n"),
            "{text}"
        );
    }

    #[test]
    fn flaps_text_lists_rates() {
        let mut devices = std::collections::BTreeMap::new();
        devices.insert(
            "LHR-A".to_string(),
            FlapStats {
                count: 30,
                total_ms: 2730,
                max_ms: 480,
            },
        );
        let flaps = FlapSummary {
            threshold_ms: 500,
            start_ms: 0,
            span_ms: 600_000,
            devices,
        };
        let text = flaps_text(&flaps);
        assert!(
            text.contains("LHR-A: 30 flaps, 3.0/min, mean 91 ms, max 480 ms"),
            "{text}"
        );
    }

    #[test]
    fn quat_matches_rotation() {
        let id = quat(&sample(0).rot);
        assert_eq!(id, [1.0, 0.0, 0.0, 0.0]);
        let z90 = quat(&[
            [0.0, -1.0, 0.0, 0.0],
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
        ]);
        let h = std::f32::consts::FRAC_1_SQRT_2;
        for (a, b) in z90.iter().zip([h, 0.0, 0.0, h]) {
            assert!((a - b).abs() < 1e-6, "{z90:?}");
        }
    }

    #[test]
    fn pose_log_is_rate_limited_and_bytes_are_counted() {
        let dir = temp_dir("poses");
        let mut s = SessionWriter::in_dir(dir.clone(), true).unwrap();
        for t in [1000, 1050, 1099, 1100, 1150, 1201] {
            s.pose("LHR-A", &sample(t));
        }
        s.pose("LHR-B", &sample(1050));
        let text = std::fs::read_to_string(dir.join("poses.csv")).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], POSE_HEADER);
        assert_eq!(
            lines[1],
            "1000,LHR-A,0.5000,1.2500,-2.0000,1.00000,0.00000,0.00000,0.00000,RunningOk,1"
        );
        let a: Vec<&str> = lines
            .iter()
            .filter(|l| l.contains("LHR-A"))
            .copied()
            .collect();
        assert_eq!(a.len(), 3, "{text}");
        assert!(
            a[1].starts_with("1100,") && a[2].starts_with("1201,"),
            "{text}"
        );
        assert_eq!(lines.len(), 5);
        let on_disk: u64 = ["events.jsonl", "verdicts.jsonl", "poses.csv"]
            .iter()
            .map(|f| std::fs::metadata(dir.join(f)).unwrap().len())
            .sum();
        assert_eq!(s.bytes_written(), on_disk);
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn poses_off_by_default_and_memory_writes_nothing() {
        let dir = temp_dir("noposes");
        let mut s = SessionWriter::in_dir(dir.clone(), false).unwrap();
        s.pose("LHR-A", &sample(1000));
        assert!(!dir.join("poses.csv").exists());
        assert_eq!(s.bytes_written(), 0);
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);

        let mem_dir = temp_dir("memory");
        let mut m = SessionWriter::memory(mem_dir.clone());
        m.verdict(&verdict(None, None, vec![]));
        m.set_devices(&[]);
        m.write_report("x", &FlapSummary::default()).unwrap();
        assert_eq!(m.verdicts().len(), 1);
        assert!(!mem_dir.exists());
    }

    #[test]
    fn empty_session_is_discarded_but_recorded_one_is_kept() {
        let dir = temp_dir("empty");
        let mut s = SessionWriter::in_dir(dir.clone(), false).unwrap();
        s.set_usb(&[]);
        s.write_report("nothing", &FlapSummary::default()).unwrap();
        assert!(s.discard_if_empty());
        assert!(!dir.exists());

        let dir = temp_dir("kept");
        let mut s = SessionWriter::in_dir(dir.clone(), false).unwrap();
        s.event(&SignalEvent::at(
            1,
            Source::Api,
            None,
            Kind::SyncAcquired,
            "x",
        ));
        assert!(!s.discard_if_empty());
        assert!(dir.join("events.jsonl").is_file());
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn line(ev: &SignalEvent) -> String {
        serde_json::to_string(ev).unwrap()
    }

    fn api(t: u64, dev: &str, kind: Kind, detail: &str) -> SignalEvent {
        SignalEvent::at(t, Source::Api, Some(dev.into()), kind, detail)
    }

    #[test]
    fn merge_log_replaces_old_log_events_using_the_session_clock() {
        let utc = 1_790_988_720_306;
        let old_log = SignalEvent::at(
            utc,
            Source::Log,
            None,
            Kind::StandbyStart,
            "Fri Oct 02 2026 19:52:00.306 [Info] - 0 - entering standby",
        );
        let events = vec![
            api(
                utc - 1_000,
                "LHR-A",
                Kind::DeviceActivated,
                "device 3 connected=true",
            ),
            old_log,
            api(utc + 10_000, "LHR-A", Kind::PoseValid(true), "valid"),
        ];
        let log = "\
Fri Oct 02 2026 19:40:00.000 [Info] - 0 - entering standby
Fri Oct 02 2026 19:52:00.306 [Info] - 0 - entering standby
Fri Oct 02 2026 19:52:03.842 [Info] - lighthouse: LHR-A C: Resetting tracking: no optical samples for 2004ms
Tue Oct 06 2026 19:38:27.394 [Info] - lighthouse: D373DE2627: Packet received after 4.131s, keepalive (0/1)
";
        let (merged, n) = merge_log(events.clone(), log).unwrap();
        assert_eq!(n, 2);
        let kinds: Vec<&Kind> = merged.iter().map(|e| &e.kind).collect();
        assert_eq!(kinds.len(), 4);
        assert_eq!(merged[1].t_ms, utc);
        assert_eq!(merged[2].t_ms, utc + 3_536);
        assert!(matches!(merged[2].kind, Kind::OpticalLoss { .. }));
        let no_log: Vec<SignalEvent> = events
            .into_iter()
            .filter(|e| e.source != Source::Log)
            .collect();
        assert!(merge_log(no_log, log).is_err());
    }

    #[test]
    fn roster_is_inferred_from_events_and_steamvr_configs() {
        let events = vec![
            api(1, "HMD1", Kind::DeviceActivated, "device 0 connected=true"),
            api(2, "LHB-1", Kind::DeviceActivated, "device 3 connected=true"),
            api(3, "LHR-X", Kind::DeviceActivated, "device 4 connected=true"),
            api(4, "LHR-Y", Kind::DeviceActivated, "device 5 connected=true"),
        ];
        let lookup =
            |s: &str| (s == "LHR-X").then(|| ("Knuckles Right".into(), "controller".into()));
        let r = inferred_roster(&events, &lookup);
        let got: Vec<(&str, DeviceClass, &str)> = r
            .iter()
            .map(|d| (d.serial.as_str(), d.class, d.model.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("HMD1", DeviceClass::Hmd, ""),
                ("LHB-1", DeviceClass::TrackingReference, ""),
                ("LHR-X", DeviceClass::Controller, "Knuckles Right"),
            ]
        );
    }

    #[test]
    fn replay_reanalyzes_events_and_reads_old_roster() {
        let dir = temp_dir("replay");
        std::fs::create_dir_all(&dir).unwrap();
        let t0 = 1_000_000;
        let drop = Kind::TrackingState {
            from: TrackState::RunningOk,
            to: TrackState::CalibratingOutOfRange,
        };
        let back = Kind::TrackingState {
            from: TrackState::CalibratingOutOfRange,
            to: TrackState::RunningOk,
        };
        let mut lines = vec![
            line(&api(
                t0,
                "HMD1",
                Kind::DeviceActivated,
                "device 0 connected=true",
            )),
            "{\"t_ms\":5,\"kind\":\"SomethingFromTheFuture\"}".to_string(),
        ];
        for i in 0..4u64 {
            let t = t0 + 20_000 + i * 5_000;
            lines.push(line(&api(t, "LHR-B", drop.clone(), "drop")));
            lines.push(line(&api(t + 90, "LHR-B", back.clone(), "back")));
        }
        let t = t0 + 60_000;
        lines.push(line(&api(t, "LHR-A", drop.clone(), "drop")));
        lines.push(line(&api(t + 10, "LHR-A", Kind::PoseValid(false), "lost")));
        lines.push(line(&api(
            t + 4_000,
            "LHR-A",
            Kind::PoseValid(true),
            "valid",
        )));
        lines.push(line(&api(t + 4_000, "LHR-A", back.clone(), "back")));
        lines.push(line(&api(
            t0 + 90_000,
            "LHR-B",
            Kind::BatteryLevel { pct: 50.0 },
            "b",
        )));
        std::fs::write(dir.join("events.jsonl"), lines.join("\n")).unwrap();
        std::fs::write(
            dir.join("report.txt"),
            "devices seen this session:\n  LHR-A Knuckles Left class=Controller dongle=D1 lighthouse=true\n  LHR-B VIVE Tracker 3.0 MV class=GenericTracker dongle=D2 lighthouse=true\n\nold stuff\n",
        )
        .unwrap();

        let r = replay(&dir, Names::default(), None).unwrap();
        assert_eq!(r.skipped, 1);
        assert_eq!(r.engine.devices.len(), 2);
        let rows = &r.summary.rows;
        assert_eq!(rows[0].serial, "LHR-A", "{rows:?}");
        assert_eq!(rows[0].label, "left controller");
        assert_eq!(rows[0].lost_ms, 4_000);
        assert_eq!(rows[0].incidents, 1);
        assert_eq!(rows[1].serial, "LHR-B");
        assert_eq!(rows[1].flaps, 4);
        assert_eq!(rows[1].incidents, 0);
        let text = r.engine.report_text();
        assert!(text.contains("== Summary =="), "{text}");
        assert!(text.contains("LHR-A (left controller) |"), "{text}");
        assert!(text.contains("+0:01:00"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

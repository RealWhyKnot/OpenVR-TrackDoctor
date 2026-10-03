use crate::correlate::{FlapSummary, Tag, Verdict};
use crate::event::SignalEvent;
use crate::signals::openvr::FrameSample;
use anyhow::Context;
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const POSE_INTERVAL_MS: u64 = 100;
const POSE_HEADER: &str = "t_ms,serial,x,y,z,qw,qx,qy,qz,state,valid";

pub struct SessionWriter {
    dir: PathBuf,
    events: std::fs::File,
    verdicts: std::fs::File,
    poses: Option<std::fs::File>,
    pose_last: HashMap<String, u64>,
    bytes: u64,
    all_verdicts: Vec<Verdict>,
    roster: Vec<String>,
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

impl SessionWriter {
    pub fn new(poses: bool) -> anyhow::Result<Self> {
        let base = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let dir = base
            .join("trackdoctor")
            .join("sessions")
            .join(format!("session-{stamp}"));
        Self::in_dir(dir, poses)
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
            events: std::fs::File::create(dir.join("events.jsonl"))?,
            verdicts: std::fs::File::create(dir.join("verdicts.jsonl"))?,
            poses,
            pose_last: HashMap::new(),
            bytes,
            all_verdicts: Vec::new(),
            roster: Vec::new(),
            dir,
        })
    }

    pub fn roster_line(&mut self, line: &str) {
        if !self.roster.iter().any(|l| l == line) {
            self.roster.push(line.to_string());
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn bytes_written(&self) -> u64 {
        self.bytes
    }

    pub fn event(&mut self, ev: &SignalEvent) {
        if let Ok(line) = serde_json::to_string(ev) {
            write_line(&mut self.events, &mut self.bytes, &line);
        }
    }

    pub fn pose(&mut self, serial: &str, f: &FrameSample) {
        let Some(file) = self.poses.as_mut() else {
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
        if let Ok(line) = serde_json::to_string(v) {
            write_line(&mut self.verdicts, &mut self.bytes, &line);
        }
        self.all_verdicts.push(v.clone());
    }

    pub fn finish(&mut self, flaps: &FlapSummary) -> anyhow::Result<PathBuf> {
        let path = self.dir.join("report.txt");
        let mut text = String::from("devices seen this session:\n");
        for l in &self.roster {
            text.push_str(&format!("  {l}\n"));
        }
        text.push('\n');
        text.push_str(&render(&self.all_verdicts, Some(flaps)));
        std::fs::write(&path, text)?;
        std::fs::write(
            self.dir.join("flaps.json"),
            serde_json::to_string_pretty(flaps)?,
        )?;
        Ok(path)
    }
}

pub fn incident(v: &Verdict) -> String {
    let mut parts = Vec::new();
    if let Some(d) = v.duration_ms {
        parts.push(format!("outage {:.1}s", d as f64 / 1000.0));
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

fn render_flaps(f: &FlapSummary) -> String {
    if f.devices.is_empty() {
        return String::new();
    }
    let mut out = format!(
        "\nflaps (tracking left RunningOk for under {} ms with a valid pose; counted, no verdict):\n",
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

pub fn render(verdicts: &[Verdict], flaps: Option<&FlapSummary>) -> String {
    let flap_text = flaps.map(render_flaps).unwrap_or_default();
    if verdicts.is_empty() {
        return format!("no tracking incidents recorded this session\n{flap_text}");
    }
    let mut out = String::new();
    let t0 = verdicts.iter().map(|v| v.t_start_ms).min().unwrap_or(0);
    for v in verdicts {
        let rel = (v.t_start_ms.saturating_sub(t0)) as f64 / 1000.0;
        out.push_str(&format!(
            "[{rel:9.1}s] {} | {:?} confidence {:?}{}\n    {}\n",
            v.device,
            v.cause,
            v.confidence,
            incident(v),
            v.cause.describe()
        ));
        for e in &v.evidence {
            out.push_str(&format!("      {e}\n"));
        }
        if !v.alternates.is_empty() {
            let alts: Vec<String> = v.alternates.iter().map(|a| format!("{a:?}")).collect();
            out.push_str(&format!("      also possible: {}\n", alts.join(", ")));
        }
    }
    out.push_str("\nsummary by device:\n");
    let mut counts: std::collections::BTreeMap<(String, String), (usize, usize)> =
        Default::default();
    for v in verdicts {
        let c = counts
            .entry((v.device.clone(), format!("{:?}", v.cause)))
            .or_default();
        c.0 += 1;
        if v.tags.contains(&Tag::Standby) {
            c.1 += 1;
        }
    }
    for ((dev, cause), (n, standby)) in counts {
        let note = if standby > 0 {
            format!(" ({standby} standby-related)")
        } else {
            String::new()
        };
        out.push_str(&format!("  {dev}: {cause} x{n}{note}\n"));
    }
    out.push_str(&flap_text);
    out
}

pub fn render_file(verdicts_jsonl: &Path) -> anyhow::Result<String> {
    let text = std::fs::read_to_string(verdicts_jsonl)?;
    let verdicts: Vec<Verdict> = text
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let flaps: Option<FlapSummary> = verdicts_jsonl
        .parent()
        .map(|d| d.join("flaps.json"))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok());
    Ok(render(&verdicts, flaps.as_ref()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::correlate::{Cause, Confidence, FlapStats};
    use crate::event::TrackState;

    fn verdict(duration_ms: Option<u64>, pose_valid: Option<bool>, tags: Vec<Tag>) -> Verdict {
        Verdict {
            device: "LHR-A".into(),
            t_start_ms: 1000,
            t_end_ms: 3000,
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
    fn render_distinguishes_long_lost_outage_from_blip() {
        let text = render(
            &[
                verdict(Some(74_358), Some(false), vec![Tag::Standby]),
                verdict(Some(600), Some(true), vec![]),
            ],
            None,
        );
        assert!(
            text.contains("| outage 74.4s, pose lost, tags Standby"),
            "{text}"
        );
        assert!(
            text.contains("| outage 0.6s, pose stayed valid\n"),
            "{text}"
        );
        assert!(
            text.contains("LHR-A: Unknown x2 (1 standby-related)"),
            "{text}"
        );
    }

    #[test]
    fn render_lists_flap_rates() {
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
            span_ms: 600_000,
            devices,
        };
        let text = render(&[], Some(&flaps));
        assert!(text.starts_with("no tracking incidents"), "{text}");
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
    fn poses_off_by_default() {
        let dir = temp_dir("noposes");
        let mut s = SessionWriter::in_dir(dir.clone(), false).unwrap();
        s.pose("LHR-A", &sample(1000));
        assert!(!dir.join("poses.csv").exists());
        assert_eq!(s.bytes_written(), 0);
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn report_file_reads_flaps_beside_verdicts() {
        let dir = temp_dir("rerender");
        let mut s = SessionWriter::in_dir(dir.clone(), false).unwrap();
        s.verdict(&verdict(Some(1200), Some(true), vec![]));
        let mut flaps = FlapSummary {
            threshold_ms: 500,
            span_ms: 60_000,
            ..Default::default()
        };
        flaps.devices.insert(
            "LHR-A".into(),
            FlapStats {
                count: 4,
                total_ms: 400,
                max_ms: 150,
            },
        );
        s.finish(&flaps).unwrap();
        drop(s);
        let text = render_file(&dir.join("verdicts.jsonl")).unwrap();
        assert!(text.contains("outage 1.2s"), "{text}");
        assert!(text.contains("LHR-A: 4 flaps, 4.0/min"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

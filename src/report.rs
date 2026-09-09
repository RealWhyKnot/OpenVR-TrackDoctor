use crate::correlate::Verdict;
use crate::event::SignalEvent;
use anyhow::Context;
use std::io::Write;
use std::path::{Path, PathBuf};

pub struct SessionWriter {
    dir: PathBuf,
    events: std::fs::File,
    verdicts: std::fs::File,
    all_verdicts: Vec<Verdict>,
}

impl SessionWriter {
    pub fn new() -> anyhow::Result<Self> {
        let stamp = chrono_stamp();
        let dir = std::env::current_exe()?
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("sessions")
            .join(stamp);
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        Ok(Self {
            events: std::fs::File::create(dir.join("events.jsonl"))?,
            verdicts: std::fs::File::create(dir.join("verdicts.jsonl"))?,
            all_verdicts: Vec::new(),
            dir,
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn event(&mut self, ev: &SignalEvent) {
        if let Ok(line) = serde_json::to_string(ev) {
            let _ = writeln!(self.events, "{line}");
        }
    }

    pub fn verdict(&mut self, v: &Verdict) {
        if let Ok(line) = serde_json::to_string(v) {
            let _ = writeln!(self.verdicts, "{line}");
        }
        self.all_verdicts.push(v.clone());
    }

    pub fn finish(&mut self) -> anyhow::Result<PathBuf> {
        let path = self.dir.join("report.txt");
        std::fs::write(&path, render(&self.all_verdicts))?;
        Ok(path)
    }
}

pub fn render(verdicts: &[Verdict]) -> String {
    if verdicts.is_empty() {
        return "no tracking incidents recorded this session\n".to_string();
    }
    let mut out = String::new();
    let t0 = verdicts.iter().map(|v| v.t_start_ms).min().unwrap_or(0);
    for v in verdicts {
        let rel = (v.t_start_ms.saturating_sub(t0)) as f64 / 1000.0;
        out.push_str(&format!(
            "[{rel:9.1}s] {} | {:?} confidence {:?}\n    {}\n",
            v.device, v.cause, v.confidence,
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
    let mut counts: std::collections::BTreeMap<(String, String), usize> = Default::default();
    for v in verdicts {
        *counts.entry((v.device.clone(), format!("{:?}", v.cause))).or_default() += 1;
    }
    for ((dev, cause), n) in counts {
        out.push_str(&format!("  {dev}: {cause} x{n}\n"));
    }
    out
}

pub fn render_file(verdicts_jsonl: &Path) -> anyhow::Result<String> {
    let text = std::fs::read_to_string(verdicts_jsonl)?;
    let verdicts: Vec<Verdict> = text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    Ok(render(&verdicts))
}

fn chrono_stamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("session-{secs}")
}

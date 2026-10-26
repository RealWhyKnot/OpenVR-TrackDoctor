use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use trackdoctor::gui::sessions;
use trackdoctor::names::{Names, data_dir};
use trackdoctor::report::{PACKETS_FILE, PACKETS_SUMMARY, read_json, sessions_dir};
use trackdoctor::signals::packets::{self, Line, Totals};
use trackdoctor::store;

macro_rules! out {
    ($($arg:tt)*) => {
        if writeln!(std::io::stdout(), $($arg)*).is_err() {
            std::process::exit(0);
        }
    };
}

const USAGE: &str = "tdinspect: look inside TrackDoctor's data while building features

  tdinspect sessions                   list recorded sessions, newest first
  tdinspect packets [SESSION]          per-dongle radio summary from a dev-build session
  tdinspect gaps [SESSION] [DONGLE]    every packet gap from a dev-build session
  tdinspect events [SESSION] [TEXT]    events, optionally only lines containing TEXT
  tdinspect diagnose [SESSION]         run a session's saved events through the diagnosis
  tdinspect track [SESSION] [SERIAL]   movement track as CSV on stdout
  tdinspect live-packets [SECONDS]     listen to the dongles read-only and print each second
  tdinspect log [LINES]                the end of the dev-build debug log
  tdinspect logscan [FILE]             run a SteamVR server log through the diagnosis and list
                                       the line shapes the parser doesn't know yet

SESSION is a folder path, a session-... name, or how many sessions back (0 is the newest).
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |i: usize| args.get(i).map(String::as_str);
    let result = match arg(0) {
        Some("sessions") => list_sessions(),
        Some("packets") => session(arg(1)).map(|d| packet_summary(&d)),
        Some("gaps") => session(arg(1)).map(|d| packet_gaps(&d, arg(2))),
        Some("events") => session(arg(1)).map(|d| events(&d, arg(2))),
        Some("diagnose") => session(arg(1)).map(|d| diagnose(&d)),
        Some("track") => session(arg(1)).map(|d| track_csv(&d, arg(2))),
        Some("live-packets") => {
            live_packets(arg(1).and_then(|s| s.parse().ok()).unwrap_or(10));
            Ok(())
        }
        Some("logscan") => logscan(arg(1)),
        Some("log") => {
            debug_log(arg(1).and_then(|s| s.parse().ok()).unwrap_or(60));
            Ok(())
        }
        _ => Err(USAGE.to_string()),
    };
    if let Err(e) = result {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

fn newest_first() -> Vec<PathBuf> {
    let mut dirs = store::session_dirs(&sessions_dir());
    dirs.sort_by_key(|d| std::cmp::Reverse(sessions::start_ms(d).unwrap_or(0)));
    dirs
}

fn session(pick: Option<&str>) -> Result<PathBuf, String> {
    let pick = pick.unwrap_or("0");
    if let Ok(back) = pick.parse::<usize>() {
        return newest_first()
            .into_iter()
            .nth(back)
            .ok_or_else(|| format!("there are fewer than {} sessions", back + 1));
    }
    let path = PathBuf::from(pick);
    if path.is_dir() {
        return Ok(path);
    }
    let named = sessions_dir().join(pick);
    if named.is_dir() {
        return Ok(named);
    }
    Err(format!("no session called {pick}"))
}

fn list_sessions() -> Result<(), String> {
    for (i, d) in newest_first().iter().enumerate() {
        let marks: Vec<&str> = [
            (store::has(d, "events.jsonl"), "events"),
            (store::has(d, trackdoctor::track::RAW_FILE), "track"),
            (store::has(d, PACKETS_FILE), "packets"),
        ]
        .into_iter()
        .filter_map(|(on, name)| on.then_some(name))
        .collect();
        out!(
            "{i:>3}  {}  {:>9}  {}  [{}]",
            d.file_name().and_then(|n| n.to_str()).unwrap_or("?"),
            store::fmt_size(store::size(d)),
            sessions::title(d),
            marks.join(", ")
        );
    }
    Ok(())
}

fn label(names: &Names, serial: Option<&str>) -> String {
    match serial {
        Some(s) => match names.get(s) {
            Some(n) => format!("{n} ({s})"),
            None => s.to_string(),
        },
        None => "no device".to_string(),
    }
}

fn read_lines(dir: &Path) -> Vec<Line> {
    store::read_text(dir, PACKETS_FILE)
        .map(|t| packets::parse_lines(&t))
        .unwrap_or_default()
}

fn packet_summary(dir: &Path) {
    let names = Names::load_default();
    let mut all: Vec<Totals> = read_json(&dir.join(PACKETS_SUMMARY)).unwrap_or_default();
    if all.is_empty() {
        all = packets::totals(&read_lines(dir)).into_values().collect();
    }
    if all.is_empty() {
        out!(
            "{} has no packet data. Only dev builds record it.",
            dir.display()
        );
        return;
    }
    out!(
        "{:<12} {:<34} {:>7} {:>8} {:>6} {:>9} {:>6} {:>9} {:>6} {:>5} {:>5}",
        "dongle",
        "device",
        "secs",
        "pkt/s",
        "hgaps",
        "hgap max",
        "agaps",
        "air ms",
        "air %",
        "bad",
        "odd"
    );
    for t in &all {
        out!(
            "{:<12} {:<34} {:>7} {:>8.0} {:>6} {:>9} {:>6} {:>9} {:>6.2} {:>5} {:>5}",
            t.dongle,
            label(&names, t.device.as_deref()),
            t.seconds,
            t.packets_per_s(),
            t.host_gaps,
            t.host_gap_max_ms,
            t.air_gaps,
            t.air_ms,
            t.air_share() * 100.0,
            t.malformed,
            t.odd
        );
    }
    out!();
    out!("tracker clock steps between packets (1 step is about 1.37 ms):");
    out!(
        "{:<12} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9}",
        "dongle",
        "0",
        "1",
        "2",
        "3",
        "4-7",
        "8-15",
        "16-63",
        "64+"
    );
    for t in &all {
        let row: String = t.clock.iter().map(|c| format!(" {c:>9}")).collect();
        out!("{:<12}{row}", t.dongle);
    }
    for t in &all {
        if t.worst.is_empty() {
            continue;
        }
        out!();
        out!("worst gaps on {}:", t.dongle);
        for g in &t.worst {
            out!(
                "  {}  host {:>5} ms  air {:>5}  flags {} -> {}",
                wall(g.t_ms),
                g.host_ms,
                g.air_ms.map_or("?".to_string(), |a| format!("{a} ms")),
                flags(g.before),
                flags(g.after)
            );
        }
    }
}

fn wall(ms: u64) -> String {
    match trackdoctor::win::local_time(ms) {
        Some((_, _, _, h, mi)) => {
            format!("{h:02}:{mi:02}:{:02}.{:03}", (ms / 1000) % 60, ms % 1000)
        }
        None => ms.to_string(),
    }
}

fn flags(f: Option<u8>) -> String {
    f.map_or("--".to_string(), |f| format!("{f:02X}"))
}

fn packet_gaps(dir: &Path, dongle: Option<&str>) {
    out!("t,dongle,host_ms,air_ms,before,after");
    for l in read_lines(dir) {
        if let Line::Gap(g) = l
            && dongle.is_none_or(|d| d == g.dongle)
        {
            out!(
                "{},{},{},{},{},{}",
                g.t_ms,
                g.dongle,
                g.host_ms,
                g.air_ms.map_or(String::new(), |a| a.to_string()),
                flags(g.before),
                flags(g.after)
            );
        }
    }
}

fn events(dir: &Path, filter: Option<&str>) {
    let names = Names::load_default();
    for ev in sessions::read_events(dir) {
        let line = format!(
            "{}  {:?}  {}  {:?}  {}",
            wall(ev.t_ms),
            ev.source,
            label(&names, ev.device.as_deref()),
            ev.kind,
            ev.detail
        );
        if filter.is_none_or(|f| line.to_lowercase().contains(&f.to_lowercase())) {
            out!("{line}");
        }
    }
}

fn diagnose(dir: &Path) {
    let names = Names::load_default();
    let mut diagnosis = trackdoctor::diagnose::Diagnosis::default();
    for ev in sessions::read_events(dir) {
        diagnosis.ingest(&ev);
    }
    for f in diagnosis.findings(&|s| label(&names, Some(s))) {
        out!("{}", f.title);
        out!("  {}", f.what);
        for (i, step) in f.steps.iter().enumerate() {
            out!("  {}. {step}", i + 1);
        }
    }
}

fn track_csv(dir: &Path, serial: Option<&str>) {
    let Some(track) = store::read_track(dir) else {
        eprintln!("{} has no movement track", dir.display());
        return;
    };
    out!("t_ms,serial,class,x,y,z,qw,qx,qy,qz,state,valid");
    for d in &track.devices {
        if serial.is_some_and(|s| s != d.serial) {
            continue;
        }
        for s in &d.samples {
            out!(
                "{},{},{:?},{:.4},{:.4},{:.4},{:.5},{:.5},{:.5},{:.5},{:?},{}",
                track.t0 + u64::from(s.t),
                d.serial,
                d.class,
                s.pos[0],
                s.pos[1],
                s.pos[2],
                s.q[0],
                s.q[1],
                s.q[2],
                s.q[3],
                s.state(),
                u8::from(s.valid())
            );
        }
    }
}

fn live_packets(secs: u64) {
    let names = Names::load_default();
    let owners: BTreeMap<String, String> = sessions::last_roster()
        .into_iter()
        .filter(|d| !d.dongle.is_empty())
        .map(|d| (d.dongle, d.serial))
        .collect();
    let mut all: Vec<Line> = Vec::new();
    out!("listening to every dongle for {secs} s (read-only)");
    packets::listen(secs, |lines| {
        for l in lines {
            match l {
                Line::Second(s) if s.reports > 0 => out!(
                    "{:<12} {:>5} pkt  imu {:>4}  light {:>4}  heartbeat {:>3}  host gaps {:>2} (max {:>4} ms)  air gaps {:>2} ({:>4} ms)  odd {}",
                    s.dongle,
                    s.packets,
                    s.imu,
                    s.light,
                    s.heartbeats,
                    s.host_gaps,
                    s.host_gap_max_ms,
                    s.air_gaps,
                    s.air_ms,
                    s.odd
                ),
                Line::Gap(g) => out!(
                    "{:<12} gap: host {} ms, air {}",
                    g.dongle,
                    g.host_ms,
                    g.air_ms.map_or("?".to_string(), |a| format!("{a} ms"))
                ),
                Line::Note { dongle, text, .. } => out!(
                    "{dongle:<12} {text} [{}]",
                    label(&names, owners.get(dongle).map(String::as_str))
                ),
                Line::Second(_) => {}
            }
        }
        all.extend_from_slice(lines);
    });
    out!();
    let totals = packets::totals(&all);
    if totals.values().all(|t| t.reports == 0) {
        out!("No dongle sent anything. A dongle only sends while its tracker or controller is on.");
    }
    for t in totals.values().filter(|t| t.reports > 0) {
        out!("{}", packets::rollup(t));
    }
}

fn debug_log(lines: usize) {
    let path = trackdoctor::debug::path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        out!(
            "no debug log at {}. Only dev builds write one (data folder: {}).",
            path.display(),
            data_dir().display()
        );
        return;
    };
    let all: Vec<&str> = text.lines().collect();
    for l in &all[all.len().saturating_sub(lines)..] {
        out!("{l}");
    }
}

fn logscan(file: Option<&str>) -> Result<(), String> {
    use trackdoctor::signals::logtail::{parse_line, vrserver_log_path};
    let path = file.map_or_else(vrserver_log_path, PathBuf::from);
    let bytes = match (path.parent(), path.file_name().and_then(|n| n.to_str())) {
        (Some(dir), Some(name)) => store::read_bytes(dir, name),
        _ => None,
    }
    .ok_or_else(|| format!("can't read {}", path.display()))?;
    let text = String::from_utf8_lossy(&bytes);
    let now = trackdoctor::event::now_ms();
    let mut diagnosis = trackdoctor::diagnose::Diagnosis::default();
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    let mut unknown: BTreeMap<String, (usize, String)> = BTreeMap::new();
    let mut lines = 0usize;
    for line in text.lines() {
        lines += 1;
        match parse_line(line.trim_end(), now) {
            Some(ev) => {
                let name = format!("{:?}", ev.kind);
                let name = name
                    .split(['{', '(', ' '])
                    .next()
                    .unwrap_or_default()
                    .to_string();
                *kinds.entry(name).or_default() += 1;
                diagnosis.ingest(&ev);
            }
            None => {
                let body = line.split_once("] - ").map_or(line, |(_, b)| b);
                let mut shape = String::new();
                let mut last_hash = false;
                for c in body.chars().take(90) {
                    if c.is_ascii_digit() {
                        if !last_hash {
                            shape.push('#');
                        }
                        last_hash = true;
                    } else {
                        shape.push(c);
                        last_hash = false;
                    }
                }
                let e = unknown.entry(shape).or_insert((0, body.to_string()));
                e.0 += 1;
            }
        }
    }
    out!("{} lines from {}", lines, path.display());
    out!("");
    out!("Parsed:");
    for (k, n) in &kinds {
        out!("  {n:>6}  {k}");
    }
    out!("");
    out!("Findings:");
    for f in diagnosis.findings(&|s| s.to_string()) {
        out!("  {}", f.title);
        out!("    {}", f.what);
        for (i, step) in f.steps.iter().enumerate() {
            out!("    {}. {step}", i + 1);
        }
    }
    for n in diagnosis.setup_notes() {
        out!("  {n}");
    }
    out!("");
    out!("{}", trackdoctor::report::sight_section(&diagnosis.sight));
    let mut top: Vec<(&String, &(usize, String))> = unknown.iter().collect();
    top.sort_by_key(|a| std::cmp::Reverse(a.1.0));
    out!("Line shapes the parser skips, most common first:");
    for (shape, (n, sample)) in top.iter().take(400) {
        out!("  {n:>6}  {shape}");
        if sample.len() > shape.len() + 20 {
            out!(
                "          e.g. {}",
                sample.chars().take(140).collect::<String>()
            );
        }
    }
    Ok(())
}

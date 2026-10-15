use super::present::{self, BAD, DIM, FAINT, GOOD, NORMAL, ReportView, RoomView, UsbView, WARN};
use super::ui;
use crate::bases::BaseRecord;
use crate::engine::{Engine, Msg};
use crate::event::{Kind, SignalEvent, Source, now_ms};
use crate::signals::usb::UsbDongle;
use crate::summary::{self, Summary, fmt_dur};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Duration;

const TICK_MS: u64 = 1_000;
const ROOM_TICK_MS: u64 = 1_000 / 6;
const REPORT_MS: u64 = 1_000;
const NAMES_MS: u64 = 2_000;
const FEED_ENTRIES: usize = 300;
const BUFFER_MAX: usize = 20_000;

pub enum Update {
    Status {
        state: String,
        tone: i32,
        detail: String,
    },
    Devices(Vec<ui::Device>),
    Feed(Vec<ui::Line>),
    Report(ReportView),
    Usb(UsbView),
    Room(RoomView),
    Names(Vec<ui::NameRow>),
    Recording(Option<PathBuf>),
    SessionEnded,
}

pub type Factory = Box<dyn FnMut(bool) -> (Engine, Option<String>) + Send>;

pub struct Recorder {
    engine: Option<Engine>,
    factory: Factory,
    poses: Arc<AtomicBool>,
    room: Arc<AtomicBool>,
    started: u64,
    buffered: Vec<SignalEvent>,
    raw_buffered: Vec<String>,
    usb: Option<Vec<UsbDongle>>,
    base_db: Option<Vec<BaseRecord>>,
    feed: VecDeque<Vec<ui::Line>>,
    feed_dirty: bool,
    devices: Vec<ui::Device>,
    names: Vec<ui::NameRow>,
    summary: Summary,
    vr_status: String,
    ended: bool,
    unsaved: bool,
    packets_noted: bool,
    next_report: u64,
    next_names: u64,
}

impl Recorder {
    pub fn new(factory: Factory, poses: Arc<AtomicBool>) -> Self {
        Self {
            engine: None,
            factory,
            poses,
            room: Arc::new(AtomicBool::new(false)),
            started: 0,
            buffered: Vec::new(),
            raw_buffered: Vec::new(),
            usb: None,
            base_db: None,
            feed: VecDeque::new(),
            feed_dirty: false,
            devices: Vec::new(),
            names: Vec::new(),
            summary: Summary::default(),
            vr_status: String::new(),
            ended: false,
            unsaved: false,
            packets_noted: false,
            next_report: 0,
            next_names: 0,
        }
    }

    pub fn recording(&self) -> bool {
        self.engine.is_some()
    }

    pub fn room_shown(&self) -> Arc<AtomicBool> {
        self.room.clone()
    }

    fn period(&self) -> u64 {
        if self.room.load(Ordering::Relaxed) {
            ROOM_TICK_MS
        } else {
            TICK_MS
        }
    }

    fn say(&mut self, lines: Vec<ui::Line>) {
        self.feed.push_front(lines);
        self.feed.truncate(FEED_ENTRIES);
        self.feed_dirty = true;
    }

    fn note(&mut self, text: impl Into<String>, tone: i32) {
        let text = text.into();
        self.say(vec![present::line(text, tone)]);
    }

    fn start(&mut self, now: u64) -> Vec<Update> {
        let (mut engine, problem) = (self.factory)(self.poses.load(Ordering::Relaxed));
        if let Some(u) = self.usb.clone() {
            engine.handle(Msg::Usb(u));
        }
        if let Some(db) = self.base_db.clone() {
            engine.handle(Msg::BaseDb(db));
        }
        for ev in std::mem::take(&mut self.buffered) {
            engine.handle(Msg::Event(ev));
        }
        let raw = std::mem::take(&mut self.raw_buffered);
        if !raw.is_empty() {
            engine.handle(Msg::RawLog(raw));
        }
        let dir = engine.session.dir().to_path_buf();
        crate::dlog!(
            "recorder: started {}{}",
            dir.display(),
            problem
                .as_deref()
                .map(|p| format!(" ({p})"))
                .unwrap_or_default()
        );
        self.engine = Some(engine);
        self.started = now;
        self.ended = false;
        self.packets_noted = false;
        self.summary = Summary::default();
        self.next_report = 0;
        self.next_names = 0;
        self.note("SteamVR is running. Recording a new session.", GOOD);
        self.unsaved = problem.is_some();
        if let Some(p) = problem {
            self.note(p, BAD);
        }
        vec![Update::Recording(Some(dir))]
    }

    pub fn handle(&mut self, msg: Msg, now: u64) -> Vec<Update> {
        let mut out = Vec::new();
        match msg {
            Msg::Status(s) => {
                self.vr_status = s;
                return out;
            }
            Msg::SteamVrExited => {
                if self.engine.is_some() {
                    out.extend(self.finish(now));
                }
                return out;
            }
            Msg::Device(_) if self.engine.is_none() => out.extend(self.start(now)),
            Msg::Packets(ref b) if self.engine.is_some() && !self.packets_noted => {
                let heard = b.dongles_heard();
                if heard > 0 {
                    self.packets_noted = true;
                    let plural = if heard == 1 { "" } else { "s" };
                    self.note(
                        format!(
                            "Dev build: recording radio packets too. {heard} dongle{plural} sending so far."
                        ),
                        DIM,
                    );
                }
            }
            _ => {}
        }
        let Some(engine) = self.engine.as_mut() else {
            match msg {
                Msg::Event(ev) if ev.source == Source::Log => {
                    if ev.kind == Kind::LogRotated {
                        self.buffered.clear();
                    }
                    if self.buffered.len() < BUFFER_MAX {
                        self.buffered.push(ev);
                    }
                }
                Msg::RawLog(lines) => {
                    let room = BUFFER_MAX.saturating_sub(self.raw_buffered.len());
                    self.raw_buffered.extend(lines.into_iter().take(room));
                }
                Msg::Usb(u) => self.usb = Some(u),
                Msg::BaseDb(db) => self.base_db = Some(db),
                _ => {}
            }
            return out;
        };
        match &msg {
            Msg::Usb(u) => self.usb = Some(u.clone()),
            Msg::BaseDb(db) => self.base_db = Some(db.clone()),
            _ => {}
        }
        let t0 = self.started;
        let mut lines = Vec::new();
        for ev in engine.handle(msg) {
            let label = ev
                .device
                .as_deref()
                .map(|d| engine.label(d))
                .unwrap_or_default();
            if let Some(l) = present::anomaly_line(&ev, &label, t0) {
                lines.push(l);
            }
        }
        for l in lines {
            self.say(vec![l]);
        }
        out
    }

    pub fn tick(&mut self, now: u64) -> Vec<Update> {
        let mut out = Vec::new();
        let t0 = self.started;
        if let Some(engine) = self.engine.as_mut() {
            let verdicts = engine.tick(now);
            engine.snapshot(now);
            let notices = std::mem::take(&mut engine.notices);
            let entries: Vec<Vec<ui::Line>> = verdicts
                .iter()
                .map(|v| present::verdict_lines(v, &engine.label(&v.device), t0))
                .collect();
            for e in entries {
                self.say(e);
            }
            for n in notices {
                self.note(n, WARN);
            }
        }
        if let Some(engine) = self.engine.as_mut() {
            if now >= self.next_names {
                self.next_names = now + NAMES_MS;
                if engine.names.refresh() {
                    self.next_report = 0;
                }
            }
            let devices = present::devices(engine);
            if devices != self.devices {
                self.devices = devices.clone();
                out.push(Update::Devices(devices));
            }
            if now >= self.next_report {
                self.next_report = now + REPORT_MS;
                self.summary = engine.summary();
                out.push(Update::Report(present::report(
                    engine,
                    &self.summary,
                    false,
                )));
                out.push(Update::Usb(present::usb(engine, &self.summary)));
                let names = present::name_rows(engine);
                if names != self.names {
                    self.names = names.clone();
                    out.push(Update::Names(names));
                }
            }
            if self.room.load(Ordering::Relaxed) {
                out.push(Update::Room(present::live_room(engine, &self.summary)));
            }
            let incidents = engine.session.verdicts().len();
            out.push(Update::Status {
                state: "Recording".into(),
                tone: GOOD,
                detail: format!(
                    "{} so far, {}, {:.0} KB saved",
                    fmt_dur(now.saturating_sub(self.started)),
                    summary::plural(incidents, "incident"),
                    engine.session.bytes_written() as f64 / 1024.0
                ),
            });
        } else if self.ended {
            out.push(Update::Status {
                state: "SteamVR closed".into(),
                tone: DIM,
                detail: "The session is saved. A new one starts when SteamVR runs again.".into(),
            });
        } else {
            out.push(Update::Status {
                state: "Waiting for SteamVR".into(),
                tone: FAINT,
                detail: waiting_detail(&self.vr_status),
            });
        }
        if self.feed_dirty {
            self.feed_dirty = false;
            out.push(Update::Feed(self.feed.iter().flatten().cloned().collect()));
        }
        out
    }

    pub fn finish(&mut self, now: u64) -> Vec<Update> {
        let Some(mut engine) = self.engine.take() else {
            return Vec::new();
        };
        let saved = crate::app::wind_down(&mut engine);
        let summary = engine.summary();
        let mut out = vec![
            Update::Report(present::report(&engine, &summary, true)),
            Update::Usb(present::usb(&engine, &summary)),
            Update::Room(present::live_room(&engine, &summary)),
            Update::Recording(None),
            Update::SessionEnded,
        ];
        let line = match saved {
            Ok(Some(_)) if self.unsaved => (
                format!(
                    "SteamVR closed after {}. This session couldn't be saved, so it is only on the pages until you close TrackDoctor.",
                    fmt_dur(now.saturating_sub(self.started))
                ),
                BAD,
            ),
            Ok(Some(_)) => (
                format!(
                    "SteamVR closed after {}. The session is saved under History.",
                    fmt_dur(now.saturating_sub(self.started))
                ),
                NORMAL,
            ),
            Ok(None) => ("SteamVR closed. Nothing was recorded.".to_string(), NORMAL),
            Err(e) => (
                format!("SteamVR closed, but saving the report failed: {e}"),
                BAD,
            ),
        };
        self.note(line.0, line.1);
        self.ended = true;
        self.buffered.clear();
        self.raw_buffered.clear();
        out.extend(self.tick(now));
        out
    }
}

fn waiting_detail(status: &str) -> String {
    let status = status.trim();
    if status.is_empty() || status.starts_with("connected") {
        "Start SteamVR and recording begins by itself.".into()
    } else {
        let reason = status
            .strip_prefix("waiting for SteamVR:")
            .unwrap_or(status)
            .trim();
        if reason.is_empty() {
            "Start SteamVR and recording begins by itself.".into()
        } else {
            format!("Start SteamVR and recording begins by itself. ({reason})")
        }
    }
}

pub fn run(
    rx: Receiver<Msg>,
    mut recorder: Recorder,
    stop: Arc<AtomicBool>,
    sink: impl Fn(Vec<Update>),
) {
    let mut next_tick = now_ms();
    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let period = recorder.period();
        let now = now_ms();
        if next_tick > now + period {
            next_tick = now;
        }
        let wait = next_tick.saturating_sub(now).min(period);
        let mut out = Vec::new();
        match rx.recv_timeout(Duration::from_millis(wait)) {
            Ok(msg) => {
                out.extend(recorder.handle(msg, now_ms()));
                for _ in 0..5_000 {
                    match rx.try_recv() {
                        Ok(msg) => out.extend(recorder.handle(msg, now_ms())),
                        Err(_) => break,
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        let now = now_ms();
        if now >= next_tick {
            next_tick = now + recorder.period();
            out.extend(recorder.tick(now));
        }
        if !out.is_empty() {
            sink(out);
        }
    }
    let out = recorder.finish(now_ms());
    if !out.is_empty() {
        sink(out);
    }
}

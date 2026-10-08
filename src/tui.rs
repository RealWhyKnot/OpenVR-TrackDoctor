use crate::correlate::{Cause, Confidence, Verdict};
use crate::engine::{Engine, Msg};
use crate::event::{DeviceClass, Kind, TrackState, now_ms};
use crate::report::{fmt_clock, incident};
use crate::signals::usb::crowding;
use crate::summary::{self, Level, Summary, TreeCtx, fmt_dur};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::canvas::Canvas;
use ratatui::widgets::{Block, Borders, Paragraph, Row, Table, Tabs, Wrap};
use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

const TABS: [&str; 4] = ["1 Live", "2 Summary", "3 USB", "4 Room"];
const DRAW_EVERY: Duration = Duration::from_millis(250);
const TICK_EVERY_MS: u64 = 100;
const MAX_DRAIN: usize = 20_000;
const FEED_MAX: usize = 400;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Quit,
    Next,
    Prev,
    Tab(usize),
    Names,
    Other,
}

pub fn key(k: &KeyEvent) -> Key {
    if k.kind == KeyEventKind::Release {
        return Key::Other;
    }
    match k.code {
        KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => Key::Quit,
        KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => Key::Quit,
        KeyCode::Tab | KeyCode::Right => Key::Next,
        KeyCode::BackTab | KeyCode::Left => Key::Prev,
        KeyCode::Char(c @ '1'..='4') => Key::Tab(c as usize - '1' as usize),
        KeyCode::Char('n') | KeyCode::Char('N') => Key::Names,
        _ => Key::Other,
    }
}

fn level_style(l: Level) -> Style {
    match l {
        Level::Head => Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
        Level::Plain => Style::default(),
        Level::Good => Style::default().fg(Color::Green),
        Level::Warn => Style::default().fg(Color::Yellow),
        Level::Bad => Style::default().fg(Color::LightRed),
        Level::Dim => Style::default().fg(Color::DarkGray),
    }
}

struct Ui {
    tab: usize,
    status: String,
    feed: VecDeque<(String, Level)>,
    start_ms: u64,
    summary: Summary,
    summary_at: u64,
}

impl Ui {
    fn push(&mut self, text: String, level: Level) {
        self.feed.push_back((text, level));
        while self.feed.len() > FEED_MAX {
            self.feed.pop_front();
        }
    }

    fn verdict(&mut self, engine: &Engine, v: &Verdict) {
        let who = match engine.label(&v.device) {
            l if l.is_empty() => v.device.clone(),
            l => format!("{} {l}", v.device),
        };
        let quiet = v.cause == Cause::Unknown && v.confidence == Confidence::Low;
        self.push(
            format!(
                "{} {who}: {} ({}){}",
                fmt_clock(v.t_start_ms.saturating_sub(self.start_ms)),
                summary::plain_cause(&v.cause),
                summary::confidence_word(v.confidence),
                incident(v)
            ),
            if quiet { Level::Plain } else { Level::Warn },
        );
        if quiet {
            return;
        }
        self.push(format!("    {}", v.cause.describe()), Level::Dim);
        for e in v.evidence.iter().take(3) {
            self.push(format!("      {e}"), Level::Dim);
        }
    }
}

fn name_devices(engine: &Engine) -> String {
    let list: Vec<(String, String)> = engine
        .devices
        .iter()
        .filter(|d| d.class != DeviceClass::TrackingReference)
        .map(|d| {
            (
                d.serial.clone(),
                format!("{} ({})", summary::kind_label(&d.model, d.class), d.model),
            )
        })
        .collect();
    if let Err(e) = engine.names.add_missing(&list) {
        return format!("could not write {}: {e}", engine.names.path().display());
    }
    match std::process::Command::new("notepad.exe")
        .arg(engine.names.path())
        .spawn()
    {
        Ok(_) => format!(
            "Opened {} in Notepad. Type a name after each serial and save; names show up here right away.",
            engine.names.path().display()
        ),
        Err(_) => format!(
            "Edit {} to name your devices; names show up here right away.",
            engine.names.path().display()
        ),
    }
}

pub fn run(mut engine: Engine, rx: Receiver<Msg>, note: Option<String>) -> anyhow::Result<Engine> {
    let mut terminal = ratatui::init();
    let mut ui = Ui {
        tab: 0,
        status: "starting".into(),
        feed: VecDeque::new(),
        start_ms: now_ms(),
        summary: Summary::default(),
        summary_at: 0,
    };
    if let Some(n) = note {
        ui.push(n, Level::Warn);
    }
    if engine.names.is_empty() {
        ui.push(
            "Tip: press n to give your trackers names (left foot, waist, ...).".into(),
            Level::Dim,
        );
    }
    let started = Instant::now();
    let mut next_tick = 0u64;
    let mut next_names = 0u64;
    let mut last_draw: Option<Instant> = None;
    let mut update_noted = false;

    let result = (|| -> anyhow::Result<()> {
        loop {
            if crate::win::stop_requested() {
                return Ok(());
            }
            for _ in 0..MAX_DRAIN {
                match rx.try_recv() {
                    Ok(Msg::Status(s)) => ui.status = s,
                    Ok(Msg::SteamVrExited) => {
                        ui.push("SteamVR closed. Waiting for it to start again; press q to stop and see the summary.".into(), Level::Warn);
                    }
                    Ok(msg) => {
                        for ev in engine.handle(msg) {
                            let anomaly = matches!(
                                ev.kind,
                                Kind::Jump { .. }
                                    | Kind::OrientationJump { .. }
                                    | Kind::Drift { .. }
                                    | Kind::SnapBack { .. }
                                    | Kind::PoseFrozen { .. }
                                    | Kind::Parked(_)
                            );
                            if anomaly {
                                let who = ev.device.as_deref().unwrap_or("?");
                                ui.push(
                                    format!(
                                        "{} {who} {}: {}",
                                        fmt_clock(ev.t_ms.saturating_sub(ui.start_ms)),
                                        engine.label(who),
                                        ev.detail
                                    ),
                                    Level::Dim,
                                );
                            }
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return Ok(()),
                }
            }
            let now = now_ms();
            if now >= next_tick {
                next_tick = now + TICK_EVERY_MS;
                for v in engine.tick(now) {
                    ui.verdict(&engine, &v);
                }
                engine.snapshot(now);
                for n in std::mem::take(&mut engine.notices) {
                    ui.push(n, Level::Warn);
                }
                if !update_noted && let Some(release) = crate::update::found() {
                    update_noted = true;
                    let version = release
                        .version()
                        .map_or(release.tag_name.clone(), |v| v.to_string());
                    ui.push(
                        format!("TrackDoctor {version} is available. Press q when you're done and you'll be asked to update."),
                        Level::Good,
                    );
                }
            }
            if now >= next_names {
                next_names = now + 2_000;
                engine.names.refresh();
            }

            if last_draw.is_none_or(|t| t.elapsed() >= DRAW_EVERY) {
                if ui.tab != 0 && now.saturating_sub(ui.summary_at) >= 1_000 {
                    ui.summary = engine.summary();
                    ui.summary_at = now;
                }
                let elapsed = started.elapsed().as_millis() as u64;
                terminal.draw(|f| draw(f, &engine, &ui, elapsed))?;
                last_draw = Some(Instant::now());
            }

            if crossterm::event::poll(Duration::from_millis(50))?
                && let Event::Key(k) = crossterm::event::read()?
            {
                match key(&k) {
                    Key::Quit => return Ok(()),
                    Key::Next => ui.tab = (ui.tab + 1) % TABS.len(),
                    Key::Prev => ui.tab = (ui.tab + TABS.len() - 1) % TABS.len(),
                    Key::Tab(t) => ui.tab = t,
                    Key::Names => {
                        let msg = name_devices(&engine);
                        ui.push(msg, Level::Good);
                    }
                    Key::Other => continue,
                }
                ui.summary = engine.summary();
                ui.summary_at = now;
                last_draw = None;
            }
        }
    })();

    ratatui::restore();
    result.map(|_| engine)
}

fn draw(f: &mut Frame, engine: &Engine, ui: &Ui, elapsed_ms: u64) {
    let [header, tabs, body] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(5),
    ])
    .areas(f.area());

    let incidents = engine.session.verdicts().len();
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "TrackDoctor ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(
                "{} | {} | recording {} | {} | {:.0} KB saved",
                crate::VERSION,
                ui.status,
                fmt_dur(elapsed_ms),
                summary::plural(incidents, "incident"),
                engine.session.bytes_written() as f64 / 1024.0
            )),
        ])),
        header,
    );
    let [tab_area, keys_area] =
        Layout::horizontal([Constraint::Min(40), Constraint::Length(46)]).areas(tabs);
    f.render_widget(
        Tabs::new(TABS)
            .select(ui.tab)
            .style(Style::default().fg(Color::DarkGray))
            .highlight_style(Style::default().fg(Color::Black).bg(Color::Cyan)),
        tab_area,
    );
    f.render_widget(
        Paragraph::new("Tab switch view   n name devices   q stop")
            .style(Style::default().fg(Color::DarkGray)),
        keys_area,
    );
    match ui.tab {
        0 => live(f, body, engine, ui),
        1 => summary_view(f, body, ui),
        2 => usb_view(f, body, engine, ui),
        _ => room_view(f, body, engine, ui),
    }
}

fn live(f: &mut Frame, area: Rect, engine: &Engine, ui: &Ui) {
    let mut devices: Vec<_> = engine.meta.iter().collect();
    devices.sort_by_key(|(idx, m)| (m.class == DeviceClass::TrackingReference, **idx));
    let rows: Vec<Row> = devices
        .iter()
        .map(|(idx, m)| {
            let live = engine.live.get(idx).copied();
            let base = m.class == DeviceClass::TrackingReference;
            let (state, style) = match live {
                None => ("-".to_string(), Style::default().fg(Color::DarkGray)),
                Some(l) if !l.connected => {
                    ("off".to_string(), Style::default().fg(Color::DarkGray))
                }
                Some(l) if l.parked => (
                    "parked, ignored".to_string(),
                    Style::default().fg(Color::DarkGray),
                ),
                Some(_) if base => ("on".to_string(), Style::default().fg(Color::DarkGray)),
                Some(l) => {
                    let color = match l.state {
                        TrackState::RunningOk => Color::Green,
                        TrackState::Uninitialized => Color::DarkGray,
                        _ => Color::LightRed,
                    };
                    (l.state.plain(), Style::default().fg(color))
                }
            };
            let pose = match live {
                Some(l) if !base && l.connected => if l.valid { "ok" } else { "lost" }.to_string(),
                _ => String::new(),
            };
            let (flaps, incidents) = if base || m.class == DeviceClass::Hmd {
                (String::new(), String::new())
            } else {
                (
                    engine.correlator.flap_count(&m.serial).to_string(),
                    engine.incident_count(&m.serial).to_string(),
                )
            };
            Row::new(vec![
                m.serial.clone(),
                engine.label(&m.serial),
                state,
                pose,
                m.battery_pct
                    .filter(|_| !base)
                    .map(|b| format!("{b:.0}%"))
                    .unwrap_or_default(),
                flaps,
                incidents,
                m.dongle.clone(),
            ])
            .style(style)
        })
        .collect();
    let [table_area, feed_area] = Layout::vertical([
        Constraint::Length(rows.len().max(1) as u16 + 3),
        Constraint::Min(4),
    ])
    .areas(area);
    let waiting = rows.is_empty();
    f.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(16),
                Constraint::Length(18),
                Constraint::Length(20),
                Constraint::Length(5),
                Constraint::Length(5),
                Constraint::Length(6),
                Constraint::Length(9),
                Constraint::Length(11),
            ],
        )
        .header(
            Row::new(vec![
                "device",
                "name",
                "state",
                "pose",
                "batt",
                "flaps",
                "incidents",
                "dongle",
            ])
            .style(Style::default().fg(Color::Cyan)),
        )
        .block(Block::default().borders(Borders::ALL).title(if waiting {
            " devices (start SteamVR; this screen fills in once it is running) "
        } else {
            " devices "
        })),
        table_area,
    );
    let height = feed_area.height.saturating_sub(2) as usize;
    let lines: Vec<Line> = ui
        .feed
        .iter()
        .rev()
        .take(height)
        .rev()
        .map(|(s, l)| Line::styled(s.clone(), level_style(*l)))
        .collect();
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" what happened (newest at the bottom) "),
        ),
        feed_area,
    );
}

fn summary_view(f: &mut Frame, area: Rect, ui: &Ui) {
    let s = &ui.summary;
    let mut lines = vec![Line::styled(
        format!(
            "So far: {}. Worst first, ranked by time spent not tracking. Press q to stop and keep this summary.",
            fmt_dur(s.span_ms)
        ),
        Style::default().fg(Color::Cyan),
    )];
    let table = summary::table_lines(s);
    for (i, l) in table.into_iter().enumerate() {
        let style = match i {
            0 => Style::default().fg(Color::DarkGray),
            _ => match s.rows.get(i - 1) {
                Some(r) if !r.trouble() => level_style(Level::Good),
                Some(r) if i <= 3 && r.lost_ms > 0 => level_style(Level::Bad),
                Some(_) => level_style(Level::Warn),
                None => Style::default(),
            },
        };
        lines.push(Line::styled(l, style));
    }
    if s.rows.is_empty() {
        lines.push(Line::styled(
            "No trackers or controllers yet.",
            level_style(Level::Dim),
        ));
    }
    lines.push(Line::raw(""));
    for n in summary::notes(s) {
        lines.push(Line::styled(n, level_style(Level::Dim)));
    }
    let tips = summary::tips(s);
    if !tips.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled("What to try", level_style(Level::Head)));
        for t in tips {
            lines.push(Line::raw(format!("  {t}")));
        }
    }
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(" summary ")),
        area,
    );
}

fn usb_view(f: &mut Frame, area: Rect, engine: &Engine, ui: &Ui) {
    let ctx = TreeCtx {
        devices: &engine.devices,
        summary: Some(&ui.summary),
        radio: &engine.radio,
        names: &engine.names,
    };
    let mut lines: Vec<Line> = summary::usb_tree(&engine.usb, &ctx)
        .into_iter()
        .map(|l| Line::styled(l.text, level_style(l.level)))
        .collect();
    lines.push(Line::raw(""));
    for w in crowding(&engine.usb) {
        lines.push(Line::styled(w, level_style(Level::Warn)));
    }
    lines.push(Line::styled(
        "#N = trouble rank from the Summary view. Radio gaps come from SteamVR's keepalive log lines.",
        level_style(Level::Dim),
    ));
    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" USB: which controller and port each dongle uses "),
        ),
        area,
    );
}

pub struct MapPoint {
    pub x: f64,
    pub y: f64,
    pub label: String,
    pub color: Color,
}

pub fn room_bounds(points: &[(f64, f64)], w: u16, h: u16) -> ([f64; 2], [f64; 2]) {
    let (mut x0, mut x1, mut y0, mut y1) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
    for &(x, y) in points {
        x0 = x0.min(x);
        x1 = x1.max(x);
        y0 = y0.min(y);
        y1 = y1.max(y);
    }
    if points.is_empty() {
        (x0, x1, y0, y1) = (-1.0, 1.0, -1.0, 1.0);
    }
    let margin = 0.5;
    let (mut dx, mut dy) = (x1 - x0 + 2.0 * margin, y1 - y0 + 2.0 * margin);
    let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let (w, h) = (w.max(1) as f64, h.max(1) as f64);
    let want_dy = 2.0 * dx * h / w;
    if want_dy > dy {
        dy = want_dy;
    } else {
        dx = dy * w / (2.0 * h);
    }
    (
        [cx - dx / 2.0, cx + dx / 2.0],
        [cy - dy / 2.0, cy + dy / 2.0],
    )
}

fn room_view(f: &mut Frame, area: Rect, engine: &Engine, ui: &Ui) {
    let [map_area, legend_area] =
        Layout::horizontal([Constraint::Min(30), Constraint::Length(60)]).areas(area);
    let s = &ui.summary;
    let mut points = Vec::new();
    let mut legend: Vec<Line> = Vec::new();
    let mut metas: Vec<_> = engine.meta.iter().collect();
    metas.sort_by_key(|(idx, _)| **idx);
    for (idx, m) in metas {
        let Some(l) = engine.live.get(idx) else {
            continue;
        };
        if !l.connected || l.parked {
            continue;
        }
        let (label, color) = match m.class {
            DeviceClass::Hmd => ("H".to_string(), Color::White),
            DeviceClass::TrackingReference => ("B".to_string(), Color::Cyan),
            _ => {
                let pos = s.rows.iter().position(|r| r.serial == m.serial);
                let row = pos.map(|p| &s.rows[p]);
                let color = match (pos, row) {
                    (_, Some(r)) if !r.trouble() => Color::Green,
                    (Some(p), Some(r)) if p < 3 && r.lost_ms > 0 => Color::LightRed,
                    (_, Some(_)) => Color::Yellow,
                    _ => Color::Gray,
                };
                let mut label = pos.map_or("*".to_string(), |p| (p + 1).to_string());
                if l.state != TrackState::RunningOk {
                    label.push('?');
                }
                let detail = row.map_or(String::new(), |r| {
                    if r.trouble() {
                        format!(
                            "  {} lost, {}",
                            fmt_dur(r.lost_ms),
                            summary::plural(r.incidents, "incident")
                        )
                    } else {
                        "  clean".into()
                    }
                });
                legend.push(Line::styled(
                    format!(
                        "{label:>3} {} {}{detail}",
                        m.serial,
                        engine.label(&m.serial)
                    ),
                    Style::default().fg(color),
                ));
                (label, color)
            }
        };
        points.push(MapPoint {
            x: l.pos[0] as f64,
            y: -l.pos[2] as f64,
            label,
            color,
        });
    }
    let inner_w = map_area.width.saturating_sub(2);
    let inner_h = map_area.height.saturating_sub(2);
    let xy: Vec<(f64, f64)> = points.iter().map(|p| (p.x, p.y)).collect();
    let (xb, yb) = room_bounds(&xy, inner_w, inner_h);
    let title = if points.is_empty() {
        " room from above (waiting for SteamVR positions) "
    } else {
        " room from above, up = forward "
    };
    f.render_widget(
        Canvas::default()
            .block(Block::default().borders(Borders::ALL).title(title))
            .x_bounds(xb)
            .y_bounds(yb)
            .paint(|ctx| {
                for p in &points {
                    ctx.print(
                        p.x,
                        p.y,
                        Line::styled(
                            p.label.clone(),
                            Style::default().fg(p.color).add_modifier(Modifier::BOLD),
                        ),
                    );
                }
            }),
        map_area,
    );
    legend.push(Line::raw(""));
    legend.push(Line::styled(
        "H headset   B base station",
        level_style(Level::Dim),
    ));
    legend.push(Line::styled(
        "number = trouble rank, ? = not tracking now",
        level_style(Level::Dim),
    ));
    legend.push(Line::styled(
        "red = worst, yellow = some trouble, green = clean",
        level_style(Level::Dim),
    ));
    legend.push(Line::styled(
        "Move a tracker to see which number it is.",
        level_style(Level::Dim),
    ));
    f.render_widget(
        Paragraph::new(legend)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(" legend ")),
        legend_area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::DevLive;
    use crate::event::{SignalEvent, Source};
    use crate::names::Names;
    use crate::report::SessionWriter;
    use crate::signals::openvr::DeviceMeta;
    use crate::signals::usb::UsbDongle;

    const T0: u64 = 1_000_000;

    fn fake_engine() -> Engine {
        let mut e = Engine::new(
            SessionWriter::memory(std::path::PathBuf::from("session-test")),
            Names::default(),
        );
        let devs = [
            (0, "HMD-1", "Quest", DeviceClass::Hmd, "", [0.4, 1.6, 0.6]),
            (
                1,
                "LHB-9417E0B3",
                "Valve SR Imp",
                DeviceClass::TrackingReference,
                "",
                [-1.2, 2.2, -1.8],
            ),
            (
                2,
                "LHB-9D8DEA7B",
                "Valve SR Imp",
                DeviceClass::TrackingReference,
                "",
                [1.2, 2.2, 1.8],
            ),
            (
                3,
                "LHR-7E902102",
                "Knuckles Left",
                DeviceClass::Controller,
                "0C58EE58E6",
                [-0.3, 1.1, -0.2],
            ),
            (
                4,
                "LHR-905BD201",
                "VIVE Tracker 3.0 MV",
                DeviceClass::GenericTracker,
                "E1B9096D38",
                [0.1, 0.1, 0.1],
            ),
            (
                5,
                "LHR-617D30D7",
                "VIVE Tracker 3.0 MV",
                DeviceClass::GenericTracker,
                "D373DE2627",
                [0.0, 1.0, 0.0],
            ),
        ];
        for (idx, serial, model, class, dongle, pos) in devs {
            e.handle(Msg::Device(DeviceMeta {
                idx,
                serial: serial.into(),
                model: model.into(),
                class,
                dongle: dongle.into(),
                battery_pct: Some(80.0),
                is_lighthouse: class != DeviceClass::Hmd,
            }));
            e.live.insert(
                idx,
                DevLive {
                    state: if idx == 4 {
                        TrackState::CalibratingOutOfRange
                    } else {
                        TrackState::RunningOk
                    },
                    valid: true,
                    connected: true,
                    parked: false,
                    pos,
                },
            );
        }
        e.handle(Msg::Usb(vec![
            UsbDongle {
                serial: Some("0C58EE58E6".into()),
                product: "Watchman Dongle".into(),
                controller: "Renesas USB controller (PCI 0102.0002.0600.0000)".into(),
                bus: "r".into(),
                ports: vec![8],
                hubs: vec![],
            },
            UsbDongle {
                serial: Some("E1B9096D38".into()),
                product: "Watchman Dongle".into(),
                controller: "AMD USB controller (PCI 0801.0003)".into(),
                bus: "a".into(),
                ports: vec![3],
                hubs: vec![],
            },
        ]));
        let state = |t: u64, dev: &str, from: TrackState, to: TrackState| {
            Msg::Event(SignalEvent::at(
                t,
                Source::Api,
                Some(dev.into()),
                Kind::TrackingState { from, to },
                "s",
            ))
        };
        e.handle(state(
            T0,
            "LHR-7E902102",
            TrackState::Uninitialized,
            TrackState::RunningOk,
        ));
        for i in 0..5 {
            let t = T0 + 20_000 + i * 3_000;
            e.handle(state(
                t,
                "LHR-905BD201",
                TrackState::RunningOk,
                TrackState::CalibratingOutOfRange,
            ));
            e.handle(state(
                t + 80,
                "LHR-905BD201",
                TrackState::CalibratingOutOfRange,
                TrackState::RunningOk,
            ));
        }
        let t = T0 + 40_000;
        e.handle(state(
            t,
            "LHR-7E902102",
            TrackState::RunningOk,
            TrackState::CalibratingOutOfRange,
        ));
        e.handle(Msg::Event(SignalEvent::at(
            t + 5,
            Source::Api,
            Some("LHR-7E902102".into()),
            Kind::PoseValid(false),
            "lost",
        )));
        e.handle(state(
            t + 6_000,
            "LHR-7E902102",
            TrackState::CalibratingOutOfRange,
            TrackState::RunningOk,
        ));
        let mut now = T0;
        while now < T0 + 60_000 {
            now += 100;
            e.tick(now);
        }
        e
    }

    fn render(e: &Engine, tab: usize) -> String {
        let mut ui = Ui {
            tab,
            status: "connected to SteamVR".into(),
            feed: VecDeque::new(),
            start_ms: T0,
            summary: e.summary(),
            summary_at: 0,
        };
        for v in e.session.verdicts() {
            ui.verdict(e, v);
        }
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30)).unwrap();
        term.draw(|f| draw(f, e, &ui, 125_000)).unwrap();
        let buf = term.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        if std::env::var_os("TRACKDOCTOR_SHOW").is_some() {
            println!("{out}");
        }
        out
    }

    #[test]
    fn every_tab_renders_its_content() {
        let e = fake_engine();
        let live = render(&e, 0);
        assert!(live.contains("LHR-7E902102"), "{live}");
        assert!(live.contains("left controller"), "{live}");
        assert!(live.contains("searching for bases"), "{live}");
        assert!(live.contains("dropout, cause unclear"), "{live}");
        let sum = render(&e, 1);
        assert!(sum.contains(" 1  LHR-7E902102 left controller"), "{sum}");
        assert!(sum.contains("What to try"), "{sum}");
        let usb = render(&e, 2);
        assert!(
            usb.contains("port 8  dongle 0C58EE58E6 -> LHR-7E902102 left controller  #1"),
            "{usb}"
        );
        let room = render(&e, 3);
        assert!(room.contains("room from above"), "{room}");
        assert!(room.contains("2? LHR-905BD201 tracker"), "{room}");
        assert!(room.matches('B').count() >= 3, "{room}");
        assert!(room.matches('H').count() >= 2, "{room}");
        assert!(live.contains("1 incident |"), "{live}");
        assert!(live.contains("(unsure)"), "{live}");
    }

    fn k(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn ctrl_c_and_q_quit_and_digits_pick_tabs() {
        assert_eq!(
            key(&k(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Key::Quit
        );
        assert_eq!(key(&k(KeyCode::Char('c'), KeyModifiers::NONE)), Key::Other);
        assert_eq!(key(&k(KeyCode::Char('q'), KeyModifiers::NONE)), Key::Quit);
        assert_eq!(key(&k(KeyCode::Esc, KeyModifiers::NONE)), Key::Quit);
        assert_eq!(key(&k(KeyCode::Char('3'), KeyModifiers::NONE)), Key::Tab(2));
        assert_eq!(key(&k(KeyCode::Char('5'), KeyModifiers::NONE)), Key::Other);
        assert_eq!(key(&k(KeyCode::BackTab, KeyModifiers::SHIFT)), Key::Prev);
        let mut release = k(KeyCode::Char('q'), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        assert_eq!(key(&release), Key::Other);
    }

    #[test]
    fn room_bounds_keep_square_meters_and_contain_points() {
        let pts = [(-1.15, -1.8), (1.15, 1.8), (0.0, 0.0)];
        let (xb, yb) = room_bounds(&pts, 80, 20);
        for (x, y) in pts {
            assert!(x > xb[0] && x < xb[1] && y > yb[0] && y < yb[1]);
        }
        let mx = (xb[1] - xb[0]) / 80.0;
        let my = (yb[1] - yb[0]) / 20.0;
        assert!((my - 2.0 * mx).abs() < 1e-9, "{mx} {my}");
        let (xb, yb) = room_bounds(&[], 10, 10);
        assert!(xb[0] < 0.0 && xb[1] > 0.0 && yb[0] < 0.0 && yb[1] > 0.0);
    }
}

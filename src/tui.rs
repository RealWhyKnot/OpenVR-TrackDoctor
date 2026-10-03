use crate::engine::{Engine, Msg};
use crate::event::{Kind, TrackState, now_ms};
use crate::report::incident;
use crossterm::event::{Event, KeyCode};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph, Row, Table};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

pub fn run(
    mut engine: Engine,
    rx: Receiver<Msg>,
    audit: Vec<String>,
    session_dir: PathBuf,
    stop: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let mut terminal = ratatui::init();
    let mut status = String::from("starting collectors");
    let mut feed: VecDeque<(String, u8)> = VecDeque::new();
    for a in &audit {
        feed.push_back((a.clone(), if a.starts_with("WARNING") { 2 } else { 0 }));
    }

    let result = (|| -> anyhow::Result<()> {
        loop {
            if stop.load(Ordering::SeqCst) {
                return Ok(());
            }
            loop {
                match rx.try_recv() {
                    Ok(Msg::Status(s)) => status = s,
                    Ok(msg) => {
                        for ev in engine.handle(msg) {
                            let anomaly = matches!(
                                ev.kind,
                                Kind::Jump { .. }
                                    | Kind::OrientationJump { .. }
                                    | Kind::Drift { .. }
                                    | Kind::SnapBack { .. }
                                    | Kind::PoseFrozen { .. }
                            );
                            if anomaly {
                                feed.push_back((
                                    format!(
                                        "{} {}",
                                        ev.device.as_deref().unwrap_or("?"),
                                        ev.detail
                                    ),
                                    1,
                                ));
                            }
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return Ok(()),
                }
            }
            for v in engine.tick(now_ms()) {
                feed.push_back((
                    format!(
                        "{} -> {:?} ({:?}){}",
                        v.device,
                        v.cause,
                        v.confidence,
                        incident(&v)
                    ),
                    2,
                ));
                feed.push_back((format!("  {}", v.cause.describe()), 0));
                for e in v.evidence.iter().take(6) {
                    feed.push_back((format!("  {e}"), 0));
                }
            }
            while feed.len() > 300 {
                feed.pop_front();
            }

            let mut devices: Vec<_> = engine.meta.iter().collect();
            devices.sort_by_key(|(idx, _)| **idx);
            let rows: Vec<Row> = devices
                .iter()
                .map(|(idx, m)| {
                    let live = engine.live.get(idx).copied();
                    let (state_text, style) = match live {
                        None => ("-".to_string(), Style::default().fg(Color::DarkGray)),
                        Some(l) if !l.connected => (
                            "disconnected".to_string(),
                            Style::default().fg(Color::DarkGray),
                        ),
                        Some(l) if l.parked => (
                            "parked, ignored".to_string(),
                            Style::default().fg(Color::DarkGray),
                        ),
                        Some(l) => {
                            let color = match l.state {
                                TrackState::RunningOk => Color::Green,
                                TrackState::Uninitialized => Color::DarkGray,
                                _ => Color::Red,
                            };
                            (format!("{:?}", l.state), Style::default().fg(color))
                        }
                    };
                    Row::new(vec![
                        m.serial.clone(),
                        format!("{:?}", m.class),
                        m.model.clone(),
                        state_text,
                        live.map(|l| if l.valid { "yes" } else { "no" })
                            .unwrap_or("-")
                            .to_string(),
                        m.dongle.clone(),
                        m.battery_pct
                            .map(|b| format!("{b:.0}%"))
                            .unwrap_or_default(),
                        engine.correlator.flap_count(&m.serial).to_string(),
                    ])
                    .style(style)
                })
                .collect();

            let written_kb = engine.session.bytes_written() as f64 / 1024.0;
            terminal.draw(|f| {
                let [header, table_area, feed_area] = Layout::vertical([
                    Constraint::Length(1),
                    Constraint::Length(rows.len() as u16 + 3),
                    Constraint::Min(5),
                ])
                .areas(f.area());

                f.render_widget(
                    Paragraph::new(format!(
                        "trackdoctor | {status} | session {} ({written_kb:.1} KB written) | q quits",
                        session_dir.display()
                    )),
                    header,
                );
                f.render_widget(
                    Table::new(
                        rows.clone(),
                        [
                            Constraint::Length(18),
                            Constraint::Length(17),
                            Constraint::Length(22),
                            Constraint::Length(21),
                            Constraint::Length(5),
                            Constraint::Length(12),
                            Constraint::Length(5),
                            Constraint::Length(6),
                        ],
                    )
                    .header(
                        Row::new(vec![
                            "serial", "class", "model", "state", "valid", "dongle", "batt",
                            "flaps",
                        ])
                        .style(Style::default().fg(Color::Cyan)),
                    )
                    .block(Block::default().borders(Borders::ALL).title("devices")),
                    table_area,
                );
                let lines: Vec<Line> = feed
                    .iter()
                    .rev()
                    .take(feed_area.height.saturating_sub(2) as usize)
                    .rev()
                    .map(|(s, level)| match level {
                        2 => Line::styled(s.clone(), Style::default().fg(Color::Yellow)),
                        1 => Line::styled(s.clone(), Style::default().fg(Color::Magenta)),
                        _ => Line::raw(s.clone()),
                    })
                    .collect();
                f.render_widget(
                    Paragraph::new(lines).block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title("anomalies + verdicts"),
                    ),
                    feed_area,
                );
            })?;

            if crossterm::event::poll(Duration::from_millis(100))?
                && let Event::Key(k) = crossterm::event::read()?
                && (k.code == KeyCode::Char('q') || k.code == KeyCode::Esc)
            {
                return Ok(());
            }
        }
    })();

    ratatui::restore();
    match engine.finish() {
        Ok(path) => println!("report: {}", path.display()),
        Err(e) => eprintln!("report write failed: {e}"),
    }
    result
}

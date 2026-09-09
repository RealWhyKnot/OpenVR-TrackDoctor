use crate::engine::{Engine, Msg};
use crate::event::now_ms;
use crossterm::event::{Event, KeyCode};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph, Row, Table};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

pub fn run(mut engine: Engine, rx: Receiver<Msg>, audit: Vec<String>, session_dir: PathBuf) -> anyhow::Result<()> {
    let mut terminal = ratatui::init();
    let mut status = String::from("starting collectors");
    let mut feed: VecDeque<(String, bool)> = VecDeque::new();
    for a in &audit {
        feed.push_back((a.clone(), a.starts_with("WARNING")));
    }

    let result = (|| -> anyhow::Result<()> {
        loop {
            loop {
                match rx.try_recv() {
                    Ok(Msg::Status(s)) => status = s,
                    Ok(msg) => {
                        engine.handle(msg);
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return Ok(()),
                }
            }
            for v in engine.tick(now_ms()) {
                feed.push_back((format!("{} -> {:?} ({:?})", v.device, v.cause, v.confidence), true));
                feed.push_back((format!("  {}", v.cause.describe()), false));
                for e in v.evidence.iter().take(6) {
                    feed.push_back((format!("  {e}"), false));
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
                    let (state, valid) = engine
                        .last_state
                        .get(idx)
                        .cloned()
                        .unwrap_or_else(|| ("?".into(), false));
                    let style = match state.as_str() {
                        "RunningOk" => Style::default().fg(Color::Green),
                        "?" | "Uninitialized" => Style::default().fg(Color::DarkGray),
                        _ => Style::default().fg(Color::Red),
                    };
                    Row::new(vec![
                        m.serial.clone(),
                        m.class.clone(),
                        m.model.clone(),
                        state,
                        if valid { "yes".into() } else { "no".into() },
                        m.dongle.clone(),
                        m.battery_pct.map(|b| format!("{b:.0}%")).unwrap_or_default(),
                    ])
                    .style(style)
                })
                .collect();

            terminal.draw(|f| {
                let [header, table_area, feed_area] = Layout::vertical([
                    Constraint::Length(1),
                    Constraint::Length(rows.len() as u16 + 3),
                    Constraint::Min(5),
                ])
                .areas(f.area());

                f.render_widget(
                    Paragraph::new(format!("trackdoctor | {status} | session {} | q quits", session_dir.display())),
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
                        ],
                    )
                    .header(Row::new(vec!["serial", "class", "model", "state", "valid", "dongle", "batt"]).style(Style::default().fg(Color::Cyan)))
                    .block(Block::default().borders(Borders::ALL).title("devices")),
                    table_area,
                );
                let lines: Vec<Line> = feed
                    .iter()
                    .rev()
                    .take(feed_area.height.saturating_sub(2) as usize)
                    .rev()
                    .map(|(s, hot)| {
                        if *hot {
                            Line::styled(s.clone(), Style::default().fg(Color::Yellow))
                        } else {
                            Line::raw(s.clone())
                        }
                    })
                    .collect();
                f.render_widget(
                    Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title("verdicts + warnings")),
                    feed_area,
                );
            })?;

            if crossterm::event::poll(Duration::from_millis(100))?
                && let Event::Key(k) = crossterm::event::read()?
                    && (k.code == KeyCode::Char('q') || k.code == KeyCode::Esc) {
                        return Ok(());
                    }
        }
    })();

    ratatui::restore();
    let path = engine.session.finish()?;
    println!("report: {}", path.display());
    result
}

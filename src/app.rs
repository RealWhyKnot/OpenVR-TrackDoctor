use crate::engine::{Engine, Msg};
use crate::event::now_ms;
use crate::names::{Names, data_dir};
use crate::report::{self, SessionWriter};
use crate::signals::{self, usb};
use crate::summary::{self, DeviceRecord, TreeCtx};
use crate::win;
use anyhow::anyhow;
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::Duration;

pub fn spawn_collectors(tx: Sender<Msg>) {
    let vr_tx = tx.clone();
    std::thread::spawn(move || signals::openvr::run(vr_tx));

    let log_tx = tx.clone();
    std::thread::spawn(move || {
        let mut tail = signals::logtail::LogTail::new(signals::logtail::vrserver_log_path());
        loop {
            for ev in tail.poll() {
                if log_tx.send(Msg::Event(ev)).is_err() {
                    return;
                }
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    });

    std::thread::spawn(move || {
        let watch = usb::UsbWatch::new();
        if tx.send(Msg::Usb(usb::snapshot())).is_err() {
            return;
        }
        let mut watch = match watch {
            Ok(w) => w,
            Err(e) => {
                let _ = tx.send(Msg::Status(format!("USB watcher unavailable: {e}")));
                return;
            }
        };
        loop {
            let events = watch.poll();
            let changed = !events.is_empty();
            for ev in events {
                if tx.send(Msg::Event(ev)).is_err() {
                    return;
                }
            }
            if changed && tx.send(Msg::Usb(usb::snapshot())).is_err() {
                return;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    });
}

fn lock_path() -> PathBuf {
    data_dir().join("recorder.lock")
}

#[cfg(windows)]
fn try_lock() -> Option<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    let _ = std::fs::create_dir_all(data_dir());
    std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .share_mode(0)
        .open(lock_path())
        .ok()
}

#[cfg(not(windows))]
fn try_lock() -> Option<std::fs::File> {
    let _ = std::fs::create_dir_all(data_dir());
    std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock_path())
        .ok()
}

pub fn recorder_running() -> bool {
    lock_path().exists() && try_lock().is_none()
}

fn wind_down(engine: &mut Engine) -> anyhow::Result<Option<PathBuf>> {
    let now = now_ms();
    engine.tick(now + 3_000);
    let path = if engine.session.discard_if_empty() {
        Ok(None)
    } else {
        engine.finish().map(Some)
    };
    win::finished();
    path
}

pub fn background() -> anyhow::Result<()> {
    let Some(_lock) = try_lock() else {
        return Ok(());
    };
    win::install_handlers();
    let mut engine = Engine::new(SessionWriter::new(false)?, Names::load_default());
    let (tx, rx) = mpsc::channel();
    spawn_collectors(tx);
    let mut next_tick = 0;
    let mut next_names = 0;
    while !win::stop_requested() {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(Msg::SteamVrExited) => break,
            Ok(Msg::Status(_)) => {}
            Ok(msg) => {
                engine.handle(msg);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        let now = now_ms();
        if now >= next_tick {
            next_tick = now + 250;
            engine.tick(now);
            engine.snapshot(now);
            engine.notices.clear();
        }
        if now >= next_names {
            next_names = now + 10_000;
            engine.names.refresh();
        }
    }
    wind_down(&mut engine).map(|_| ())
}

pub fn live(poses: bool) -> anyhow::Result<()> {
    win::install_handlers();
    crate::update::spawn_check();
    let engine = Engine::new(SessionWriter::new(poses)?, Names::load_default());
    let (tx, rx) = mpsc::channel();
    spawn_collectors(tx);
    let note = recorder_running().then(|| {
        "The background recorder is also running (it starts with SteamVR), so this session is recorded twice.".to_string()
    });
    let mut engine = crate::tui::run(engine, rx, note)?;
    let path = wind_down(&mut engine);
    if win::closing() {
        return Ok(());
    }
    match path {
        Ok(Some(p)) => {
            print!("{}", summary::render(&engine.summary()));
            println!("\nFull report: {}", p.display());
        }
        Ok(None) => println!("Nothing was recorded: SteamVR never connected."),
        Err(e) => eprintln!("Could not write the report: {e}"),
    }
    if let Some(release) = crate::update::found() {
        crate::update::offer(&release, true);
    }
    win::pause_if_own_console();
    Ok(())
}

pub fn dump(poses: bool) -> anyhow::Result<()> {
    win::install_handlers();
    let mut engine = Engine::new(SessionWriter::new(poses)?, Names::load_default());
    println!("session dir: {}", engine.session.dir().display());
    let (tx, rx) = mpsc::channel();
    spawn_collectors(tx);
    let mut next_tick = 0;
    while !win::stop_requested() {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(Msg::Status(s)) => println!("status: {s}"),
            Ok(Msg::SteamVrExited) => println!("status: SteamVR exited"),
            Ok(Msg::Device(d)) => {
                println!(
                    "device {}: {} {} class={:?} dongle={} battery={:?} lighthouse={}",
                    d.idx, d.serial, d.model, d.class, d.dongle, d.battery_pct, d.is_lighthouse
                );
                engine.handle(Msg::Device(d));
            }
            Ok(msg) => {
                for ev in engine.handle(msg) {
                    println!("{}", serde_json::to_string(&ev)?);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        for n in std::mem::take(&mut engine.notices) {
            println!("{n}");
        }
        let now = now_ms();
        if now >= next_tick {
            next_tick = now + 100;
            for v in engine.tick(now) {
                println!("VERDICT {}", serde_json::to_string(&v)?);
            }
            engine.snapshot(now);
        }
    }
    match wind_down(&mut engine)? {
        Some(path) => println!("report: {}", path.display()),
        None => println!("nothing recorded; empty session removed"),
    }
    Ok(())
}

pub fn report(arg: Option<&str>, full: bool, log: Option<&str>) -> anyhow::Result<()> {
    let dir = match arg {
        Some(p) => {
            let p = PathBuf::from(p);
            if p.is_file() {
                p.parent().map(PathBuf::from).unwrap_or(p)
            } else {
                p
            }
        }
        None => report::latest_session().ok_or_else(|| {
            anyhow!(
                "no sessions recorded yet in {}",
                report::sessions_dir().display()
            )
        })?,
    };
    let r = report::replay(&dir, Names::load_default(), log.map(std::path::Path::new))?;
    let text = r.engine.report_text();
    let shown = if full {
        text.as_str()
    } else {
        text.split("\n== Devices seen ==").next().unwrap_or(&text)
    };
    print!("{shown}");
    let mut notes = vec![format!(
        "Re-analyzed {} events from {}",
        r.events,
        dir.display()
    )];
    if r.skipped > 0 {
        notes.push(format!(
            "{} lines were from an older format and skipped",
            r.skipped
        ));
    }
    if let Some(n) = r.log_events {
        notes.push(format!("{n} SteamVR log events re-read from the given log"));
    }
    println!("\n{}.", notes.join("; "));
    if !full {
        println!(
            "Every incident with its evidence: trackdoctor report --full \"{}\"",
            dir.display()
        );
    }
    win::pause_if_own_console();
    Ok(())
}

pub fn usb_layout() -> anyhow::Result<()> {
    let dongles = usb::snapshot();
    let devices: Vec<DeviceRecord> = signals::openvr::list_devices()
        .unwrap_or_default()
        .into_iter()
        .map(|m| DeviceRecord {
            serial: m.serial,
            model: m.model,
            class: m.class,
            dongle: m.dongle,
            lighthouse: m.is_lighthouse,
        })
        .collect();
    let names = Names::load_default();
    let radio = Default::default();
    let ctx = TreeCtx {
        devices: &devices,
        summary: None,
        radio: &radio,
        names: &names,
    };
    print!(
        "{}",
        summary::render_tree(&summary::usb_tree(&dongles, &ctx))
    );
    for w in usb::crowding(&dongles) {
        println!("{w}");
    }
    if devices.is_empty() {
        println!("\nStart SteamVR to see which tracker uses each dongle.");
    }
    win::pause_if_own_console();
    Ok(())
}

pub fn autostart(arg: Option<&str>) -> anyhow::Result<i32> {
    let result = match arg {
        Some("on") => crate::autostart::enable(),
        Some("off") => crate::autostart::disable(),
        Some("status") | None => crate::autostart::status(),
        Some(other) => {
            return Err(anyhow!(
                "unknown autostart option '{other}'; use on, off or status"
            ));
        }
    };
    match result {
        Ok(msg) => {
            println!("{msg}");
            Ok(0)
        }
        Err(e) => {
            eprintln!("{e:#}");
            Ok(2)
        }
    }
}

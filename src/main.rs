mod correlate;
mod detect;
mod engine;
mod event;
mod report;
mod signals;
mod tui;

use engine::{Engine, Msg};
use signals::openvr::VrMsg;
use std::sync::mpsc;
use std::time::Duration;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("report") => {
            let path = args.get(1).map(std::path::PathBuf::from).ok_or_else(|| anyhow::anyhow!("usage: trackdoctor report <verdicts.jsonl>"))?;
            print!("{}", report::render_file(&path)?);
            Ok(())
        }
        Some("dump") => run(true),
        None => run(false),
        Some(other) => Err(anyhow::anyhow!("unknown command '{other}'; usage: trackdoctor [dump | report <verdicts.jsonl>]")),
    }
}

fn spawn_collectors(tx: mpsc::Sender<Msg>) -> Vec<String> {
    let vr_tx = tx.clone();
    std::thread::spawn(move || {
        let (itx, irx) = mpsc::channel();
        std::thread::spawn(move || signals::openvr::run(itx));
        for m in irx {
            let mapped = match m {
                VrMsg::Event(e) => Msg::Event(e),
                VrMsg::Frame(f) => Msg::Frame(f),
                VrMsg::Device(d) => Msg::Device(d),
                VrMsg::Status(s) => Msg::Status(s),
            };
            if vr_tx.send(mapped).is_err() {
                return;
            }
        }
    });

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

    match signals::usb::UsbWatch::new() {
        Ok((mut watch, audit)) => {
            let usb_tx = tx;
            std::thread::spawn(move || loop {
                for ev in watch.poll() {
                    if usb_tx.send(Msg::Event(ev)).is_err() {
                        return;
                    }
                }
                std::thread::sleep(Duration::from_millis(500));
            });
            audit
        }
        Err(e) => vec![format!("usb watcher unavailable: {e}")],
    }
}

fn run(dump: bool) -> anyhow::Result<()> {
    let session = report::SessionWriter::new()?;
    let session_dir = session.dir().to_path_buf();
    let mut engine = Engine::new(session);
    let (tx, rx) = mpsc::channel();
    let audit = spawn_collectors(tx);

    if dump {
        for line in &audit {
            println!("audit: {line}");
        }
        println!("session dir: {}", session_dir.display());
        loop {
            match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(Msg::Status(s)) => println!("status: {s}"),
                Ok(Msg::Device(d)) => {
                    println!(
                        "device {}: {} {} class={} dongle={} battery={:?} lighthouse={}",
                        d.idx, d.serial, d.model, d.class, d.dongle, d.battery_pct, d.is_lighthouse
                    );
                    engine.handle(Msg::Device(d));
                }
                Ok(msg) => {
                    for ev in engine.handle(msg) {
                        println!("{}", serde_json::to_string(&ev)?);
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            for v in engine.tick(event::now_ms()) {
                println!("VERDICT {}", serde_json::to_string(&v)?);
            }
        }
        let path = engine.session.finish()?;
        println!("report: {}", path.display());
        Ok(())
    } else {
        tui::run(engine, rx, audit, session_dir)
    }
}

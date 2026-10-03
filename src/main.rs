mod correlate;
mod detect;
mod engine;
mod event;
mod report;
mod signals;
mod tui;

use engine::{Engine, Msg};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

fn main() -> anyhow::Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let poses = args.iter().any(|a| a == "--poses");
    args.retain(|a| a != "--poses");
    match args.first().map(String::as_str) {
        Some("report") => {
            let path = args
                .get(1)
                .map(std::path::PathBuf::from)
                .ok_or_else(|| anyhow::anyhow!("usage: trackdoctor report <verdicts.jsonl>"))?;
            print!("{}", report::render_file(&path)?);
            Ok(())
        }
        Some("dump") => run(true, poses),
        None => run(false, poses),
        Some(other) => Err(anyhow::anyhow!(
            "unknown command '{other}'; usage: trackdoctor [dump] [--poses] | report <verdicts.jsonl>"
        )),
    }
}

fn spawn_collectors(tx: mpsc::Sender<Msg>) -> Vec<String> {
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

    match signals::usb::UsbWatch::new() {
        Ok((mut watch, audit)) => {
            let usb_tx = tx;
            std::thread::spawn(move || {
                loop {
                    for ev in watch.poll() {
                        if usb_tx.send(Msg::Event(ev)).is_err() {
                            return;
                        }
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
            });
            audit
        }
        Err(e) => vec![format!("usb watcher unavailable: {e}")],
    }
}

fn shutdown_flag() -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    let f = flag.clone();
    let _ = ctrlc::set_handler(move || f.store(true, Ordering::SeqCst));
    flag
}

fn run(dump: bool, poses: bool) -> anyhow::Result<()> {
    let session = report::SessionWriter::new(poses)?;
    let session_dir = session.dir().to_path_buf();
    let mut engine = Engine::new(session);
    let (tx, rx) = mpsc::channel();
    let audit = spawn_collectors(tx);
    let stop = shutdown_flag();

    if dump {
        for line in &audit {
            println!("audit: {line}");
        }
        println!("session dir: {}", session_dir.display());
        while !stop.load(Ordering::SeqCst) {
            match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(Msg::Status(s)) => println!("status: {s}"),
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
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            for v in engine.tick(event::now_ms()) {
                println!("VERDICT {}", serde_json::to_string(&v)?);
            }
        }
        let path = engine.finish()?;
        println!("report: {}", path.display());
        Ok(())
    } else {
        tui::run(engine, rx, audit, session_dir, stop)
    }
}

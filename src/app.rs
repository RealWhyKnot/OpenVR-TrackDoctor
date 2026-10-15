use crate::bases;
use crate::engine::{Engine, Msg};
use crate::event::now_ms;
use crate::names::data_dir;
use crate::signals::{self, usb};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::time::Duration;

pub fn spawn(name: &str, f: impl FnOnce() + Send + 'static) {
    if let Err(e) = std::thread::Builder::new().name(name.into()).spawn(f) {
        crate::dlog!("couldn't start the {name} thread: {e}");
    }
}

pub fn spawn_collectors(tx: Sender<Msg>) {
    let gate = Arc::new(signals::packets::Gate::default());
    let vr_tx = tx.clone();
    let vr_gate = gate.clone();
    spawn("openvr", move || signals::openvr::run(vr_tx, vr_gate));

    if crate::DEV {
        let cap_tx = tx.clone();
        spawn("packets", move || signals::packets::run(cap_tx, gate));
    }

    let db_tx = tx.clone();
    spawn("lighthouse-db", move || {
        let Some(path) = signals::logtail::lighthouse_db_path() else {
            return;
        };
        let mut last = None;
        loop {
            let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
            if modified.is_some() && modified != last {
                last = modified;
                if db_tx.send(Msg::BaseDb(bases::read_db(&path))).is_err() {
                    return;
                }
            }
            std::thread::sleep(Duration::from_secs(10));
        }
    });

    let log_tx = tx.clone();
    spawn("vrserver-log", move || {
        let mut tail = signals::logtail::LogTail::new(signals::logtail::vrserver_log_path());
        if crate::DEV {
            tail.raw = Some(Vec::new());
        }
        loop {
            let events = tail.poll();
            if let Some(raw) = tail.raw.as_mut()
                && !raw.is_empty()
                && log_tx.send(Msg::RawLog(std::mem::take(raw))).is_err()
            {
                return;
            }
            for ev in events {
                if ev.kind == crate::event::Kind::LogRotated {
                    crate::dlog!("vrserver log: SteamVR started a new run ({})", ev.detail);
                }
                if log_tx.send(Msg::Event(ev)).is_err() {
                    return;
                }
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    });

    spawn("usb", move || {
        let watch = usb::UsbWatch::new();
        if tx.send(Msg::Usb(usb::snapshot())).is_err() {
            return;
        }
        let mut watch = match watch {
            Ok(w) => w,
            Err(e) => {
                let _ = tx.send(Msg::Status(format!(
                    "waiting for SteamVR: the USB check couldn't start ({e})"
                )));
                return;
            }
        };
        loop {
            let events = watch.poll();
            let changed = !events.is_empty();
            for ev in &events {
                crate::dlog!("usb: {:?} {}", ev.kind, ev.detail);
            }
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
pub fn try_lock() -> Option<std::fs::File> {
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
pub fn try_lock() -> Option<std::fs::File> {
    let _ = std::fs::create_dir_all(data_dir());
    std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock_path())
        .ok()
}

pub fn wind_down(engine: &mut Engine) -> anyhow::Result<Option<PathBuf>> {
    let now = now_ms();
    engine.tick(now + 3_000);
    let dir = engine.session.dir().to_path_buf();
    if engine.session.discard_if_empty() {
        crate::dlog!("recorder: nothing recorded, dropped {}", dir.display());
        return Ok(None);
    }
    let saved = engine.finish();
    engine.session.close();
    if saved.is_ok() {
        let packed = crate::store::pack(&dir);
        crate::dlog!(
            "recorder: saved {} ({}), packing {}",
            dir.display(),
            crate::store::fmt_size(crate::store::size(&dir)),
            match &packed {
                Ok(()) => "worked".to_string(),
                Err(e) => format!("failed: {e}"),
            }
        );
    } else if let Err(e) = &saved {
        crate::dlog!("recorder: saving {} failed: {e}", dir.display());
    }
    saved.map(Some)
}

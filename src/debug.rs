use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

pub const MAX_BYTES: u64 = 4 * 1024 * 1024;

struct Log {
    path: PathBuf,
    file: std::fs::File,
    bytes: u64,
}

static LOG: Mutex<Option<Log>> = Mutex::new(None);
static ON: AtomicBool = AtomicBool::new(false);

#[macro_export]
macro_rules! dlog {
    ($($arg:tt)*) => {
        if $crate::DEV {
            $crate::debug::write(format_args!($($arg)*));
        }
    };
}

pub fn path() -> PathBuf {
    crate::names::data_dir().join("debug.log")
}

pub fn old_path(path: &Path) -> PathBuf {
    path.with_file_name("debug.old.log")
}

pub fn init() {
    if !crate::DEV || cfg!(test) {
        return;
    }
    ON.store(true, Ordering::Relaxed);
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let trace = std::backtrace::Backtrace::force_capture();
        write(format_args!("panic: {info}\n{trace}"));
        previous(info);
    }));
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    write(format_args!(
        "TrackDoctor {} started as a dev build from {exe}",
        crate::VERSION
    ));
}

pub fn write(args: fmt::Arguments) {
    if !ON.load(Ordering::Relaxed) {
        return;
    }
    let line = line(&crate::win::local_clock(), args);
    let Ok(mut guard) = LOG.lock() else {
        return;
    };
    if guard.is_none() {
        *guard = open(path());
    }
    if let Some(log) = guard.as_mut() {
        append(log, &line);
    }
}

fn line(clock: &str, args: fmt::Arguments) -> String {
    let thread = std::thread::current();
    let name = thread.name().unwrap_or("thread");
    format!("{clock} [{name}] {args}\n")
}

fn open(path: PathBuf) -> Option<Log> {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok()?;
    let bytes = file.metadata().map(|m| m.len()).unwrap_or(0);
    Some(Log { path, file, bytes })
}

fn append(log: &mut Log, line: &str) {
    if log.bytes > 0 && log.bytes + line.len() as u64 > MAX_BYTES {
        let old = old_path(&log.path);
        let _ = std::fs::remove_file(&old);
        if std::fs::rename(&log.path, &old).is_ok()
            && let Some(fresh) = open(log.path.clone())
        {
            *log = fresh;
        }
    }
    if log.file.write_all(line.as_bytes()).is_ok() {
        log.bytes += line.len() as u64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("trackdoctor-debug-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn lines_carry_clock_thread_and_text() {
        let l = line("2026-10-15 07:00:00.000", format_args!("opened {}", 3));
        assert!(l.starts_with("2026-10-15 07:00:00.000 ["));
        assert!(l.ends_with("] opened 3\n"));
    }

    #[test]
    fn a_full_log_rolls_over_to_the_old_file() {
        let dir = scratch("roll");
        let path = dir.join("debug.log");
        let mut log = open(path.clone()).unwrap();
        let big = "x".repeat(1024) + "\n";
        let lines = MAX_BYTES as usize / big.len() + 1;
        for _ in 0..lines {
            append(&mut log, &big);
        }
        append(&mut log, "last\n");
        let old = std::fs::metadata(old_path(&path)).unwrap().len();
        let now = std::fs::read_to_string(&path).unwrap();
        assert!(old <= MAX_BYTES && old > MAX_BYTES / 2);
        assert!(now.ends_with("last\n") && (now.len() as u64) < MAX_BYTES);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

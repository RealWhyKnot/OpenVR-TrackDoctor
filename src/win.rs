use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

static STOP: AtomicBool = AtomicBool::new(false);
static CLOSING: AtomicBool = AtomicBool::new(false);
static DONE: AtomicBool = AtomicBool::new(false);

pub fn stop_requested() -> bool {
    STOP.load(Ordering::SeqCst)
}

pub fn request_stop() {
    STOP.store(true, Ordering::SeqCst);
}

pub fn closing() -> bool {
    CLOSING.load(Ordering::SeqCst)
}

pub fn finished() {
    DONE.store(true, Ordering::SeqCst);
}

const CLOSE_WAIT: Duration = Duration::from_millis(4500);

#[cfg(windows)]
mod ffi {
    unsafe extern "system" {
        pub fn SetConsoleCtrlHandler(
            handler: Option<unsafe extern "system" fn(u32) -> i32>,
            add: i32,
        ) -> i32;
        pub fn GetConsoleProcessList(list: *mut u32, count: u32) -> u32;
    }
}

#[cfg(windows)]
unsafe extern "system" fn on_ctrl(kind: u32) -> i32 {
    STOP.store(true, Ordering::SeqCst);
    if kind >= 2 {
        CLOSING.store(true, Ordering::SeqCst);
        let start = Instant::now();
        while !DONE.load(Ordering::SeqCst) && start.elapsed() < CLOSE_WAIT {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    1
}

#[cfg(windows)]
pub fn install_handlers() {
    unsafe {
        ffi::SetConsoleCtrlHandler(Some(on_ctrl), 1);
    }
}

#[cfg(not(windows))]
pub fn install_handlers() {
    let _ = (Instant::now(), CLOSE_WAIT);
}

#[cfg(windows)]
pub fn own_console() -> bool {
    let mut pids = [0u32; 4];
    let n = unsafe { ffi::GetConsoleProcessList(pids.as_mut_ptr(), pids.len() as u32) };
    n == 1
}

#[cfg(not(windows))]
pub fn own_console() -> bool {
    false
}

pub fn pause_if_own_console() {
    if closing() || !own_console() {
        return;
    }
    println!("\nPress Enter to close this window.");
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
}

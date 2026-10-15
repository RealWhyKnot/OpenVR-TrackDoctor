#[cfg(windows)]
mod ffi {
    #[repr(C)]
    #[derive(Default)]
    pub struct SystemTime {
        pub year: u16,
        pub month: u16,
        pub day_of_week: u16,
        pub day: u16,
        pub hour: u16,
        pub minute: u16,
        pub second: u16,
        pub ms: u16,
    }

    #[repr(C)]
    pub struct FileTime {
        pub low: u32,
        pub high: u32,
    }

    unsafe extern "system" {
        pub fn GetLocalTime(time: *mut SystemTime);
        pub fn FileTimeToSystemTime(file: *const FileTime, out: *mut SystemTime) -> i32;
        pub fn SystemTimeToTzSpecificLocalTime(
            zone: *const core::ffi::c_void,
            utc: *const SystemTime,
            local: *mut SystemTime,
        ) -> i32;
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        pub fn FindWindowW(class: *const u16, title: *const u16) -> isize;
        pub fn ShowWindow(window: isize, cmd: i32) -> i32;
        pub fn SetForegroundWindow(window: isize) -> i32;
        pub fn IsIconic(window: isize) -> i32;
    }
}

#[cfg(windows)]
pub fn local_stamp() -> String {
    let mut t = ffi::SystemTime::default();
    unsafe { ffi::GetLocalTime(&mut t) };
    format!(
        "{:04}-{:02}-{:02}-{:02}{:02}",
        t.year, t.month, t.day, t.hour, t.minute
    )
}

#[cfg(not(windows))]
pub fn local_stamp() -> String {
    crate::event::now_ms().to_string()
}

#[cfg(windows)]
pub fn local_clock() -> String {
    let mut t = ffi::SystemTime::default();
    unsafe { ffi::GetLocalTime(&mut t) };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        t.year, t.month, t.day, t.hour, t.minute, t.second, t.ms
    )
}

#[cfg(not(windows))]
pub fn local_clock() -> String {
    crate::event::now_ms().to_string()
}

#[cfg(windows)]
pub fn local_time(unix_ms: u64) -> Option<(u16, u16, u16, u16, u16)> {
    let ticks = unix_ms * 10_000 + 116_444_736_000_000_000;
    let file = ffi::FileTime {
        low: ticks as u32,
        high: (ticks >> 32) as u32,
    };
    let mut utc = ffi::SystemTime::default();
    let mut local = ffi::SystemTime::default();
    unsafe {
        if ffi::FileTimeToSystemTime(&file, &mut utc) == 0
            || ffi::SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) == 0
        {
            return None;
        }
    }
    Some((local.year, local.month, local.day, local.hour, local.minute))
}

#[cfg(not(windows))]
pub fn local_time(unix_ms: u64) -> Option<(u16, u16, u16, u16, u16)> {
    let mins = unix_ms / 60_000;
    Some((1970, 1, 1, ((mins / 60) % 24) as u16, (mins % 60) as u16))
}

#[cfg(windows)]
pub fn focus_existing(title: &str) -> bool {
    let title: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let window = ffi::FindWindowW(std::ptr::null(), title.as_ptr());
        if window == 0 {
            return false;
        }
        if ffi::IsIconic(window) != 0 {
            ffi::ShowWindow(window, 9);
        }
        ffi::SetForegroundWindow(window);
    }
    true
}

#[cfg(not(windows))]
pub fn focus_existing(_title: &str) -> bool {
    false
}

pub fn open_folder(path: &std::path::Path) {
    let _ = crate::update::system_tool("..\\explorer.exe")
        .arg(path)
        .spawn();
}

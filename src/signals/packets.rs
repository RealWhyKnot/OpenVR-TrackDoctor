use crate::engine::Msg;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

pub const SLOT: usize = 29;
pub const CLOCK_UNIT_US: f64 = 65_536.0 / 48.0;
pub const GAP_MS: u32 = 20;
pub const AIR_GAP_UNITS: u16 = 4;
pub const CLOCK_BUCKETS: usize = 8;
pub const WORST_KEPT: usize = 20;
const CLOCK_SLACK_US: u64 = 50_000;
const WRAP_SAFE_US: u64 = 80_000_000;

pub const IMU: u8 = 0x80;
pub const META: u8 = 0x40;
pub const INPUT: u8 = 0x20;
pub const LIGHT: u8 = 0x10;
pub const HEARTBEAT: u8 = 0xE2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slot {
    pub clock: u16,
    pub flags: Option<u8>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    pub slots: Vec<Slot>,
    pub malformed: u32,
    pub off: bool,
}

pub fn parse(report: &[u8]) -> Parsed {
    let mut out = Parsed::default();
    let Some((&id, body)) = report.split_first() else {
        return out;
    };
    let count = match id {
        0x23 => 1,
        0x24 => 2,
        0x26 => {
            out.off = true;
            return out;
        }
        _ => return out,
    };
    for k in 0..count {
        let Some(s) = body.get(k * SLOT..(k + 1) * SLOT) else {
            out.malformed += 1;
            continue;
        };
        let size = s[1] as usize;
        if size == 0 {
            continue;
        }
        if size + 2 > SLOT {
            out.malformed += 1;
            continue;
        }
        out.slots.push(Slot {
            clock: u16::from_be_bytes([s[0], s[2]]),
            flags: (size >= 2).then_some(s[3]),
        });
    }
    out
}

pub fn clock_bucket(units: u16) -> usize {
    match units {
        0..=3 => units as usize,
        4..=7 => 4,
        8..=15 => 5,
        16..=63 => 6,
        _ => 7,
    }
}

pub fn units_us(units: u16) -> u64 {
    (units as f64 * CLOCK_UNIT_US).round() as u64
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Second {
    pub t_ms: u64,
    pub dongle: String,
    pub reports: u32,
    pub doubles: u32,
    pub packets: u32,
    pub imu: u32,
    pub light: u32,
    pub input: u32,
    pub meta: u32,
    pub heartbeats: u32,
    pub malformed: u32,
    pub off: u32,
    pub odd: u32,
    pub host_gaps: u32,
    pub host_gap_ms: u32,
    pub host_gap_max_ms: u32,
    pub air_gaps: u32,
    pub air_ms: u32,
    pub clock: [u32; CLOCK_BUCKETS],
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Gap {
    pub t_ms: u64,
    pub dongle: String,
    pub host_ms: u32,
    pub air_ms: Option<u32>,
    pub before: Option<u8>,
    pub after: Option<u8>,
}

impl Gap {
    pub fn worst_ms(&self) -> u32 {
        self.host_ms.max(self.air_ms.unwrap_or(0))
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Line {
    Second(Second),
    Gap(Gap),
    Note {
        t_ms: u64,
        dongle: String,
        text: String,
    },
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Batch {
    pub lines: Vec<Line>,
}

impl Batch {
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub fn dongles_heard(&self) -> usize {
        let mut seen: Vec<&str> = self
            .lines
            .iter()
            .filter_map(|l| match l {
                Line::Second(s) if s.packets > 0 => Some(s.dongle.as_str()),
                _ => None,
            })
            .collect();
        seen.sort_unstable();
        seen.dedup();
        seen.len()
    }
}

pub struct Link {
    pub dongle: String,
    last_host_us: Option<u64>,
    last_clock: Option<u16>,
    last_flags: Option<u8>,
    sec: Second,
    gaps: Vec<Gap>,
}

impl Link {
    pub fn new(dongle: &str) -> Self {
        Self {
            dongle: dongle.to_string(),
            last_host_us: None,
            last_clock: None,
            last_flags: None,
            sec: Second {
                dongle: dongle.to_string(),
                ..Second::default()
            },
            gaps: Vec::new(),
        }
    }

    pub fn report(&mut self, host_us: u64, wall_ms: u64, bytes: &[u8]) {
        let p = parse(bytes);
        let s = &mut self.sec;
        s.reports += 1;
        s.malformed += p.malformed;
        if p.off {
            s.off += 1;
            self.last_host_us = None;
            self.last_clock = None;
            self.last_flags = None;
            return;
        }
        if p.slots.is_empty() {
            return;
        }
        if p.slots.len() == 2 {
            s.doubles += 1;
        }
        let host_gap_us = self.last_host_us.map(|prev| host_us.saturating_sub(prev));
        self.last_host_us = Some(host_us);
        let host_ms = host_gap_us.map_or(0, |us| (us / 1000) as u32);
        if host_ms >= GAP_MS {
            s.host_gaps += 1;
            s.host_gap_ms += host_ms;
            s.host_gap_max_ms = s.host_gap_max_ms.max(host_ms);
        }
        for (k, slot) in p.slots.iter().enumerate() {
            s.packets += 1;
            match slot.flags {
                Some(HEARTBEAT) => s.heartbeats += 1,
                Some(f) => {
                    s.imu += u32::from(f & IMU != 0);
                    s.meta += u32::from(f & META != 0);
                    s.input += u32::from(f & INPUT != 0);
                    s.light += u32::from(f & LIGHT != 0);
                }
                None => {}
            }
            let slot_host_us = if k == 0 { host_gap_us } else { Some(0) };
            let air_ms = match (self.last_clock, slot_host_us) {
                (Some(prev), Some(hus)) if hus < WRAP_SAFE_US => {
                    let units = slot.clock.wrapping_sub(prev);
                    let aus = units_us(units);
                    if aus > hus + CLOCK_SLACK_US {
                        s.odd += 1;
                        None
                    } else {
                        s.clock[clock_bucket(units)] += 1;
                        if units >= AIR_GAP_UNITS {
                            s.air_gaps += 1;
                            s.air_ms += (aus / 1000) as u32;
                        }
                        Some((aus / 1000) as u32)
                    }
                }
                _ => None,
            };
            let this_host_ms = if k == 0 { host_ms } else { 0 };
            if this_host_ms >= GAP_MS || air_ms.is_some_and(|a| a >= GAP_MS) {
                self.gaps.push(Gap {
                    t_ms: wall_ms,
                    dongle: self.dongle.clone(),
                    host_ms: this_host_ms,
                    air_ms,
                    before: self.last_flags,
                    after: slot.flags,
                });
            }
            self.last_clock = Some(slot.clock);
            self.last_flags = slot.flags;
        }
    }

    pub fn take(&mut self, wall_ms: u64, out: &mut Vec<Line>) {
        out.extend(self.gaps.drain(..).map(Line::Gap));
        if self.sec.reports == 0 {
            return;
        }
        let fresh = Second {
            dongle: self.dongle.clone(),
            ..Second::default()
        };
        let mut done = std::mem::replace(&mut self.sec, fresh);
        done.t_ms = wall_ms;
        out.push(Line::Second(done));
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Totals {
    pub dongle: String,
    pub device: Option<String>,
    pub first_ms: u64,
    pub last_ms: u64,
    pub seconds: u64,
    pub reports: u64,
    pub doubles: u64,
    pub packets: u64,
    pub imu: u64,
    pub light: u64,
    pub input: u64,
    pub meta: u64,
    pub heartbeats: u64,
    pub malformed: u64,
    pub off: u64,
    pub odd: u64,
    pub host_gaps: u64,
    pub host_gap_ms: u64,
    pub host_gap_max_ms: u32,
    pub air_gaps: u64,
    pub air_ms: u64,
    pub clock: [u64; CLOCK_BUCKETS],
    pub worst: Vec<Gap>,
}

impl Totals {
    pub fn add(&mut self, line: &Line) {
        match line {
            Line::Second(s) => {
                if self.first_ms == 0 {
                    self.first_ms = s.t_ms;
                }
                self.last_ms = self.last_ms.max(s.t_ms);
                self.seconds += u64::from(s.packets > 0);
                self.reports += u64::from(s.reports);
                self.doubles += u64::from(s.doubles);
                self.packets += u64::from(s.packets);
                self.imu += u64::from(s.imu);
                self.light += u64::from(s.light);
                self.input += u64::from(s.input);
                self.meta += u64::from(s.meta);
                self.heartbeats += u64::from(s.heartbeats);
                self.malformed += u64::from(s.malformed);
                self.off += u64::from(s.off);
                self.odd += u64::from(s.odd);
                self.host_gaps += u64::from(s.host_gaps);
                self.host_gap_ms += u64::from(s.host_gap_ms);
                self.host_gap_max_ms = self.host_gap_max_ms.max(s.host_gap_max_ms);
                self.air_gaps += u64::from(s.air_gaps);
                self.air_ms += u64::from(s.air_ms);
                for (t, c) in self.clock.iter_mut().zip(s.clock) {
                    *t += u64::from(c);
                }
            }
            Line::Gap(g) => {
                self.worst.push(g.clone());
                self.worst.sort_by_key(|g| std::cmp::Reverse(g.worst_ms()));
                self.worst.truncate(WORST_KEPT);
            }
            Line::Note { .. } => {}
        }
    }

    pub fn packets_per_s(&self) -> f64 {
        if self.seconds == 0 {
            0.0
        } else {
            self.packets as f64 / self.seconds as f64
        }
    }

    pub fn air_share(&self) -> f64 {
        if self.seconds == 0 {
            0.0
        } else {
            self.air_ms as f64 / (self.seconds as f64 * 1000.0)
        }
    }
}

pub fn dongle_of(line: &Line) -> &str {
    match line {
        Line::Second(s) => &s.dongle,
        Line::Gap(g) => &g.dongle,
        Line::Note { dongle, .. } => dongle,
    }
}

pub fn totals(lines: &[Line]) -> BTreeMap<String, Totals> {
    let mut out: BTreeMap<String, Totals> = BTreeMap::new();
    for l in lines {
        let d = dongle_of(l);
        out.entry(d.to_string())
            .or_insert_with(|| Totals {
                dongle: d.to_string(),
                ..Totals::default()
            })
            .add(l);
    }
    out
}

pub fn parse_lines(text: &str) -> Vec<Line> {
    text.lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

pub fn rollup(t: &Totals) -> String {
    format!(
        "{}: {:.0} packets/s over {} s, {} host gaps (longest {} ms), {} tracker-clock gaps adding up to {} ms, {} heartbeats, {} malformed, {} odd",
        t.dongle,
        t.packets_per_s(),
        t.seconds,
        t.host_gaps,
        t.host_gap_max_ms,
        t.air_gaps,
        t.air_ms,
        t.heartbeats,
        t.malformed,
        t.odd
    )
}

#[derive(Default)]
pub struct Gate {
    live: AtomicBool,
    busy: AtomicBool,
}

impl Gate {
    pub fn open(&self) {
        self.live.store(true, Ordering::SeqCst);
    }

    pub fn close(&self, limit: Duration) {
        self.live.store(false, Ordering::SeqCst);
        let end = Instant::now() + limit;
        while self.busy.load(Ordering::SeqCst) && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    pub fn is_open(&self) -> bool {
        self.live.load(Ordering::SeqCst)
    }

    fn enter(&self) -> bool {
        self.busy.store(true, Ordering::SeqCst);
        self.is_open()
    }

    fn leave(&self) {
        self.busy.store(false, Ordering::SeqCst);
    }
}

#[cfg(windows)]
pub fn run(tx: Sender<Msg>, gate: Arc<Gate>) {
    capture::run(tx, gate)
}

#[cfg(not(windows))]
pub fn run(_tx: Sender<Msg>, _gate: Arc<Gate>) {}

#[cfg(windows)]
pub fn listen(secs: u64, each: impl FnMut(&[Line])) {
    capture::listen(secs, each)
}

#[cfg(not(windows))]
pub fn listen(_secs: u64, _each: impl FnMut(&[Line])) {}

#[cfg(windows)]
mod capture {
    use super::{Batch, Gate, Line, Link, Totals, rollup};
    use crate::engine::Msg;
    use crate::event::now_ms;
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::sync::mpsc::{Receiver, Sender, channel};
    use std::time::{Duration, Instant};

    mod ffi {
        use core::ffi::c_void;

        pub type Handle = *mut c_void;
        pub const INVALID: Handle = -1isize as Handle;
        pub const GENERIC_READ: u32 = 0x8000_0000;
        pub const SHARE_READ_WRITE: u32 = 3;
        pub const OPEN_EXISTING: u32 = 3;
        pub const FLAG_OVERLAPPED: u32 = 0x4000_0000;
        pub const DIGCF_PRESENT: u32 = 0x2;
        pub const DIGCF_DEVICEINTERFACE: u32 = 0x10;
        pub const ERROR_IO_PENDING: i32 = 997;
        pub const ERROR_IO_INCOMPLETE: i32 = 996;
        pub const WAIT_TIMEOUT: u32 = 258;
        pub const WAIT_FAILED: u32 = 0xFFFF_FFFF;

        #[repr(C)]
        #[derive(Default, Clone, Copy)]
        pub struct Guid {
            pub a: u32,
            pub b: u16,
            pub c: u16,
            pub d: [u8; 8],
        }

        #[repr(C)]
        #[derive(Default)]
        pub struct InterfaceData {
            pub size: u32,
            pub class: Guid,
            pub flags: u32,
            pub reserved: usize,
        }

        #[repr(C)]
        pub struct Overlapped {
            pub internal: usize,
            pub internal_high: usize,
            pub offset: u32,
            pub offset_high: u32,
            pub event: Handle,
        }

        #[repr(C)]
        #[derive(Default)]
        pub struct Caps {
            pub usage: u16,
            pub usage_page: u16,
            pub input_len: u16,
            pub output_len: u16,
            pub feature_len: u16,
            pub reserved: [u16; 17],
            pub counts: [u16; 10],
        }

        #[link(name = "hid")]
        unsafe extern "system" {
            pub fn HidD_GetHidGuid(guid: *mut Guid);
            pub fn HidD_GetSerialNumberString(dev: Handle, buf: *mut u16, len: u32) -> u8;
            pub fn HidD_GetPreparsedData(dev: Handle, data: *mut *mut c_void) -> u8;
            pub fn HidD_FreePreparsedData(data: *mut c_void) -> u8;
            pub fn HidD_SetNumInputBuffers(dev: Handle, count: u32) -> u8;
            pub fn HidP_GetCaps(data: *mut c_void, caps: *mut Caps) -> i32;
        }

        #[link(name = "setupapi")]
        unsafe extern "system" {
            pub fn SetupDiGetClassDevsW(
                guid: *const Guid,
                enumerator: *const u16,
                parent: *mut c_void,
                flags: u32,
            ) -> Handle;
            pub fn SetupDiEnumDeviceInterfaces(
                set: Handle,
                info: *mut c_void,
                guid: *const Guid,
                index: u32,
                data: *mut InterfaceData,
            ) -> i32;
            pub fn SetupDiGetDeviceInterfaceDetailW(
                set: Handle,
                data: *const InterfaceData,
                detail: *mut c_void,
                size: u32,
                required: *mut u32,
                info: *mut c_void,
            ) -> i32;
            pub fn SetupDiDestroyDeviceInfoList(set: Handle) -> i32;
        }

        unsafe extern "system" {
            pub fn CreateFileW(
                name: *const u16,
                access: u32,
                share: u32,
                security: *const c_void,
                disposition: u32,
                flags: u32,
                template: Handle,
            ) -> Handle;
            pub fn CloseHandle(h: Handle) -> i32;
            pub fn CreateEventW(
                security: *const c_void,
                manual: i32,
                initial: i32,
                name: *const u16,
            ) -> Handle;
            pub fn ReadFile(
                h: Handle,
                buf: *mut u8,
                len: u32,
                read: *mut u32,
                ov: *mut Overlapped,
            ) -> i32;
            pub fn GetOverlappedResult(
                h: Handle,
                ov: *mut Overlapped,
                count: *mut u32,
                wait: i32,
            ) -> i32;
            pub fn CancelIoEx(h: Handle, ov: *mut Overlapped) -> i32;
            pub fn WaitForMultipleObjects(
                count: u32,
                handles: *const Handle,
                all: i32,
                ms: u32,
            ) -> u32;
        }
    }

    use ffi::*;

    const REPORTS_PER_READ: usize = 8;
    const INPUT_BUFFERS: u32 = 512;
    const MAX_PORTS: usize = 63;
    const SCAN_EVERY: Duration = Duration::from_secs(5);
    const FLUSH_EVERY: Duration = Duration::from_secs(1);
    const ROLLUP_EVERY: Duration = Duration::from_secs(60);

    pub struct Found {
        pub path: String,
        pub wide: Vec<u16>,
    }

    pub fn dongle_paths() -> Vec<Found> {
        let mut out = Vec::new();
        unsafe {
            let mut guid = Guid::default();
            HidD_GetHidGuid(&mut guid);
            let set = SetupDiGetClassDevsW(
                &guid,
                std::ptr::null(),
                std::ptr::null_mut(),
                DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
            );
            if set == INVALID || set.is_null() {
                return out;
            }
            let mut index = 0;
            loop {
                let mut data = InterfaceData {
                    size: std::mem::size_of::<InterfaceData>() as u32,
                    ..InterfaceData::default()
                };
                if SetupDiEnumDeviceInterfaces(set, std::ptr::null_mut(), &guid, index, &mut data)
                    == 0
                {
                    break;
                }
                index += 1;
                let mut need = 0u32;
                SetupDiGetDeviceInterfaceDetailW(
                    set,
                    &data,
                    std::ptr::null_mut(),
                    0,
                    &mut need,
                    std::ptr::null_mut(),
                );
                if need < 8 {
                    continue;
                }
                let mut buf = vec![0u64; need as usize / 8 + 1];
                buf[0] = 8;
                if SetupDiGetDeviceInterfaceDetailW(
                    set,
                    &data,
                    buf.as_mut_ptr().cast(),
                    need,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                ) == 0
                {
                    continue;
                }
                let base = buf.as_ptr().cast::<u8>().add(4).cast::<u16>();
                let max = (need as usize).saturating_sub(4) / 2;
                let wide: Vec<u16> = (0..max)
                    .map(|i| base.add(i).read_unaligned())
                    .take_while(|&c| c != 0)
                    .collect();
                let path = String::from_utf16_lossy(&wide);
                if path.to_ascii_lowercase().contains("vid_28de&pid_2101") {
                    let mut wide = wide;
                    wide.push(0);
                    out.push(Found { path, wide });
                }
            }
            SetupDiDestroyDeviceInfoList(set);
        }
        out
    }

    struct Refusal {
        why: String,
        retry: bool,
    }

    struct Port {
        path: String,
        link: Link,
        handle: Handle,
        event: Handle,
        ov: Box<Overlapped>,
        buf: Vec<u8>,
        report_len: usize,
        pending: bool,
    }

    impl Port {
        fn open(found: &Found) -> Result<Port, Refusal> {
            unsafe {
                let handle = CreateFileW(
                    found.wide.as_ptr(),
                    GENERIC_READ,
                    SHARE_READ_WRITE,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FLAG_OVERLAPPED,
                    std::ptr::null_mut(),
                );
                if handle == INVALID || handle.is_null() {
                    return Err(Refusal {
                        why: format!("open failed ({})", std::io::Error::last_os_error()),
                        retry: true,
                    });
                }
                let mut pre: *mut core::ffi::c_void = std::ptr::null_mut();
                let mut caps = Caps::default();
                let ok = HidD_GetPreparsedData(handle, &mut pre) != 0;
                if ok {
                    HidP_GetCaps(pre, &mut caps);
                    HidD_FreePreparsedData(pre);
                }
                if !ok || caps.usage_page != 0xFF00 || caps.input_len < 2 {
                    CloseHandle(handle);
                    return Err(Refusal {
                        why: format!(
                            "not the radio collection (page {:04X}, input {})",
                            caps.usage_page, caps.input_len
                        ),
                        retry: !ok,
                    });
                }
                let mut sn = [0u16; 64];
                let serial = if HidD_GetSerialNumberString(handle, sn.as_mut_ptr(), 128) != 0 {
                    String::from_utf16_lossy(&sn[..sn.iter().position(|&c| c == 0).unwrap_or(64)])
                } else {
                    String::new()
                };
                HidD_SetNumInputBuffers(handle, INPUT_BUFFERS);
                let event = CreateEventW(std::ptr::null(), 1, 0, std::ptr::null());
                if event.is_null() {
                    CloseHandle(handle);
                    return Err(Refusal {
                        why: "couldn't create an event".into(),
                        retry: true,
                    });
                }
                let report_len = caps.input_len as usize;
                Ok(Port {
                    path: found.path.clone(),
                    link: Link::new(if serial.is_empty() {
                        &found.path
                    } else {
                        &serial
                    }),
                    handle,
                    event,
                    ov: Box::new(Overlapped {
                        internal: 0,
                        internal_high: 0,
                        offset: 0,
                        offset_high: 0,
                        event,
                    }),
                    buf: vec![0; report_len * REPORTS_PER_READ],
                    report_len,
                    pending: false,
                })
            }
        }

        fn arm(&mut self) -> Result<(), String> {
            if self.pending {
                return Ok(());
            }
            unsafe {
                self.ov.internal = 0;
                self.ov.internal_high = 0;
                let ok = ReadFile(
                    self.handle,
                    self.buf.as_mut_ptr(),
                    self.buf.len() as u32,
                    std::ptr::null_mut(),
                    &mut *self.ov,
                );
                let err = std::io::Error::last_os_error();
                if ok == 0 && err.raw_os_error() != Some(ERROR_IO_PENDING) {
                    return Err(format!("read failed ({err})"));
                }
            }
            self.pending = true;
            Ok(())
        }

        fn collect(&mut self, host_us: u64, wall_ms: u64) -> Result<bool, String> {
            if !self.pending {
                return Ok(false);
            }
            let mut n = 0u32;
            let ok = unsafe { GetOverlappedResult(self.handle, &mut *self.ov, &mut n, 0) };
            if ok == 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() == Some(ERROR_IO_INCOMPLETE) {
                    return Ok(false);
                }
                self.pending = false;
                return Err(format!("read failed ({err})"));
            }
            self.pending = false;
            let n = (n as usize).min(self.buf.len());
            for chunk in self.buf[..n].chunks(self.report_len) {
                self.link.report(host_us, wall_ms, chunk);
            }
            Ok(true)
        }
    }

    impl Drop for Port {
        fn drop(&mut self) {
            unsafe {
                if self.pending {
                    CancelIoEx(self.handle, &mut *self.ov);
                    let mut n = 0u32;
                    GetOverlappedResult(self.handle, &mut *self.ov, &mut n, 1);
                }
                CloseHandle(self.event);
                CloseHandle(self.handle);
            }
        }
    }

    struct Capture {
        ports: Vec<Port>,
        refused: BTreeMap<String, Refusal>,
        start: Instant,
        minute: BTreeMap<String, Totals>,
        lines: Vec<Line>,
    }

    impl Capture {
        fn new() -> Self {
            Self {
                ports: Vec::new(),
                refused: BTreeMap::new(),
                start: Instant::now(),
                minute: BTreeMap::new(),
                lines: Vec::new(),
            }
        }

        fn note(&mut self, dongle: &str, text: String) {
            crate::dlog!("packets {dongle}: {text}");
            self.lines.push(Line::Note {
                t_ms: now_ms(),
                dongle: dongle.to_string(),
                text,
            });
        }

        fn open_found(&mut self, found: Vec<Found>) {
            for f in found {
                if self.ports.len() >= MAX_PORTS {
                    break;
                }
                let wrong = self.refused.get(&f.path).is_some_and(|r| !r.retry);
                if wrong || self.ports.iter().any(|p| p.path == f.path) {
                    continue;
                }
                match Port::open(&f) {
                    Ok(p) => {
                        self.refused.remove(&f.path);
                        let dongle = p.link.dongle.clone();
                        self.ports.push(p);
                        self.note(&dongle, "listening (read-only)".into());
                    }
                    Err(r) => {
                        if self.refused.get(&f.path).map(|o| &o.why) != Some(&r.why) {
                            crate::dlog!("packets: skipped {}: {}", f.path, r.why);
                        }
                        self.refused.insert(f.path, r);
                    }
                }
            }
        }

        fn drop_port(&mut self, i: usize, why: String) {
            let mut port = self.ports.remove(i);
            let now = now_ms();
            port.link.take(now, &mut self.lines);
            let dongle = port.link.dongle.clone();
            drop(port);
            self.note(&dongle, format!("stopped listening: {why}"));
        }

        fn close_all(&mut self, why: &str) {
            while !self.ports.is_empty() {
                self.drop_port(self.ports.len() - 1, why.to_string());
            }
            self.refused.clear();
        }

        fn pump(&mut self, wait: Duration) {
            let mut failed = Vec::new();
            for (i, p) in self.ports.iter_mut().enumerate() {
                if let Err(e) = p.arm() {
                    failed.push((i, e));
                }
            }
            for (i, e) in failed.into_iter().rev() {
                self.drop_port(i, e);
            }
            if self.ports.is_empty() {
                std::thread::sleep(wait);
                return;
            }
            let events: Vec<Handle> = self.ports.iter().map(|p| p.event).collect();
            let r = unsafe {
                WaitForMultipleObjects(
                    events.len() as u32,
                    events.as_ptr(),
                    0,
                    wait.as_millis() as u32,
                )
            };
            if r == WAIT_TIMEOUT {
                return;
            }
            if r == WAIT_FAILED {
                let e = std::io::Error::last_os_error();
                self.close_all(&format!("waiting failed ({e})"));
                return;
            }
            let host_us = self.start.elapsed().as_micros() as u64;
            let wall_ms = now_ms();
            let mut failed = Vec::new();
            for (i, p) in self.ports.iter_mut().enumerate() {
                match p.collect(host_us, wall_ms) {
                    Ok(true) => {
                        if let Err(e) = p.arm() {
                            failed.push((i, e));
                        }
                    }
                    Ok(false) => {}
                    Err(e) => failed.push((i, e)),
                }
            }
            for (i, e) in failed.into_iter().rev() {
                self.drop_port(i, e);
            }
        }

        fn flush(&mut self) -> Batch {
            let now = now_ms();
            for p in &mut self.ports {
                p.link.take(now, &mut self.lines);
            }
            let lines = std::mem::take(&mut self.lines);
            for l in &lines {
                let d = super::dongle_of(l).to_string();
                self.minute
                    .entry(d.clone())
                    .or_insert_with(|| Totals {
                        dongle: d,
                        ..Totals::default()
                    })
                    .add(l);
            }
            Batch { lines }
        }

        fn log_minute(&mut self) {
            for t in std::mem::take(&mut self.minute).values() {
                if t.reports > 0 {
                    crate::dlog!("packets {}", rollup(t));
                }
            }
        }
    }

    fn spawn_finder() -> (Sender<()>, Receiver<Vec<Found>>) {
        let (ask_tx, ask_rx) = channel::<()>();
        let (found_tx, found_rx) = channel();
        crate::app::spawn("dongle-scan", move || {
            while ask_rx.recv().is_ok() {
                let started = Instant::now();
                let found = dongle_paths();
                let took = started.elapsed().as_millis();
                if took >= super::GAP_MS as u128 / 2 {
                    crate::dlog!("packets: looking for dongles took {took} ms");
                }
                if found_tx.send(found).is_err() {
                    return;
                }
            }
        });
        (ask_tx, found_rx)
    }

    pub fn run(tx: Sender<Msg>, gate: Arc<Gate>) {
        let mut cap = Capture::new();
        let (ask, found) = spawn_finder();
        let mut asked = false;
        let mut last_scan: Option<Instant> = None;
        let mut last_flush = Instant::now();
        let mut last_rollup = Instant::now();
        loop {
            if !gate.enter() {
                cap.close_all("SteamVR closed");
                let batch = cap.flush();
                cap.log_minute();
                let sent = batch.is_empty() || tx.send(Msg::Packets(batch)).is_ok();
                gate.leave();
                if !sent {
                    return;
                }
                if asked && found.recv_timeout(Duration::from_secs(5)).is_ok() {
                    asked = false;
                }
                last_scan = None;
                std::thread::sleep(Duration::from_millis(250));
                continue;
            }
            if let Ok(list) = found.try_recv() {
                asked = false;
                cap.open_found(list);
            }
            if !asked && last_scan.is_none_or(|t| t.elapsed() >= SCAN_EVERY) {
                asked = ask.send(()).is_ok();
                last_scan = Some(Instant::now());
            }
            cap.pump(Duration::from_millis(200));
            if last_flush.elapsed() >= FLUSH_EVERY {
                last_flush = Instant::now();
                let batch = cap.flush();
                if !batch.is_empty() && tx.send(Msg::Packets(batch)).is_err() {
                    cap.close_all("TrackDoctor is closing");
                    gate.leave();
                    return;
                }
            }
            if last_rollup.elapsed() >= ROLLUP_EVERY {
                last_rollup = Instant::now();
                cap.log_minute();
            }
        }
    }

    pub fn listen(secs: u64, mut each: impl FnMut(&[Line])) {
        let mut cap = Capture::new();
        cap.open_found(dongle_paths());
        let end = Instant::now() + Duration::from_secs(secs);
        let mut last_flush = Instant::now();
        while Instant::now() < end {
            cap.pump(Duration::from_millis(200));
            if last_flush.elapsed() >= FLUSH_EVERY {
                last_flush = Instant::now();
                let batch = cap.flush();
                each(&batch.lines);
            }
        }
        cap.close_all("done");
        each(&cap.flush().lines);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(clock: u16, size: u8, flags: u8) -> [u8; SLOT] {
        let mut s = [0u8; SLOT];
        s[0] = (clock >> 8) as u8;
        s[1] = size;
        s[2] = clock as u8;
        s[3] = flags;
        s
    }

    fn one(clock: u16, flags: u8) -> Vec<u8> {
        let mut r = vec![0x23];
        r.extend(slot(clock, 12, flags));
        r.extend([0u8; SLOT]);
        r
    }

    fn two(a: u16, b: u16, flags: u8) -> Vec<u8> {
        let mut r = vec![0x24];
        r.extend(slot(a, 12, flags));
        r.extend(slot(b, 12, flags));
        r
    }

    #[test]
    fn single_and_double_reports_give_their_slots() {
        let p = parse(&one(0x1234, 0x80));
        assert_eq!(
            p.slots,
            vec![Slot {
                clock: 0x1234,
                flags: Some(0x80)
            }]
        );
        let p = parse(&two(5, 6, 0x90));
        assert_eq!(p.slots.len(), 2);
        assert_eq!(p.slots[1].clock, 6);
    }

    #[test]
    fn empty_oversized_and_short_slots_are_handled() {
        let mut r = vec![0x24];
        r.extend(slot(1, 12, 0x80));
        r.extend([0u8; SLOT]);
        assert_eq!(parse(&r).slots.len(), 1);
        let mut r = vec![0x23];
        r.extend(slot(1, 28, 0x80));
        assert_eq!(parse(&r).malformed, 1);
        assert_eq!(parse(&[0x24, 1, 2]).malformed, 2);
        let mut r = vec![0x23];
        r.extend(slot(9, 1, 0));
        assert_eq!(parse(&r).slots[0].flags, None);
        assert!(parse(&[0x26, 1]).off);
        assert_eq!(parse(&[0x01, 1]), Parsed::default());
    }

    #[test]
    fn clock_buckets_cover_every_delta() {
        assert_eq!(clock_bucket(0), 0);
        assert_eq!(clock_bucket(3), 3);
        assert_eq!(clock_bucket(4), 4);
        assert_eq!(clock_bucket(15), 5);
        assert_eq!(clock_bucket(16), 6);
        assert_eq!(clock_bucket(64), 7);
        assert_eq!(clock_bucket(u16::MAX), 7);
    }

    fn take(l: &mut Link) -> (Second, Vec<Gap>) {
        let mut out = Vec::new();
        l.take(99, &mut out);
        let mut sec = None;
        let mut gaps = Vec::new();
        for line in out {
            match line {
                Line::Second(s) => sec = Some(s),
                Line::Gap(g) => gaps.push(g),
                Line::Note { .. } => {}
            }
        }
        (sec.unwrap_or_default(), gaps)
    }

    #[test]
    fn a_steady_stream_has_no_gaps() {
        let mut l = Link::new("D1");
        for i in 0..100u16 {
            l.report(u64::from(i) * 1365, 0, &one(i, IMU));
        }
        let (s, gaps) = take(&mut l);
        assert_eq!((s.reports, s.packets, s.imu), (100, 100, 100));
        assert_eq!((s.host_gaps, s.air_gaps, s.odd), (0, 0, 0));
        assert_eq!(s.clock[1], 99);
        assert!(gaps.is_empty());
        assert_eq!(s.t_ms, 99);
    }

    #[test]
    fn packets_lost_in_the_air_move_the_tracker_clock() {
        let mut l = Link::new("D1");
        l.report(0, 0, &one(100, IMU));
        l.report(1_365, 0, &one(101, IMU | LIGHT));
        l.report(1_365 + 40_960, 7, &one(131, IMU));
        let (s, gaps) = take(&mut l);
        assert_eq!((s.host_gaps, s.host_gap_max_ms), (1, 40));
        assert_eq!((s.air_gaps, s.air_ms), (1, 40));
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].host_ms, 40);
        assert_eq!(gaps[0].air_ms, Some(40));
        assert_eq!(gaps[0].before, Some(IMU | LIGHT));
        assert_eq!(gaps[0].t_ms, 7);
    }

    #[test]
    fn a_usb_hold_up_leaves_the_tracker_clock_alone() {
        let mut l = Link::new("D1");
        l.report(0, 0, &one(100, IMU));
        l.report(30_000, 0, &two(101, 102, IMU));
        let (s, gaps) = take(&mut l);
        assert_eq!((s.host_gaps, s.air_gaps, s.doubles), (1, 0, 1));
        assert_eq!(gaps[0].air_ms, Some(1));
    }

    #[test]
    fn a_clock_running_ahead_of_the_host_is_counted_as_odd() {
        let mut l = Link::new("D1");
        l.report(0, 0, &one(100, IMU));
        l.report(1_000, 0, &one(5_000, IMU));
        let (s, gaps) = take(&mut l);
        assert_eq!((s.odd, s.air_gaps), (1, 0));
        assert!(gaps.is_empty());
    }

    #[test]
    fn turning_off_resets_the_link_so_reconnects_are_not_gaps() {
        let mut l = Link::new("D1");
        l.report(0, 0, &one(100, IMU));
        l.report(500, 0, &[0x26, 1]);
        l.report(90_000_000, 0, &one(7, HEARTBEAT));
        let (s, gaps) = take(&mut l);
        assert_eq!((s.off, s.heartbeats, s.host_gaps), (1, 1, 0));
        assert!(gaps.is_empty());
    }

    #[test]
    fn the_clock_wraps_cleanly() {
        let mut l = Link::new("D1");
        l.report(0, 0, &one(u16::MAX, IMU));
        l.report(1_365, 0, &one(0, IMU));
        let (s, _) = take(&mut l);
        assert_eq!((s.clock[1], s.odd), (1, 0));
    }

    #[test]
    fn totals_add_up_and_keep_the_worst_gaps() {
        let mut lines = Vec::new();
        for k in 0..30u32 {
            lines.push(Line::Gap(Gap {
                t_ms: u64::from(k),
                dongle: "D1".into(),
                host_ms: k,
                air_ms: Some(100 - k),
                before: None,
                after: None,
            }));
        }
        lines.push(Line::Second(Second {
            t_ms: 5,
            dongle: "D1".into(),
            reports: 10,
            packets: 500,
            air_ms: 250,
            ..Second::default()
        }));
        lines.push(Line::Second(Second {
            t_ms: 6,
            dongle: "D1".into(),
            packets: 0,
            ..Second::default()
        }));
        let t = totals(&lines);
        let d1 = &t["D1"];
        assert_eq!(
            (d1.seconds, d1.packets, d1.first_ms, d1.last_ms),
            (1, 500, 5, 6)
        );
        assert_eq!(d1.worst.len(), WORST_KEPT);
        assert_eq!(d1.worst[0].worst_ms(), 100);
        assert!((d1.packets_per_s() - 500.0).abs() < 1e-9);
        assert!((d1.air_share() - 0.25).abs() < 1e-9);
    }

    #[test]
    fn lines_round_trip_through_json() {
        let lines = vec![
            Line::Note {
                t_ms: 1,
                dongle: "D1".into(),
                text: "listening".into(),
            },
            Line::Second(Second {
                dongle: "D1".into(),
                packets: 3,
                ..Second::default()
            }),
        ];
        let text: String = lines
            .iter()
            .map(|l| serde_json::to_string(l).unwrap() + "\n")
            .collect();
        assert!(text.starts_with("{\"note\":"));
        assert_eq!(parse_lines(&text), lines);
        assert_eq!(Batch { lines }.dongles_heard(), 1);
    }
}

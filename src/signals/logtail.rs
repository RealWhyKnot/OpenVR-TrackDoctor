use crate::event::{Kind, SignalEvent, Source, now_ms};
use regex::Regex;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::OnceLock;

struct Patterns {
    stamp: Regex,
    device: Regex,
    calibration_failed: Regex,
    optical_loss: Regex,
    sync_acquired: Regex,
    back_facing: Regex,
    disconnected: Regex,
    bind_receiver: Regex,
    bind_dongle: Regex,
    malformed: Regex,
    imu_off_scale: Regex,
    imu_hid_error: Regex,
    bootstrap_fail: Regex,
    ootx: Regex,
    standby: Regex,
    power_off: Regex,
    base_moved: Regex,
    base_moved_tracking: Regex,
    recenter: Regex,
    laser_fault: Regex,
    no_optical: Regex,
}

fn patterns() -> &'static Patterns {
    static P: OnceLock<Patterns> = OnceLock::new();
    P.get_or_init(|| Patterns {
        stamp: Regex::new(
            r"^[A-Z][a-z]{2} ([A-Z][a-z]{2}) (\d{2}) (\d{4}) (\d{2}):(\d{2}):(\d{2})\.(\d{3}) \[[A-Za-z]+\] - ",
        )
        .unwrap(),
        device: Regex::new(r"(LHR-[0-9A-Fa-f]+)").unwrap(),
        calibration_failed: Regex::new(
            r"Calibration failed: no optical samples from base (\S+) for (\d+)\s*ms",
        )
        .unwrap(),
        optical_loss: Regex::new(r"no optical samples (?:from base (\S+) )?for (\d+)\s*ms").unwrap(),
        sync_acquired: Regex::new(r"tdm sync acquired").unwrap(),
        back_facing: Regex::new(r"Dropped (?:\d+ rejected updates, )?(\d+) back-facing hits")
            .unwrap(),
        disconnected: Regex::new(r"Disconnected from receiver (\S+)").unwrap(),
        bind_receiver: Regex::new(r"Connected to receiver (\S+)").unwrap(),
        bind_dongle: Regex::new(r"Connected Dongle:?\s+(\S+)").unwrap(),
        malformed: Regex::new(r"(?i)malformed wireless packet").unwrap(),
        imu_off_scale: Regex::new(r"(?i)IMU went off ?scale").unwrap(),
        imu_hid_error: Regex::new(r"IMU HID device error").unwrap(),
        bootstrap_fail: Regex::new(
            r"Samples didn't yield successful bootstrap pose|did not successfully get a bootstrap pose",
        )
        .unwrap(),
        ootx: Regex::new(r"\(ootx\) selected").unwrap(),
        standby: Regex::new(r"^(\d+) - (entering|leaving) standby").unwrap(),
        power_off: Regex::new(r"Device LHR-[0-9A-Fa-f]+ powering off upon entering standby").unwrap(),
        base_moved: Regex::new(r"Moving base (\S+) (\d+)mm and ([\d.]+) deg").unwrap(),
        base_moved_tracking: Regex::new(r"Moving the base for tracking too").unwrap(),
        recenter: Regex::new(r"OVR runtime requested recenter").unwrap(),
        laser_fault: Regex::new(r"Basestation (\S+) sending strong signals from one laser")
            .unwrap(),
        no_optical: Regex::new(r"No optical frames in past").unwrap(),
    })
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

const QUARTER_HOUR_MS: i64 = 15 * 60 * 1000;

fn stamp_ms(c: &regex::Captures, read_ms: u64) -> Option<u64> {
    let month = MONTHS.iter().position(|m| *m == &c[1])? as i64 + 1;
    let n = |i: usize| c[i].parse::<i64>().ok();
    let days = days_from_civil(n(3)?, month, n(2)?);
    let naive = days * 86_400_000 + n(4)? * 3_600_000 + n(5)? * 60_000 + n(6)? * 1000 + n(7)?;
    let offset = (read_ms as i64 - naive + QUARTER_HOUR_MS / 2).div_euclid(QUARTER_HOUR_MS)
        * QUARTER_HOUR_MS;
    u64::try_from(naive + offset).ok()
}

pub fn parse_line(line: &str, read_ms: u64) -> Option<SignalEvent> {
    let p = patterns();
    let (t_ms, msg) = match p.stamp.captures(line) {
        Some(c) => (
            stamp_ms(&c, read_ms).unwrap_or(read_ms),
            &line[c.get(0).map_or(0, |m| m.end())..],
        ),
        None => (read_ms, line),
    };
    let device = p.device.captures(msg).map(|c| c[1].to_string());
    let kind = if let Some(c) = p.calibration_failed.captures(msg) {
        Kind::CalibrationFailed {
            base: c[1].to_string(),
            outage_ms: c[2].parse().unwrap_or(0),
        }
    } else if let Some(c) = p.optical_loss.captures(msg) {
        Kind::OpticalLoss {
            base: c
                .get(1)
                .map(|b| b.as_str().trim_end_matches(':').to_string()),
            outage_ms: c[2].parse().unwrap_or(0),
        }
    } else if p.sync_acquired.is_match(msg) {
        Kind::SyncAcquired
    } else if let Some(c) = p.back_facing.captures(msg) {
        Kind::BackFacingHits {
            count: c[1].parse().unwrap_or(0),
        }
    } else if let Some(c) = p.laser_fault.captures(msg) {
        Kind::BaseLaserFault {
            base: c[1].to_string(),
        }
    } else if p.disconnected.is_match(msg) {
        Kind::WirelessDisconnect
    } else if let Some(c) = p
        .bind_receiver
        .captures(msg)
        .or_else(|| p.bind_dongle.captures(msg))
    {
        Kind::DongleBind {
            dongle: c[1].to_string(),
        }
    } else if p.malformed.is_match(msg) {
        Kind::MalformedPacket
    } else if p.imu_off_scale.is_match(msg) {
        Kind::ImuOffScale
    } else if p.imu_hid_error.is_match(msg) {
        Kind::ImuHidError
    } else if p.bootstrap_fail.is_match(msg) {
        Kind::BootstrapFail
    } else if p.ootx.is_match(msg) {
        Kind::OotxSelected
    } else if let Some(c) = p.standby.captures(msg) {
        if &c[1] != "0" {
            return None;
        }
        if &c[2] == "entering" {
            Kind::StandbyStart
        } else {
            Kind::StandbyEnd
        }
    } else if p.power_off.is_match(msg) {
        Kind::DevicePowerOff
    } else if let Some(c) = p.base_moved.captures(msg) {
        Kind::BaseMoved {
            base: c[1].to_string(),
            mm: c[2].parse().unwrap_or(0),
            deg: c[3].parse().unwrap_or(0.0),
        }
    } else if p.base_moved_tracking.is_match(msg) {
        Kind::BaseMovedForTracking
    } else if p.recenter.is_match(msg) {
        Kind::OvrRecenter
    } else if p.no_optical.is_match(msg) {
        Kind::NoOpticalFrames
    } else {
        return None;
    };
    Some(SignalEvent::at(
        t_ms,
        Source::Log,
        device,
        kind,
        line.trim(),
    ))
}

pub fn vrserver_log_path() -> PathBuf {
    let fallback = PathBuf::from(r"C:\Program Files (x86)\Steam\logs\vrserver.txt");
    let Some(local) = std::env::var_os("LOCALAPPDATA") else {
        return fallback;
    };
    let vrpath = PathBuf::from(local).join(r"openvr\openvrpaths.vrpath");
    let Ok(text) = std::fs::read_to_string(vrpath) else {
        return fallback;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return fallback;
    };
    let dir = match &json["log"] {
        serde_json::Value::Array(a) => a.first().and_then(|v| v.as_str()),
        serde_json::Value::String(s) => Some(s.as_str()),
        _ => None,
    };
    match dir {
        Some(d) => PathBuf::from(d).join("vrserver.txt"),
        None => fallback,
    }
}

pub struct LogTail {
    path: PathBuf,
    pos: u64,
    partial: String,
}

impl LogTail {
    pub fn new(path: PathBuf) -> Self {
        let pos = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        Self {
            path,
            pos,
            partial: String::new(),
        }
    }

    pub fn poll(&mut self) -> Vec<SignalEvent> {
        let mut out = Vec::new();
        let Ok(mut f) = File::open(&self.path) else {
            return out;
        };
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        if len < self.pos {
            self.pos = 0;
            self.partial.clear();
            out.push(SignalEvent::new(
                Source::Log,
                None,
                Kind::LogRotated,
                "vrserver.txt rotated (SteamVR restart)",
            ));
        }
        if len == self.pos {
            return out;
        }
        if f.seek(SeekFrom::Start(self.pos)).is_err() {
            return out;
        }
        let mut buf = Vec::new();
        if f.read_to_end(&mut buf).is_err() {
            return out;
        }
        self.pos += buf.len() as u64;
        self.partial.push_str(&String::from_utf8_lossy(&buf));
        let read_ms = now_ms();
        while let Some(nl) = self.partial.find('\n') {
            let line: String = self.partial.drain(..=nl).collect();
            if let Some(ev) = parse_line(line.trim_end(), read_ms) {
                out.push(ev);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STANDBY_LOCAL_UTC_MS: u64 = 1_790_988_720_306;

    fn parse(line: &str) -> SignalEvent {
        parse_line(line, STANDBY_LOCAL_UTC_MS + 137).unwrap()
    }

    fn kind(line: &str) -> Kind {
        parse(line).kind
    }

    #[test]
    fn stamp_uses_log_time_not_read_time() {
        let ev = parse("Fri Oct 02 2026 19:52:00.306 [Info] - 0 - entering standby");
        assert_eq!(ev.t_ms, STANDBY_LOCAL_UTC_MS);
        let late = parse_line(
            "Fri Oct 02 2026 19:51:59.056 [Info] - lighthouse: ... Moving the base for tracking too, which might cause a pop",
            STANDBY_LOCAL_UTC_MS + 900,
        )
        .unwrap();
        assert_eq!(late.t_ms, STANDBY_LOCAL_UTC_MS - 1250);
    }

    #[test]
    fn stamp_handles_half_hour_zones() {
        let naive_utc =
            days_from_civil(2026, 10, 2) as u64 * 86_400_000 + 19 * 3_600_000 + 52 * 60_000 + 306;
        let ist = naive_utc - (5 * 3_600_000 + 30 * 60_000);
        let ev = parse_line(
            "Fri Oct 02 2026 19:52:00.306 [Info] - 0 - entering standby",
            ist + 250,
        )
        .unwrap();
        assert_eq!(ev.t_ms, ist);
    }

    #[test]
    fn unstamped_line_uses_read_time() {
        let ev = parse_line("lighthouse: LHR-8A3B0F42 H: tdm sync acquired", 42).unwrap();
        assert_eq!(ev.t_ms, 42);
        assert_eq!(ev.kind, Kind::SyncAcquired);
    }

    #[test]
    fn steamvr_2_16_optical_loss_lines() {
        let ev = parse(
            "Fri Oct 02 2026 19:52:03.842 [Info] - lighthouse: LHR-FD4FF7E2 C: Resetting tracking: no optical samples for 2004ms",
        );
        assert_eq!(ev.device.as_deref(), Some("LHR-FD4FF7E2"));
        assert_eq!(
            ev.kind,
            Kind::OpticalLoss {
                base: None,
                outage_ms: 2004
            }
        );
        assert_eq!(
            kind(
                "Fri Oct 02 2026 02:33:08.991 [Info] - lighthouse: LHR-617D30D7 C: Resetting tracking: no optical samples from base B394A63C for 2004ms"
            ),
            Kind::OpticalLoss {
                base: Some("B394A63C".into()),
                outage_ms: 2004
            }
        );
        assert_eq!(
            kind(
                "Fri Oct 02 2026 19:53:19.840 [Info] - lighthouse: LHR-FD4FF7E2 C: Calibration failed: no optical samples from base B394A63C for 2001ms"
            ),
            Kind::CalibrationFailed {
                base: "B394A63C".into(),
                outage_ms: 2001
            }
        );
    }

    #[test]
    fn steamvr_2_16_bootstrap_line() {
        assert_eq!(
            kind(
                "Fri Oct 02 2026 19:53:24.845 [Info] - lighthouse: LHR-FD4FF7E2 C: Trying to start tracking from base 9D8DEA7B: Samples didn't yield successful bootstrap pose"
            ),
            Kind::BootstrapFail
        );
        assert_eq!(
            parse_line("lighthouse: LHR-8A3B0F42 H: Saw samples from a new base, but did not successfully get a bootstrap pose", 0).unwrap().kind,
            Kind::BootstrapFail
        );
    }

    #[test]
    fn steamvr_2_16_receiver_lines() {
        let ev = parse(
            "Fri Oct 02 2026 19:52:30.909 [Info] - lighthouse: LHR-10268F5C: Disconnected from receiver BC1B52F144",
        );
        assert_eq!(ev.device.as_deref(), Some("LHR-10268F5C"));
        assert_eq!(ev.kind, Kind::WirelessDisconnect);
        let hid = parse(
            "Fri Oct 02 2026 19:52:30.909 [Info] - lighthouse: Lighthouse IMU HID device error",
        );
        assert_eq!(hid.device, None);
        assert_eq!(hid.kind, Kind::ImuHidError);
        assert_eq!(
            kind(
                "Fri Oct 02 2026 19:52:33.546 [Info] - lighthouse: LHR-10268F5C: Connected to receiver BC1B52F144"
            ),
            Kind::DongleBind {
                dongle: "BC1B52F144".into()
            }
        );
        assert_eq!(
            parse_line(
                "lighthouse: Connected Dongle: 8063CF813A firmware 1462663157",
                0
            )
            .unwrap()
            .kind,
            Kind::DongleBind {
                dongle: "8063CF813A".into()
            }
        );
    }

    #[test]
    fn steamvr_2_16_base_move_lines() {
        assert_eq!(
            kind(
                "Fri Oct 02 2026 19:53:20.406 [Info] - lighthouse: Moving base 9D8DEA7B 27mm and 0.3 deg because of relationship with C538EFC9, which is closer to the origin"
            ),
            Kind::BaseMoved {
                base: "9D8DEA7B".into(),
                mm: 27,
                deg: 0.3
            }
        );
        assert_eq!(
            kind(
                "Fri Oct 02 2026 19:51:53.128 [Info] - lighthouse: ... Moving the base for tracking too, which might cause a pop"
            ),
            Kind::BaseMovedForTracking
        );
    }

    #[test]
    fn steamvr_2_16_standby_and_recenter_lines() {
        assert_eq!(
            kind("Fri Oct 02 2026 19:52:00.306 [Info] - 0 - entering standby"),
            Kind::StandbyStart
        );
        assert_eq!(
            kind("Fri Oct 02 2026 19:52:33.621 [Info] - 0 - leaving standby"),
            Kind::StandbyEnd
        );
        assert!(
            parse_line(
                "Fri Oct 02 2026 03:05:52.229 [Info] - 11 - entering standby",
                0
            )
            .is_none()
        );
        let off = parse(
            "Fri Oct 02 2026 20:06:30.686 [Info] - lighthouse: Device LHR-617D30D7 powering off upon entering standby.",
        );
        assert_eq!(off.device.as_deref(), Some("LHR-617D30D7"));
        assert_eq!(off.kind, Kind::DevicePowerOff);
        assert_eq!(
            kind("Fri Oct 02 2026 19:56:33.365 [Info] - oculus: OVR runtime requested recenter"),
            Kind::OvrRecenter
        );
    }

    #[test]
    fn steamvr_2_16_misc_lines() {
        assert_eq!(
            kind(
                "Fri Oct 02 2026 19:52:30.909 [Info] - lighthouse: LHR-10268F5C C: Dropped 49210 back-facing hits, 7 non-clustered hits during the previous tracking session"
            ),
            Kind::BackFacingHits { count: 49210 }
        );
        assert_eq!(
            kind(
                "Fri Oct 02 2026 20:21:16.199 [Info] - lighthouse: LHR-A43A0229 C: Dropped 2 rejected updates, 48 back-facing hits during the previous tracking session"
            ),
            Kind::BackFacingHits { count: 48 }
        );
        let malformed = parse(
            "Fri Oct 02 2026 20:08:42.584 [Info] - lighthouse: : WARNING: Malformed wireless packet v0: MSB 49 len 26 { 152, 16, 1, 18, 23, 89, 140, 22, 31, 89, 132, 145, 58, 223, 97, 132, 241, 72, 251, 30, 10, 145, 134, 231, 97, 140, }",
        );
        assert_eq!(malformed.device, None);
        assert_eq!(malformed.kind, Kind::MalformedPacket);
        assert_eq!(
            kind(
                "Fri Oct 02 2026 19:32:21.974 [Info] - lighthouse: LHR-7E902102 C: IMU went off scale."
            ),
            Kind::ImuOffScale
        );
        assert_eq!(
            kind(
                "Fri Oct 02 2026 19:18:53.360 [Info] - lighthouse: LHR-10268F5C C: No optical frames in past 5 seconds"
            ),
            Kind::NoOpticalFrames
        );
    }

    #[test]
    fn legacy_lines_still_parse() {
        assert_eq!(
            parse_line(
                "lighthouse: base 084071D2: basestation transmission profile (ootx) selected",
                0
            )
            .unwrap()
            .kind,
            Kind::OotxSelected
        );
        assert_eq!(
            parse_line("[PROBLEM] Basestation 084071D2 sending strong signals from one laser and not the other.", 0).unwrap().kind,
            Kind::BaseLaserFault {
                base: "084071D2".into()
            }
        );
    }

    #[test]
    fn unrelated_lines_ignored() {
        for line in [
            "Fri Oct 02 2026 19:53:20.406 [Info] - lighthouse: LHR-617D30D7 C: ----- RELATIONSHIP bases 9D8DEA7B <-> c538efc9 distance 4.27m, angle 176.06 deg -----",
            "Fri Oct 02 2026 19:25:47.916 [Info] - lighthouse: LHR-FD4FF7E2 C: Trying to add a secondary base 9D8DEA7B: Not enough contiguous samples for a bootstrap pose",
            "Fri Oct 02 2026 20:21:16.199 [Info] - lighthouse: LHR-A43A0229 C: Resetting tracking: IMU misalignment unreasonably large (-9.8, -3.7, -13) deg sigma 1.3",
            "Tue Sep 09 2026 - vrserver: some unrelated chatter",
        ] {
            assert!(parse_line(line, 0).is_none(), "{line}");
        }
    }

    #[test]
    fn tail_tracks_appends_and_rotation() {
        use std::io::Write;
        let path =
            std::env::temp_dir().join(format!("trackdoctor-tail-test-{}.txt", std::process::id()));
        std::fs::write(&path, "old line before start\n").unwrap();
        let mut tail = LogTail::new(path.clone());
        assert!(tail.poll().is_empty());

        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        write!(f, "lighthouse: LHR-8A3B0F42 H: tdm sync acq").unwrap();
        assert!(tail.poll().is_empty(), "partial line must not parse");
        writeln!(f, "uired").unwrap();
        writeln!(f, "WARNING: Malformed wireless packet").unwrap();
        drop(f);
        let events = tail.poll();
        assert_eq!(events.len(), 2, "split line reassembled exactly once");
        assert_eq!(events[0].kind, Kind::SyncAcquired);
        assert_eq!(events[1].kind, Kind::MalformedPacket);

        std::fs::write(&path, "lighthouse: LHR-8A3B0F42 H: tdm sync acquired\n").unwrap();
        let events = tail.poll();
        assert_eq!(events[0].kind, Kind::LogRotated);
        assert_eq!(events[1].kind, Kind::SyncAcquired);
        let _ = std::fs::remove_file(&path);
    }
}

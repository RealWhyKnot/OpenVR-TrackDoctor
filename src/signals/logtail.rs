use crate::event::{Kind, SignalEvent, Source};
use regex::Regex;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::OnceLock;

struct Patterns {
    device: Regex,
    optical_loss: Regex,
    sync_acquired: Regex,
    back_facing: Regex,
    bind_receiver: Regex,
    bind_dongle: Regex,
    malformed: Regex,
    imu_off_scale: Regex,
    bootstrap_fail: Regex,
    ootx: Regex,
    leaving_standby: Regex,
    laser_fault: Regex,
    no_optical: Regex,
}

fn patterns() -> &'static Patterns {
    static P: OnceLock<Patterns> = OnceLock::new();
    P.get_or_init(|| Patterns {
        device: Regex::new(r"(LHR-[0-9A-Fa-f]+)").unwrap(),
        optical_loss: Regex::new(r"no optical samples from base (\S+) for (\d+)\s*ms").unwrap(),
        sync_acquired: Regex::new(r"tdm sync acquired").unwrap(),
        back_facing: Regex::new(r"Dropped (\d+) back-facing hits").unwrap(),
        bind_receiver: Regex::new(r"Connected to receiver (\S+)").unwrap(),
        bind_dongle: Regex::new(r"Connected Dongle:?\s+(\S+)").unwrap(),
        malformed: Regex::new(r"(?i)malformed wireless packet").unwrap(),
        imu_off_scale: Regex::new(r"(?i)IMU went off ?scale").unwrap(),
        bootstrap_fail: Regex::new(r"did not successfully get a bootstrap pose").unwrap(),
        ootx: Regex::new(r"\(ootx\) selected").unwrap(),
        leaving_standby: Regex::new(r"leaving standby").unwrap(),
        laser_fault: Regex::new(r"Basestation (\S+) sending strong signals from one laser")
            .unwrap(),
        no_optical: Regex::new(r"No optical frames in past").unwrap(),
    })
}

pub fn parse_line(line: &str) -> Option<SignalEvent> {
    let p = patterns();
    let device = p.device.captures(line).map(|c| c[1].to_string());
    let kind = if let Some(c) = p.optical_loss.captures(line) {
        Kind::OpticalLoss {
            base: c[1].trim_end_matches(':').to_string(),
            outage_ms: c[2].parse().unwrap_or(0),
        }
    } else if p.sync_acquired.is_match(line) {
        Kind::SyncAcquired
    } else if let Some(c) = p.back_facing.captures(line) {
        Kind::BackFacingHits {
            count: c[1].parse().unwrap_or(0),
        }
    } else if let Some(c) = p.laser_fault.captures(line) {
        Kind::BaseLaserFault {
            base: c[1].to_string(),
        }
    } else if let Some(c) = p
        .bind_receiver
        .captures(line)
        .or_else(|| p.bind_dongle.captures(line))
    {
        Kind::DongleBind {
            dongle: c[1].to_string(),
        }
    } else if p.malformed.is_match(line) {
        Kind::MalformedPacket
    } else if p.imu_off_scale.is_match(line) {
        Kind::ImuOffScale
    } else if p.bootstrap_fail.is_match(line) {
        Kind::BootstrapFail
    } else if p.ootx.is_match(line) {
        Kind::OotxSelected
    } else if p.leaving_standby.is_match(line) {
        Kind::LeavingStandby
    } else if p.no_optical.is_match(line) {
        Kind::NoOpticalFrames
    } else {
        return None;
    };
    Some(SignalEvent::new(Source::Log, device, kind, line.trim()))
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
        while let Some(nl) = self.partial.find('\n') {
            let line: String = self.partial.drain(..=nl).collect();
            if let Some(ev) = parse_line(line.trim_end()) {
                out.push(ev);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optical_loss_line() {
        let ev = parse_line("Tue Sep 09 2026 12:00:00.000 - lighthouse: LHR-8A3B0F42 H: Resetting tracking: no optical samples from base 084071D2 for 2539ms").unwrap();
        assert_eq!(ev.device.as_deref(), Some("LHR-8A3B0F42"));
        assert_eq!(
            ev.kind,
            Kind::OpticalLoss {
                base: "084071D2".into(),
                outage_ms: 2539
            }
        );
    }

    #[test]
    fn sync_acquired_line() {
        let ev = parse_line("lighthouse: LHR-8A3B0F42 H: tdm sync acquired").unwrap();
        assert_eq!(ev.kind, Kind::SyncAcquired);
    }

    #[test]
    fn back_facing_line() {
        let ev = parse_line("lighthouse: LHR-8A3B0F42 H: Dropped 172 back-facing hits during the previous tracking session").unwrap();
        assert_eq!(ev.kind, Kind::BackFacingHits { count: 172 });
    }

    #[test]
    fn dongle_lines() {
        let a = parse_line("lighthouse: LHR-8A3B0F42: Connected to receiver 8063CF813A").unwrap();
        assert_eq!(
            a.kind,
            Kind::DongleBind {
                dongle: "8063CF813A".into()
            }
        );
        let b = parse_line("lighthouse: Connected Dongle: 8063CF813A firmware 1462663157").unwrap();
        assert_eq!(
            b.kind,
            Kind::DongleBind {
                dongle: "8063CF813A".into()
            }
        );
    }

    #[test]
    fn wireless_and_imu_lines() {
        assert_eq!(
            parse_line("WARNING: Malformed wireless packet")
                .unwrap()
                .kind,
            Kind::MalformedPacket
        );
        assert_eq!(
            parse_line("lighthouse: LHR-8A3B0F42 IMU went off scale")
                .unwrap()
                .kind,
            Kind::ImuOffScale
        );
    }

    #[test]
    fn lifecycle_lines() {
        assert_eq!(
            parse_line("lighthouse: LHR-8A3B0F42 H: Saw samples from a new base, but did not successfully get a bootstrap pose").unwrap().kind,
            Kind::BootstrapFail
        );
        assert_eq!(
            parse_line(
                "lighthouse: base 084071D2: basestation transmission profile (ootx) selected"
            )
            .unwrap()
            .kind,
            Kind::OotxSelected
        );
        assert_eq!(
            parse_line("lighthouse: LHR-8A3B0F42: leaving standby")
                .unwrap()
                .kind,
            Kind::LeavingStandby
        );
        assert_eq!(
            parse_line("lighthouse: LHR-8A3B0F42 H: No optical frames in past 5 seconds")
                .unwrap()
                .kind,
            Kind::NoOpticalFrames
        );
    }

    #[test]
    fn laser_fault_line() {
        let ev = parse_line("[PROBLEM] Basestation 084071D2 sending strong signals from one laser and not the other.").unwrap();
        assert_eq!(
            ev.kind,
            Kind::BaseLaserFault {
                base: "084071D2".into()
            }
        );
    }

    #[test]
    fn unrelated_line_ignored() {
        assert!(parse_line("Tue Sep 09 2026 - vrserver: some unrelated chatter").is_none());
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

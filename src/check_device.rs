use crate::diagnose::{Finding, Rule};
use crate::event::{Kind, SignalEvent};
use crate::signals::logtail::naive_ms;
use crate::summary::plural;
use std::collections::{BTreeMap, BTreeSet};

pub const EPISODE_MS: u64 = 60_000;
pub const CAUSE_MS: u64 = 30_000;
pub const RADIO_GAP_MS: u64 = 1_000;
pub const MIN_EPISODES: usize = 3;
pub const PEER_RATIO: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sign {
    ImuSilent,
    Misaligned,
}

#[derive(Clone, Copy, Debug)]
struct Episode {
    sign: Sign,
    local: Option<i64>,
}

#[derive(Clone, Debug, Default)]
struct Dev {
    episodes: Vec<Episode>,
    last_sign: Option<u64>,
    cause: Option<(u64, &'static str)>,
}

#[derive(Clone, Debug, Default)]
pub struct DeviceHealth {
    devices: BTreeMap<String, Dev>,
    room: Option<(u64, &'static str)>,
    flagged: BTreeSet<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Fault {
    pub device: String,
    pub silent: usize,
    pub misaligned: usize,
    pub span: String,
    pub peers: usize,
    pub peer_max: usize,
}

fn tracked(ev: &SignalEvent) -> Option<&str> {
    ev.device.as_deref().filter(|d| d.starts_with("LHR-"))
}

fn own_cause(kind: &Kind) -> Option<&'static str> {
    match kind {
        Kind::OpticalLoss { .. } | Kind::CalibrationFailed { .. } => Some("tracking loss"),
        Kind::WirelessDisconnect
        | Kind::WirelessReconnect
        | Kind::DongleBind { .. }
        | Kind::DeviceActivated
        | Kind::DeviceDeactivated
        | Kind::DevicePowerOff
        | Kind::BoardUnreachable { .. }
        | Kind::DeviceConfigFail { .. } => Some("reconnect"),
        Kind::RadioGap { ms, .. } if *ms >= RADIO_GAP_MS => Some("radio gap"),
        _ => None,
    }
}

fn room_cause(kind: &Kind) -> Option<&'static str> {
    match kind {
        Kind::ImuHidError => Some("dongle HID error"),
        Kind::UsbRemove { .. } | Kind::UsbAttach { .. } => Some("USB change"),
        Kind::LogRotated => Some("SteamVR start"),
        _ => None,
    }
}

fn times(n: usize) -> String {
    match n {
        1 => "once".into(),
        2 => "twice".into(),
        n => format!("{n} times"),
    }
}

fn clock(ms: i64) -> String {
    let rem = ms.rem_euclid(86_400_000);
    format!("{:02}:{:02}", rem / 3_600_000, (rem / 60_000) % 60)
}

impl DeviceHealth {
    pub fn ingest(&mut self, ev: &SignalEvent) -> bool {
        if let Some(why) = room_cause(&ev.kind) {
            self.room = Some((ev.t_ms, why));
            return false;
        }
        let Some(dev) = tracked(ev) else {
            return false;
        };
        let d = self.devices.entry(dev.to_string()).or_default();
        let sign = match &ev.kind {
            Kind::ImuMissing => Sign::ImuSilent,
            Kind::ImuMisaligned => Sign::Misaligned,
            k => {
                if let Some(why) = own_cause(k) {
                    d.cause = Some((ev.t_ms, why));
                }
                return false;
            }
        };
        let near = |c: Option<(u64, &'static str)>| {
            c.filter(|(t, _)| ev.t_ms.saturating_sub(*t) <= CAUSE_MS)
                .map(|(_, why)| why)
        };
        let fresh = d
            .last_sign
            .is_none_or(|l| ev.t_ms.saturating_sub(l) >= EPISODE_MS);
        d.last_sign = Some(ev.t_ms);
        if !fresh {
            return false;
        }
        if let Some(why) = near(d.cause).or_else(|| near(self.room)) {
            crate::dlog!("device health: {dev} {sign:?} not counted, came after a {why}");
            return false;
        }
        d.episodes.push(Episode {
            sign,
            local: naive_ms(&ev.detail),
        });
        let n = d.episodes.len();
        crate::dlog!("device health: {dev} {sign:?} counted, {n} so far");
        let flagged: BTreeSet<String> = self
            .faults(&|_| false)
            .into_iter()
            .map(|f| f.device)
            .collect();
        if flagged == self.flagged {
            return false;
        }
        crate::dlog!("device health: now flagging {:?}", flagged);
        self.flagged = flagged;
        true
    }

    pub fn faults(&self, skip: &dyn Fn(&str) -> bool) -> Vec<Fault> {
        let counts: Vec<(&String, usize)> = self
            .devices
            .iter()
            .map(|(k, d)| (k, d.episodes.len()))
            .collect();
        let mut out = Vec::new();
        for (dev, n) in &counts {
            if *n < MIN_EPISODES || skip(dev) {
                continue;
            }
            let peers: Vec<usize> = counts
                .iter()
                .filter(|(o, _)| o != dev)
                .map(|(_, m)| *m)
                .collect();
            let peer_max = peers.iter().copied().max().unwrap_or(0);
            if peers.is_empty() || peer_max * PEER_RATIO > *n {
                continue;
            }
            let eps = &self.devices[*dev].episodes;
            let count = |s: Sign| eps.iter().filter(|e| e.sign == s).count();
            let times: Vec<i64> = eps.iter().filter_map(|e| e.local).collect();
            let span = match (times.first(), times.last()) {
                (Some(a), Some(z)) if clock(*a) != clock(*z) => {
                    format!(", between {} and {}", clock(*a), clock(*z))
                }
                (Some(a), _) => format!(", at {}", clock(*a)),
                _ => String::new(),
            };
            out.push(Fault {
                device: dev.to_string(),
                silent: count(Sign::ImuSilent),
                misaligned: count(Sign::Misaligned),
                span,
                peers: peers.len(),
                peer_max,
            });
        }
        out
    }
}

pub fn explain(
    health: &DeviceHealth,
    rule: Rule,
    label: &dyn Fn(&str) -> String,
    skip: &dyn Fn(&str) -> bool,
) -> Vec<Finding> {
    health
        .faults(skip)
        .into_iter()
        .map(|f| {
            let name = label(&f.device);
            let seen = match (f.silent, f.misaligned) {
                (s, 0) => format!("{name} stopped sending motion data {}", times(s)),
                (0, m) => format!(
                    "The motion and light sensors in {name} disagreed about which way it faced {}",
                    times(m)
                ),
                (s, m) => format!(
                    "{name} stopped sending motion data {}, and its motion and light sensors disagreed about which way it faced {}",
                    times(s),
                    times(m)
                ),
            };
            let peers = match (f.peers, f.peer_max) {
                (1, 0) => "Your other device didn't have this.".to_string(),
                (1, m) => format!("Your other device had it {}.", times(m)),
                (n, 0) => format!("None of your other {} had this.", plural(n, "device")),
                (_, m) => format!("Your other devices had it {} at most.", times(m)),
            };
            Finding {
                rule,
                key: f.device.clone(),
                title: format!("{name} may have a hardware fault"),
                what: format!(
                    "{seen}{}. {peers} SteamVR logged no radio, USB or tracking trouble in the 30 seconds before each one, so the device itself is the likely cause.",
                    f.span
                ),
                steps: [
                    "Turn it off and on again.",
                    "Plug it into the PC with a USB cable and install any firmware update SteamVR offers for it.",
                    "Check it for cracks or other damage, especially if it was dropped or knocked.",
                    "If it keeps happening after that, contact the maker's support about a repair or replacement.",
                ]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Source;
    use crate::signals::logtail::parse_line;

    const T0: u64 = 1_800_000_000_000;

    fn line(t_s: u64, body: &str) -> String {
        let s = 20 * 3600 + t_s;
        format!(
            "Tue Oct 20 2026 {:02}:{:02}:{:02}.000 [Info] - lighthouse: {body}",
            s / 3600,
            (s / 60) % 60,
            s % 60
        )
    }

    fn log(h: &mut DeviceHealth, t_s: u64, body: &str) -> bool {
        let mut ev = parse_line(&line(t_s, body), 0).expect(body);
        ev.t_ms = T0 + t_s * 1000;
        h.ingest(&ev)
    }

    fn api(h: &mut DeviceHealth, t_s: u64, dev: &str, kind: Kind) -> bool {
        h.ingest(&SignalEvent::at(
            T0 + t_s * 1000,
            Source::Api,
            Some(dev.into()),
            kind,
            "",
        ))
    }

    fn room(h: &mut DeviceHealth, peers: &[&str]) {
        for (i, dev) in peers.iter().enumerate() {
            api(h, i as u64, dev, Kind::DeviceActivated);
            log(h, 60 + i as u64, &format!("{dev} C: SOB: add S-1"));
        }
    }

    const ALL: [&str; 4] = [
        "LHR-A66FAA1B",
        "LHR-905BD201",
        "LHR-617D30D7",
        "LHR-7E902102",
    ];

    fn label(s: &str) -> String {
        format!("{s} (tracker)")
    }

    #[test]
    fn misalignment_line_is_parsed_for_its_device() {
        let ev = parse_line(
            "Tue Oct 20 2026 15:28:23.438 [Info] - lighthouse: LHR-905BD201 C: Resetting tracking: IMU misalignment unreasonably large (-11, 10, 2.7) deg sigma 1.8",
            0,
        )
        .unwrap();
        assert_eq!(ev.kind, Kind::ImuMisaligned);
        assert_eq!(ev.device.as_deref(), Some("LHR-905BD201"));
    }

    #[test]
    fn one_device_with_repeated_imu_trouble_is_named() {
        let mut h = DeviceHealth::default();
        room(&mut h, &ALL);
        assert!(!log(
            &mut h,
            600,
            "LHR-A66FAA1B C: No IMU data in past 5 seconds"
        ));
        assert!(!log(
            &mut h,
            605,
            "LHR-A66FAA1B C: No IMU data in past 5 seconds"
        ));
        assert!(!log(
            &mut h,
            1500,
            "LHR-A66FAA1B C: Resetting tracking: IMU misalignment unreasonably large (4, -17, -8.5) deg sigma 1.1"
        ));
        assert!(!log(
            &mut h,
            1800,
            "LHR-905BD201 C: Resetting tracking: IMU misalignment unreasonably large (-11, 10, 2.7) deg sigma 1.8"
        ));
        assert!(
            h.faults(&|_| false).is_empty(),
            "two episodes are not enough"
        );
        assert!(log(
            &mut h,
            2400,
            "LHR-A66FAA1B C: Trying to start tracking from base B394A63C: Bootstrap failed because no IMU data was available"
        ));
        let f = h.faults(&|_| false);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].device, "LHR-A66FAA1B");
        assert_eq!((f[0].silent, f[0].misaligned), (2, 1));
        assert_eq!((f[0].peers, f[0].peer_max), (3, 1));
        let found = explain(&h, Rule::DeviceFault, &label, &|_| false);
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].title,
            "LHR-A66FAA1B (tracker) may have a hardware fault"
        );
        assert!(
            found[0].what.starts_with("LHR-A66FAA1B (tracker) stopped sending motion data twice, and its motion and light sensors disagreed about which way it faced once, between 20:10 and 20:40. Your other devices had it once at most."),
            "{}",
            found[0].what
        );
        assert_eq!(found[0].steps.len(), 4);
        assert!(explain(&h, Rule::DeviceFault, &label, &|d| d == "LHR-A66FAA1B").is_empty());
    }

    #[test]
    fn healthy_session_with_explained_imu_gaps_stays_quiet() {
        let mut h = DeviceHealth::default();
        room(&mut h, &ALL);
        for (i, dev) in ALL.iter().enumerate() {
            let t = 600 + i as u64 * 900;
            log(
                &mut h,
                t,
                &format!("{dev} C: Resetting tracking: no optical samples for 2016ms"),
            );
            log(
                &mut h,
                t + 3,
                &format!(
                    "{dev} C: Trying to start tracking from base B394A63C: Bootstrap failed because no IMU data was available"
                ),
            );
            log(&mut h, t + 200, &format!("{dev} C: IMU went off scale."));
        }
        log(&mut h, 4000, "Lighthouse IMU HID device error");
        for (i, dev) in ALL.iter().enumerate() {
            log(
                &mut h,
                4010 + i as u64,
                &format!(
                    "{dev} C: Resetting tracking: IMU misalignment unreasonably large (-10, -0.64, 12) deg sigma 2.4"
                ),
            );
        }
        for k in 0..5 {
            let t = 5000 + k * 300;
            log(
                &mut h,
                t,
                "LHR-A66FAA1B: Disconnected from receiver BC1B52F144",
            );
            log(
                &mut h,
                t + 20,
                "LHR-A66FAA1B C: No IMU data in past 5 seconds",
            );
        }
        assert!(h.faults(&|_| false).is_empty());
        assert!(h.devices.values().all(|d| d.episodes.is_empty()));
    }

    #[test]
    fn trouble_on_every_device_is_not_blamed_on_one() {
        let mut h = DeviceHealth::default();
        room(&mut h, &ALL);
        for k in 0..4u64 {
            for (i, dev) in ALL.iter().enumerate() {
                log(
                    &mut h,
                    600 + k * 600 + i as u64 * 100,
                    &format!("{dev} C: No IMU data in past 5 seconds"),
                );
            }
        }
        assert!(h.devices.values().all(|d| d.episodes.len() == 4));
        assert!(h.faults(&|_| false).is_empty());
    }

    #[test]
    fn a_lone_device_has_nothing_to_compare_against() {
        let mut h = DeviceHealth::default();
        room(&mut h, &["LHR-A66FAA1B"]);
        for k in 0..4u64 {
            log(
                &mut h,
                600 + k * 600,
                "LHR-A66FAA1B C: No IMU data in past 5 seconds",
            );
        }
        assert!(h.faults(&|_| false).is_empty());
    }
}

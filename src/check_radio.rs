use crate::diagnose::{Finding, Rule};
use crate::event::{Kind, SignalEvent};
use crate::signals::logtail::naive_ms;
use crate::summary::plural;
use std::collections::BTreeMap;

pub const SHORT_MS: u64 = 1000;
pub const LONG_MS: u64 = 100;
pub const SHARED_MS: u64 = 250;
pub const POWER_OFF_MS: u64 = 50;
pub const WINDOW_MS: u64 = 10 * 60_000;
pub const CUTOUTS: usize = 15;

#[derive(Clone, Debug)]
struct Gap {
    t_ms: u64,
    local: Option<i64>,
    dongle: String,
    key: String,
    long: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Radio {
    gaps: Vec<Gap>,
    peers: BTreeMap<String, String>,
    logged: BTreeMap<String, u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cutout {
    pub key: String,
    pub dongle: String,
    pub count: usize,
    pub from: Option<i64>,
    pub to: Option<i64>,
    pub others: usize,
    pub worst_other: Option<(String, usize)>,
}

impl Radio {
    pub fn ingest(&mut self, ev: &SignalEvent) -> bool {
        match &ev.kind {
            Kind::RadioGap { dongle, ms } => {
                let key = ev.device.clone().unwrap_or_else(|| dongle.clone());
                self.peers.insert(key.clone(), dongle.clone());
                if *ms >= SHORT_MS {
                    return false;
                }
                let long = *ms >= LONG_MS;
                let at = self.gaps.partition_point(|g| g.t_ms <= ev.t_ms);
                self.gaps.insert(
                    at,
                    Gap {
                        t_ms: ev.t_ms,
                        local: naive_ms(&ev.detail),
                        dongle: dongle.clone(),
                        key: key.clone(),
                        long,
                    },
                );
                if long && crate::DEV {
                    self.note(&key, ev.t_ms);
                }
                long
            }
            Kind::WirelessDisconnect => {
                let Some(dev) = &ev.device else {
                    return false;
                };
                let before = self.gaps.len();
                self.gaps.retain(|g| {
                    !(g.long && &g.key == dev && g.t_ms.abs_diff(ev.t_ms) <= POWER_OFF_MS)
                });
                self.gaps.len() != before
            }
            _ => false,
        }
    }

    fn note(&mut self, key: &str, t_ms: u64) {
        let Some(c) = self.cutouts().into_iter().find(|c| c.key == key) else {
            return;
        };
        if self
            .logged
            .get(key)
            .is_some_and(|&last| t_ms.saturating_sub(last) < WINDOW_MS)
        {
            return;
        }
        self.logged.insert(key.to_string(), t_ms);
        crate::dlog!(
            "radio: {key} on dongle {} cut out {} times in 10 min, other devices at most {}",
            c.dongle,
            c.count,
            c.others
        );
    }

    fn lone(&self) -> BTreeMap<&str, Vec<&Gap>> {
        let mut out: BTreeMap<&str, Vec<&Gap>> = BTreeMap::new();
        for (i, g) in self.gaps.iter().enumerate() {
            if !g.long {
                continue;
            }
            let lo = self.gaps.partition_point(|o| o.t_ms + SHARED_MS < g.t_ms);
            let shared = self.gaps[lo..]
                .iter()
                .enumerate()
                .take_while(|(_, o)| o.t_ms <= g.t_ms + SHARED_MS)
                .any(|(j, o)| lo + j != i && o.dongle != g.dongle);
            if !shared {
                out.entry(g.key.as_str()).or_default().push(g);
            }
        }
        out
    }

    pub fn cutouts(&self) -> Vec<Cutout> {
        let lone = self.lone();
        let mut out = Vec::new();
        for (key, gaps) in &lone {
            let mut best = (0, 0);
            let mut z = 0;
            for (a, g) in gaps.iter().enumerate() {
                z = z.max(a);
                while z < gaps.len() && gaps[z].t_ms - g.t_ms <= WINDOW_MS {
                    z += 1;
                }
                if z - a > best.1 {
                    best = (a, z - a);
                }
            }
            let (a, count) = best;
            if count < CUTOUTS {
                continue;
            }
            let from = gaps[a].t_ms;
            let worst_other = lone
                .iter()
                .filter(|(k, _)| *k != key)
                .map(|(k, v)| {
                    let n = v
                        .iter()
                        .filter(|o| o.t_ms >= from && o.t_ms - from <= WINDOW_MS)
                        .count();
                    (k.to_string(), n)
                })
                .max_by(|x, y| x.1.cmp(&y.1).then(y.0.cmp(&x.0)))
                .filter(|(_, n)| *n > 0);
            out.push(Cutout {
                key: key.to_string(),
                dongle: gaps[a].dongle.clone(),
                count,
                from: gaps[a].local,
                to: gaps[a + count - 1].local,
                others: worst_other.as_ref().map_or(0, |o| o.1),
                worst_other,
            });
        }
        out
    }

    pub fn peers(&self) -> usize {
        self.peers.len()
    }
}

fn clock(local: Option<i64>) -> Option<String> {
    let rem = local?.rem_euclid(86_400_000);
    Some(format!("{:02}:{:02}", rem / 3_600_000, (rem / 60_000) % 60))
}

pub fn findings(radio: &Radio, label: &dyn Fn(&str) -> String) -> Vec<Finding> {
    let compared = radio.peers() > 1;
    radio
        .cutouts()
        .into_iter()
        .map(|c| {
            let unnamed = c.key == c.dongle;
            let (title, lead) = if unnamed {
                (
                    format!("Wireless connection to dongle {} keeps dropping", c.dongle),
                    format!("The device paired to dongle {} lost contact", c.dongle),
                )
            } else {
                let name = label(&c.key);
                (
                    format!("{name}'s wireless connection keeps dropping"),
                    format!("{name} lost contact with its dongle"),
                )
            };
            let span = match (clock(c.from), clock(c.to)) {
                (Some(a), Some(z)) if a != z => format!(", from {a} to {z}"),
                (Some(a), _) => format!(", at {a}"),
                _ => String::new(),
            };
            let crowded = c.others >= CUTOUTS;
            const NEAR: &str = " It's likely too far from its dongle, or something such as your body blocks the signal.";
            let compare = match &c.worst_other {
                _ if !compared => NEAR.to_string(),
                None => format!(" None of your other devices did that in the same 10 minutes, so the trouble is between this device and its dongle.{NEAR}"),
                Some((other, n)) if crowded => format!(
                    " {} did it {} in the same 10 minutes too, so something near the dongles may be drowning them out.",
                    label(other),
                    plural(*n, "time")
                ),
                Some(_) => format!(
                    " No other device did it more than {} in the same 10 minutes, so the trouble is between this device and its dongle.{NEAR}",
                    if c.others == 1 { "once".to_string() } else { plural(c.others, "time") }
                ),
            };
            let mut steps = vec![
                "Put its dongle on a USB extension cable or in a dongle cradle, out in the open with a clear view of where you play.".to_string(),
                "Keep the dongles apart from each other and away from the back of the PC.".to_string(),
            ];
            if crowded {
                steps.push("Move the dongles away from your Wi-Fi router and other wireless gear.".to_string());
            } else {
                steps.push("If you wear it on your back or hip, your body blocks the signal whenever you face away from the dongle. A dongle higher up or nearer the middle of the room can help.".to_string());
            }
            steps.push("Pair it with a different dongle. If it still cuts out, the device itself is the likely cause.".to_string());
            Finding {
                rule: Rule::RadioLink,
                key: c.key.clone(),
                title,
                what: format!(
                    "{lead} for a tenth of a second or longer {} in 10 minutes{span}.{compare}",
                    plural(c.count, "time")
                ),
                steps,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signals::logtail::parse_line;

    const READ_MS: u64 = 1_792_656_723_175;
    const T0: u64 = 1_792_651_200_000;
    const HIP: (&str, &str) = ("LHR-A66FAA1B", "BC1B52F144");
    const ROOM: [(&str, &str); 6] = [
        ("LHR-905BD201", "E1B9096D38"),
        ("LHR-617D30D7", "D373DE2627"),
        ("LHR-10268F5C", "65D86BDA85"),
        ("LHR-A43A0229", "57F60A0058"),
        ("LHR-7E902102", "0C58EE58E6"),
        ("LHR-FD4FF7E2", "DDF8F5F1F6"),
    ];

    fn stamp(s: u64) -> String {
        let s = 4 * 3600 + 20 * 60 + s;
        format!(
            "Thu Oct 22 2026 {:02}:{:02}:{:02}.{:03}",
            s / 3600,
            (s / 60) % 60,
            s % 60,
            0
        )
    }

    fn gap(dev: (&str, &str), s: u64, ms: u64) -> SignalEvent {
        let line = format!(
            "{} [Info] - lighthouse: {}: Packet received after {}.{:03}s, keepalive (0/1)",
            stamp(s),
            dev.1,
            ms / 1000,
            ms % 1000
        );
        let mut ev = parse_line(&line, READ_MS).expect("gap line parses");
        ev.t_ms = T0 + s * 1000;
        ev.device = Some(dev.0.to_string());
        ev
    }

    fn gap_ms(dev: (&str, &str), at_ms: u64, ms: u64) -> SignalEvent {
        let mut ev = gap(dev, at_ms / 1000, ms);
        ev.t_ms += at_ms % 1000;
        ev
    }

    fn off(dev: (&str, &str), s: u64) -> SignalEvent {
        let line = format!(
            "{} [Info] - lighthouse: {}: Disconnected from receiver {}",
            stamp(s),
            dev.0,
            dev.1
        );
        let mut ev = parse_line(&line, READ_MS).expect("disconnect line parses");
        ev.t_ms = T0 + s * 1000;
        ev
    }

    fn label(s: &str) -> String {
        s.to_string()
    }

    fn startup(r: &mut Radio) {
        for (i, d) in ROOM.iter().chain([&HIP]).enumerate() {
            r.ingest(&gap(*d, i as u64, 2_154 + 1_000 * i as u64));
        }
    }

    fn ordinary(r: &mut Radio, minutes: u64) {
        for m in 0..minutes {
            for (i, d) in ROOM.iter().chain([&HIP]).enumerate() {
                let s = 60 + m * 60 + i as u64 * 7;
                r.ingest(&gap(*d, s, 52 + i as u64));
                if (m + i as u64).is_multiple_of(4) {
                    r.ingest(&gap(*d, s + 3, 118));
                }
            }
        }
    }

    #[test]
    fn a_healthy_session_stays_quiet() {
        let mut r = Radio::default();
        startup(&mut r);
        ordinary(&mut r, 45);
        assert!(r.gaps.iter().filter(|g| g.long).count() > 70);
        assert!(r.cutouts().is_empty());
        assert!(findings(&r, &label).is_empty());
    }

    #[test]
    fn a_hip_tracker_that_keeps_cutting_out_is_named_alone() {
        let mut r = Radio::default();
        startup(&mut r);
        ordinary(&mut r, 20);
        for k in 0..18 {
            r.ingest(&gap(HIP, 600 + k * 25 + 11, 140 + k * 13));
        }
        let cut = r.cutouts();
        assert_eq!(cut.len(), 1, "{cut:?}");
        assert_eq!(cut[0].key, HIP.0);
        assert_eq!(cut[0].dongle, HIP.1);
        assert!(
            cut[0].count >= 18 && cut[0].count < 18 + 5,
            "{}",
            cut[0].count
        );
        assert!(cut[0].others < 5, "{}", cut[0].others);
        let f = findings(&r, &label);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].rule, Rule::RadioLink);
        assert_eq!(
            f[0].title,
            "LHR-A66FAA1B's wireless connection keeps dropping"
        );
        assert!(
            f[0].what
                .contains(&format!("{} times in 10 minutes, from 04:", cut[0].count)),
            "{}",
            f[0].what
        );
        assert!(
            f[0].what
                .contains("trouble is between this device and its dongle"),
            "{}",
            f[0].what
        );
        assert!(f[0].steps.iter().any(|s| s.contains("back or hip")));
        assert!(!f[0].steps.iter().any(|s| s.contains("Wi-Fi")));
    }

    #[test]
    fn a_dongle_with_no_known_device_is_named_by_its_dongle() {
        let mut r = Radio::default();
        startup(&mut r);
        ordinary(&mut r, 15);
        for k in 0..16 {
            let mut ev = gap_ms(HIP, (300 + k * 20) * 1000 + 500, 210);
            ev.device = None;
            r.ingest(&ev);
        }
        let f = findings(&r, &label);
        assert_eq!(f.len(), 1, "{f:?}");
        assert_eq!(f[0].key, HIP.1);
        assert_eq!(
            f[0].title,
            "Wireless connection to dongle BC1B52F144 keeps dropping"
        );
        assert!(
            f[0].what.starts_with("The device paired to dongle BC1B52F144 lost contact for a tenth of a second or longer 16 times in 10 minutes"),
            "{}",
            f[0].what
        );
    }

    #[test]
    fn gaps_on_every_dongle_at_once_are_left_to_the_pc_check() {
        let mut r = Radio::default();
        startup(&mut r);
        for k in 0..40 {
            for (i, d) in ROOM.iter().chain([&HIP]).enumerate() {
                r.ingest(&gap_ms(*d, 120_000 + k * 12_000 + i as u64 * 20, 160));
            }
        }
        assert!(r.cutouts().is_empty());
    }

    #[test]
    fn turning_a_device_off_is_not_a_cutout() {
        let mut r = Radio::default();
        startup(&mut r);
        for k in 0..12 {
            r.ingest(&gap(HIP, 300 + k * 30, 150));
        }
        for k in 0..4 {
            let s = 320 + k * 90;
            r.ingest(&gap(HIP, s, 487));
            r.ingest(&off(HIP, s));
        }
        assert!(r.cutouts().is_empty());
        for k in 0..4 {
            r.ingest(&gap(HIP, 340 + k * 90, 210));
        }
        assert_eq!(r.cutouts().len(), 1);
    }

    #[test]
    fn reconnects_after_long_silences_are_not_counted() {
        let mut r = Radio::default();
        startup(&mut r);
        for k in 0..20 {
            r.ingest(&gap(HIP, 300 + k * 20, 5_043_341 + k));
        }
        assert!(r.cutouts().is_empty());
    }

    #[test]
    fn two_devices_cutting_out_together_point_at_the_room() {
        let mut r = Radio::default();
        startup(&mut r);
        for k in 0..16 {
            r.ingest(&gap(HIP, 300 + k * 30, 150));
            r.ingest(&gap(ROOM[3], 310 + k * 30, 170));
        }
        let f = findings(&r, &label);
        assert_eq!(f.len(), 2, "{f:?}");
        for x in &f {
            assert!(x.what.contains("16 times in 10 minutes"), "{}", x.what);
            assert!(
                x.what.contains("times in the same 10 minutes too"),
                "{}",
                x.what
            );
            assert!(x.steps.iter().any(|s| s.contains("Wi-Fi")));
        }
    }
}

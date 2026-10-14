use crate::bases::{is_big, short_serial};
use crate::event::{Kind, SignalEvent};
use crate::signals::logtail::naive_ms;
use crate::summary::plural;
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub const SETTLE_MS: u64 = 10 * 60_000;
pub const STALE_MOVES: usize = 3;
pub const BOARD_WINDOW_MS: u64 = 5 * 60_000;
pub const BOARD_FAULTS: usize = 3;
pub const SYNC_WINDOW_MS: u64 = 10 * 60_000;
pub const SYNC_BURST: usize = 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rule {
    StaleMap,
    BoardFault,
    Takeover,
    BaseRestart,
    SyncMismatch,
    BaseSignal,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Finding {
    pub rule: Rule,
    pub what: String,
    pub fix: String,
}

impl Finding {
    pub fn line(&self) -> String {
        if self.fix.is_empty() {
            self.what.clone()
        } else {
            format!("{} {}", self.what, self.fix)
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Stamp {
    t_ms: u64,
    local: Option<i64>,
}

impl Stamp {
    fn of(ev: &SignalEvent) -> Self {
        Self {
            t_ms: ev.t_ms,
            local: naive_ms(&ev.detail),
        }
    }

    fn clock(&self) -> Option<String> {
        let rem = self.local?.rem_euclid(86_400_000);
        Some(format!("{:02}:{:02}", rem / 3_600_000, (rem / 60_000) % 60))
    }
}

fn at(s: &Stamp) -> String {
    s.clock().map_or_else(String::new, |c| format!(" at {c}"))
}

#[derive(Clone, Debug, Default)]
struct Board {
    unreachable: usize,
    config: usize,
    imu: usize,
    times: Vec<Stamp>,
}

#[derive(Clone, Debug)]
struct Takeover {
    dongle: String,
    from: String,
    to: String,
    at: Stamp,
    rebound: Option<(String, Stamp)>,
}

#[derive(Clone, Debug, Default)]
pub struct Diagnosis {
    run_start: Option<u64>,
    late_moves: Vec<(Stamp, String, u32)>,
    late_disallowed: usize,
    resets: BTreeMap<String, Vec<Stamp>>,
    boards: BTreeMap<String, Board>,
    owners: HashMap<String, String>,
    takeovers: Vec<Takeover>,
    sync: BTreeMap<String, usize>,
    sync_times: Vec<u64>,
    sync_peak: usize,
    channels: BTreeMap<u32, usize>,
    rotor: usize,
    fired: BTreeSet<Rule>,
}

fn burst(times: &[u64], window_ms: u64) -> usize {
    let Some(&last) = times.last() else {
        return 0;
    };
    times
        .iter()
        .rev()
        .take_while(|&&t| last.saturating_sub(t) <= window_ms)
        .count()
}

impl Diagnosis {
    pub fn ingest(&mut self, ev: &SignalEvent) -> Vec<Rule> {
        if ev.kind == Kind::LogRotated {
            self.run_start = None;
            self.owners.clear();
            self.sync_times.clear();
            return Vec::new();
        }
        let start = *self.run_start.get_or_insert(ev.t_ms);
        let settled = ev.t_ms.saturating_sub(start) >= SETTLE_MS;
        let stamp = Stamp::of(ev);
        match &ev.kind {
            Kind::BaseMoved { base, mm, deg } if settled && is_big(*mm, *deg) => {
                self.late_moves.push((stamp, short_serial(base), *mm));
            }
            Kind::DbDisallowed if settled => self.late_disallowed += 1,
            Kind::BaseReset { base } => {
                self.resets
                    .entry(short_serial(base))
                    .or_default()
                    .push(stamp);
            }
            Kind::BoardUnreachable { dongle } | Kind::DeviceConfigFail { dongle } => {
                if let Some(dev) = ev
                    .device
                    .clone()
                    .or_else(|| self.owners.get(dongle).cloned())
                {
                    let b = self.boards.entry(dev).or_default();
                    if matches!(ev.kind, Kind::BoardUnreachable { .. }) {
                        b.unreachable += 1;
                    } else {
                        b.config += 1;
                    }
                    b.times.push(stamp);
                }
            }
            Kind::ImuMissing => {
                if let Some(dev) = &ev.device {
                    let b = self.boards.entry(dev.clone()).or_default();
                    b.imu += 1;
                    b.times.push(stamp);
                }
            }
            Kind::DongleBind { dongle } => {
                if let Some(dev) = &ev.device {
                    self.bind(dev, dongle, stamp);
                }
            }
            Kind::SyncMismatch => {
                if let Some(dev) = &ev.device {
                    *self.sync.entry(dev.clone()).or_default() += 1;
                }
                self.sync_times.push(ev.t_ms);
                self.sync_peak = self.sync_peak.max(burst(&self.sync_times, SYNC_WINDOW_MS));
            }
            Kind::ChannelConflict { ch } => *self.channels.entry(*ch).or_default() += 1,
            Kind::RotorPhase => self.rotor += 1,
            _ => {}
        }
        let mut fresh = Vec::new();
        for rule in self.active() {
            if self.fired.insert(rule) {
                fresh.push(rule);
            }
        }
        fresh
    }

    fn bind(&mut self, dev: &str, dongle: &str, stamp: Stamp) {
        for t in self
            .takeovers
            .iter_mut()
            .filter(|t| t.from == dev && t.rebound.is_none())
        {
            t.rebound = Some((dongle.to_string(), stamp));
        }
        if let Some(prev) = self.owners.get(dongle)
            && prev != dev
        {
            self.takeovers.push(Takeover {
                dongle: dongle.to_string(),
                from: prev.clone(),
                to: dev.to_string(),
                at: stamp,
                rebound: None,
            });
        }
        self.owners.retain(|_, owner| owner != dev);
        self.owners.insert(dongle.to_string(), dev.to_string());
    }

    fn faulty_boards(&self) -> impl Iterator<Item = (&String, &Board)> {
        self.boards.iter().filter(|(_, b)| {
            b.times.iter().enumerate().any(|(i, s)| {
                b.times[i..]
                    .iter()
                    .take_while(|o| o.t_ms.saturating_sub(s.t_ms) <= BOARD_WINDOW_MS)
                    .count()
                    >= BOARD_FAULTS
            })
        })
    }

    fn active(&self) -> Vec<Rule> {
        let mut out = Vec::new();
        if self.late_moves.len() >= STALE_MOVES {
            out.push(Rule::StaleMap);
        }
        if self.faulty_boards().next().is_some() {
            out.push(Rule::BoardFault);
        }
        if !self.takeovers.is_empty() {
            out.push(Rule::Takeover);
        }
        if !self.resets.is_empty() {
            out.push(Rule::BaseRestart);
        }
        if self.sync_peak >= SYNC_BURST {
            out.push(Rule::SyncMismatch);
        }
        if !self.channels.is_empty() || self.rotor > 0 {
            out.push(Rule::BaseSignal);
        }
        out
    }

    pub fn findings(&self, label: &dyn Fn(&str) -> String) -> Vec<Finding> {
        self.active()
            .into_iter()
            .flat_map(|rule| self.explain(rule, label))
            .collect()
    }

    pub fn explain(&self, rule: Rule, label: &dyn Fn(&str) -> String) -> Vec<Finding> {
        let finding = |what: String, fix: &str| Finding {
            rule,
            what,
            fix: fix.to_string(),
        };
        match rule {
            Rule::StaleMap => {
                let max = self.late_moves.iter().map(|m| m.2).max().unwrap_or(0);
                let mut bases: Vec<&str> = self.late_moves.iter().map(|m| m.1.as_str()).collect();
                bases.sort();
                bases.dedup();
                let last = self
                    .late_moves
                    .last()
                    .and_then(|m| m.0.clock())
                    .map_or_else(String::new, |c| format!(", the last at {c}"));
                let first = self.late_moves[0].0.t_ms;
                let before: Vec<String> = self
                    .resets
                    .iter()
                    .filter(|(_, times)| times.iter().any(|s| s.t_ms <= first))
                    .map(|(b, _)| b.clone())
                    .collect();
                let after = if before.is_empty() {
                    String::new()
                } else {
                    format!(" It started after base {} restarted.", join(&before))
                };
                let refused = if self.late_disallowed > 0 {
                    format!(
                        " It also refused to start tracking from its saved map {}.",
                        plural(self.late_disallowed, "time")
                    )
                } else {
                    String::new()
                };
                vec![finding(
                    format!(
                        "Base map out of date: more than 10 minutes in, SteamVR was still moving {} in its saved map ({}, up to {max} mm{last}).{after}{refused} A base was moved, re-aimed or bumped while SteamVR kept its old position, or one sits loose and keeps shifting.",
                        if bases.len() == 1 {
                            format!("base {}", bases[0])
                        } else {
                            format!("bases {}", bases.join(", "))
                        },
                        plural(self.late_moves.len(), "big move"),
                    ),
                    "Fix: once every base is in its final place, run trackdoctor resetmap. It keeps the old map as a backup and has SteamVR build a new one.",
                )]
            }
            Rule::BoardFault => self
                .faulty_boards()
                .map(|(dev, b)| {
                    let mut parts = Vec::new();
                    if b.unreachable > 0 {
                        parts.push(format!(
                            "its dongle got radio data but couldn't reach its main board {}",
                            plural(b.unreachable, "time")
                        ));
                    }
                    if b.config > 0 {
                        parts.push(format!(
                            "SteamVR couldn't read its config {}",
                            plural(b.config, "time")
                        ));
                    }
                    if b.imu > 0 {
                        parts.push(format!(
                            "no motion sensor data came through {}",
                            plural(b.imu, "time")
                        ));
                    }
                    let span = match (b.times.first().and_then(Stamp::clock), b.times.last().and_then(Stamp::clock)) {
                        (Some(a), Some(z)) if a != z => format!(", {a} to {z}"),
                        (Some(a), _) => format!(", at {a}"),
                        _ => String::new(),
                    };
                    finding(
                        format!("{} wasn't answering: {}{span}.", label(dev), join(&parts)),
                        "Fix: turn it off and on again. If it keeps happening, pair it to another dongle. If the fault follows it to the new dongle, the device itself is failing.",
                    )
                })
                .collect(),
            Rule::Takeover => self
                .takeovers
                .iter()
                .map(|t| {
                    let from = label(&t.from);
                    let what = format!(
                        "{} connected through dongle {}{}, which {from} had been using, so it lost its connection.",
                        label(&t.to),
                        t.dongle,
                        at(&t.at)
                    );
                    match &t.rebound {
                        Some((d, s)) => finding(
                            format!(
                                "{what} It got dongle {d}{}.",
                                s.clock().map_or_else(|| " later".to_string(), |c| format!(" at {c}"))
                            ),
                            "",
                        ),
                        None => finding(
                            what,
                            &format!("Fix: pair {from} again, with only that device in pairing mode."),
                        ),
                    }
                })
                .collect(),
            Rule::BaseRestart => self
                .resets
                .iter()
                .map(|(base, times)| {
                    let when: Vec<String> = times.iter().filter_map(Stamp::clock).collect();
                    let what = if times.len() == 1 {
                        format!("Base {base} restarted{}.", at(&times[0]))
                    } else if when.is_empty() {
                        format!("Base {base} restarted {} times.", times.len())
                    } else {
                        format!(
                            "Base {base} restarted {} times ({}).",
                            times.len(),
                            when.join(", ")
                        )
                    };
                    finding(
                        format!("{what} SteamVR logs this when a base loses power or gets bumped."),
                        "Fix: check its power plug and adapter and that nothing knocks it. If it moved, run trackdoctor resetmap.",
                    )
                })
                .collect(),
            Rule::SyncMismatch => {
                let total: usize = self.sync.values().sum::<usize>().max(self.sync_times.len());
                let mut by: Vec<(&String, &usize)> = self.sync.iter().collect();
                by.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
                let list: Vec<String> = by
                    .iter()
                    .take(4)
                    .map(|(d, n)| format!("{n} on {}", label(d)))
                    .collect();
                let fix = if self.active().contains(&Rule::StaleMap) {
                    "Fix: reset the base map first; these bursts came with it. If they keep coming after that, look for mirrors, windows or glossy surfaces the bases can see."
                } else {
                    "Fix: look for mirrors, windows or glossy surfaces the bases can see and cover them one at a time."
                };
                vec![finding(
                    format!(
                        "SteamVR couldn't match a light sync to the base it expected {} ({}), up to {} within 10 minutes.",
                        plural(total, "time"),
                        list.join(", "),
                        self.sync_peak
                    ),
                    fix,
                )]
            }
            Rule::BaseSignal => {
                let mut out: Vec<Finding> = self
                    .channels
                    .iter()
                    .map(|(ch, n)| {
                        finding(
                            format!(
                                "Two base stations were sending on channel {ch} ({}).",
                                plural(*n, "time")
                            ),
                            "Fix: give each base station its own channel.",
                        )
                    })
                    .collect();
                if self.rotor > 0 {
                    out.push(finding(
                        format!(
                            "A 1.0 base station's rotors fell out of phase lock {}.",
                            plural(self.rotor, "time")
                        ),
                        "Fix: make sure the bases can see each other, or connect the sync cable.",
                    ));
                }
                out
            }
        }
    }
}

fn join(parts: &[String]) -> String {
    match parts {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Source;

    const T0: u64 = 1_000_000_000;

    fn ev(t_ms: u64, device: Option<&str>, kind: Kind) -> SignalEvent {
        SignalEvent::at(t_ms, Source::Log, device.map(String::from), kind, "")
    }

    fn label(s: &str) -> String {
        s.to_string()
    }

    fn moved(t_ms: u64, mm: u32) -> SignalEvent {
        ev(
            t_ms,
            None,
            Kind::BaseMoved {
                base: "9417E0B3".into(),
                mm,
                deg: 0.2,
            },
        )
    }

    fn rules(d: &Diagnosis) -> Vec<Rule> {
        d.findings(&label).iter().map(|f| f.rule).collect()
    }

    #[test]
    fn early_moves_are_settling_and_late_ones_mean_a_stale_map() {
        let mut d = Diagnosis::default();
        for i in 0..9 {
            d.ingest(&moved(T0 + i * 20_000, 120));
        }
        assert!(
            rules(&d).is_empty(),
            "moves in the first minutes are SteamVR settling"
        );
        d.ingest(&moved(T0 + SETTLE_MS + 1, 30));
        d.ingest(&moved(T0 + SETTLE_MS + 2, 60));
        assert!(rules(&d).is_empty());
        d.ingest(&moved(T0 + SETTLE_MS + 3, 300));
        let fired = d.ingest(&moved(T0 + SETTLE_MS + 4, 80));
        assert_eq!(fired, vec![Rule::StaleMap]);
        let f = &d.findings(&label)[0];
        assert!(f.what.contains("3 big moves, up to 300 mm"), "{}", f.what);
        assert!(
            d.ingest(&moved(T0 + SETTLE_MS + 5, 80)).is_empty(),
            "fires once"
        );
    }

    #[test]
    fn a_new_run_restarts_the_settling_window() {
        let mut d = Diagnosis::default();
        d.ingest(&moved(T0, 10));
        d.ingest(&ev(T0 + SETTLE_MS, None, Kind::LogRotated));
        for i in 1..=4 {
            d.ingest(&moved(T0 + SETTLE_MS + i, 200));
        }
        assert!(rules(&d).is_empty());
    }

    #[test]
    fn board_faults_need_a_cluster_on_one_device() {
        let mut d = Diagnosis::default();
        d.ingest(&ev(
            T0,
            Some("LHR-A66FAA1B"),
            Kind::DongleBind {
                dongle: "BC1B52F144".into(),
            },
        ));
        d.ingest(&ev(
            T0 + 1000,
            None,
            Kind::BoardUnreachable {
                dongle: "BC1B52F144".into(),
            },
        ));
        d.ingest(&ev(T0 + 2000, Some("LHR-FD4FF7E2"), Kind::ImuMissing));
        d.ingest(&ev(
            T0 + BOARD_WINDOW_MS + 5000,
            None,
            Kind::BoardUnreachable {
                dongle: "BC1B52F144".into(),
            },
        ));
        assert!(
            rules(&d).is_empty(),
            "two faults five minutes apart are not a cluster"
        );
        d.ingest(&ev(
            T0 + BOARD_WINDOW_MS + 6000,
            Some("LHR-A66FAA1B"),
            Kind::DeviceConfigFail {
                dongle: "BC1B52F144".into(),
            },
        ));
        let fired = d.ingest(&ev(
            T0 + BOARD_WINDOW_MS + 7000,
            Some("LHR-A66FAA1B"),
            Kind::ImuMissing,
        ));
        assert_eq!(fired, vec![Rule::BoardFault]);
        let f = d.findings(&label);
        assert_eq!(f.len(), 1);
        assert!(f[0].what.starts_with("LHR-A66FAA1B wasn't answering: its dongle got radio data but couldn't reach its main board 2 times, SteamVR couldn't read its config 1 time and no motion sensor data came through 1 time"), "{}", f[0].what);
    }

    fn bind(t_ms: u64, dev: &str, dongle: &str) -> SignalEvent {
        ev(
            t_ms,
            Some(dev),
            Kind::DongleBind {
                dongle: dongle.into(),
            },
        )
    }

    #[test]
    fn a_device_taking_another_ones_dongle_is_flagged_until_it_repairs() {
        let mut d = Diagnosis::default();
        d.ingest(&bind(T0, "LHR-FD4FF7E2", "DDF8F5F1F6"));
        d.ingest(&bind(T0 + 1, "LHR-7E902102", "0C58EE58E6"));
        d.ingest(&bind(T0 + 2, "LHR-FD4FF7E2", "DDF8F5F1F6"));
        assert!(
            rules(&d).is_empty(),
            "reconnecting to your own dongle is fine"
        );
        let fired = d.ingest(&bind(T0 + 3, "LHR-FD4FF7E2", "0C58EE58E6"));
        assert_eq!(fired, vec![Rule::Takeover]);
        let f = &d.findings(&label)[0];
        assert!(
            f.what
                .contains("LHR-FD4FF7E2 connected through dongle 0C58EE58E6")
        );
        assert!(f.fix.contains("pair LHR-7E902102 again"));
        d.ingest(&bind(T0 + 4, "LHR-7E902102", "DDF8F5F1F6"));
        let f = &d.findings(&label);
        assert_eq!(
            f.len(),
            1,
            "taking the dongle the other one left is not a second takeover"
        );
        assert!(
            f[0].what.ends_with("It got dongle DDF8F5F1F6 later."),
            "{}",
            f[0].what
        );
        assert!(f[0].fix.is_empty());
    }

    #[test]
    fn sync_mismatches_count_only_in_bursts() {
        let mut d = Diagnosis::default();
        for i in 0..30 {
            d.ingest(&ev(T0 + i * 60_000, Some("LHR-A"), Kind::SyncMismatch));
        }
        assert!(rules(&d).is_empty(), "one a minute is background noise");
        for i in 0..20 {
            d.ingest(&ev(
                T0 + 40 * 60_000 + i * 1000,
                Some("LHR-B"),
                Kind::SyncMismatch,
            ));
        }
        assert_eq!(rules(&d), vec![Rule::SyncMismatch]);
        let f = &d.findings(&label)[0];
        assert!(
            f.what.contains("50 times (30 on LHR-A, 20 on LHR-B)"),
            "{}",
            f.what
        );
        assert!(f.fix.starts_with("Fix: look for mirrors"));
    }

    #[test]
    fn base_restarts_and_signal_faults_fire_at_once() {
        let mut d = Diagnosis::default();
        let fired = d.ingest(&ev(
            T0,
            None,
            Kind::BaseReset {
                base: "9417E0B3".into(),
            },
        ));
        assert_eq!(fired, vec![Rule::BaseRestart]);
        d.ingest(&ev(T0 + 1, Some("LHR-A"), Kind::ChannelConflict { ch: 3 }));
        d.ingest(&ev(T0 + 2, None, Kind::RotorPhase));
        let f = d.findings(&label);
        assert_eq!(f.len(), 3);
        assert!(f[0].what.starts_with("Base 9417E0B3 restarted."));
        assert!(f[1].what.contains("channel 3 (1 time)"));
        assert!(f[2].what.contains("phase lock 1 time"));
    }

    #[test]
    fn clock_comes_from_the_log_line() {
        let mut d = Diagnosis::default();
        let mut e = ev(
            T0,
            None,
            Kind::BaseReset {
                base: "9417E0B3".into(),
            },
        );
        e.detail =
            "Wed Oct 14 2026 00:26:05.120 [Info] - lighthouse: Base 9417E0B3 reset count changed."
                .into();
        d.ingest(&e);
        assert!(
            d.findings(&label)[0]
                .what
                .starts_with("Base 9417E0B3 restarted at 00:26.")
        );
    }
}

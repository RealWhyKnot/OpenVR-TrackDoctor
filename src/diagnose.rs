use crate::bases::{Relations, SETTLE_MS, Turned, is_big, short_serial};
use crate::event::{DriverStatus, Kind, SignalEvent, Source};
use crate::sight::Sight;
use crate::signals::logtail::naive_ms;
use crate::summary::plural;
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub const STALE_MOVES: usize = 3;
pub const BOARD_WINDOW_MS: u64 = 5 * 60_000;
pub const BOARD_FAULTS: usize = 3;
pub const EPISODE_MS: u64 = 1000;
pub const SYNC_WINDOW_MS: u64 = 10 * 60_000;
pub const SYNC_BURST: usize = 20;
pub const RELOCK_MS: u64 = 3 * 60_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rule {
    StaleMap,
    BaseTurned,
    BoardFault,
    Takeover,
    BaseRestart,
    SyncMismatch,
    BaseSignal,
    HardBase,
    RoomCheck,
    Update,
    Shaky,
    SlowRestart,
    DriverFailed,
    Battery,
    UsbDrop,
    RadioLink,
    RadioDrops,
    Facing,
    DeviceFault,
    BaseLayout,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Finding {
    pub rule: Rule,
    pub key: String,
    pub title: String,
    pub what: String,
    pub steps: Vec<String>,
}

impl Finding {
    pub fn line(&self) -> String {
        if self.steps.is_empty() {
            format!("{}: {}", self.title, self.what)
        } else {
            format!(
                "{}: {} Try: {}",
                self.title,
                self.what,
                self.steps.join(" ")
            )
        }
    }

    pub fn resets_map(&self) -> bool {
        matches!(
            self.rule,
            Rule::StaleMap | Rule::BaseTurned | Rule::BaseRestart
        ) || (self.rule == Rule::RoomCheck
            && matches!(self.key.as_str(), "101" | "102" | "201" | "202"))
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

impl Board {
    fn push(&mut self, stamp: Stamp) -> bool {
        if self
            .times
            .last()
            .is_some_and(|l| stamp.t_ms.saturating_sub(l.t_ms) < EPISODE_MS)
        {
            return false;
        }
        self.times.push(stamp);
        true
    }
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
    run: usize,
    late_moves: Vec<(Stamp, String, u32, usize)>,
    late_disallowed: usize,
    resets: BTreeMap<String, Vec<Stamp>>,
    boards: BTreeMap<String, Board>,
    owners: HashMap<String, String>,
    takeovers: Vec<Takeover>,
    sync: BTreeMap<String, usize>,
    sync_times: Vec<u64>,
    sync_all: Vec<u64>,
    sync_total: usize,
    sync_peak: usize,
    channels: BTreeMap<u32, usize>,
    rotor: usize,
    relations: Relations,
    pub sight: Sight,
    hard: Option<u32>,
    room: Option<u32>,
    updates: BTreeMap<String, bool>,
    pub drivers: BTreeMap<String, DriverStatus>,
    fired: BTreeSet<(Rule, String)>,
    pub jitter: crate::check_jitter::Jitter,
    restarts: crate::check_restart::Restarts,
    power: crate::check_battery::Power,
    usb: crate::check_usb::UsbHealth,
    radio: crate::check_radio::Radio,
    pub spots: crate::check_spots::Spots,
    health: crate::check_device::DeviceHealth,
    pub layout: crate::check_layout::Layout,
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
    pub fn ingest(&mut self, ev: &SignalEvent) -> bool {
        self.relations.ingest(ev);
        self.sight.ingest(ev);
        self.layout.ingest(ev);
        let checks = [
            self.jitter.ingest(ev),
            self.restarts.ingest(ev),
            self.power.ingest(ev),
            self.usb.ingest(ev),
            self.radio.ingest(ev),
            self.health.ingest(ev),
            matches!(ev.kind, Kind::TrackingState { .. }) && self.spots.check(),
        ];
        self.ingest_rules(ev) || checks.contains(&true)
    }

    fn ingest_rules(&mut self, ev: &SignalEvent) -> bool {
        if ev.kind == Kind::LogRotated {
            self.run_start = None;
            self.run += 1;
            self.owners.clear();
            self.sync_times.clear();
            self.drivers.clear();
            return false;
        }
        if ev.source == Source::Log {
            self.run_start.get_or_insert(ev.t_ms);
        }
        let settled = self
            .run_start
            .is_some_and(|s| ev.t_ms.saturating_sub(s) >= SETTLE_MS);
        let stamp = Stamp::of(ev);
        match &ev.kind {
            Kind::BaseMoved { base, mm, deg } if settled && is_big(*mm, *deg) => {
                self.late_moves
                    .push((stamp, short_serial(base), *mm, self.run));
            }
            Kind::DbDisallowed if settled => self.late_disallowed += 1,
            Kind::BaseReset { base } => {
                self.resets
                    .entry(short_serial(base))
                    .or_default()
                    .push(stamp);
            }
            Kind::BoardUnreachable { dongle } | Kind::DeviceConfigFail { dongle } => {
                let Some(dev) = ev
                    .device
                    .clone()
                    .or_else(|| self.owners.get(dongle).cloned())
                else {
                    return false;
                };
                let b = self.boards.entry(dev).or_default();
                if !b.push(stamp) {
                    return false;
                }
                if matches!(ev.kind, Kind::BoardUnreachable { .. }) {
                    b.unreachable += 1;
                } else {
                    b.config += 1;
                }
            }
            Kind::ImuMissing => {
                let Some(dev) = &ev.device else {
                    return false;
                };
                let b = self.boards.entry(dev.clone()).or_default();
                if !b.push(stamp) {
                    return false;
                }
                b.imu += 1;
            }
            Kind::DongleBind { dongle } => {
                let Some(dev) = &ev.device else {
                    return false;
                };
                self.bind(dev, dongle, stamp);
            }
            Kind::SyncMismatch => {
                if let Some(dev) = &ev.device {
                    *self.sync.entry(dev.clone()).or_default() += 1;
                }
                self.sync_total += 1;
                self.sync_times.push(ev.t_ms);
                self.sync_all.push(ev.t_ms);
                self.sync_peak = self.sync_peak.max(burst(&self.sync_times, SYNC_WINDOW_MS));
            }
            Kind::ChannelConflict { ch } => *self.channels.entry(*ch).or_default() += 1,
            Kind::RotorPhase => self.rotor += 1,
            Kind::BaseRelation { .. } => {}
            Kind::BaseSight { .. } => {
                let hard = self.sight.hard().map(|h| h.base.ch);
                if hard == self.hard {
                    return false;
                }
                self.hard = hard;
            }
            Kind::RoomCheck { code } => {
                if self.room == Some(*code) {
                    return false;
                }
                self.room = Some(*code);
            }
            Kind::UpdateAvailable { manual, waiting } => {
                let Some(dev) = &ev.device else {
                    return false;
                };
                if !*waiting {
                    return self.updates.remove(dev).is_some();
                }
                if self.updates.insert(dev.clone(), *manual) == Some(*manual) {
                    return false;
                }
            }
            Kind::BaseLayout { .. } => self.layout.audited(&ev.kind),
            Kind::Driver { name, status } => {
                let prev = self.drivers.insert(name.clone(), status.clone());
                return crate::check_software::changed(name, prev.as_ref(), status);
            }
            _ => return false,
        }
        true
    }

    pub fn setup_notes(&self) -> Vec<String> {
        const MOVERS: [&str; 5] = [
            "spacecalibrator",
            "smoothtracking",
            "gaptrack",
            "inputemulator",
            "wkopenvr",
        ];
        let mut on: Vec<String> = Vec::new();
        let mut off: Vec<String> = Vec::new();
        let mut failed: Vec<String> = Vec::new();
        for (name, status) in &self.drivers {
            match status {
                DriverStatus::Loaded(_) => on.push(name.clone()),
                DriverStatus::Disabled => off.push(name.clone()),
                DriverStatus::Failed(e) => failed.push(format!("{name} ({e})")),
            }
        }
        let movers: Vec<String> = on
            .iter()
            .filter(|n| {
                let n = n.to_ascii_lowercase();
                MOVERS.iter().any(|m| n.contains(m))
            })
            .cloned()
            .collect();
        let mut out = Vec::new();
        if !movers.is_empty() {
            out.push(format!(
                "Add-ons that change where SteamVR puts your devices are on: {}. If positions look offset or jumpy, turn them off in SteamVR's settings (Startup / Shutdown, Manage Add-ons) and compare.",
                join(&movers)
            ));
        }
        if !on.is_empty() {
            out.push(format!("SteamVR drivers loaded: {}.", on.join(", ")));
        }
        if !off.is_empty() {
            out.push(format!("Turned off in SteamVR: {}.", off.join(", ")));
        }
        if !failed.is_empty() {
            out.push(format!("SteamVR couldn't load: {}.", failed.join(", ")));
        }
        out
    }

    pub fn fresh(&mut self, found: Vec<Finding>) -> Vec<Finding> {
        found
            .into_iter()
            .filter(|f| self.fired.insert((f.rule, f.key.clone())))
            .collect()
    }

    fn stale_moves(&self) -> Vec<&(Stamp, String, u32, usize)> {
        let mut per_run: BTreeMap<usize, usize> = BTreeMap::new();
        for m in &self.late_moves {
            *per_run.entry(m.3).or_default() += 1;
        }
        self.late_moves
            .iter()
            .filter(|m| per_run[&m.3] >= STALE_MOVES)
            .collect()
    }

    fn turned(&self) -> Vec<Turned> {
        self.relations.turned_bases()
    }

    fn after_restart(&self) -> Vec<(String, usize)> {
        let mut out: Vec<(String, usize)> = Vec::new();
        for &t in &self.sync_all {
            let Some(base) = self.resets.iter().find_map(|(b, times)| {
                times
                    .iter()
                    .any(|r| t >= r.t_ms && t - r.t_ms <= RELOCK_MS)
                    .then_some(b)
            }) else {
                continue;
            };
            match out.iter_mut().find(|(b, _)| b == base) {
                Some(o) => o.1 += 1,
                None => out.push((base.clone(), 1)),
            }
        }
        out
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
        if !self.stale_moves().is_empty() {
            out.push(Rule::StaleMap);
        }
        if !self.turned().is_empty() {
            out.push(Rule::BaseTurned);
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
        if self.sight.hard().is_some() {
            out.push(Rule::HardBase);
        }
        if self.room.is_some_and(|c| c > 1) {
            out.push(Rule::RoomCheck);
        }
        if !self.updates.is_empty() {
            out.push(Rule::Update);
        }
        if self.jitter.active() {
            out.push(Rule::Shaky);
        }
        if self.restarts.active() {
            out.push(Rule::SlowRestart);
        }
        if crate::check_software::any_failed(&self.drivers) {
            out.push(Rule::DriverFailed);
        }
        if !self.power.flat().is_empty() {
            out.push(Rule::Battery);
        }
        if self.usb.any() {
            out.push(Rule::UsbDrop);
        }
        if !self.radio.cutouts().is_empty() {
            out.push(Rule::RadioLink);
        }
        if !self.radio.links().is_empty() {
            out.push(Rule::RadioDrops);
        }
        if !self.spots.found().is_empty() {
            out.push(Rule::Facing);
        }
        if !self
            .health
            .faults(&|d| self.faulty_boards().any(|(b, _)| b == d))
            .is_empty()
        {
            out.push(Rule::DeviceFault);
        }
        if !self.layout.problems().is_empty() {
            out.push(Rule::BaseLayout);
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
        let finding = |key: &str, title: String, what: String, steps: &[&str]| Finding {
            rule,
            key: key.to_string(),
            title,
            what,
            steps: steps.iter().map(|s| s.to_string()).collect(),
        };
        const RESET: &str = "Reset the base station map with the button below. TrackDoctor keeps the old map as a backup and SteamVR builds a new one.";
        match rule {
            Rule::StaleMap => {
                let moves = self.stale_moves();
                let max = moves.iter().map(|m| m.2).max().unwrap_or(0);
                let mut bases: Vec<&str> = moves.iter().map(|m| m.1.as_str()).collect();
                bases.sort();
                bases.dedup();
                let last = moves
                    .last()
                    .and_then(|m| m.0.clock())
                    .map_or_else(String::new, |c| format!(", the last at {c}"));
                let first = moves.first().map_or(0, |m| m.0.t_ms);
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
                let turned: Vec<String> = self.turned().into_iter().map(|t| t.base).collect();
                let culprit = match turned.as_slice() {
                    [] => " SteamVR moves whichever base sits farther from the origin base, so the one that shifted may be another.".to_string(),
                    [one] => format!(" Base {one} is the one that shifted."),
                    many => format!(" Bases {} are the ones that shifted.", join(many)),
                };
                vec![finding(
                    "",
                    "SteamVR's base station map is out of date".into(),
                    format!(
                        "More than 10 minutes in, SteamVR was still moving {} in its saved map ({}, up to {max} mm{last}).{after}{refused} A base was moved, re-aimed or bumped while SteamVR kept its old position, or one sits loose and keeps shifting.{culprit}",
                        if bases.len() == 1 {
                            format!("base {}", bases[0])
                        } else {
                            format!("bases {}", bases.join(", "))
                        },
                        plural(moves.len(), "big move"),
                    ),
                    &["Make sure every base is in its final place and mounted firmly.", RESET],
                )]
            }
            Rule::BaseTurned => self
                .turned()
                .into_iter()
                .map(|t| {
                    finding(
                        &t.base,
                        format!("Base {} turned on its mount", t.base),
                        format!("{} Its mount is probably sagging, or it got knocked.", t.what()),
                        &["Tighten its mount or move it somewhere solid.", RESET],
                    )
                })
                .collect(),
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
                        dev,
                        format!("{} keeps losing its radio link", label(dev)),
                        format!(
                            "{} wasn't answering: {}{span}. SteamVR logs these when packets between the dongle and the device get lost, so its radio link is weak.",
                            label(dev),
                            join(&parts)
                        ),
                        &[
                            "Plug its dongle straight into the PC rather than a hub, or put it on a USB extension cable out in the open where you play.",
                            "Turn the device off and on again.",
                            "If it keeps happening on another dongle, the device itself is failing.",
                        ],
                    )
                })
                .collect(),
            Rule::Takeover => self
                .takeovers
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    let key = i.to_string();
                    let from = label(&t.from);
                    let what = format!(
                        "{} connected through dongle {}{}, which {from} had been using, so it lost its connection.",
                        label(&t.to),
                        t.dongle,
                        at(&t.at)
                    );
                    match &t.rebound {
                        Some((d, s)) => finding(
                            &key,
                            format!("{from} switched dongles"),
                            format!(
                                "{what} It got dongle {d}{}.",
                                s.clock().map_or_else(|| " later".to_string(), |c| format!(" at {c}"))
                            ),
                            &[],
                        ),
                        None => finding(
                            &key,
                            format!("{from} lost its dongle to another device"),
                            what,
                            &[&format!("Pair {from} again, with only that device in pairing mode.")],
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
                        base,
                        format!("Base {base} restarted"),
                        format!("{what} SteamVR logs this when a base loses power or gets bumped."),
                        &[
                            "Check its power plug and adapter, and that nothing knocks it.",
                            "If it moved, reset the base station map with the button below.",
                        ],
                    )
                })
                .collect(),
            Rule::SyncMismatch => {
                let total = self.sync_total;
                let mut by: Vec<(&String, &usize)> = self.sync.iter().collect();
                by.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
                let list: Vec<String> = by
                    .iter()
                    .take(4)
                    .map(|(d, n)| format!("{n} on {}", label(d)))
                    .collect();
                let restarts = self.after_restart();
                let relock: usize = restarts.iter().map(|r| r.1).sum();
                let after = match restarts.as_slice() {
                    [] => String::new(),
                    [(b, n)] => format!(
                        " {n} of them came within 3 minutes of base {b} restarting, while trackers locked back on to it."
                    ),
                    many => format!(
                        " {relock} of them came within 3 minutes of bases {} restarting, while trackers locked back on to them.",
                        join(&many.iter().map(|r| r.0.clone()).collect::<Vec<_>>())
                    ),
                };
                let steps: &[&str] = if self.active().contains(&Rule::StaleMap) {
                    &[
                        "Reset the base map first. These bursts came with it.",
                        "If they keep coming after that, look for mirrors, windows or glossy surfaces the bases can see.",
                    ]
                } else if relock * 2 >= total {
                    &["Keep the restarting base powered steadily. Check its plug and adapter, and that nothing knocks it."]
                } else {
                    &["Look for mirrors, windows or glossy surfaces the bases can see and cover them one at a time."]
                };
                vec![finding(
                    "",
                    "Base station light arrived at the wrong time".into(),
                    format!(
                        "SteamVR couldn't match a light sync to the base it expected {} ({}), up to {} within 10 minutes. It logs this when a base's sync arrives more than 1 ms earlier or later than its last frame predicted.{after}",
                        plural(total, "time"),
                        list.join(", "),
                        self.sync_peak
                    ),
                    steps,
                )]
            }
            Rule::BaseSignal => {
                let mut out: Vec<Finding> = self
                    .channels
                    .iter()
                    .map(|(ch, n)| {
                        finding(
                            &format!("ch{ch}"),
                            format!("Two base stations share channel {ch}"),
                            format!(
                                "Two base stations were sending on channel {ch} ({}).",
                                plural(*n, "time")
                            ),
                            &["Give each base station its own channel."],
                        )
                    })
                    .collect();
                if self.rotor > 0 {
                    out.push(finding(
                        "rotor",
                        "A 1.0 base station lost its sync".into(),
                        format!(
                            "A 1.0 base station's rotors fell out of phase lock {}.",
                            plural(self.rotor, "time")
                        ),
                        &["Make sure the bases can see each other, or connect the sync cable."],
                    ));
                }
                out
            }
            Rule::HardBase => {
                let Some(h) = self.sight.hard() else {
                    return Vec::new();
                };
                let name = crate::sight::base_name(h.base.ch, h.base.id.as_ref());
                let others: Vec<f32> = self
                    .sight
                    .bases()
                    .iter()
                    .filter(|b| b.ch != h.base.ch)
                    .map(|b| b.share)
                    .collect();
                let view = if others.is_empty() || h.base.share <= 0.0 {
                    String::new()
                } else {
                    format!(
                        " Your devices had it in view {:.0}% of the time, against {:.0}% for the other bases on average.",
                        h.base.share * 100.0,
                        others.iter().sum::<f32>() / others.len() as f32 * 100.0
                    )
                };
                let mut steps = Vec::new();
                if let Some(id) = &h.base.id {
                    steps.push(format!(
                        "Find it on the Room page, where it's listed as LHB-{id}, to see which base it is."
                    ));
                }
                steps.extend([
                    "Check that nothing blocks its view of where you play, like a fan, shelf, screen, curtain or furniture.".to_string(),
                    "Aim it at the middle of the play space, tilted down a little.".to_string(),
                    "Mount it above head height, so your body blocks it less when you turn.".to_string(),
                ]);
                if let Some((dev, n)) = h.base.worst.first()
                    && *n * 2 >= h.base.drops
                {
                    steps.push(format!(
                        "{} lost it {n} of those times. If it's worn on your back or hip, it may face away from this base most of the time.",
                        label(dev)
                    ));
                }
                vec![Finding {
                    rule,
                    key: h.base.ch.to_string(),
                    title: format!("Your devices lose sight of base {name} the most"),
                    what: format!(
                        "Your trackers and controllers lost sight of base {name} {} times. {}{}{view}",
                        h.base.drops,
                        if h.others <= 1 {
                            "Your other bases were hardly ever lost.".to_string()
                        } else {
                            format!(
                                "Your other bases were typically lost about {} times each, so this one was lost about {:.1} times as often.",
                                h.others, h.ratio
                            )
                        },
                        if h.of == 0 {
                            String::new()
                        } else {
                            format!(
                                " It was the base lost most often for {} of {} devices.",
                                h.devices, h.of
                            )
                        }
                    ),
                    steps,
                }]
            }
            Rule::RoomCheck => {
                let Some(code) = self.room.filter(|c| *c > 1) else {
                    return Vec::new();
                };
                let steps: &[&str] = match code {
                    101 | 102 | 201 | 202 => &[
                        "If you moved a base on purpose, reset the base station map with the button below, or run Room Setup again.",
                        "If you didn't, check that every base is mounted firmly and has power.",
                    ],
                    _ => &["Run Room Setup again in SteamVR."],
                };
                vec![finding(
                    &code.to_string(),
                    "SteamVR's room setup needs attention".into(),
                    format!("{}.", crate::event::room_check_text(code)),
                    steps,
                )]
            }
            Rule::Update => {
                if self.updates.is_empty() {
                    return Vec::new();
                }
                let names: Vec<String> = self.updates.keys().map(|d| label(d)).collect();
                let manual = self.updates.values().any(|m| *m);
                let one = names.len() == 1;
                let steps: &[&str] = match (manual, one) {
                    (false, true) => &["Plug it into the PC with a USB cable, and SteamVR offers the update."],
                    (false, false) => &["Plug each one into the PC with a USB cable, and SteamVR offers the update."],
                    (true, true) => &[
                        "Plug it into the PC with a USB cable.",
                        "SteamVR says this update has to be installed by hand, so follow the instructions it shows for the device.",
                    ],
                    (true, false) => &[
                        "Plug each one into the PC with a USB cable.",
                        "SteamVR says at least one of them has to be updated by hand, so follow the instructions it shows for that device.",
                    ],
                };
                vec![finding(
                    "updates",
                    if names.len() == 1 {
                        "A firmware update is waiting".into()
                    } else {
                        "Firmware updates are waiting".into()
                    },
                    format!(
                        "SteamVR says {} {} a firmware update waiting.",
                        join(&names),
                        if names.len() == 1 { "has" } else { "have" }
                    ),
                    steps,
                )]
            }
            Rule::Shaky => self.jitter.explain(label),
            Rule::SlowRestart => self.restarts.findings(label),
            Rule::DriverFailed => crate::check_software::findings(&self.drivers),
            Rule::Battery => crate::check_battery::findings(&self.power, label),
            Rule::UsbDrop => self.usb.findings(rule, label),
            Rule::RadioLink => crate::check_radio::findings(&self.radio, label),
            Rule::RadioDrops => crate::check_radio::led_findings(&self.radio, label),
            Rule::Facing => self.spots.explain(label),
            Rule::BaseLayout => crate::check_layout::explain(&self.layout),
            Rule::DeviceFault => crate::check_device::explain(&self.health, rule, label, &|d| {
                self.faulty_boards().any(|(b, _)| b == d)
            }),
        }
    }
}

pub fn join(parts: &[String]) -> String {
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

    fn feed(d: &mut Diagnosis, e: &SignalEvent) -> Vec<Rule> {
        d.ingest(e);
        let found = d.findings(&label);
        d.fresh(found).into_iter().map(|f| f.rule).collect()
    }

    #[test]
    fn repeated_lines_of_one_attempt_are_one_episode() {
        let mut d = Diagnosis::default();
        for ms in [0, 5, 11] {
            feed(&mut d, &ev(T0 + ms, Some("LHR-A66FAA1B"), Kind::ImuMissing));
        }
        assert!(
            rules(&d).is_empty(),
            "one bootstrap attempt logs three lines"
        );
        feed(
            &mut d,
            &ev(T0 + 20_000, Some("LHR-A66FAA1B"), Kind::ImuMissing),
        );
        let fired = feed(
            &mut d,
            &ev(T0 + 40_000, Some("LHR-A66FAA1B"), Kind::ImuMissing),
        );
        assert_eq!(fired, vec![Rule::BoardFault]);
        assert!(d.findings(&label)[0].what.contains("came through 3 times"));
    }

    #[test]
    fn every_new_finding_gets_its_own_notice() {
        let mut d = Diagnosis::default();
        let reset = |t: u64, base: &str| ev(t, None, Kind::BaseReset { base: base.into() });
        assert_eq!(
            feed(&mut d, &reset(T0, "9417E0B3")),
            vec![Rule::BaseRestart]
        );
        assert!(feed(&mut d, &reset(T0 + 1, "9417E0B3")).is_empty());
        assert_eq!(
            feed(&mut d, &reset(T0 + 2, "9D8DEA7B")),
            vec![Rule::BaseRestart]
        );
    }

    #[test]
    fn late_moves_only_count_within_one_run() {
        let mut d = Diagnosis::default();
        feed(&mut d, &moved(T0, 10));
        feed(&mut d, &moved(T0 + SETTLE_MS, 200));
        feed(&mut d, &moved(T0 + SETTLE_MS + 1, 200));
        feed(&mut d, &ev(T0 + SETTLE_MS + 2, None, Kind::LogRotated));
        feed(&mut d, &moved(T0 + SETTLE_MS + 3, 10));
        feed(&mut d, &moved(T0 + 2 * SETTLE_MS + 3, 200));
        assert!(
            rules(&d).is_empty(),
            "two late moves in one run and one in the next"
        );
    }

    #[test]
    fn settling_waits_for_steamvr_not_for_trackdoctor() {
        let mut d = Diagnosis::default();
        let usb = SignalEvent::at(
            T0,
            Source::Usb,
            None,
            Kind::UsbAttach {
                port: "1-2".into(),
                dongle: None,
            },
            "",
        );
        feed(&mut d, &usb);
        for i in 0..4 {
            feed(&mut d, &moved(T0 + 2 * SETTLE_MS + i, 200));
        }
        assert!(
            rules(&d).is_empty(),
            "SteamVR's first log line starts the clock"
        );
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

    fn relations(d: &mut Diagnosis, t0: u64, turn: f32) -> Vec<Rule> {
        let rel = |a: &str, b: &str, mm: u32, deg: f32| Kind::BaseRelation {
            a: a.into(),
            b: b.into(),
            mm,
            deg,
        };
        let mut fired = Vec::new();
        for (i, (a, b, mm, deg)) in [
            ("C538EFC9", "9417E0B3", 3530, 121.7),
            ("9417E0B3", "9D8DEA7B", 2430, 62.8),
            ("C538EFC9", "9D8DEA7B", 4270, 58.3),
        ]
        .into_iter()
        .enumerate()
        {
            for (late, d0) in [(false, 0.0), (true, turn)] {
                for k in 0..2 {
                    let t = t0 + u64::from(late) * 20 * 60_000 + k * 60_000 + i as u64;
                    let deg = if b == "9D8DEA7B" && a == "C538EFC9" {
                        deg
                    } else {
                        deg + d0
                    };
                    fired.extend(feed(d, &ev(t, Some("LHR-A"), rel(a, b, mm, deg))));
                }
            }
        }
        fired
    }

    #[test]
    fn a_turned_base_is_named_once_and_in_the_stale_map_finding() {
        let mut d = Diagnosis::default();
        feed(&mut d, &moved(T0, 10));
        assert!(relations(&mut d, T0, 2.0).is_empty());
        let mut d = Diagnosis::default();
        feed(&mut d, &moved(T0, 10));
        assert_eq!(relations(&mut d, T0, 5.0), vec![Rule::BaseTurned]);
        for i in 0..3 {
            feed(&mut d, &moved(T0 + 30 * 60_000 + i, 200));
        }
        let f = d.findings(&label);
        assert!(
            f[0].what
                .ends_with("Base 9417E0B3 is the one that shifted."),
            "{}",
            f[0].what
        );
        assert!(
            f[1].what.starts_with("Base 9417E0B3 turned about 5 deg"),
            "{}",
            f[1].what
        );
        feed(&mut d, &ev(T0 + 40 * 60_000, None, Kind::LogRotated));
        assert!(relations(&mut d, T0 + 41 * 60_000, 8.0).is_empty());
        let turned: Vec<Finding> = d
            .findings(&label)
            .into_iter()
            .filter(|f| f.rule == Rule::BaseTurned)
            .collect();
        assert_eq!(turned.len(), 1);
        assert!(turned[0].what.contains("about 8 deg"), "{}", turned[0].what);
    }

    #[test]
    fn early_moves_are_settling_and_late_ones_mean_a_stale_map() {
        let mut d = Diagnosis::default();
        for i in 0..9 {
            feed(&mut d, &moved(T0 + i * 20_000, 120));
        }
        assert!(
            rules(&d).is_empty(),
            "moves in the first minutes are SteamVR settling"
        );
        feed(&mut d, &moved(T0 + SETTLE_MS + 1, 30));
        feed(&mut d, &moved(T0 + SETTLE_MS + 2, 60));
        assert!(rules(&d).is_empty());
        feed(&mut d, &moved(T0 + SETTLE_MS + 3, 300));
        let fired = feed(&mut d, &moved(T0 + SETTLE_MS + 4, 80));
        assert_eq!(fired, vec![Rule::StaleMap]);
        let f = &d.findings(&label)[0];
        assert!(f.what.contains("3 big moves, up to 300 mm"), "{}", f.what);
        assert!(
            feed(&mut d, &moved(T0 + SETTLE_MS + 5, 80)).is_empty(),
            "fires once"
        );
    }

    #[test]
    fn a_new_run_restarts_the_settling_window() {
        let mut d = Diagnosis::default();
        feed(&mut d, &moved(T0, 10));
        feed(&mut d, &ev(T0 + SETTLE_MS, None, Kind::LogRotated));
        for i in 1..=4 {
            feed(&mut d, &moved(T0 + SETTLE_MS + i, 200));
        }
        assert!(rules(&d).is_empty());
    }

    #[test]
    fn board_faults_need_a_cluster_on_one_device() {
        let mut d = Diagnosis::default();
        feed(
            &mut d,
            &ev(
                T0,
                Some("LHR-A66FAA1B"),
                Kind::DongleBind {
                    dongle: "BC1B52F144".into(),
                },
            ),
        );
        feed(
            &mut d,
            &ev(
                T0 + 1000,
                None,
                Kind::BoardUnreachable {
                    dongle: "BC1B52F144".into(),
                },
            ),
        );
        feed(
            &mut d,
            &ev(T0 + 2000, Some("LHR-FD4FF7E2"), Kind::ImuMissing),
        );
        feed(
            &mut d,
            &ev(
                T0 + BOARD_WINDOW_MS + 5000,
                None,
                Kind::BoardUnreachable {
                    dongle: "BC1B52F144".into(),
                },
            ),
        );
        assert!(
            rules(&d).is_empty(),
            "two faults five minutes apart are not a cluster"
        );
        feed(
            &mut d,
            &ev(
                T0 + BOARD_WINDOW_MS + 6000,
                Some("LHR-A66FAA1B"),
                Kind::DeviceConfigFail {
                    dongle: "BC1B52F144".into(),
                },
            ),
        );
        let fired = feed(
            &mut d,
            &ev(
                T0 + BOARD_WINDOW_MS + 7000,
                Some("LHR-A66FAA1B"),
                Kind::ImuMissing,
            ),
        );
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
        feed(&mut d, &bind(T0, "LHR-FD4FF7E2", "DDF8F5F1F6"));
        feed(&mut d, &bind(T0 + 1, "LHR-7E902102", "0C58EE58E6"));
        feed(&mut d, &bind(T0 + 2, "LHR-FD4FF7E2", "DDF8F5F1F6"));
        assert!(
            rules(&d).is_empty(),
            "reconnecting to your own dongle is fine"
        );
        let fired = feed(&mut d, &bind(T0 + 3, "LHR-FD4FF7E2", "0C58EE58E6"));
        assert_eq!(fired, vec![Rule::Takeover]);
        let f = &d.findings(&label)[0];
        assert!(
            f.what
                .contains("LHR-FD4FF7E2 connected through dongle 0C58EE58E6")
        );
        assert_eq!(
            f.steps,
            vec!["Pair LHR-7E902102 again, with only that device in pairing mode."]
        );
        feed(&mut d, &bind(T0 + 4, "LHR-7E902102", "DDF8F5F1F6"));
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
        assert!(f[0].steps.is_empty());
    }

    #[test]
    fn sync_mismatches_count_only_in_bursts() {
        let mut d = Diagnosis::default();
        for i in 0..30 {
            feed(
                &mut d,
                &ev(T0 + i * 60_000, Some("LHR-A"), Kind::SyncMismatch),
            );
        }
        assert!(rules(&d).is_empty(), "one a minute is background noise");
        for i in 0..20 {
            feed(
                &mut d,
                &ev(
                    T0 + 40 * 60_000 + i * 1000,
                    Some("LHR-B"),
                    Kind::SyncMismatch,
                ),
            );
        }
        assert_eq!(rules(&d), vec![Rule::SyncMismatch]);
        let f = &d.findings(&label)[0];
        assert!(
            f.what.contains("50 times (30 on LHR-A, 20 on LHR-B)"),
            "{}",
            f.what
        );
        assert!(f.steps[0].starts_with("Look for mirrors"));
    }

    #[test]
    fn sync_bursts_after_a_base_restart_point_at_its_power() {
        let mut d = Diagnosis::default();
        feed(
            &mut d,
            &ev(
                T0,
                None,
                Kind::BaseReset {
                    base: "9417E0B3".into(),
                },
            ),
        );
        for i in 0..20 {
            feed(
                &mut d,
                &ev(T0 + 5000 + i * 1000, Some("LHR-A"), Kind::SyncMismatch),
            );
        }
        feed(
            &mut d,
            &ev(T0 + 10 * 60_000, Some("LHR-A"), Kind::SyncMismatch),
        );
        let f = d
            .findings(&label)
            .into_iter()
            .find(|f| f.rule == Rule::SyncMismatch)
            .unwrap();
        assert!(
            f.what
                .ends_with("20 of them came within 3 minutes of base 9417E0B3 restarting, while trackers locked back on to it."),
            "{}",
            f.what
        );
        assert!(f.steps[0].starts_with("Keep the restarting base powered"));
    }

    #[test]
    fn base_restarts_and_signal_faults_fire_at_once() {
        let mut d = Diagnosis::default();
        let fired = feed(
            &mut d,
            &ev(
                T0,
                None,
                Kind::BaseReset {
                    base: "9417E0B3".into(),
                },
            ),
        );
        assert_eq!(fired, vec![Rule::BaseRestart]);
        feed(
            &mut d,
            &ev(T0 + 1, Some("LHR-A"), Kind::ChannelConflict { ch: 3 }),
        );
        feed(&mut d, &ev(T0 + 2, None, Kind::RotorPhase));
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
        feed(&mut d, &e);
        assert!(
            d.findings(&label)[0]
                .what
                .starts_with("Base 9417E0B3 restarted at 00:26.")
        );
    }

    fn sight(t: u64, dev: &str, ch: u32, gained: bool) -> SignalEvent {
        let all = [1, 10, 15];
        ev(
            t,
            Some(dev),
            Kind::BaseSight {
                ch,
                gained,
                seeing: all.into_iter().filter(|c| gained || *c != ch).collect(),
                ids: vec![
                    (1, "9417E0B3".into()),
                    (10, "9D8DEA7B".into()),
                    (15, "B394A63C".into()),
                ],
            },
        )
    }

    #[test]
    fn a_base_lost_far_more_often_becomes_one_finding_with_steps() {
        let mut d = Diagnosis::default();
        let devs = ["LHR-7E902102", "LHR-905BD201", "LHR-617D30D7"];
        for dev in devs {
            for ch in [1, 10, 15] {
                feed(&mut d, &sight(T0, dev, ch, true));
            }
        }
        let mut fired = Vec::new();
        for i in 0..24u64 {
            let ch = match i % 8 {
                0 | 4 => 1,
                6 => 15,
                _ => 10,
            };
            for dev in devs {
                let t = T0 + 1_000 + i * 2_000;
                fired.extend(feed(&mut d, &sight(t, dev, ch, false)));
                fired.extend(feed(&mut d, &sight(t + 500, dev, ch, true)));
            }
        }
        assert_eq!(fired, vec![Rule::HardBase]);
        let f = d
            .findings(&label)
            .into_iter()
            .find(|f| f.rule == Rule::HardBase)
            .unwrap();
        assert!(f.title.contains("9D8DEA7B"), "{}", f.title);
        assert!(f.what.contains("45 times"), "{}", f.what);
        assert!(f.steps.len() >= 3);
    }

    #[test]
    fn room_setup_and_waiting_firmware_show_up_until_cleared() {
        let mut d = Diagnosis::default();
        assert!(feed(&mut d, &ev(T0, None, Kind::RoomCheck { code: 1 })).is_empty());
        assert_eq!(
            feed(&mut d, &ev(T0 + 1, None, Kind::RoomCheck { code: 101 })),
            vec![Rule::RoomCheck]
        );
        let room = d
            .findings(&label)
            .into_iter()
            .find(|f| f.rule == Rule::RoomCheck)
            .unwrap();
        assert!(room.resets_map());
        feed(&mut d, &ev(T0 + 2, None, Kind::RoomCheck { code: 1 }));
        assert!(rules(&d).iter().all(|r| *r != Rule::RoomCheck));
        assert_eq!(
            feed(
                &mut d,
                &ev(
                    T0 + 3,
                    Some("LHR-905BD201"),
                    Kind::UpdateAvailable {
                        manual: false,
                        waiting: true
                    }
                )
            ),
            vec![Rule::Update]
        );
        assert!(
            feed(
                &mut d,
                &ev(
                    T0 + 4,
                    Some("LHR-617D30D7"),
                    Kind::UpdateAvailable {
                        manual: false,
                        waiting: true
                    }
                )
            )
            .is_empty(),
            "a second device joins the same notice"
        );
        let f = d
            .findings(&label)
            .into_iter()
            .find(|f| f.rule == Rule::Update)
            .unwrap();
        assert_eq!(f.title, "Firmware updates are waiting");
        for (t, dev) in [(T0 + 5, "LHR-905BD201"), (T0 + 6, "LHR-617D30D7")] {
            feed(
                &mut d,
                &ev(
                    t,
                    Some(dev),
                    Kind::UpdateAvailable {
                        manual: false,
                        waiting: false,
                    },
                ),
            );
        }
        assert!(!rules(&d).contains(&Rule::Update));
    }

    #[test]
    fn add_ons_that_move_devices_are_named_in_the_setup_notes() {
        let mut d = Diagnosis::default();
        for (name, status) in [
            (
                "01spacecalibrator",
                DriverStatus::Loaded("D:\\calib".into()),
            ),
            ("lighthouse", DriverStatus::Loaded("C:\\lh".into())),
            ("SmoothTracking", DriverStatus::Disabled),
            (
                "VirtualDesktop",
                DriverStatus::Failed("VRInitError_Init_HmdNotFound(108)".into()),
            ),
        ] {
            d.ingest(&ev(
                T0,
                None,
                Kind::Driver {
                    name: name.into(),
                    status,
                },
            ));
        }
        let notes = d.setup_notes();
        assert!(notes[0].contains("01spacecalibrator"), "{notes:?}");
        assert!(!notes[0].contains("SmoothTracking"));
        assert!(
            notes
                .iter()
                .any(|n| n.contains("couldn't load: VirtualDesktop"))
        );
    }
}

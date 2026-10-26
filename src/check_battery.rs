use crate::diagnose::{Finding, Rule, join};
use crate::event::{Kind, SignalEvent};
use crate::signals::logtail::naive_ms;
use std::collections::BTreeMap;

pub const LOW_PCT: f32 = 25.0;
pub const EMPTY_PCT: f32 = 10.0;
pub const OFF_MS: u64 = 2_000;
pub const GROUP_MS: u64 = 1_000;
pub const CONFIRM_MS: u64 = 60_000;
const REPORT_EVENTS: u8 = 4;

#[derive(Clone, Debug)]
struct Loss {
    dev: String,
    t_ms: u64,
    pct: f32,
    local: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Flat {
    pub dev: String,
    pub pct: f32,
    pub times: usize,
    pub clocks: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Power {
    levels: BTreeMap<String, f32>,
    quiet: Vec<(u64, String)>,
    links: Vec<(u64, String)>,
    usb: Vec<u64>,
    pending: Vec<Loss>,
    confirmed: Vec<Loss>,
    now: u64,
    report: u8,
}

fn clock(local: Option<i64>) -> Option<String> {
    let rem = local?.rem_euclid(86_400_000);
    Some(format!("{:02}:{:02}", rem / 3_600_000, (rem / 60_000) % 60))
}

impl Power {
    pub fn ingest(&mut self, ev: &SignalEvent) -> bool {
        self.now = self.now.max(ev.t_ms);
        let dev = ev.device.as_deref().filter(|d| d.starts_with("LHR-"));
        match (&ev.kind, dev) {
            (Kind::BatteryLevel { pct }, Some(d)) => {
                self.levels.insert(d.to_string(), *pct);
            }
            (Kind::StandbyStart | Kind::DevicePowerOff, Some(d)) => {
                self.quiet.push((ev.t_ms, d.to_string()));
            }
            (Kind::UsbRemove { .. }, _) => self.usb.push(ev.t_ms),
            (Kind::WirelessDisconnect | Kind::DeviceDeactivated, Some(d)) => self.lost(ev, d),
            _ => {}
        }
        self.settle();
        let fresh = self.report > 0;
        self.report = self.report.saturating_sub(1);
        fresh
    }

    fn lost(&mut self, ev: &SignalEvent, dev: &str) {
        self.links.push((ev.t_ms, dev.to_string()));
        let local = naive_ms(&ev.detail);
        if let Some(l) = self
            .pending
            .iter_mut()
            .chain(self.confirmed.iter_mut())
            .find(|l| l.dev == dev && l.t_ms.abs_diff(ev.t_ms) <= OFF_MS)
        {
            l.local = l.local.or(local);
            return;
        }
        let Some(&pct) = self.levels.get(dev) else {
            return;
        };
        if pct > LOW_PCT {
            return;
        }
        self.pending.push(Loss {
            dev: dev.to_string(),
            t_ms: ev.t_ms,
            pct,
            local,
        });
    }

    fn excuse(&self, l: &Loss) -> Option<&'static str> {
        if self
            .quiet
            .iter()
            .any(|(t, d)| *d == l.dev && t.abs_diff(l.t_ms) <= OFF_MS)
        {
            Some("it was switched off")
        } else if self
            .links
            .iter()
            .any(|(t, d)| *d != l.dev && t.abs_diff(l.t_ms) <= GROUP_MS)
        {
            Some("another device dropped at the same moment")
        } else if self.usb.iter().any(|t| t.abs_diff(l.t_ms) <= OFF_MS) {
            Some("a USB device was unplugged")
        } else {
            None
        }
    }

    fn settle(&mut self) {
        let now = self.now;
        let (ready, wait): (Vec<Loss>, Vec<Loss>) = std::mem::take(&mut self.pending)
            .into_iter()
            .partition(|l| now >= l.t_ms + CONFIRM_MS);
        self.pending = wait;
        for l in ready {
            if let Some(why) = self.excuse(&l) {
                crate::dlog!(
                    "battery: {} dropped at {:.0}% but {why}, not counted",
                    l.dev,
                    l.pct
                );
                continue;
            }
            crate::dlog!(
                "battery: {} dropped at {:.0}% with no sign of being switched off",
                l.dev,
                l.pct
            );
            self.confirmed.push(l);
            self.report = REPORT_EVENTS;
        }
        let keep = |t: &u64| t.saturating_add(CONFIRM_MS + OFF_MS) >= now;
        self.quiet.retain(|(t, _)| keep(t));
        self.links.retain(|(t, _)| keep(t));
        self.usb.retain(keep);
    }

    pub fn flat(&self) -> Vec<Flat> {
        let mut by: BTreeMap<&str, Flat> = BTreeMap::new();
        for l in &self.confirmed {
            let f = by.entry(&l.dev).or_insert_with(|| Flat {
                dev: l.dev.clone(),
                pct: l.pct,
                times: 0,
                clocks: Vec::new(),
            });
            f.times += 1;
            f.pct = f.pct.min(l.pct);
            f.clocks.extend(clock(l.local));
        }
        by.into_values().collect()
    }
}

pub fn findings(power: &Power, label: &dyn Fn(&str) -> String) -> Vec<Finding> {
    power
        .flat()
        .into_iter()
        .map(|f| {
            let name = label(&f.dev);
            let when = match (f.times, f.clocks.as_slice()) {
                (1, [c]) => format!(" at {c}"),
                (1, _) => String::new(),
                (n, cs) if cs.len() == n => format!(" {n} times ({})", join(cs)),
                (n, _) => format!(" {n} times"),
            };
            let (title, cause) = if f.pct <= EMPTY_PCT {
                (
                    format!("{name} ran out of battery"),
                    "so it most likely went flat.",
                )
            } else {
                (
                    format!("{name} dropped out with a low battery"),
                    "Unless you took it out of range, the battery probably ran too low.",
                )
            };
            let joiner = if f.pct <= EMPTY_PCT { ", " } else { ". " };
            Finding {
                rule: Rule::Battery,
                key: f.dev.clone(),
                title,
                what: format!(
                    "{name} lost its connection{when}. Its battery last read {:.0}% and SteamVR didn't see it being switched off{joiner}{cause}",
                    f.pct
                ),
                steps: vec![
                    "Charge it fully before you play again.".to_string(),
                    "If it won't charge, try another USB cable and charger.".to_string(),
                    "Watch its battery on the Live page while you play. If it drains much faster than your other devices of the same type, its battery is probably wearing out.".to_string(),
                ],
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnose::Diagnosis;
    use crate::event::Source;

    const T0: u64 = 1_792_053_540_000;
    const MIN: u64 = 60_000;
    const RIGHT: &str = "LHR-A43A0229";
    const LEFT: &str = "LHR-7E902102";
    const TRACKER: &str = "LHR-10268F5C";

    fn api(t: u64, dev: &str, kind: Kind) -> SignalEvent {
        let detail = kind.to_string();
        SignalEvent::at(t, Source::Api, Some(dev.into()), kind, detail)
    }

    fn log(t: u64, dev: &str, kind: Kind, line: &str) -> SignalEvent {
        SignalEvent::at(t, Source::Log, Some(dev.into()), kind, line)
    }

    fn battery(t: u64, dev: &str, pct: f32) -> SignalEvent {
        api(t, dev, Kind::BatteryLevel { pct })
    }

    fn drop_link(t: u64, dev: &str, clock: &str) -> Vec<SignalEvent> {
        vec![
            api(t, dev, Kind::DeviceDeactivated),
            api(t, dev, Kind::DeviceDeactivated),
            log(
                t,
                dev,
                Kind::WirelessDisconnect,
                &format!(
                    "Thu Oct 15 2026 {clock} [Info] - lighthouse: {dev}: Disconnected from receiver 65D86BDA85"
                ),
            ),
        ]
    }

    fn switch_off(t: u64, dev: &str) -> Vec<SignalEvent> {
        let mut out = vec![
            api(t, dev, Kind::StandbyStart),
            log(
                t,
                dev,
                Kind::DevicePowerOff,
                &format!("lighthouse: Device {dev} powering off upon entering standby."),
            ),
        ];
        out.extend(drop_link(t + 490, dev, "07:00:00.000"));
        out
    }

    fn ticks(from: u64, to: u64, dev: &str) -> Vec<SignalEvent> {
        (from..to)
            .step_by(20_000)
            .map(|t| api(t, dev, Kind::PoseValid(true)))
            .collect()
    }

    fn run(events: &[SignalEvent]) -> (Diagnosis, Vec<Vec<Rule>>) {
        let mut d = Diagnosis::default();
        let mut fired = Vec::new();
        for e in events {
            d.ingest(e);
            let found = d.findings(&|s: &str| s.to_string());
            fired.push(d.fresh(found).into_iter().map(|f| f.rule).collect());
        }
        (d, fired)
    }

    fn battery_cards(d: &Diagnosis) -> Vec<Finding> {
        d.findings(&|s: &str| s.to_string())
            .into_iter()
            .filter(|f| f.rule == Rule::Battery)
            .collect()
    }

    fn sorted(mut v: Vec<SignalEvent>) -> Vec<SignalEvent> {
        v.sort_by_key(|e| e.t_ms);
        v
    }

    fn dying_controllers() -> Vec<SignalEvent> {
        let mut ev = Vec::new();
        for (i, pct) in [28.0, 23.0, 17.0, 12.0, 7.0].into_iter().enumerate() {
            ev.push(battery(T0 + i as u64 * 15 * MIN, RIGHT, pct));
        }
        for (i, pct) in [24.0, 19.0, 14.0, 9.0].into_iter().enumerate() {
            ev.push(battery(T0 + 2 * MIN + i as u64 * 17 * MIN, LEFT, pct));
        }
        ev.extend(ticks(T0, T0 + 85 * MIN, RIGHT));
        ev.extend(ticks(T0, T0 + 86 * MIN, LEFT));
        ev.extend(drop_link(T0 + 85 * MIN, RIGHT, "06:28:54.150"));
        ev.extend(drop_link(T0 + 85 * MIN + 18_651, LEFT, "06:29:12.801"));
        ev.extend(ticks(T0 + 86 * MIN, T0 + 110 * MIN, TRACKER));
        ev.push(api(T0 + 90 * MIN, RIGHT, Kind::StandbyStart));
        ev.push(api(T0 + 90 * MIN + 18_000, LEFT, Kind::StandbyStart));
        sorted(ev)
    }

    #[test]
    fn controllers_that_go_flat_mid_session_are_named() {
        let events = dying_controllers();
        let (d, fired) = run(&events);
        let cards = battery_cards(&d);
        assert_eq!(cards.len(), 2, "{cards:?}");
        let right = cards.iter().find(|f| f.key == RIGHT).unwrap();
        assert_eq!(right.title, format!("{RIGHT} ran out of battery"));
        assert_eq!(
            right.what,
            format!(
                "{RIGHT} lost its connection at 06:28. Its battery last read 7% and SteamVR didn't see it being switched off, so it most likely went flat."
            )
        );
        let left = cards.iter().find(|f| f.key == LEFT).unwrap();
        assert!(left.what.contains("at 06:29"), "{}", left.what);
        assert!(left.what.contains("last read 9%"), "{}", left.what);
        assert_eq!(left.steps.len(), 3);
        let notices = fired
            .iter()
            .flatten()
            .filter(|r| **r == Rule::Battery)
            .count();
        assert_eq!(notices, 2, "one notice per device");
    }

    #[test]
    fn a_drop_is_only_counted_once_the_switch_off_window_has_passed() {
        let events = dying_controllers();
        let lost = T0 + 85 * MIN;
        let mut d = Diagnosis::default();
        for e in events.iter().filter(|e| e.t_ms < lost + CONFIRM_MS) {
            d.ingest(e);
        }
        assert!(battery_cards(&d).is_empty());
        for e in events.iter().filter(|e| e.t_ms >= lost + CONFIRM_MS) {
            d.ingest(e);
        }
        assert_eq!(battery_cards(&d).len(), 2);
    }

    #[test]
    fn switching_off_at_the_end_of_a_healthy_session_says_nothing() {
        let mut ev = Vec::new();
        for (i, pct) in [61.0, 49.0, 37.0, 25.0, 14.0].into_iter().enumerate() {
            ev.push(battery(T0 + i as u64 * 25 * MIN, RIGHT, pct));
            ev.push(battery(T0 + i as u64 * 25 * MIN + 30_000, LEFT, pct + 2.0));
        }
        for (i, pct) in [100.0, 95.0, 90.0, 85.0].into_iter().enumerate() {
            ev.push(battery(T0 + i as u64 * 22 * MIN, TRACKER, pct));
        }
        ev.extend(ticks(T0, T0 + 130 * MIN, TRACKER));
        ev.extend(drop_link(T0 + 40 * MIN, TRACKER, "05:00:00.000"));
        ev.extend(switch_off(T0 + 120 * MIN, RIGHT));
        ev.extend(switch_off(T0 + 121 * MIN, LEFT));
        ev.extend(ticks(T0 + 121 * MIN, T0 + 125 * MIN, TRACKER));
        let (d, fired) = run(&sorted(ev));
        assert!(battery_cards(&d).is_empty());
        assert!(fired.iter().flatten().all(|r| *r != Rule::Battery));
    }

    #[test]
    fn steamvr_turning_both_controllers_off_together_is_not_a_flat_battery() {
        let mut ev = vec![battery(T0, RIGHT, 11.0), battery(T0 + 5 * MIN, LEFT, 11.0)];
        ev.extend(drop_link(T0 + 7 * MIN, RIGHT, "04:16:05.913"));
        ev.extend(drop_link(T0 + 7 * MIN + 17, LEFT, "04:16:05.930"));
        ev.extend(ticks(T0, T0 + 20 * MIN, TRACKER));
        let (d, _) = run(&sorted(ev));
        assert!(battery_cards(&d).is_empty());
    }

    #[test]
    fn a_tracker_that_keeps_dropping_on_a_low_battery_lists_each_time() {
        let mut ev = vec![
            battery(T0, TRACKER, 26.0),
            battery(T0 + 20 * MIN, TRACKER, 21.0),
        ];
        ev.extend(drop_link(T0 + 25 * MIN, TRACKER, "03:49:30.510"));
        ev.push(api(T0 + 41 * MIN, TRACKER, Kind::DeviceActivated));
        ev.extend(drop_link(T0 + 45 * MIN, TRACKER, "04:09:53.647"));
        ev.extend(ticks(T0, T0 + 50 * MIN, RIGHT));
        let (d, _) = run(&sorted(ev));
        let cards = battery_cards(&d);
        assert_eq!(cards.len(), 1);
        assert_eq!(
            cards[0].title,
            format!("{TRACKER} dropped out with a low battery")
        );
        assert_eq!(
            cards[0].what,
            format!(
                "{TRACKER} lost its connection 2 times (03:49 and 04:09). Its battery last read 21% and SteamVR didn't see it being switched off. Unless you took it out of range, the battery probably ran too low."
            )
        );
    }

    #[test]
    fn devices_with_plenty_of_battery_or_no_reading_are_left_to_other_checks() {
        let mut ev = vec![
            battery(T0, TRACKER, 100.0),
            battery(T0, "1PASH5D1P17365", 0.0),
        ];
        ev.extend(drop_link(T0 + 10 * MIN, TRACKER, "05:00:00.000"));
        ev.extend(drop_link(T0 + 20 * MIN, RIGHT, "05:10:00.000"));
        ev.push(api(
            T0 + 30 * MIN,
            "1PASH5D1P17365",
            Kind::DeviceDeactivated,
        ));
        ev.extend(ticks(T0, T0 + 40 * MIN, LEFT));
        let (d, _) = run(&sorted(ev));
        assert!(battery_cards(&d).is_empty());
    }
}

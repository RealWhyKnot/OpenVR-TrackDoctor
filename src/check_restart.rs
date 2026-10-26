use crate::diagnose::{Finding, Rule, join};
use crate::event::{Kind, SignalEvent, TrackState};
use crate::signals::logtail::naive_ms;
use crate::summary::plural;
use std::collections::{BTreeMap, VecDeque};

pub const REPEATS: usize = 2;
pub const SAME_DROP_MS: u64 = 30_000;
pub const BACK_MAX_MS: u64 = 5 * 60_000;
const LOG_CAP: usize = 20;
const MAX_CLOCKS: usize = 4;

#[derive(Clone, Debug)]
struct Episode {
    t_ms: u64,
    last_ms: u64,
    local: Option<i64>,
    back_ms: Option<u64>,
}

#[derive(Clone, Debug, Default)]
struct Device {
    episodes: Vec<Episode>,
    back: VecDeque<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct Restarts {
    devices: BTreeMap<String, Device>,
    logged: usize,
}

fn clock(local: Option<i64>) -> Option<String> {
    let rem = local?.rem_euclid(86_400_000);
    Some(format!("{:02}:{:02}", rem / 3_600_000, (rem / 60_000) % 60))
}

fn seconds(s: u64) -> String {
    match s {
        0 => "less than a second".into(),
        1 => "about a second".into(),
        s => format!("about {s} seconds"),
    }
}

impl Restarts {
    pub fn ingest(&mut self, ev: &SignalEvent) -> bool {
        let Some(dev) = &ev.device else {
            return false;
        };
        match &ev.kind {
            Kind::TrackingState {
                to: TrackState::RunningOk,
                ..
            } => {
                let d = self.devices.entry(dev.clone()).or_default();
                d.back.push_back(ev.t_ms);
                while d
                    .back
                    .front()
                    .is_some_and(|&b| ev.t_ms.saturating_sub(b) > BACK_MAX_MS)
                {
                    d.back.pop_front();
                }
                let mut changed = false;
                for e in d.episodes.iter_mut().filter(|e| {
                    e.back_ms.is_none() && ev.t_ms >= e.t_ms && ev.t_ms - e.t_ms <= BACK_MAX_MS
                }) {
                    e.back_ms = Some(ev.t_ms - e.t_ms);
                    changed = true;
                }
                changed && d.episodes.len() >= REPEATS
            }
            Kind::RestartFailed { base, hits } => {
                let d = self.devices.entry(dev.clone()).or_default();
                if let Some(last) = d.episodes.last_mut()
                    && ev.t_ms.saturating_sub(last.last_ms) <= SAME_DROP_MS
                    && !d.back.iter().any(|&b| b >= last.t_ms && b <= ev.t_ms)
                {
                    last.last_ms = last.last_ms.max(ev.t_ms);
                    return false;
                }
                let back_ms = d
                    .back
                    .iter()
                    .copied()
                    .filter(|&b| b >= ev.t_ms)
                    .min()
                    .map(|b| b - ev.t_ms);
                d.episodes.push(Episode {
                    t_ms: ev.t_ms,
                    last_ms: ev.t_ms,
                    local: naive_ms(&ev.detail),
                    back_ms,
                });
                let n = d.episodes.len();
                if self.logged < LOG_CAP {
                    self.logged += 1;
                    crate::dlog!(
                        "restart: {dev} couldn't restart from base {base} (hits {hits:?}), episode {n}, flagged {}",
                        n >= REPEATS
                    );
                }
                n >= REPEATS
            }
            _ => false,
        }
    }

    pub fn active(&self) -> bool {
        self.devices.values().any(|d| d.episodes.len() >= REPEATS)
    }

    pub fn findings(&self, label: &dyn Fn(&str) -> String) -> Vec<Finding> {
        self.devices
            .iter()
            .filter(|(_, d)| d.episodes.len() >= REPEATS)
            .map(|(dev, d)| {
                let name = label(dev);
                let n = d.episodes.len();
                let clocks: Vec<String> = d.episodes.iter().filter_map(|e| clock(e.local)).collect();
                let when = if clocks.len() == n && n <= MAX_CLOCKS {
                    format!(" (at {})", join(&clocks))
                } else {
                    String::new()
                };
                let mut secs: Vec<u64> = d
                    .episodes
                    .iter()
                    .filter_map(|e| e.back_ms)
                    .map(|ms| (ms + 500) / 1000)
                    .collect();
                secs.sort_unstable();
                let back = match (secs.first(), secs.last()) {
                    (Some(&a), Some(&b)) if a == b => {
                        format!(" It took {} to come back.", seconds(a))
                    }
                    (Some(0), Some(&b)) => format!(" It took up to {b} seconds to come back."),
                    (Some(&a), Some(&b)) => format!(" It took {a} to {b} seconds to come back."),
                    _ => String::new(),
                };
                Finding {
                    rule: Rule::SlowRestart,
                    key: dev.clone(),
                    title: format!("{name} is slow to get its tracking back"),
                    what: format!(
                        "{name} lost tracking {}{when} and didn't get it back right away. Each time, SteamVR tried to restart it from a base station, but too few of its sensors saw that base long enough to work out where it was.{back}",
                        plural(n, "time")
                    ),
                    steps: vec![
                        "Check that nothing covers it, like clothing, a strap or your hand.".into(),
                        "If it's worn on your body, turn it so its top faces out into the room.".into(),
                        "Check the Room page to see whether it happens in one spot.".into(),
                    ],
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Source;
    use crate::signals::logtail::parse_line;

    const T0: u64 = 1_000_000_000;
    const DEV: &str = "LHR-A66FAA1B";

    fn log(line: &str, t_ms: u64) -> SignalEvent {
        let mut ev = parse_line(line, 0).unwrap();
        ev.t_ms = t_ms;
        ev
    }

    fn back(t_ms: u64, dev: &str) -> SignalEvent {
        SignalEvent::at(
            t_ms,
            Source::Api,
            Some(dev.into()),
            Kind::TrackingState {
                from: TrackState::CalibratingOutOfRange,
                to: TrackState::RunningOk,
            },
            "",
        )
    }

    fn label(s: &str) -> String {
        format!("Left foot ({s})")
    }

    const FIRST: [&str; 4] = [
        "Tue Oct 20 2026 15:20:17.420 [Info] - lighthouse: LHR-A66FAA1B C: Trying to start tracking from base 9417E0B3: Not enough contiguous samples for a bootstrap pose",
        "Tue Oct 20 2026 15:20:17.420 [Info] - lighthouse: LHR-A66FAA1B C: Trying to start tracking from base 9D8DEA7B: Not enough contiguous samples for a bootstrap pose",
        "Tue Oct 20 2026 15:20:17.420 [Info] - lighthouse: LHR-A66FAA1B C: Trying to start tracking from base B394A63C: Not enough contiguous samples for a bootstrap pose",
        "Tue Oct 20 2026 15:20:17.420 [Info] - lighthouse: LHR-A66FAA1B C: Trying to start tracking from base C538EFC9: Not enough contiguous samples for a bootstrap pose",
    ];
    const SECOND: &str = "Tue Oct 20 2026 15:25:07.682 [Info] - lighthouse: LHR-A66FAA1B C: Trying to start tracking from base 9417E0B3: Not enough contiguous samples for a bootstrap pose";
    const SECOND_AT: u64 = T0 + 290_262;

    #[test]
    fn one_failed_restart_stays_quiet() {
        let mut r = Restarts::default();
        let flagged: Vec<bool> = FIRST.iter().map(|l| r.ingest(&log(l, T0))).collect();
        assert_eq!(flagged, [false; 4]);
        assert!(!r.active());
        assert!(r.findings(&label).is_empty());
    }

    #[test]
    fn lines_of_one_dropout_are_one_episode_until_tracking_returns() {
        let mut r = Restarts::default();
        r.ingest(&log(FIRST[0], T0));
        assert!(!r.ingest(&log(FIRST[1], T0)));
        assert!(
            !r.ingest(&log(FIRST[2], T0 + SAME_DROP_MS)),
            "still the same dropout"
        );
        assert!(!r.active());
        r.ingest(&back(T0 + SAME_DROP_MS + 100, DEV));
        assert!(
            r.ingest(&log(FIRST[3], T0 + SAME_DROP_MS + 200)),
            "tracking came back in between, so this is a second dropout"
        );
        assert!(r.active());
    }

    #[test]
    fn a_long_gap_is_a_new_dropout_even_without_a_recovery() {
        let mut r = Restarts::default();
        r.ingest(&log(FIRST[0], T0));
        assert!(r.ingest(&log(SECOND, SECOND_AT)));
        let found = r.findings(&label);
        assert_eq!(found.len(), 1);
        assert!(
            found[0].what.ends_with("where it was."),
            "{}",
            found[0].what
        );
    }

    #[test]
    fn two_dropouts_on_one_device_make_one_finding() {
        let mut r = Restarts::default();
        for l in FIRST {
            r.ingest(&log(l, T0));
        }
        assert!(!r.ingest(&back(T0 + 9_300, DEV)));
        assert!(r.ingest(&log(SECOND, SECOND_AT)));
        assert!(r.ingest(&back(SECOND_AT + 7_300, DEV)));
        assert!(!r.ingest(&back(SECOND_AT + 9_000, DEV)), "already filled");
        let found = r.findings(&label);
        assert_eq!(found.len(), 1);
        let f = &found[0];
        assert_eq!(f.rule, Rule::SlowRestart);
        assert_eq!(f.key, DEV);
        assert_eq!(
            f.title,
            "Left foot (LHR-A66FAA1B) is slow to get its tracking back"
        );
        assert_eq!(
            f.what,
            "Left foot (LHR-A66FAA1B) lost tracking 2 times (at 15:20 and 15:25) and didn't get it back right away. Each time, SteamVR tried to restart it from a base station, but too few of its sensors saw that base long enough to work out where it was. It took 7 to 9 seconds to come back."
        );
        assert_eq!(f.steps.len(), 3);
    }

    #[test]
    fn recovery_seen_before_the_log_line_still_counts() {
        let mut r = Restarts::default();
        r.ingest(&back(T0 + 2_600, DEV));
        r.ingest(&log(FIRST[0], T0));
        r.ingest(&back(SECOND_AT + 800, DEV));
        r.ingest(&log(SECOND, SECOND_AT));
        let what = &r.findings(&label)[0].what;
        assert!(
            what.ends_with("It took 1 to 3 seconds to come back."),
            "{what}"
        );
    }

    #[test]
    fn a_recovery_minutes_later_is_not_counted() {
        let mut r = Restarts::default();
        r.ingest(&log(FIRST[0], T0));
        r.ingest(&back(T0 + 1_000, DEV));
        r.ingest(&log(SECOND, SECOND_AT));
        r.ingest(&back(SECOND_AT + BACK_MAX_MS + 1, DEV));
        let what = &r.findings(&label)[0].what;
        assert!(
            what.ends_with("It took about a second to come back."),
            "{what}"
        );
    }

    #[test]
    fn dropouts_on_different_devices_stay_apart() {
        let mut r = Restarts::default();
        r.ingest(&log(FIRST[0], T0));
        r.ingest(&log(
            "Sat Oct 24 2026 17:38:18.867 [Info] - lighthouse: LHR-617D30D7 C: Trying to start tracking from base B394A63C: Available samples not sufficient for booting (hits: 2+1)",
            SECOND_AT,
        ));
        r.ingest(&back(SECOND_AT + 500, "LHR-617D30D7"));
        assert!(!r.active(), "one dropout each on two devices");
        assert!(r.findings(&label).is_empty());
    }
}

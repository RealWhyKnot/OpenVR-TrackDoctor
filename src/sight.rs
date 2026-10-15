use crate::event::{Kind, SignalEvent};
use std::collections::{BTreeMap, BTreeSet};

pub const HARD_RATIO: f64 = 2.0;
pub const HARD_MIN_DROPS: usize = 40;
pub const HARD_DEVICE_DROPS: usize = 5;
pub const LOW_MS: u64 = 10_000;

#[derive(Clone, Debug, Default)]
struct DevSight {
    seeing: BTreeSet<u32>,
    open: Option<u64>,
    watched_ms: u64,
    low_ms: u64,
    visible: BTreeMap<u32, u64>,
    drops: BTreeMap<u32, usize>,
}

impl DevSight {
    fn close(&mut self, t: u64) {
        if let Some(since) = self.open.take() {
            let span = t.saturating_sub(since);
            self.watched_ms += span;
            if self.seeing.len() <= 1 {
                self.low_ms += span;
            }
            for ch in &self.seeing {
                *self.visible.entry(*ch).or_default() += span;
            }
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Sight {
    devs: BTreeMap<String, DevSight>,
    ids: BTreeMap<u32, String>,
    last: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BaseSeen {
    pub ch: u32,
    pub id: Option<String>,
    pub drops: usize,
    pub share: f32,
    pub worst: Vec<(String, usize)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DevSeen {
    pub serial: String,
    pub watched_ms: u64,
    pub low_ms: u64,
    pub drops: usize,
    pub worst: Option<(u32, usize)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Hard {
    pub base: BaseSeen,
    pub ratio: f64,
    pub others: usize,
    pub devices: usize,
    pub of: usize,
}

impl Sight {
    pub fn ingest(&mut self, ev: &SignalEvent) -> bool {
        self.last = self.last.max(ev.t_ms);
        match &ev.kind {
            Kind::BaseSight {
                ch,
                gained,
                seeing,
                ids,
            } => {
                for (c, id) in ids {
                    self.ids.insert(*c, id.clone());
                }
                let Some(dev) = &ev.device else {
                    return false;
                };
                let d = self.devs.entry(dev.clone()).or_default();
                d.close(ev.t_ms);
                if !gained {
                    *d.drops.entry(*ch).or_default() += 1;
                }
                d.seeing = seeing.iter().copied().collect();
                d.open = Some(ev.t_ms);
                !gained
            }
            Kind::LogRotated => {
                for d in self.devs.values_mut() {
                    d.close(ev.t_ms);
                    d.seeing.clear();
                }
                false
            }
            Kind::DeviceDeactivated
            | Kind::DevicePowerOff
            | Kind::WirelessDisconnect
            | Kind::StandbyStart => {
                let Some(d) = ev.device.as_ref().and_then(|s| self.devs.get_mut(s)) else {
                    return false;
                };
                d.close(ev.t_ms);
                d.seeing.clear();
                false
            }
            _ => false,
        }
    }

    fn settled(&self) -> BTreeMap<String, DevSight> {
        let mut devs = self.devs.clone();
        for d in devs.values_mut() {
            d.close(self.last);
        }
        devs
    }

    pub fn is_empty(&self) -> bool {
        self.devs.is_empty()
    }

    pub fn id(&self, ch: u32) -> Option<&String> {
        self.ids.get(&ch)
    }

    pub fn bases(&self) -> Vec<BaseSeen> {
        let devs = self.settled();
        let mut chans: BTreeSet<u32> = self.ids.keys().copied().collect();
        for d in devs.values() {
            chans.extend(d.visible.keys());
            chans.extend(d.drops.keys());
        }
        let mut out: Vec<BaseSeen> = chans
            .into_iter()
            .map(|ch| {
                let mut worst: Vec<(String, usize)> = devs
                    .iter()
                    .filter_map(|(s, d)| d.drops.get(&ch).map(|n| (s.clone(), *n)))
                    .filter(|(_, n)| *n > 0)
                    .collect();
                worst.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
                let watched: u64 = devs.values().map(|d| d.watched_ms).sum();
                let seen: u64 = devs.values().filter_map(|d| d.visible.get(&ch)).sum();
                BaseSeen {
                    ch,
                    id: self.ids.get(&ch).cloned(),
                    drops: worst.iter().map(|w| w.1).sum(),
                    share: if watched == 0 {
                        0.0
                    } else {
                        (seen as f64 / watched as f64) as f32
                    },
                    worst,
                }
            })
            .collect();
        out.sort_by(|a, b| b.drops.cmp(&a.drops).then(a.ch.cmp(&b.ch)));
        out
    }

    pub fn devices(&self) -> Vec<DevSeen> {
        self.settled()
            .into_iter()
            .map(|(serial, d)| DevSeen {
                worst: d
                    .drops
                    .iter()
                    .max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0)))
                    .map(|(c, n)| (*c, *n)),
                drops: d.drops.values().sum(),
                serial,
                watched_ms: d.watched_ms,
                low_ms: d.low_ms,
            })
            .collect()
    }

    pub fn hard(&self) -> Option<Hard> {
        let bases = self.bases();
        if bases.len() < 2 {
            return None;
        }
        let worst = bases[0].clone();
        if worst.drops < HARD_MIN_DROPS {
            return None;
        }
        let mut rest: Vec<usize> = bases[1..].iter().map(|b| b.drops).collect();
        rest.sort_unstable();
        let mid = rest[rest.len() / 2].max(1);
        let ratio = worst.drops as f64 / mid as f64;
        let counted: Vec<DevSeen> = self
            .devices()
            .into_iter()
            .filter(|d| d.drops >= HARD_DEVICE_DROPS)
            .collect();
        let devices = counted
            .iter()
            .filter(|d| d.worst.is_some_and(|w| w.0 == worst.ch))
            .count();
        (ratio >= HARD_RATIO && devices * 2 >= counted.len()).then_some(Hard {
            base: worst,
            ratio,
            others: mid,
            devices,
            of: counted.len(),
        })
    }

    pub fn low(&self) -> Vec<DevSeen> {
        let mut out: Vec<DevSeen> = self
            .devices()
            .into_iter()
            .filter(|d| d.low_ms >= LOW_MS)
            .collect();
        out.sort_by(|a, b| b.low_ms.cmp(&a.low_ms).then(a.serial.cmp(&b.serial)));
        out
    }

    pub fn seeing_at(&self, serial: &str) -> Option<usize> {
        self.devs
            .get(serial)
            .filter(|d| d.open.is_some())
            .map(|d| d.seeing.len())
    }
}

pub fn base_name(ch: u32, id: Option<&String>) -> String {
    match id {
        Some(id) => format!("{id} (channel {ch})"),
        None => format!("on channel {ch}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Source;

    fn sight(t: u64, dev: &str, ch: u32, gained: bool, seeing: &[u32]) -> SignalEvent {
        SignalEvent::at(
            t,
            Source::Log,
            Some(dev.into()),
            Kind::BaseSight {
                ch,
                gained,
                seeing: seeing.to_vec(),
                ids: vec![(10, "9D8DEA7B".into())],
            },
            "",
        )
    }

    #[test]
    fn counts_drops_and_time_in_view() {
        let mut s = Sight::default();
        s.ingest(&sight(0, "LHR-A", 1, true, &[1]));
        s.ingest(&sight(1000, "LHR-A", 10, true, &[1, 10]));
        s.ingest(&sight(5000, "LHR-A", 10, false, &[1]));
        s.ingest(&sight(21_000, "LHR-A", 10, true, &[1, 10]));
        let bases = s.bases();
        let ten = bases.iter().find(|b| b.ch == 10).unwrap();
        assert_eq!(ten.drops, 1);
        assert_eq!(ten.id.as_deref(), Some("9D8DEA7B"));
        assert!((ten.share - 4000.0 / 21_000.0).abs() < 1e-4);
        let one = bases.iter().find(|b| b.ch == 1).unwrap();
        assert!((one.share - 1.0).abs() < 1e-4);
        let dev = &s.devices()[0];
        assert_eq!(dev.low_ms, 1000 + 16_000);
        assert_eq!(dev.worst, Some((10, 1)));
        assert_eq!(s.low().len(), 1);
        assert_eq!(s.seeing_at("LHR-A"), Some(2));
    }

    #[test]
    fn a_base_dropped_far_more_than_the_rest_is_hard_to_see() {
        let mut s = Sight::default();
        let mut t = 0;
        for (ch, n) in [(10u32, 60usize), (1, 25), (16, 20), (15, 10)] {
            for _ in 0..n {
                t += 100;
                s.ingest(&sight(t, "LHR-A", ch, true, &[1, 10, 15, 16]));
                t += 100;
                s.ingest(&sight(t, "LHR-A", ch, false, &[1, 10, 15, 16]));
            }
        }
        let hard = s.hard().unwrap();
        assert_eq!(hard.base.ch, 10);
        assert_eq!(hard.others, 20);
        assert!((hard.ratio - 3.0).abs() < 1e-9);
    }

    #[test]
    fn even_drops_or_too_few_flag_nothing() {
        let mut s = Sight::default();
        let mut t = 0;
        for ch in [1u32, 10] {
            for _ in 0..30 {
                t += 100;
                s.ingest(&sight(t, "LHR-A", ch, false, &[]));
            }
        }
        assert_eq!(s.hard(), None);
        let mut s = Sight::default();
        for i in 0..30 {
            s.ingest(&sight(i * 100, "LHR-A", 10, false, &[]));
        }
        s.ingest(&sight(5000, "LHR-A", 1, true, &[1]));
        assert_eq!(s.hard(), None);
    }

    #[test]
    fn turning_off_stops_the_clock() {
        let mut s = Sight::default();
        s.ingest(&sight(0, "LHR-A", 1, true, &[1, 10]));
        s.ingest(&SignalEvent::at(
            2000,
            Source::Api,
            Some("LHR-A".into()),
            Kind::DeviceDeactivated,
            "",
        ));
        s.ingest(&sight(600_000, "LHR-B", 1, true, &[1]));
        let a = s
            .devices()
            .into_iter()
            .find(|d| d.serial == "LHR-A")
            .unwrap();
        assert_eq!(a.watched_ms, 2000);
        assert_eq!(s.seeing_at("LHR-A"), None);
    }
}

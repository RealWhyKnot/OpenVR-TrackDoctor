use crate::diagnose::{Finding, Rule, join};
use crate::event::{Kind, SignalEvent};
use crate::summary::plural;
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub const BACK_MS: u64 = 10_000;
pub const TOGETHER_MS: u64 = 1_000;
pub const CHAIN_MS: u64 = 15_000;
pub const CHAIN_DONGLES: usize = 3;
pub const REPEATS: usize = 2;

const DONGLE_PID: &str = "2101";

#[derive(Clone, Debug)]
struct Gone {
    port: String,
    t_ms: u64,
    device: Option<String>,
    name: String,
    dongle: bool,
}

#[derive(Clone, Debug)]
pub struct PortDrop {
    pub key: String,
    pub port: String,
    pub device: Option<String>,
    pub name: String,
    pub dongle: bool,
    pub t_ms: u64,
    pub back_ms: u64,
}

impl PortDrop {
    fn bus(&self) -> &str {
        split_port(&self.port).0
    }

    fn parent(&self) -> (&str, &str) {
        let (bus, chain) = split_port(&self.port);
        (bus, chain.rsplit_once('.').map_or("", |(p, _)| p))
    }

    fn back_at(&self) -> u64 {
        self.t_ms + self.back_ms
    }

    fn who(&self, label: &dyn Fn(&str) -> String) -> String {
        match (&self.device, self.dongle) {
            (Some(d), true) => format!("the dongle for {}", label(d)),
            (None, true) => format!("dongle {}", self.name),
            (Some(d), false) => format!("{} ({})", self.name, label(d)),
            (None, false) => self.name.clone(),
        }
    }
}

fn split_port(port: &str) -> (&str, &str) {
    port.rsplit_once(':').unwrap_or((port, ""))
}

fn product(detail: &str) -> (String, bool) {
    let mut words = detail.split_whitespace();
    let ids = words.next().unwrap_or_default();
    let dongle = ids
        .rsplit_once(':')
        .is_some_and(|(_, pid)| pid.eq_ignore_ascii_case(DONGLE_PID));
    let name = detail
        .split_once(' ')
        .map(|(_, rest)| rest.split(" sn=").next().unwrap_or(rest).trim())
        .filter(|n| !n.is_empty() && *n != "?")
        .unwrap_or("Valve USB device");
    (name.to_string(), dongle)
}

#[derive(Clone, Debug, Default)]
pub struct UsbHealth {
    gone: HashMap<String, Gone>,
    drops: Vec<PortDrop>,
    note: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sorted {
    pub together: Vec<Vec<usize>>,
    pub chains: Vec<Vec<usize>>,
    pub repeats: BTreeMap<String, Vec<usize>>,
}

impl UsbHealth {
    pub fn ingest(&mut self, ev: &SignalEvent) -> bool {
        let (port, serial, removed) = match &ev.kind {
            Kind::UsbRemove { port, dongle } => (port, dongle, true),
            Kind::UsbAttach { port, dongle } => (port, dongle, false),
            _ => return false,
        };
        let key = serial.clone().unwrap_or_else(|| format!("port {port}"));
        if removed {
            let (product, dongle) = product(&ev.detail);
            let name = if dongle {
                serial.clone().unwrap_or(product)
            } else {
                product
            };
            self.gone.insert(
                key,
                Gone {
                    port: port.clone(),
                    t_ms: ev.t_ms,
                    device: ev.device.clone(),
                    name,
                    dongle,
                },
            );
            return false;
        }
        let Some(g) = self.gone.remove(&key) else {
            return false;
        };
        let back_ms = ev.t_ms.saturating_sub(g.t_ms);
        if g.port != *port {
            crate::dlog!(
                "usb check: {key} moved from {} to {port}, not counted as a drop",
                g.port
            );
            return false;
        }
        if back_ms > BACK_MS {
            crate::dlog!("usb check: {key} came back after {back_ms} ms, too slow to count");
            return false;
        }
        self.drops.push(PortDrop {
            key,
            port: g.port,
            device: g.device.or_else(|| ev.device.clone()),
            name: g.name,
            dongle: g.dongle,
            t_ms: g.t_ms,
            back_ms,
        });
        let s = self.sorted();
        let note = format!(
            "{} drops, {} together, {} in a row, {} repeating",
            self.drops.len(),
            s.together.len(),
            s.chains.len(),
            s.repeats.len()
        );
        if note != self.note {
            crate::dlog!("usb check: {note}");
            self.note = note;
        }
        true
    }

    pub fn drops(&self) -> &[PortDrop] {
        &self.drops
    }

    pub fn any(&self) -> bool {
        let s = self.sorted();
        !s.together.is_empty() || !s.chains.is_empty() || !s.repeats.is_empty()
    }

    pub fn sorted(&self) -> Sorted {
        let mut order: Vec<usize> = (0..self.drops.len()).collect();
        order.sort_by_key(|&i| self.drops[i].t_ms);
        let mut used: BTreeSet<usize> = BTreeSet::new();
        let mut together = Vec::new();
        for (n, &i) in order.iter().enumerate() {
            if used.contains(&i) {
                continue;
            }
            let a = &self.drops[i];
            let mut group = vec![i];
            for &j in &order[n + 1..] {
                let b = &self.drops[j];
                if b.t_ms.saturating_sub(a.t_ms) > TOGETHER_MS {
                    break;
                }
                let overlaps = group.iter().all(|&k| b.t_ms < self.drops[k].back_at());
                let fresh = group.iter().all(|&k| self.drops[k].key != b.key);
                if !used.contains(&j) && b.bus() == a.bus() && overlaps && fresh {
                    group.push(j);
                }
            }
            if group.len() >= 2 {
                used.extend(&group);
                together.push(group);
            }
        }
        let rest: Vec<usize> = order
            .iter()
            .copied()
            .filter(|i| !used.contains(i))
            .collect();
        let mut chains = Vec::new();
        let mut run: Vec<usize> = Vec::new();
        let close = |run: &mut Vec<usize>, chains: &mut Vec<Vec<usize>>| {
            let keys: BTreeSet<&str> = run.iter().map(|&k| self.drops[k].key.as_str()).collect();
            if keys.len() >= CHAIN_DONGLES {
                chains.push(std::mem::take(run));
            } else {
                run.clear();
            }
        };
        for &i in &rest {
            if let Some(&last) = run.last()
                && self.drops[i].t_ms.saturating_sub(self.drops[last].t_ms) > CHAIN_MS
            {
                close(&mut run, &mut chains);
            }
            run.push(i);
        }
        close(&mut run, &mut chains);
        used.extend(chains.iter().flatten());
        let mut repeats: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for &i in &order {
            if !used.contains(&i) {
                repeats
                    .entry(self.drops[i].key.clone())
                    .or_default()
                    .push(i);
            }
        }
        repeats.retain(|_, v| v.len() >= REPEATS);
        Sorted {
            together,
            chains,
            repeats,
        }
    }

    pub fn findings(&self, rule: Rule, label: &dyn Fn(&str) -> String) -> Vec<Finding> {
        let s = self.sorted();
        let mut out = Vec::new();
        for group in &s.together {
            out.push(self.together(rule, group, label));
        }
        for chain in &s.chains {
            out.push(self.chain(rule, chain));
        }
        for (key, list) in &s.repeats {
            out.push(self.repeat(rule, key, list, label));
        }
        out
    }

    fn together(&self, rule: Rule, group: &[usize], label: &dyn Fn(&str) -> String) -> Finding {
        let drops: Vec<&PortDrop> = group.iter().map(|&i| &self.drops[i]).collect();
        let first = drops[0];
        let names: Vec<String> = drops.iter().map(|d| d.who(label)).collect();
        let hub =
            drops.iter().all(|d| d.parent() == first.parent()) && !first.parent().1.is_empty();
        let all_dongles = drops.iter().all(|d| d.dongle);
        let place = if hub {
            "one USB hub"
        } else {
            "one USB controller"
        };
        let title = if all_dongles {
            format!("Dongles on {place} dropped off together")
        } else {
            format!("Valve USB devices on {place} dropped off together")
        };
        let why = if hub {
            "They're plugged into the same USB hub, so the hub probably lost power or reset."
        } else {
            "They share one USB controller in the PC, so that controller or its ports probably lost power or reset."
        };
        let steps: &[&str] = if hub {
            &[
                "Give the hub its own power adapter, or swap it for a powered hub.",
                "Plug these dongles straight into ports on the PC instead.",
            ]
        } else {
            &[
                "Move some of them to ports on another USB controller. The USB page shows which controller each one is on.",
                "If they go through a hub or the PC's front ports, plug them straight into the back of the PC.",
            ]
        };
        Finding {
            rule,
            key: format!("together {}", first.t_ms),
            title,
            what: format!(
                "{} dropped off USB together{} and came back on their own. {why}",
                capital(&join(&names)),
                at(first.t_ms)
            ),
            steps: steps.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn chain(&self, rule: Rule, chain: &[usize]) -> Finding {
        let drops: Vec<&PortDrop> = chain.iter().map(|&i| &self.drops[i]).collect();
        let first = drops[0];
        let last = drops[drops.len() - 1];
        let keys: BTreeSet<&str> = drops.iter().map(|d| d.key.as_str()).collect();
        let buses: BTreeSet<&str> = drops.iter().map(|d| d.bus()).collect();
        let span = match (clock(first.t_ms), clock(last.t_ms)) {
            (Some(a), Some(z)) if a != z => format!("Between {a} and {z}, "),
            (Some(a), _) => format!("At {a}, "),
            _ => String::new(),
        };
        let total = if drops.len() > keys.len() {
            format!(" {}", plural(drops.len(), "time"))
        } else {
            String::new()
        };
        let lost = if drops.iter().any(|d| d.device.is_some()) {
            " Each time, the device using that dongle lost its connection for a moment."
        } else {
            ""
        };
        let spread = if buses.len() >= 2 {
            format!(
                " They're on {} different USB controllers, so one hub or port losing power doesn't explain it.",
                buses.len()
            )
        } else {
            " They're all on one USB controller.".to_string()
        };
        let mut steps = Vec::new();
        if buses.len() < 2 {
            steps.push("Move some of them to ports on another USB controller. The USB page shows which controller each one is on.".to_string());
        }
        steps.push("If it happens again, check what you started just before it, like a game or an overlay, and try a session without it.".to_string());
        Finding {
            rule,
            key: format!("chain {}", first.t_ms),
            title: "Your dongles dropped off USB one after another".into(),
            what: format!(
                "{span}{} of your dongles dropped off USB{total}, one at a time, and each came back by itself.{lost}{spread}",
                keys.len()
            ),
            steps,
        }
    }

    fn repeat(
        &self,
        rule: Rule,
        key: &str,
        list: &[usize],
        label: &dyn Fn(&str) -> String,
    ) -> Finding {
        let drops: Vec<&PortDrop> = list.iter().map(|&i| &self.drops[i]).collect();
        let d = drops[drops.len() - 1];
        let who = d.who(label);
        let times: Vec<String> = drops.iter().filter_map(|d| clock(d.t_ms)).collect();
        let when = if times.is_empty() {
            String::new()
        } else {
            format!(" ({})", join(&times))
        };
        let lost = match &d.device {
            Some(dev) => format!(" {} lost its connection each time.", capital(&label(dev))),
            None => String::new(),
        };
        let mut steps = vec![
            "Plug it into a different USB port, straight into the PC rather than a hub."
                .to_string(),
        ];
        if d.dongle {
            steps.push(
                "If it's on an extension cable, push both ends in firmly or try another cable."
                    .to_string(),
            );
        }
        Finding {
            rule,
            key: format!("repeat {key}"),
            title: format!("{} keeps dropping off USB", capital(&who)),
            what: format!(
                "It dropped off USB and came back on the same port {}{when}.{lost}",
                plural(drops.len(), "time")
            ),
            steps,
        }
    }
}

fn capital(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

fn clock(t_ms: u64) -> Option<String> {
    let (_, _, _, h, m) = crate::win::local_time(t_ms)?;
    Some(format!("{h:02}:{m:02}"))
}

fn at(t_ms: u64) -> String {
    clock(t_ms).map_or_else(String::new, |c| format!(" at {c}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Source;

    const T0: u64 = 1_792_609_714_233;
    const AMD_A: &str = "PCIROOT(0)#PCI(0801)#PCI(0003)#USBROOT(0)";
    const AMD_B: &str = "PCIROOT(0)#PCI(0102)#PCI(0000)#USBROOT(0)";
    const RENESAS: &str = "PCIROOT(0)#PCI(0102)#PCI(0002)#PCI(0600)#PCI(0000)#USBROOT(0)";

    fn usb(
        t: u64,
        removed: bool,
        bus: &str,
        ports: &str,
        sn: &str,
        dev: Option<&str>,
    ) -> SignalEvent {
        let port = format!("{bus}:{ports}");
        let detail = format!("28de:2101 Watchman Dongle sn={sn} port={port}");
        let kind = if removed {
            Kind::UsbRemove {
                port,
                dongle: Some(sn.into()),
            }
        } else {
            Kind::UsbAttach {
                port,
                dongle: Some(sn.into()),
            }
        };
        SignalEvent::at(t, Source::Usb, dev.map(String::from), kind, detail)
    }

    fn label(s: &str) -> String {
        s.to_string()
    }

    fn run(events: &[SignalEvent]) -> UsbHealth {
        let mut h = UsbHealth::default();
        for e in events {
            h.ingest(e);
        }
        h
    }

    #[test]
    fn a_hub_losing_power_drops_its_dongles_together() {
        let h = run(&[
            usb(T0, true, RENESAS, "5.1", "BC1B52F144", Some("LHR-A66FAA1B")),
            usb(T0, true, RENESAS, "5.2", "EC149B7BC3", None),
            usb(
                T0 + 501,
                true,
                RENESAS,
                "5.3",
                "65D86BDA85",
                Some("LHR-A43A0229"),
            ),
            usb(
                T0 + 1_505,
                false,
                RENESAS,
                "5.1",
                "BC1B52F144",
                Some("LHR-A66FAA1B"),
            ),
            usb(T0 + 1_505, false, RENESAS, "5.2", "EC149B7BC3", None),
            usb(
                T0 + 2_006,
                false,
                RENESAS,
                "5.3",
                "65D86BDA85",
                Some("LHR-A43A0229"),
            ),
        ]);
        let s = h.sorted();
        assert_eq!(s.together, vec![vec![0, 1, 2]]);
        assert!(s.chains.is_empty() && s.repeats.is_empty(), "{s:?}");
        let f = h.findings(Rule::UsbDrop, &label);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].title, "Dongles on one USB hub dropped off together");
        assert!(f[0].what.starts_with(
            "The dongle for LHR-A66FAA1B, dongle EC149B7BC3 and the dongle for LHR-A43A0229 dropped off USB together"
        ), "{}", f[0].what);
        assert!(f[0].steps[0].contains("powered hub"));
    }

    #[test]
    fn dongles_on_one_controller_without_a_hub_name_the_controller() {
        let h = run(&[
            usb(T0, true, AMD_A, "1", "57F60A0058", None),
            usb(T0 + 500, true, AMD_A, "4", "65D86BDA85", None),
            usb(T0 + 1_000, false, AMD_A, "1", "57F60A0058", None),
            usb(T0 + 1_500, false, AMD_A, "4", "65D86BDA85", None),
        ]);
        let f = h.findings(Rule::UsbDrop, &label);
        assert_eq!(f.len(), 1);
        assert_eq!(
            f[0].title,
            "Dongles on one USB controller dropped off together"
        );
    }

    #[test]
    fn the_recorded_sweep_reads_as_one_after_another_not_together() {
        let h = run(&[
            usb(T0, true, AMD_A, "1", "57F60A0058", Some("LHR-A43A0229")),
            usb(
                T0 + 2_007,
                false,
                AMD_A,
                "1",
                "57F60A0058",
                Some("LHR-A43A0229"),
            ),
            usb(
                T0 + 2_513,
                true,
                AMD_B,
                "9",
                "D373DE2627",
                Some("LHR-617D30D7"),
            ),
            usb(
                T0 + 3_518,
                false,
                AMD_B,
                "9",
                "D373DE2627",
                Some("LHR-617D30D7"),
            ),
            usb(
                T0 + 4_523,
                true,
                AMD_B,
                "7",
                "E1B9096D38",
                Some("LHR-905BD201"),
            ),
            usb(
                T0 + 5_528,
                false,
                AMD_B,
                "7",
                "E1B9096D38",
                Some("LHR-905BD201"),
            ),
            usb(T0 + 11_544, true, RENESAS, "5.2", "EC149B7BC3", None),
            usb(T0 + 12_549, false, RENESAS, "5.2", "EC149B7BC3", None),
            usb(
                T0 + 12_549,
                true,
                RENESAS,
                "7",
                "DDF8F5F1F6",
                Some("LHR-FD4FF7E2"),
            ),
            usb(
                T0 + 13_554,
                false,
                RENESAS,
                "7",
                "DDF8F5F1F6",
                Some("LHR-FD4FF7E2"),
            ),
            usb(
                T0 + 16_068,
                true,
                RENESAS,
                "5.1",
                "BC1B52F144",
                Some("LHR-A66FAA1B"),
            ),
            usb(
                T0 + 17_073,
                false,
                RENESAS,
                "5.1",
                "BC1B52F144",
                Some("LHR-A66FAA1B"),
            ),
            usb(
                T0 + 25_000,
                true,
                RENESAS,
                "5.1",
                "BC1B52F144",
                Some("LHR-A66FAA1B"),
            ),
            usb(
                T0 + 32_008,
                false,
                RENESAS,
                "5.1",
                "BC1B52F144",
                Some("LHR-A66FAA1B"),
            ),
            usb(
                T0 + 71_125,
                true,
                AMD_A,
                "4",
                "65D86BDA85",
                Some("LHR-10268F5C"),
            ),
            usb(
                T0 + 72_130,
                false,
                AMD_A,
                "4",
                "65D86BDA85",
                Some("LHR-10268F5C"),
            ),
        ]);
        let s = h.sorted();
        assert!(s.together.is_empty(), "{s:?}");
        assert!(s.repeats.is_empty(), "{s:?}");
        assert_eq!(s.chains, vec![vec![0, 1, 2, 3, 4, 5, 6]]);
        let f = h.findings(Rule::UsbDrop, &label);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].title, "Your dongles dropped off USB one after another");
        assert!(
            f[0].what.contains("6 of your dongles dropped off USB"),
            "{}",
            f[0].what
        );
        assert!(
            f[0].what
                .contains("6 of your dongles dropped off USB 7 times, one at a time"),
            "{}",
            f[0].what
        );
        assert!(
            f[0].what.contains("on 3 different USB controllers"),
            "{}",
            f[0].what
        );
    }

    #[test]
    fn one_dongle_dropping_twice_on_the_same_port_gets_its_own_card() {
        let h = run(&[
            usb(T0, true, RENESAS, "8", "0C58EE58E6", Some("LHR-7E902102")),
            usb(
                T0 + 1_005,
                false,
                RENESAS,
                "8",
                "0C58EE58E6",
                Some("LHR-7E902102"),
            ),
            usb(
                T0 + 600_000,
                true,
                RENESAS,
                "8",
                "0C58EE58E6",
                Some("LHR-7E902102"),
            ),
            usb(
                T0 + 603_006,
                false,
                RENESAS,
                "8",
                "0C58EE58E6",
                Some("LHR-7E902102"),
            ),
        ]);
        let f = h.findings(Rule::UsbDrop, &label);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].key, "repeat 0C58EE58E6");
        assert_eq!(
            f[0].title,
            "The dongle for LHR-7E902102 keeps dropping off USB"
        );
        assert!(f[0].what.contains("same port 2 times"), "{}", f[0].what);
        assert!(
            f[0].what
                .ends_with("LHR-7E902102 lost its connection each time.")
        );
    }

    #[test]
    fn moving_dongles_to_other_ports_is_not_a_drop() {
        let h = run(&[
            usb(T0, true, AMD_A, "1", "D373DE2627", Some("LHR-617D30D7")),
            usb(T0, true, AMD_A, "3", "65D86BDA85", Some("LHR-A43A0229")),
            usb(
                T0 + 6_005,
                false,
                RENESAS,
                "5.4",
                "D373DE2627",
                Some("LHR-617D30D7"),
            ),
            usb(
                T0 + 23_517,
                false,
                RENESAS,
                "5.3",
                "65D86BDA85",
                Some("LHR-A43A0229"),
            ),
            usb(T0 + 30_000, true, AMD_B, "7", "EC149B7BC3", None),
            usb(T0 + 34_503, false, AMD_A, "4", "EC149B7BC3", None),
        ]);
        assert!(h.drops().is_empty());
        assert!(!h.any());
    }

    #[test]
    fn a_healthy_session_and_a_single_drop_stay_quiet() {
        let mut h = run(&[usb(T0, false, AMD_A, "1", "57F60A0058", None)]);
        assert!(!h.any());
        h.ingest(&usb(T0 + 60_000, true, AMD_A, "1", "57F60A0058", None));
        h.ingest(&usb(T0 + 61_000, false, AMD_A, "1", "57F60A0058", None));
        assert_eq!(h.drops().len(), 1);
        assert!(!h.any());
        assert!(h.findings(Rule::UsbDrop, &label).is_empty());
    }

    #[test]
    fn a_dongle_gone_longer_than_the_limit_is_left_out() {
        let h = run(&[
            usb(T0, true, AMD_A, "1", "57F60A0058", None),
            usb(T0 + BACK_MS + 1, false, AMD_A, "1", "57F60A0058", None),
        ]);
        assert!(h.drops().is_empty());
    }

    #[test]
    fn product_names_come_from_the_detail() {
        assert_eq!(
            product("28de:2101 Watchman Dongle sn=57F60A0058 port=x:1"),
            ("Watchman Dongle".to_string(), true)
        );
        assert_eq!(
            product("28de:2613 Valve Index HMD sn=? port=x:1"),
            ("Valve Index HMD".to_string(), false)
        );
        assert_eq!(
            product("28de:2300 ? sn=? port=x:1"),
            ("Valve USB device".to_string(), false)
        );
    }
}

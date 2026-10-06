use crate::correlate::{Cause, Confidence, FlapSummary, Tag, Verdict};
use crate::event::DeviceClass;
use crate::names::Names;
use crate::signals::usb::{CROWDED, UsbDongle};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct DeviceRecord {
    pub serial: String,
    pub model: String,
    pub class: DeviceClass,
    pub dongle: String,
    pub lighthouse: bool,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RadioStats {
    pub count: u64,
    pub max_ms: u64,
}

pub const RADIO_OFF_MS: u64 = 60_000;

#[derive(Clone, Debug)]
pub struct Row {
    pub serial: String,
    pub label: String,
    pub lost_ms: u64,
    pub incidents: usize,
    pub flaps: u64,
    pub flaps_per_h: f64,
    pub longest_ms: u64,
    pub main: String,
    pub main_cause: Option<Cause>,
}

impl Row {
    pub fn trouble(&self) -> bool {
        self.lost_ms > 0 || self.incidents > 0 || self.flaps > 0
    }
}

#[derive(Clone, Debug, Default)]
pub struct Summary {
    pub span_ms: u64,
    pub rows: Vec<Row>,
    pub parked: Vec<String>,
    pub standby: usize,
    pub shifts: usize,
    pub base_faults: Vec<String>,
}

impl Summary {
    pub fn rank(&self, serial: &str) -> Option<usize> {
        self.rows
            .iter()
            .position(|r| r.serial == serial)
            .filter(|&i| self.rows[i].trouble())
            .map(|i| i + 1)
    }

    pub fn row(&self, serial: &str) -> Option<&Row> {
        self.rows.iter().find(|r| r.serial == serial)
    }
}

pub fn kind_label(model: &str, class: DeviceClass) -> &'static str {
    let m = model.to_ascii_lowercase();
    match class {
        DeviceClass::Hmd => "headset",
        DeviceClass::TrackingReference => "base station",
        _ if m.contains("left") => "left controller",
        _ if m.contains("right") => "right controller",
        DeviceClass::GenericTracker => "tracker",
        _ if m.contains("tracker") => "tracker",
        DeviceClass::Controller => "controller",
        _ => "device",
    }
}

pub fn label(serial: &str, devices: &[DeviceRecord], names: &Names) -> String {
    if let Some(n) = names.get(serial) {
        return n.to_string();
    }
    devices
        .iter()
        .find(|d| d.serial == serial)
        .map_or_else(String::new, |d| kind_label(&d.model, d.class).to_string())
}

pub fn plain_cause(c: &Cause) -> String {
    match c {
        Cause::UsbReset => "USB dongle reset".into(),
        Cause::UsbBandwidthSuspect => "several dongles dropped at once".into(),
        Cause::RfDropout => "wireless link dropped".into(),
        Cause::OcclusionOneBase(b) => format!("blocked from base {b}"),
        Cause::OcclusionBothBases => "lost sight of all bases".into(),
        Cause::NeverAcquiredBase => "never locked on to the bases".into(),
        Cause::ReflectionJump => "jumped (reflection?)".into(),
        Cause::ImuDriftDuringDropout => "drifted while the bases lost it".into(),
        Cause::BaseStandbyOrPowerdown(b) => format!("base {b} went to sleep"),
        Cause::BaseHardwareFault(b) => format!("base {b} hardware fault"),
        Cause::PlayspaceShift => "playspace shifted".into(),
        Cause::DriverStall => "pose froze".into(),
        Cause::Unknown => "dropout, cause unclear".into(),
    }
}

pub fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}

pub fn confidence_word(c: Confidence) -> &'static str {
    match c {
        Confidence::High => "confident",
        Confidence::Medium => "likely",
        Confidence::Low => "unsure",
    }
}

const FLICKER: &str = "brief flicker";

fn tip(c: Option<&Cause>) -> &'static str {
    match c {
        Some(Cause::UsbReset | Cause::UsbBandwidthSuspect | Cause::DriverStall) => {
            "its USB dongle reset or stalled. Move the dongle to another USB controller (see the USB view), avoid unpowered hubs, and turn off USB selective suspend in Windows power options."
        }
        Some(Cause::RfDropout) => {
            "the wireless link dropped while USB stayed up. Put the dongle on a USB extension cable with a clear view of the play space, away from the PC case and other dongles."
        }
        Some(
            Cause::OcclusionOneBase(_)
            | Cause::OcclusionBothBases
            | Cause::ImuDriftDuringDropout
            | Cause::NeverAcquiredBase,
        ) => {
            "the base stations could not see it. Check what blocks their view of where it is worn; mounting a base higher or adding one on the blind side helps."
        }
        Some(Cause::ReflectionJump) => {
            "it jumped while everything reported healthy, which often means a reflection. Cover mirrors, windows and glossy surfaces the bases can see."
        }
        Some(Cause::BaseStandbyOrPowerdown(_) | Cause::BaseHardwareFault(_)) => {
            "a base station slept, lost power or faulted. Check its power cable and SteamVR's base station power management setting."
        }
        Some(Cause::PlayspaceShift) => {
            "the whole playspace moved, usually the headset relocalizing. This is not a fault of the device."
        }
        Some(Cause::Unknown) => {
            "short dropouts with no clear signal. Check whether they happen in one spot or pose; the Room view shows where it was."
        }
        None => {
            "tracking flickered in and out while the pose stayed valid. Compare with your other devices: one far above the rest usually sits where the bases barely see it."
        }
    }
}

fn counted(v: &Verdict) -> bool {
    !v.tags.contains(&Tag::Standby)
        && !matches!(v.cause, Cause::PlayspaceShift | Cause::BaseHardwareFault(_))
        && !v.device.starts_with("base ")
}

fn is_tracked(serial: &str, devices: &[DeviceRecord]) -> bool {
    match devices.iter().find(|d| d.serial == serial) {
        Some(d) => {
            d.lighthouse
                && matches!(
                    d.class,
                    DeviceClass::Controller | DeviceClass::GenericTracker
                )
        }
        None => serial.starts_with("LHR-"),
    }
}

pub fn build(
    verdicts: &[Verdict],
    flaps: &FlapSummary,
    devices: &[DeviceRecord],
    parked: &HashSet<String>,
    names: &Names,
) -> Summary {
    let mut serials: Vec<String> = devices
        .iter()
        .map(|d| d.serial.clone())
        .chain(verdicts.iter().map(|v| v.device.clone()))
        .chain(flaps.devices.keys().cloned())
        .filter(|s| is_tracked(s, devices))
        .collect();
    serials.sort();
    serials.dedup();

    let hours = flaps.span_ms as f64 / 3_600_000.0;
    let mut out = Summary {
        span_ms: flaps.span_ms,
        ..Default::default()
    };
    for v in verdicts {
        if v.tags.contains(&Tag::Standby) {
            out.standby += 1;
        }
        match &v.cause {
            Cause::PlayspaceShift => out.shifts += 1,
            Cause::BaseHardwareFault(b) if !out.base_faults.contains(b) => {
                out.base_faults.push(b.clone())
            }
            _ => {}
        }
    }
    for serial in serials {
        if parked.contains(&serial) {
            out.parked.push(serial);
            continue;
        }
        let mine: Vec<&Verdict> = verdicts
            .iter()
            .filter(|v| v.device == serial && counted(v))
            .collect();
        let f = flaps.devices.get(&serial).cloned().unwrap_or_default();
        let mut causes: BTreeMap<String, (usize, u64, Cause)> = BTreeMap::new();
        for v in &mine {
            let e = causes
                .entry(plain_cause(&v.cause))
                .or_insert((0, 0, v.cause.clone()));
            e.0 += 1;
            e.1 += v.duration_ms.unwrap_or(0);
        }
        let main = causes
            .iter()
            .max_by_key(|(_, (n, ms, _))| (*n, *ms))
            .map(|(k, (_, _, c))| (k.clone(), Some(c.clone())));
        let (main, main_cause) = match main {
            Some(m) => m,
            None if f.count > 0 => (FLICKER.to_string(), None),
            None => (String::new(), None),
        };
        out.rows.push(Row {
            label: label(&serial, devices, names),
            lost_ms: mine.iter().map(|v| v.duration_ms.unwrap_or(0)).sum::<u64>() + f.total_ms,
            incidents: mine.len(),
            flaps: f.count,
            flaps_per_h: if hours > 0.0 {
                f.count as f64 / hours
            } else {
                0.0
            },
            longest_ms: mine
                .iter()
                .filter_map(|v| v.duration_ms)
                .max()
                .unwrap_or(0)
                .max(f.max_ms),
            main,
            main_cause,
            serial,
        });
    }
    out.rows.sort_by(|a, b| {
        b.lost_ms
            .cmp(&a.lost_ms)
            .then(b.incidents.cmp(&a.incidents))
            .then(b.flaps.cmp(&a.flaps))
            .then(a.serial.cmp(&b.serial))
    });
    out
}

pub fn fmt_dur(ms: u64) -> String {
    let s = ms / 1000;
    if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else if s < 3600 {
        format!("{}m {:02}s", s / 60, s % 60)
    } else {
        format!("{}h {:02}m", s / 3600, (s % 3600) / 60)
    }
}

fn bar(ms: u64, max: u64, width: usize) -> String {
    let n = if max == 0 || ms == 0 {
        0
    } else {
        ((ms as f64 / max as f64) * width as f64).ceil() as usize
    };
    format!("{:<width$}", "#".repeat(n.min(width)))
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(n.saturating_sub(1)).collect();
        t.push('~');
        t
    }
}

pub fn table_lines(s: &Summary) -> Vec<String> {
    let max = s.rows.iter().map(|r| r.lost_ms).max().unwrap_or(0);
    let mut out = vec![format!(
        " #  {:<31} {:<18} {:>9} {:>8}  {}",
        "device", "not tracking", "incidents", "flaps/h", "main problem"
    )];
    for (i, r) in s.rows.iter().enumerate() {
        let rank = if r.trouble() {
            format!("{:>2}", i + 1)
        } else {
            "  ".to_string()
        };
        let dev = format!("{} {}", r.serial, clip(&r.label, 18));
        if !r.trouble() {
            out.push(format!("{rank}  {dev:<31} clean"));
            continue;
        }
        out.push(format!(
            "{rank}  {dev:<31} {} {:>9} {:>9} {:>8.1}  {}",
            bar(r.lost_ms, max, 8),
            fmt_dur(r.lost_ms),
            r.incidents,
            r.flaps_per_h,
            r.main
        ));
    }
    out
}

pub fn notes(s: &Summary) -> Vec<String> {
    let mut out = Vec::new();
    if !s.parked.is_empty() {
        out.push(format!(
            "Ignored: {} (parked far outside the play space, e.g. a SpaceCalibrator reference).",
            s.parked.join(", ")
        ));
    }
    if s.standby > 0 {
        out.push(format!(
            "{} outage(s) happened while the headset was in standby or devices were powering off; not counted.",
            s.standby
        ));
    }
    if s.shifts > 0 {
        out.push(format!(
            "{} playspace shift(s): everything moved together, usually the headset relocalizing; not counted.",
            s.shifts
        ));
    }
    if !s.base_faults.is_empty() {
        out.push(format!(
            "Base station hardware warnings from SteamVR: {}.",
            s.base_faults.join(", ")
        ));
    }
    out
}

pub fn tips(s: &Summary) -> Vec<String> {
    let mut groups: Vec<(&'static str, Vec<&str>)> = Vec::new();
    for r in s.rows.iter().filter(|r| r.trouble()) {
        let t = tip(r.main_cause.as_ref());
        match groups.iter_mut().find(|(g, _)| *g == t) {
            Some((_, devs)) => devs.push(&r.serial),
            None => groups.push((t, vec![&r.serial])),
        }
    }
    groups
        .into_iter()
        .map(|(t, devs)| format!("{}: {t}", devs.join(", ")))
        .collect()
}

pub fn render(s: &Summary) -> String {
    let mut out = format!(
        "Session length {}. Worst first, ranked by time spent not tracking.\n",
        fmt_dur(s.span_ms)
    );
    if s.rows.is_empty() {
        out.push_str("No trackers or controllers were seen.\n");
    }
    for l in table_lines(s) {
        out.push_str(&l);
        out.push('\n');
    }
    for l in notes(s) {
        out.push_str(&l);
        out.push('\n');
    }
    let tips = tips(s);
    if !tips.is_empty() {
        out.push_str("\nWhat to try:\n");
        for t in tips {
            out.push_str(&format!("  {t}\n"));
        }
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Head,
    Plain,
    Good,
    Warn,
    Bad,
    Dim,
}

pub struct TreeLine {
    pub text: String,
    pub level: Level,
}

pub struct TreeCtx<'a> {
    pub devices: &'a [DeviceRecord],
    pub summary: Option<&'a Summary>,
    pub radio: &'a BTreeMap<String, RadioStats>,
    pub names: &'a Names,
}

impl TreeCtx<'_> {
    fn leaf(&self, d: &UsbDongle) -> (String, Level) {
        let serial = d.serial.as_deref().unwrap_or("?");
        let paired = self
            .devices
            .iter()
            .find(|m| !m.dongle.is_empty() && Some(m.dongle.as_str()) == d.serial.as_deref());
        let mut text = match paired {
            Some(_) => format!("dongle {serial}"),
            None if d.product.to_ascii_lowercase().contains("watchman") => {
                format!("dongle {serial}")
            }
            None => format!("{} {serial}", d.product),
        };
        let mut level = Level::Dim;
        match paired {
            Some(m) => {
                text.push_str(&format!(
                    " -> {} {}",
                    m.serial,
                    label(&m.serial, self.devices, self.names)
                ));
                let parked = self.summary.is_some_and(|s| s.parked.contains(&m.serial));
                let row = self.summary.and_then(|s| s.row(&m.serial));
                let rank = self.summary.and_then(|s| s.rank(&m.serial));
                if parked {
                    text.push_str("  parked");
                } else if let (Some(r), Some(k)) = (row, rank) {
                    text.push_str(&format!(
                        "  #{k}  {} not tracking, {}, {:.0} flaps/h",
                        fmt_dur(r.lost_ms),
                        plural(r.incidents, "incident"),
                        r.flaps_per_h
                    ));
                    level = if k <= 3 && r.lost_ms > 0 {
                        Level::Bad
                    } else {
                        Level::Warn
                    };
                } else if row.is_some() {
                    text.push_str("  clean");
                    level = Level::Good;
                } else {
                    level = Level::Plain;
                }
            }
            None if text.starts_with("dongle") => text.push_str(" -> nothing paired"),
            None => {}
        }
        if let Some(r) = d.serial.as_ref().and_then(|s| self.radio.get(s))
            && r.count > 0
        {
            text.push_str(&format!(
                "  radio gaps {} (longest {})",
                r.count,
                fmt_dur(r.max_ms)
            ));
        }
        (text, level)
    }

    fn walk(&self, items: &[&UsbDongle], depth: usize, indent: &str, out: &mut Vec<TreeLine>) {
        let mut groups: Vec<(u8, Vec<&UsbDongle>)> = Vec::new();
        for d in items {
            let Some(&p) = d.ports.get(depth) else {
                continue;
            };
            match groups.iter_mut().find(|(q, _)| *q == p) {
                Some((_, g)) => g.push(d),
                None => groups.push((p, vec![d])),
            }
        }
        groups.sort_by_key(|(p, _)| *p);
        let n = groups.len();
        for (i, (port, group)) in groups.into_iter().enumerate() {
            let last = i + 1 == n;
            let branch = if last { "'- " } else { "|- " };
            let child = format!("{indent}{}", if last { "    " } else { "|   " });
            if group.len() == 1 && group[0].ports.len() == depth + 1 {
                let (text, level) = self.leaf(group[0]);
                out.push(TreeLine {
                    text: format!("{indent}{branch}port {port}  {text}"),
                    level,
                });
                continue;
            }
            let hub = group[0]
                .hubs
                .get(depth)
                .cloned()
                .unwrap_or_else(|| "hub".into());
            let direct = group.iter().filter(|d| d.ports.len() == depth + 2).count();
            let (warn, level) = if direct >= CROWDED {
                (
                    format!("  ! {direct} Valve devices on this hub"),
                    Level::Warn,
                )
            } else {
                (String::new(), Level::Plain)
            };
            out.push(TreeLine {
                text: format!("{indent}{branch}port {port}  {hub}{warn}"),
                level,
            });
            self.walk(&group, depth + 1, &child, out);
        }
    }
}

pub fn usb_tree(dongles: &[UsbDongle], ctx: &TreeCtx) -> Vec<TreeLine> {
    let mut out = Vec::new();
    if dongles.is_empty() {
        out.push(TreeLine {
            text: "No Valve USB devices found.".into(),
            level: Level::Dim,
        });
        return out;
    }
    let mut buses: Vec<(&str, &str)> = Vec::new();
    for d in dongles {
        if !buses.contains(&(d.controller.as_str(), d.bus.as_str())) {
            buses.push((&d.controller, &d.bus));
        }
    }
    for (controller, bus) in buses {
        let items: Vec<&UsbDongle> = dongles
            .iter()
            .filter(|d| d.controller == controller && d.bus == bus)
            .collect();
        let direct = items.iter().filter(|d| d.ports.len() == 1).count();
        let plural = if items.len() == 1 { "" } else { "s" };
        let (warn, level) = if direct >= CROWDED {
            (
                format!("  ! {direct} on the root hub, crowded"),
                Level::Warn,
            )
        } else {
            (String::new(), Level::Head)
        };
        out.push(TreeLine {
            text: format!("{controller}  {} Valve device{plural}{warn}", items.len()),
            level,
        });
        ctx.walk(&items, 0, " ", &mut out);
    }
    out
}

pub fn render_tree(lines: &[TreeLine]) -> String {
    lines.iter().map(|l| format!("{}\n", l.text)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::correlate::{Confidence, FlapStats};

    fn v(device: &str, cause: Cause, ms: Option<u64>, tags: Vec<Tag>) -> Verdict {
        Verdict {
            device: device.into(),
            t_start_ms: 0,
            t_end_ms: 0,
            cause,
            confidence: Confidence::Medium,
            duration_ms: ms,
            pose_valid: Some(false),
            tags,
            evidence: vec![],
            alternates: vec![],
        }
    }

    fn rec(serial: &str, model: &str, class: DeviceClass, dongle: &str) -> DeviceRecord {
        DeviceRecord {
            serial: serial.into(),
            model: model.into(),
            class,
            dongle: dongle.into(),
            lighthouse: true,
        }
    }

    fn roster() -> Vec<DeviceRecord> {
        vec![
            rec("HMD1", "Meta Quest Pro", DeviceClass::Hmd, ""),
            rec("LHB-1", "Valve SR Imp", DeviceClass::TrackingReference, ""),
            rec("LHR-A", "Knuckles Left", DeviceClass::Controller, "D1"),
            rec(
                "LHR-B",
                "VIVE Tracker 3.0 MV",
                DeviceClass::GenericTracker,
                "D2",
            ),
            rec(
                "LHR-C",
                "VIVE Tracker 3.0 MV",
                DeviceClass::GenericTracker,
                "D3",
            ),
            rec(
                "LHR-P",
                "VIVE Tracker 3.0 MV",
                DeviceClass::GenericTracker,
                "D4",
            ),
        ]
    }

    fn flaps(span_ms: u64, entries: &[(&str, u64, u64)]) -> FlapSummary {
        let mut f = FlapSummary {
            threshold_ms: 500,
            span_ms,
            ..Default::default()
        };
        for (d, count, total) in entries {
            f.devices.insert(
                d.to_string(),
                FlapStats {
                    count: *count,
                    total_ms: *total,
                    max_ms: 100,
                },
            );
        }
        f
    }

    fn sample() -> Summary {
        let verdicts = vec![
            v("LHR-A", Cause::OcclusionBothBases, Some(30_000), vec![]),
            v("LHR-A", Cause::OcclusionBothBases, Some(11_000), vec![]),
            v("LHR-A", Cause::UsbReset, Some(500), vec![]),
            v("LHR-B", Cause::ReflectionJump, None, vec![]),
            v("LHR-B", Cause::Unknown, Some(80_000), vec![Tag::Standby]),
            v("LHR-B", Cause::PlayspaceShift, None, vec![]),
            v("LHR-P", Cause::Unknown, Some(9_000), vec![]),
            v(
                "base 9417",
                Cause::BaseHardwareFault("9417".into()),
                None,
                vec![],
            ),
        ];
        let parked: HashSet<String> = ["LHR-P".to_string()].into();
        build(
            &verdicts,
            &flaps(3_600_000, &[("LHR-B", 120, 9_000), ("LHR-A", 2, 200)]),
            &roster(),
            &parked,
            &Names::default(),
        )
    }

    #[test]
    fn ranks_by_time_not_tracking_and_skips_standby_and_parked() {
        let s = sample();
        let order: Vec<&str> = s.rows.iter().map(|r| r.serial.as_str()).collect();
        assert_eq!(order, ["LHR-A", "LHR-B", "LHR-C"]);
        let a = &s.rows[0];
        assert_eq!(a.lost_ms, 41_700);
        assert_eq!(a.incidents, 3);
        assert_eq!(a.main, "lost sight of all bases");
        assert_eq!(a.label, "left controller");
        assert_eq!(a.longest_ms, 30_000);
        let b = &s.rows[1];
        assert_eq!(b.lost_ms, 9_000, "standby outage must not count");
        assert_eq!(b.incidents, 1);
        assert!((b.flaps_per_h - 120.0).abs() < 1e-9);
        assert_eq!(s.rank("LHR-C"), None);
        assert_eq!(s.rank("LHR-B"), Some(2));
        assert_eq!(s.parked, ["LHR-P"]);
        assert_eq!((s.standby, s.shifts), (1, 1));
        assert_eq!(s.base_faults, ["9417"]);
    }

    #[test]
    fn render_shows_table_notes_and_grouped_tips() {
        let text = render(&sample());
        assert!(text.starts_with("Session length 1h 00m."), "{text}");
        assert!(
            text.contains(" 1  LHR-A left controller           ######## "),
            "{text}"
        );
        assert!(text.contains("41.7s"), "{text}");
        assert!(text.contains("    LHR-C tracker"), "{text}");
        assert!(text.contains("clean"), "{text}");
        assert!(text.contains("Ignored: LHR-P"), "{text}");
        assert!(
            text.contains("LHR-A: the base stations could not see it"),
            "{text}"
        );
        assert!(text.contains("LHR-B: it jumped"), "{text}");
    }

    #[test]
    fn flaps_only_device_gets_flicker_cause() {
        let s = build(
            &[],
            &flaps(1_800_000, &[("LHR-B", 30, 2_700)]),
            &roster(),
            &HashSet::new(),
            &Names::default(),
        );
        assert_eq!(s.rows[0].serial, "LHR-B");
        assert_eq!(s.rows[0].main, FLICKER);
        assert!((s.rows[0].flaps_per_h - 60.0).abs() < 1e-9);
        assert!(tips(&s)[0].starts_with("LHR-B: tracking flickered"));
    }

    #[test]
    fn old_sessions_without_roster_still_rank_lighthouse_serials() {
        let s = build(
            &[
                v("LHR-X", Cause::Unknown, Some(700), vec![]),
                v("1PASH", Cause::Unknown, Some(5), vec![]),
            ],
            &flaps(60_000, &[]),
            &[],
            &HashSet::new(),
            &Names::default(),
        );
        assert_eq!(s.rows.len(), 1);
        assert_eq!(s.rows[0].serial, "LHR-X");
        assert_eq!(s.rows[0].label, "");
    }

    fn dongle(serial: &str, ports: &[u8], hubs: &[&str], controller: &str) -> UsbDongle {
        UsbDongle {
            serial: Some(serial.into()),
            product: "Watchman Dongle".into(),
            controller: controller.into(),
            bus: controller.into(),
            ports: ports.to_vec(),
            hubs: hubs.iter().map(|h| h.to_string()).collect(),
        }
    }

    #[test]
    fn usb_tree_nests_hubs_and_marks_ranks() {
        let s = sample();
        let radio: BTreeMap<String, RadioStats> = [(
            "D1".to_string(),
            RadioStats {
                count: 4,
                max_ms: 300,
            },
        )]
        .into();
        let names = Names::default();
        let devices = roster();
        let ctx = TreeCtx {
            devices: &devices,
            summary: Some(&s),
            radio: &radio,
            names: &names,
        };
        let dongles = vec![
            dongle("D1", &[1], &[], "AMD USB controller (PCI 0801.0003)"),
            dongle("D2", &[3], &[], "AMD USB controller (PCI 0801.0003)"),
            dongle("D4", &[4], &[], "AMD USB controller (PCI 0801.0003)"),
            dongle(
                "D3",
                &[5, 2],
                &["Generic USB Hub 045B:0209"],
                "Renesas USB controller",
            ),
            dongle(
                "D9",
                &[5, 1],
                &["Generic USB Hub 045B:0209"],
                "Renesas USB controller",
            ),
        ];
        let text = render_tree(&usb_tree(&dongles, &ctx));
        let expected = "\
AMD USB controller (PCI 0801.0003)  3 Valve devices  ! 3 on the root hub, crowded
 |- port 1  dongle D1 -> LHR-A left controller  #1  41.7s not tracking, 3 incidents, 2 flaps/h  radio gaps 4 (longest 0.3s)
 |- port 3  dongle D2 -> LHR-B tracker  #2  9.0s not tracking, 1 incident, 120 flaps/h
 '- port 4  dongle D4 -> LHR-P tracker  parked
Renesas USB controller  2 Valve devices
 '- port 5  Generic USB Hub 045B:0209
     |- port 1  dongle D9 -> nothing paired
     '- port 2  dongle D3 -> LHR-C tracker  clean
";
        assert_eq!(text, expected);
    }
}

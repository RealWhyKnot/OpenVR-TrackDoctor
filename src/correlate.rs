use crate::event::{Kind, SignalEvent, Source, TrackState};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::mem::Discriminant;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum Cause {
    UsbReset,
    UsbBandwidthSuspect,
    RfDropout,
    OcclusionOneBase(String),
    OcclusionBothBases,
    NeverAcquiredBase,
    ReflectionJump,
    ImuDriftDuringDropout,
    BaseStandbyOrPowerdown(String),
    BaseHardwareFault(String),
    PlayspaceShift,
    DriverStall,
    Unknown,
}

impl Cause {
    pub fn describe(&self) -> String {
        match self {
            Self::UsbReset => "USB reset: this device's dongle re-enumerated on the USB bus".into(),
            Self::UsbBandwidthSuspect => "USB trouble across multiple dongles at once: suspect shared hub/controller bandwidth".into(),
            Self::RfDropout => "RF dropout: device lost the wireless link to its dongle (USB stayed fine)".into(),
            Self::OcclusionOneBase(b) => format!("occlusion: lost sight of base station {b}"),
            Self::OcclusionBothBases => "occlusion: lost sight of all visible base stations".into(),
            Self::NeverAcquiredBase => "never acquired: device could not bootstrap a pose from any base station".into(),
            Self::ReflectionJump => "pose jump while everything reported healthy: reflection or solver glitch (probabilistic)".into(),
            Self::ImuDriftDuringDropout => "IMU dead-reckoning drift during an optical dropout, corrected on reacquire".into(),
            Self::BaseStandbyOrPowerdown(b) => format!("base station {b} went to standby or powered down (multiple devices affected)"),
            Self::BaseHardwareFault(b) => format!("base station {b} hardware fault reported by the lighthouse driver"),
            Self::PlayspaceShift => "coherent shift across devices: playspace/calibration moved (e.g. HMD relocalization), not a device fault".into(),
            Self::DriverStall => "pose updates stopped while the device stayed nominally healthy: driver or USB stall".into(),
            Self::Unknown => "anomaly with no matching signal pattern".into(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Confidence {
    Low,
    Medium,
    High,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tag {
    Standby,
    OvrRecenter,
    BaseMoved,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Verdict {
    pub device: String,
    pub t_start_ms: u64,
    pub t_end_ms: u64,
    pub cause: Cause,
    pub confidence: Confidence,
    pub duration_ms: Option<u64>,
    pub pose_valid: Option<bool>,
    #[serde(default)]
    pub tags: Vec<Tag>,
    pub evidence: Vec<String>,
    pub alternates: Vec<Cause>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct FlapStats {
    pub count: u64,
    pub total_ms: u64,
    pub max_ms: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct FlapSummary {
    pub threshold_ms: u64,
    pub span_ms: u64,
    pub devices: BTreeMap<String, FlapStats>,
}

struct Case {
    device: String,
    t_open: u64,
    outage: Option<u64>,
    valid_at_open: bool,
}

struct Outage {
    device: String,
    start: u64,
    end: Option<u64>,
    pose_lost: bool,
    cased: bool,
}

struct Held {
    v: Verdict,
    outage_start: u64,
}

struct Cooldown {
    start: u64,
    until: u64,
    significant: bool,
}

pub struct Correlator {
    ring: VecDeque<SignalEvent>,
    cases: Vec<Case>,
    pending: Vec<Verdict>,
    held: Vec<Held>,
    hmd: Option<String>,
    suppress_until: u64,
    cooldown: HashMap<(String, Discriminant<Cause>), Cooldown>,
    laser_seen: HashMap<String, u64>,
    playspace_until: u64,
    pose_valid: HashMap<String, bool>,
    outages: Vec<Outage>,
    standby: Vec<(u64, Option<u64>)>,
    parked: HashSet<String>,
    flaps: BTreeMap<String, FlapStats>,
    first_ms: u64,
    last_ms: u64,
    retention_ms: u64,
    lookback_ms: u64,
    lookforward_ms: u64,
    settle_ms: u64,
    cooldown_ms: u64,
    startup_grace_ms: u64,
    flap_ms: u64,
    significant_ms: u64,
    max_hold_ms: u64,
    standby_tail_ms: u64,
    near_ms: u64,
}

impl Default for Correlator {
    fn default() -> Self {
        Self {
            ring: VecDeque::new(),
            cases: Vec::new(),
            pending: Vec::new(),
            held: Vec::new(),
            hmd: None,
            suppress_until: 0,
            cooldown: HashMap::new(),
            laser_seen: HashMap::new(),
            playspace_until: 0,
            pose_valid: HashMap::new(),
            outages: Vec::new(),
            standby: Vec::new(),
            parked: HashSet::new(),
            flaps: BTreeMap::new(),
            first_ms: 0,
            last_ms: 0,
            retention_ms: 30_000,
            lookback_ms: 10_000,
            lookforward_ms: 2_000,
            settle_ms: 500,
            cooldown_ms: 10_000,
            startup_grace_ms: 10_000,
            flap_ms: 500,
            significant_ms: 2_000,
            max_hold_ms: 120_000,
            standby_tail_ms: 5_000,
            near_ms: 2_000,
        }
    }
}

fn is_anomaly(ev: &SignalEvent) -> bool {
    match &ev.kind {
        Kind::Jump { .. }
        | Kind::OrientationJump { .. }
        | Kind::Drift { .. }
        | Kind::SnapBack { .. }
        | Kind::PoseFrozen { .. }
        | Kind::DeviceDeactivated
        | Kind::WirelessDisconnect => true,
        Kind::UsbRemove { dongle, .. } => dongle.is_some(),
        _ => false,
    }
}

fn fmt_evidence(ev: &SignalEvent, t_open: u64) -> String {
    let src = match ev.source {
        Source::Api => "A",
        Source::Log => "B",
        Source::Probe => "C",
        Source::Usb => "D",
    };
    format!("[{src}] {}: {}", fmt_offset(ev.t_ms, t_open), ev.detail)
}

fn fmt_offset(t_ms: u64, t_open: u64) -> String {
    let off = t_ms as i64 - t_open as i64;
    format!("{:+.1}s", off as f32 / 1000.0)
}

impl Correlator {
    pub fn set_hmd(&mut self, serial: String) {
        self.hmd = Some(serial);
    }

    pub fn set_parked(&mut self, serial: &str, parked: bool) {
        if parked {
            self.parked.insert(serial.to_string());
            self.outages.retain(|o| o.device != serial || o.cased);
        } else {
            self.parked.remove(serial);
        }
    }

    pub fn flap_count(&self, serial: &str) -> u64 {
        self.flaps.get(serial).map_or(0, |f| f.count)
    }

    pub fn flap_summary(&self) -> FlapSummary {
        FlapSummary {
            threshold_ms: self.flap_ms,
            span_ms: self.last_ms.saturating_sub(self.first_ms),
            devices: self.flaps.clone(),
        }
    }

    pub fn ingest(&mut self, ev: &SignalEvent) {
        if self.suppress_until == 0 {
            self.suppress_until = ev.t_ms + self.startup_grace_ms;
            self.first_ms = ev.t_ms;
        }
        self.last_ms = self.last_ms.max(ev.t_ms);
        if ev.kind == Kind::LogRotated {
            self.suppress_until = ev.t_ms + self.startup_grace_ms;
        }
        self.ring.push_back(ev.clone());

        match &ev.kind {
            Kind::BaseLaserFault { base } => {
                let last = self.laser_seen.get(base).copied().unwrap_or(0);
                if ev.t_ms.saturating_sub(last) > 300_000 {
                    self.laser_seen.insert(base.clone(), ev.t_ms);
                    self.pending.push(Verdict {
                        device: format!("base {base}"),
                        t_start_ms: ev.t_ms,
                        t_end_ms: ev.t_ms,
                        cause: Cause::BaseHardwareFault(base.clone()),
                        confidence: Confidence::High,
                        duration_ms: None,
                        pose_valid: None,
                        tags: Vec::new(),
                        evidence: vec![fmt_evidence(ev, ev.t_ms)],
                        alternates: vec![],
                    });
                }
                return;
            }
            Kind::StandbyStart => {
                if !self.standby.last().is_some_and(|s| s.1.is_none()) {
                    self.standby.push((ev.t_ms, None));
                    if self.standby.len() > 64 {
                        self.standby.remove(0);
                    }
                }
                return;
            }
            Kind::StandbyEnd => {
                if let Some(s) = self.standby.last_mut()
                    && s.1.is_none()
                {
                    s.1 = Some(ev.t_ms.max(s.0));
                }
                return;
            }
            _ => {}
        }

        let Some(device) = ev.device.clone() else {
            return;
        };
        match &ev.kind {
            Kind::PoseValid(valid) => {
                self.pose_changed(device, *valid);
                return;
            }
            Kind::TrackingState { from, to } => {
                self.state_changed(device, *from, *to, ev.t_ms);
                return;
            }
            _ => {}
        }
        if !is_anomaly(ev) || ev.t_ms < self.suppress_until || self.parked.contains(&device) {
            return;
        }
        if self.hmd.as_deref() == Some(device.as_str())
            && matches!(
                ev.kind,
                Kind::Jump { .. }
                    | Kind::OrientationJump { .. }
                    | Kind::PoseFrozen { .. }
                    | Kind::Drift { .. }
            )
        {
            return;
        }
        self.open_case(device, ev.t_ms, None);
    }

    fn pose_changed(&mut self, device: String, valid: bool) {
        self.pose_valid.insert(device.clone(), valid);
        if valid {
            return;
        }
        let Some(o) = self
            .outages
            .iter_mut()
            .find(|o| o.device == device && o.end.is_none())
        else {
            return;
        };
        o.pose_lost = true;
        if !o.cased {
            o.cased = true;
            let start = o.start;
            self.open_case(device, start, Some(start));
        }
    }

    fn state_changed(&mut self, device: String, from: TrackState, to: TrackState, t: u64) {
        let open = self
            .outages
            .iter()
            .position(|o| o.device == device && o.end.is_none());
        if to == TrackState::RunningOk {
            let Some(i) = open else {
                return;
            };
            let o = &mut self.outages[i];
            o.end = Some(t);
            if o.cased {
                return;
            }
            let dur = t.saturating_sub(o.start);
            if dur < self.flap_ms && !o.pose_lost {
                self.outages.remove(i);
                let f = self.flaps.entry(device).or_default();
                f.count += 1;
                f.total_ms += dur;
                f.max_ms = f.max_ms.max(dur);
            } else {
                o.cased = true;
                let start = o.start;
                self.open_case(device, start, Some(start));
            }
            return;
        }
        if from != TrackState::RunningOk
            || open.is_some()
            || t < self.suppress_until
            || self.parked.contains(&device)
        {
            return;
        }
        let valid = self.pose_valid.get(&device).copied().unwrap_or(true);
        self.outages.push(Outage {
            device: device.clone(),
            start: t,
            end: None,
            pose_lost: !valid,
            cased: !valid,
        });
        if !valid {
            self.open_case(device, t, Some(t));
        }
    }

    fn open_case(&mut self, device: String, t: u64, outage: Option<u64>) {
        let window = self.lookforward_ms;
        if let Some(c) = self.cases.iter_mut().find(|c| {
            c.device == device
                && c.t_open.abs_diff(t) < window
                && (outage.is_none() || c.outage.is_none() || c.outage == outage)
        }) {
            if c.outage.is_none() {
                c.outage = outage;
            }
            return;
        }
        let valid_at_open = self.pose_valid.get(&device).copied().unwrap_or(true);
        self.cases.push(Case {
            device,
            t_open: t,
            outage,
            valid_at_open,
        });
    }

    fn significant(&self, v: &Verdict) -> bool {
        v.confidence != Confidence::Low
            || v.pose_valid == Some(false)
            || v.duration_ms.is_some_and(|d| d >= self.significant_ms)
    }

    fn standby_overlap(&self, lo: u64, hi: u64) -> Option<(u64, Option<u64>)> {
        self.standby.iter().copied().find(|&(s, e)| {
            hi >= s && lo <= e.map_or(u64::MAX, |e| e.saturating_add(self.standby_tail_ms))
        })
    }

    pub fn tick(&mut self, now_ms: u64) -> Vec<Verdict> {
        while let Some(front) = self.ring.front() {
            if now_ms.saturating_sub(front.t_ms) > self.retention_ms {
                self.ring.pop_front();
            } else {
                break;
            }
        }

        let due: Vec<(String, u64)> = self
            .outages
            .iter_mut()
            .filter(|o| {
                !o.cased && o.end.is_none() && now_ms.saturating_sub(o.start) >= self.flap_ms
            })
            .map(|o| {
                o.cased = true;
                (o.device.clone(), o.start)
            })
            .collect();
        for (device, start) in due {
            self.open_case(device, start, Some(start));
        }

        let mut out = std::mem::take(&mut self.pending);
        let mut ready = Vec::new();
        let mut remaining = Vec::new();
        for case in std::mem::take(&mut self.cases) {
            if now_ms < case.t_open + self.lookforward_ms + self.settle_ms {
                remaining.push(case);
                continue;
            }
            let v = classify(
                &case,
                &self.ring,
                self.hmd.as_deref(),
                self.lookback_ms,
                self.lookforward_ms,
                self.near_ms,
            );
            match case.outage {
                Some(outage_start) => self.held.push(Held { v, outage_start }),
                None => ready.push(v),
            }
        }
        self.cases = remaining;

        for h in std::mem::take(&mut self.held) {
            let outage = self
                .outages
                .iter()
                .find(|o| o.device == h.v.device && o.start == h.outage_start);
            match outage {
                Some(o) if o.end.is_none() && now_ms.saturating_sub(o.start) < self.max_hold_ms => {
                    self.held.push(h)
                }
                _ => ready.push(finish_outage(h, outage, now_ms)),
            }
        }

        for mut v in ready {
            let hi = v.t_start_ms + v.duration_ms.unwrap_or(0);
            if let Some((s, e)) = self.standby_overlap(v.t_start_ms, hi) {
                if !v.tags.contains(&Tag::Standby) {
                    v.tags.push(Tag::Standby);
                }
                let until = e.map_or(" (still in standby)".to_string(), |e| {
                    format!(" to {}", fmt_offset(e, v.t_start_ms))
                });
                v.evidence.push(format!(
                    "HMD standby from {}{until}",
                    fmt_offset(s, v.t_start_ms)
                ));
            }
            if v.cause == Cause::PlayspaceShift {
                if now_ms < self.playspace_until {
                    continue;
                }
                self.playspace_until = v.t_end_ms + 5_000;
            }
            let key = (v.device.clone(), std::mem::discriminant(&v.cause));
            let significant = self.significant(&v);
            if let Some(c) = self.cooldown.get(&key)
                && v.t_start_ms >= c.start
                && v.t_start_ms < c.until
                && (c.significant || !significant)
            {
                continue;
            }
            self.cooldown.insert(
                key,
                Cooldown {
                    start: v.t_start_ms,
                    until: v.t_end_ms + self.cooldown_ms,
                    significant,
                },
            );
            out.push(v);
        }

        let retention = self.retention_ms;
        self.outages
            .retain(|o| o.end.is_none_or(|e| now_ms.saturating_sub(e) <= retention));
        out
    }
}

fn finish_outage(mut h: Held, outage: Option<&Outage>, now_ms: u64) -> Verdict {
    if let Some(o) = outage {
        let end = o.end.unwrap_or(now_ms);
        let dur = end.saturating_sub(o.start);
        h.v.duration_ms = Some(dur);
        h.v.pose_valid = Some(!o.pose_lost);
        h.v.t_end_ms = h.v.t_end_ms.max(end);
        if o.end.is_none() {
            h.v.evidence.push(format!(
                "outage still ongoing after {:.1}s",
                dur as f32 / 1000.0
            ));
        }
    }
    h.v
}

struct Window<'a> {
    all: Vec<&'a SignalEvent>,
    mine: Vec<&'a SignalEvent>,
    t_open: u64,
}

impl<'a> Window<'a> {
    fn evidence<F: Fn(&SignalEvent) -> bool>(&self, pred: F) -> Vec<String> {
        self.all
            .iter()
            .filter(|e| pred(e))
            .map(|e| fmt_evidence(e, self.t_open))
            .collect()
    }

    fn my_evidence<F: Fn(&Kind) -> bool>(&self, pred: F) -> Vec<String> {
        self.mine
            .iter()
            .filter(|e| pred(&e.kind))
            .map(|e| fmt_evidence(e, self.t_open))
            .collect()
    }

    fn mine_has<F: Fn(&Kind) -> bool>(&self, pred: F) -> bool {
        self.mine.iter().any(|e| pred(&e.kind))
    }

    fn near<F: Fn(&Kind) -> bool>(&self, near_ms: u64, pred: F) -> Vec<String> {
        self.evidence(|e| e.t_ms.abs_diff(self.t_open) <= near_ms && pred(&e.kind))
    }
}

fn classify(
    case: &Case,
    ring: &VecDeque<SignalEvent>,
    hmd: Option<&str>,
    lookback_ms: u64,
    lookforward_ms: u64,
    near_ms: u64,
) -> Verdict {
    let lo = case.t_open.saturating_sub(lookback_ms);
    let hi = case.t_open + lookforward_ms;
    let all: Vec<&SignalEvent> = ring
        .iter()
        .filter(|e| e.t_ms >= lo && e.t_ms <= hi)
        .collect();
    let mine: Vec<&SignalEvent> = all
        .iter()
        .copied()
        .filter(|e| e.device.as_deref() == Some(case.device.as_str()))
        .collect();
    let w = Window {
        all,
        mine,
        t_open: case.t_open,
    };

    let mine_dongle = my_dongle(&w);
    let stray_usb: Vec<String> = w.evidence(|e| {
        matches!(&e.kind, Kind::UsbRemove { dongle, .. } | Kind::UsbAttach { dongle, .. }
            if dongle.as_deref() != mine_dongle.as_deref())
    });

    let mut v = decide(case, &w, hmd, hi, near_ms);
    v.evidence.extend(stray_usb);

    let recenter = w.near(near_ms, |k| *k == Kind::OvrRecenter);
    if !recenter.is_empty() {
        v.tags.push(Tag::OvrRecenter);
        v.evidence.extend(recenter);
    }
    if !w
        .near(near_ms, |k| *k == Kind::BaseMovedForTracking)
        .is_empty()
    {
        v.tags.push(Tag::BaseMoved);
        v.evidence.extend(w.near(near_ms, |k| {
            matches!(k, Kind::BaseMoved { .. } | Kind::BaseMovedForTracking)
        }));
    }
    let power_off = w.my_evidence(|k| *k == Kind::DevicePowerOff);
    if !power_off.is_empty() {
        v.tags.push(Tag::Standby);
        v.evidence.extend(power_off);
    }
    v.pose_valid = Some(
        case.valid_at_open
            && !w
                .mine
                .iter()
                .any(|e| e.t_ms >= case.t_open && e.kind == Kind::PoseValid(false)),
    );
    let mut seen = HashSet::new();
    v.evidence.retain(|e| seen.insert(e.clone()));
    v
}

fn my_dongle(w: &Window) -> Option<String> {
    w.mine.iter().find_map(|e| match &e.kind {
        Kind::UsbRemove { dongle, .. } | Kind::UsbAttach { dongle, .. } => dongle.clone(),
        _ => None,
    })
}

fn decide(case: &Case, w: &Window, hmd: Option<&str>, hi: u64, near_ms: u64) -> Verdict {
    let verdict = |cause: Cause,
                   confidence: Confidence,
                   evidence: Vec<String>,
                   alternates: Vec<Cause>| Verdict {
        device: case.device.clone(),
        t_start_ms: case.t_open,
        t_end_ms: hi,
        cause,
        confidence,
        duration_ms: None,
        pose_valid: None,
        tags: Vec::new(),
        evidence,
        alternates,
    };

    let my_removal = w.mine_has(|k| matches!(k, Kind::UsbRemove { .. }));
    if my_removal {
        let removed_dongles: HashSet<&str> = w
            .all
            .iter()
            .filter_map(|e| match &e.kind {
                Kind::UsbRemove {
                    dongle: Some(d), ..
                } => Some(d.as_str()),
                _ => None,
            })
            .collect();
        let mut evidence = w.my_evidence(|k| {
            matches!(
                k,
                Kind::UsbRemove { .. }
                    | Kind::UsbAttach { .. }
                    | Kind::DeviceDeactivated
                    | Kind::PoseFrozen { .. }
                    | Kind::WirelessDisconnect
            )
        });
        if removed_dongles.len() > 1 {
            evidence.extend(w.evidence(|e| matches!(e.kind, Kind::UsbRemove { .. })));
            evidence.dedup();
            return verdict(
                Cause::UsbBandwidthSuspect,
                Confidence::Medium,
                evidence,
                vec![Cause::UsbReset],
            );
        }
        return verdict(Cause::UsbReset, Confidence::High, evidence, vec![]);
    }

    if w.mine_has(|k| *k == Kind::WirelessDisconnect) {
        let mut evidence = w.my_evidence(|k| {
            matches!(
                k,
                Kind::WirelessDisconnect
                    | Kind::WirelessReconnect
                    | Kind::DongleBind { .. }
                    | Kind::DeviceDeactivated
                    | Kind::BatteryLevel { .. }
            )
        });
        let t_dc = w
            .mine
            .iter()
            .find(|e| e.kind == Kind::WirelessDisconnect)
            .map_or(case.t_open, |e| e.t_ms);
        let hid = w.evidence(|e| e.kind == Kind::ImuHidError && e.t_ms.abs_diff(t_dc) <= 1_000);
        let malformed = w.evidence(|e| matches!(e.kind, Kind::MalformedPacket));
        let conf = if malformed.is_empty() {
            Confidence::Medium
        } else {
            Confidence::High
        };
        let alternates = if hid.is_empty() {
            vec![]
        } else {
            vec![Cause::UsbReset]
        };
        evidence.extend(hid);
        evidence.extend(malformed);
        return verdict(Cause::RfDropout, conf, evidence, alternates);
    }

    let losses: Vec<&SignalEvent> = w
        .mine
        .iter()
        .copied()
        .filter(|e| matches!(e.kind, Kind::OpticalLoss { .. }))
        .collect();
    let named: HashSet<String> = losses
        .iter()
        .filter_map(|e| match &e.kind {
            Kind::OpticalLoss { base: Some(b), .. } => Some(b.clone()),
            _ => None,
        })
        .collect();

    for base in &named {
        let same_base =
            |k: &Kind| matches!(k, Kind::OpticalLoss { base: Some(b), .. } if b == base);
        let other_devices: HashSet<&str> = w
            .all
            .iter()
            .filter(|e| same_base(&e.kind))
            .filter_map(|e| e.device.as_deref())
            .filter(|d| *d != case.device)
            .collect();
        if !other_devices.is_empty() {
            let evidence = w.evidence(|e| {
                same_base(&e.kind)
                    || matches!(
                        e.kind,
                        Kind::StandbyStart | Kind::StandbyEnd | Kind::OotxSelected
                    )
            });
            return verdict(
                Cause::BaseStandbyOrPowerdown(base.clone()),
                if other_devices.len() >= 2 {
                    Confidence::High
                } else {
                    Confidence::Medium
                },
                evidence,
                vec![Cause::OcclusionOneBase(base.clone())],
            );
        }
    }

    let never_valid = !case.valid_at_open && !w.mine_has(|k| *k == Kind::PoseValid(true));
    if never_valid
        && w.mine_has(|k| matches!(k, Kind::BootstrapFail | Kind::CalibrationFailed { .. }))
    {
        let evidence = w.my_evidence(|k| {
            matches!(
                k,
                Kind::BootstrapFail
                    | Kind::CalibrationFailed { .. }
                    | Kind::OpticalLoss { .. }
                    | Kind::DeviceActivated
                    | Kind::OotxSelected
                    | Kind::PoseValid(_)
            )
        });
        return verdict(Cause::NeverAcquiredBase, Confidence::High, evidence, vec![]);
    }

    let no_frames = w.mine_has(|k| *k == Kind::NoOpticalFrames);
    let mut optical_evidence = w.my_evidence(|k| {
        matches!(
            k,
            Kind::OpticalLoss { .. }
                | Kind::CalibrationFailed { .. }
                | Kind::SyncAcquired
                | Kind::NoOpticalFrames
                | Kind::ImuOffScale
                | Kind::Drift { .. }
                | Kind::SnapBack { .. }
                | Kind::TrackingState { .. }
                | Kind::PoseValid(_)
        )
    });

    if w.mine_has(|k| matches!(k, Kind::Drift { .. })) {
        let conf = if losses.is_empty() && !no_frames {
            Confidence::Medium
        } else {
            Confidence::High
        };
        return verdict(
            Cause::ImuDriftDuringDropout,
            conf,
            optical_evidence,
            vec![Cause::OcclusionBothBases],
        );
    }

    if !losses.is_empty() || no_frames {
        let all_bases = no_frames
            || named.len() >= 2
            || losses
                .iter()
                .any(|e| matches!(e.kind, Kind::OpticalLoss { base: None, .. }));
        if let Some(base) = named.iter().next().filter(|_| !all_bases) {
            let kept_tracking = case.outage.is_none()
                && case.valid_at_open
                && !w.mine_has(|k| *k == Kind::PoseValid(false));
            if kept_tracking {
                optical_evidence.push(
                    "device kept RunningOk tracking with a valid pose, so another base carried it"
                        .into(),
                );
                return verdict(
                    Cause::OcclusionOneBase(base.clone()),
                    Confidence::High,
                    optical_evidence,
                    vec![Cause::ReflectionJump],
                );
            }
            return verdict(
                Cause::OcclusionOneBase(base.clone()),
                Confidence::Medium,
                optical_evidence,
                vec![Cause::OcclusionBothBases],
            );
        }
        return verdict(
            Cause::OcclusionBothBases,
            Confidence::High,
            optical_evidence,
            vec![],
        );
    }

    let jumpers: HashSet<&str> = w
        .all
        .iter()
        .filter(|e| matches!(e.kind, Kind::Jump { .. }))
        .filter_map(|e| e.device.as_deref())
        .collect();
    let coherent =
        jumpers.len() >= 3 || (jumpers.len() >= 2 && hmd.is_some_and(|h| jumpers.contains(h)));
    let my_jump = w.mine_has(|k| {
        matches!(
            k,
            Kind::Jump { .. } | Kind::OrientationJump { .. } | Kind::SnapBack { .. }
        )
    });
    let recentered = !w.near(near_ms, |k| *k == Kind::OvrRecenter).is_empty();
    let base_popped = !w
        .near(near_ms, |k| *k == Kind::BaseMovedForTracking)
        .is_empty();
    if coherent || (my_jump && (recentered || base_popped)) {
        let mut evidence = Vec::new();
        if coherent {
            let mut devices: Vec<&str> = jumpers.into_iter().collect();
            devices.sort_unstable();
            evidence.push(format!("coherent jumps on: {}", devices.join(", ")));
        }
        evidence.extend(w.evidence(|e| matches!(e.kind, Kind::Jump { .. })));
        return verdict(
            Cause::PlayspaceShift,
            if recentered {
                Confidence::High
            } else {
                Confidence::Medium
            },
            evidence,
            vec![Cause::ReflectionJump],
        );
    }

    if w.mine_has(|k| matches!(k, Kind::PoseFrozen { .. })) {
        let evidence = w.my_evidence(|k| {
            matches!(
                k,
                Kind::PoseFrozen { .. } | Kind::TrackingState { .. } | Kind::DeviceDeactivated
            )
        });
        return verdict(
            Cause::DriverStall,
            Confidence::Medium,
            evidence,
            vec![Cause::UsbReset],
        );
    }

    if my_jump {
        let mut evidence = w.my_evidence(|k| {
            matches!(
                k,
                Kind::Jump { .. } | Kind::OrientationJump { .. } | Kind::SnapBack { .. }
            )
        });
        evidence.extend(w.evidence(|e| matches!(e.kind, Kind::BackFacingHits { .. })));
        return verdict(
            Cause::ReflectionJump,
            Confidence::Medium,
            evidence,
            vec![Cause::OcclusionOneBase("?".into())],
        );
    }

    let evidence: Vec<String> = w.mine.iter().map(|e| fmt_evidence(e, w.t_open)).collect();
    verdict(Cause::Unknown, Confidence::Low, evidence, vec![])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Source;

    const T0: u64 = 1_000_000;
    const DEV: &str = "LHR-AAAA1111";

    fn ev(
        t_ms: u64,
        source: Source,
        device: Option<&str>,
        kind: Kind,
        detail: &str,
    ) -> SignalEvent {
        SignalEvent {
            t_ms,
            source,
            device: device.map(String::from),
            kind,
            detail: detail.into(),
        }
    }

    fn warmed() -> Correlator {
        Correlator {
            suppress_until: 1,
            ..Correlator::default()
        }
    }

    fn run_with(c: &mut Correlator, events: Vec<SignalEvent>) -> Vec<Verdict> {
        let last = events.last().map(|e| e.t_ms).unwrap_or(0);
        for e in &events {
            c.ingest(e);
        }
        let mut v = c.tick(last + 3000);
        v.extend(c.tick(last + 200_000));
        v
    }

    fn run(events: Vec<SignalEvent>) -> Vec<Verdict> {
        run_with(&mut warmed(), events)
    }

    fn drive(c: &mut Correlator, mut events: Vec<SignalEvent>, until: u64) -> Vec<Verdict> {
        events.sort_by_key(|e| e.t_ms);
        let mut it = events.into_iter().peekable();
        let mut now = it.peek().map_or(until, |e| e.t_ms);
        let mut out = Vec::new();
        while now <= until {
            while let Some(e) = it.next_if(|e| e.t_ms <= now) {
                c.ingest(&e);
            }
            out.extend(c.tick(now));
            now += 100;
        }
        out
    }

    fn state(t: u64, device: &str, from: TrackState, to: TrackState) -> SignalEvent {
        ev(
            t,
            Source::Api,
            Some(device),
            Kind::TrackingState { from, to },
            "tracking change",
        )
    }

    fn state_drop(t: u64, device: &str) -> SignalEvent {
        state(
            t,
            device,
            TrackState::RunningOk,
            TrackState::RunningOutOfRange,
        )
    }

    fn flap_start(t: u64, device: &str) -> SignalEvent {
        state(
            t,
            device,
            TrackState::RunningOk,
            TrackState::CalibratingOutOfRange,
        )
    }

    fn flap_end(t: u64, device: &str) -> SignalEvent {
        state(
            t,
            device,
            TrackState::CalibratingOutOfRange,
            TrackState::RunningOk,
        )
    }

    fn pose(t: u64, device: &str, valid: bool) -> SignalEvent {
        ev(
            t,
            Source::Api,
            Some(device),
            Kind::PoseValid(valid),
            "pose valid change",
        )
    }

    fn loss(t: u64, device: &str, base: Option<&str>) -> SignalEvent {
        ev(
            t,
            Source::Log,
            Some(device),
            Kind::OpticalLoss {
                base: base.map(String::from),
                outage_ms: 2004,
            },
            "Resetting tracking: no optical samples for 2004ms",
        )
    }

    fn jump(t: u64, device: &str) -> SignalEvent {
        ev(
            t,
            Source::Api,
            Some(device),
            Kind::Jump {
                meters: 0.3,
                accel_mps2: 400.0,
            },
            "jump",
        )
    }

    fn log(t: u64, kind: Kind, detail: &str) -> SignalEvent {
        ev(t, Source::Log, None, kind, detail)
    }

    #[test]
    fn own_dongle_removal_is_usb_reset() {
        let v = run(vec![
            ev(
                T0,
                Source::Api,
                Some(DEV),
                Kind::DeviceDeactivated,
                "device 4 gone",
            ),
            ev(
                T0 + 100,
                Source::Usb,
                Some(DEV),
                Kind::UsbRemove {
                    port: "USB1:3.2".into(),
                    dongle: Some("8063CF813A".into()),
                },
                "28de:2101 sn=8063CF813A",
            ),
        ]);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].cause, Cause::UsbReset);
        assert_eq!(v[0].confidence, Confidence::High);
    }

    #[test]
    fn foreign_dongle_removal_does_not_blame_this_device() {
        let v = run(vec![
            state_drop(T0, DEV),
            loss(T0 + 300, DEV, Some("084071D2")),
            ev(
                T0 + 400,
                Source::Usb,
                Some("LHR-ZZZZ9999"),
                Kind::UsbRemove {
                    port: "USB1:9".into(),
                    dongle: Some("FFFF000011".into()),
                },
                "stray dongle pulled",
            ),
        ]);
        let mine: Vec<&Verdict> = v.iter().filter(|x| x.device == DEV).collect();
        assert_eq!(mine[0].cause, Cause::OcclusionOneBase("084071D2".into()));
        assert!(
            mine[0].evidence.iter().any(|e| e.contains("stray dongle")),
            "stray usb kept as evidence: {:?}",
            mine[0].evidence
        );
    }

    #[test]
    fn multi_dongle_usb_is_bandwidth_suspect() {
        let v = run(vec![
            ev(
                T0,
                Source::Usb,
                Some(DEV),
                Kind::UsbRemove {
                    port: "USB1:3.1".into(),
                    dongle: Some("AAAA".into()),
                },
                "dongle a",
            ),
            ev(
                T0 + 200,
                Source::Usb,
                Some("LHR-BBBB2222"),
                Kind::UsbRemove {
                    port: "USB1:3.4".into(),
                    dongle: Some("BBBB".into()),
                },
                "dongle b",
            ),
        ]);
        assert!(
            v.iter().any(|x| x.cause == Cause::UsbBandwidthSuspect),
            "{v:?}"
        );
    }

    #[test]
    fn wireless_without_usb_is_rf() {
        let v = run(vec![
            log(
                T0 - 500,
                Kind::MalformedPacket,
                "WARNING: Malformed wireless packet",
            ),
            ev(
                T0,
                Source::Api,
                Some(DEV),
                Kind::WirelessDisconnect,
                "device 4 wireless disconnect",
            ),
        ]);
        assert_eq!(v[0].cause, Cause::RfDropout);
        assert_eq!(v[0].confidence, Confidence::High);
    }

    #[test]
    fn log_receiver_disconnect_is_rf_with_hid_alternate() {
        let v = run(vec![
            log(
                T0,
                Kind::ImuHidError,
                "lighthouse: Lighthouse IMU HID device error",
            ),
            ev(
                T0,
                Source::Log,
                Some(DEV),
                Kind::WirelessDisconnect,
                "lighthouse: LHR-AAAA1111: Disconnected from receiver BC1B52F144",
            ),
        ]);
        assert_eq!(v.len(), 1, "{v:?}");
        assert_eq!(v[0].cause, Cause::RfDropout);
        assert_eq!(v[0].confidence, Confidence::Medium);
        assert_eq!(v[0].alternates, vec![Cause::UsbReset]);
        assert!(v[0].evidence.iter().any(|e| e.contains("HID device error")));
    }

    #[test]
    fn one_named_base_without_proof_is_medium() {
        let v = run(vec![
            state_drop(T0, DEV),
            loss(T0 + 400, DEV, Some("084071D2")),
        ]);
        assert_eq!(v[0].cause, Cause::OcclusionOneBase("084071D2".into()));
        assert_eq!(v[0].confidence, Confidence::Medium);
        assert_eq!(v[0].alternates, vec![Cause::OcclusionBothBases]);
    }

    #[test]
    fn one_named_base_while_tracking_is_high() {
        let v = run(vec![
            pose(T0 - 5000, DEV, true),
            jump(T0, DEV),
            loss(T0 + 300, DEV, Some("084071D2")),
        ]);
        assert_eq!(v[0].cause, Cause::OcclusionOneBase("084071D2".into()));
        assert_eq!(v[0].confidence, Confidence::High);
        assert!(v[0].evidence.iter().any(|e| e.contains("another base")));
    }

    #[test]
    fn calibration_timeout_is_not_occlusion() {
        let v = run(vec![
            pose(T0 - 5000, DEV, true),
            flap_start(T0, DEV),
            ev(
                T0 + 800,
                Source::Log,
                Some(DEV),
                Kind::CalibrationFailed {
                    base: "B394A63C".into(),
                    outage_ms: 2001,
                },
                "Calibration failed: no optical samples from base B394A63C for 2001ms",
            ),
            flap_end(T0 + 900, DEV),
        ]);
        assert_eq!(v.len(), 1, "{v:?}");
        assert!(
            !matches!(v[0].cause, Cause::OcclusionOneBase(_)),
            "{:?}",
            v[0]
        );
        assert!(
            v[0].evidence
                .iter()
                .any(|e| e.contains("Calibration failed"))
        );
    }

    #[test]
    fn two_base_loss_is_full_occlusion() {
        let v = run(vec![
            state_drop(T0, DEV),
            loss(T0 + 300, DEV, Some("084071D2")),
            loss(T0 + 500, DEV, Some("1D2B44AA")),
        ]);
        assert_eq!(v[0].cause, Cause::OcclusionBothBases);
    }

    #[test]
    fn baseless_reset_is_full_occlusion() {
        let v = run(vec![state_drop(T0, DEV), loss(T0 + 300, DEV, None)]);
        assert_eq!(v[0].cause, Cause::OcclusionBothBases);
        assert_eq!(v[0].confidence, Confidence::High);
    }

    #[test]
    fn fleet_wide_base_loss_is_base_standby() {
        let v = run(vec![
            state_drop(T0, DEV),
            loss(T0 + 100, DEV, Some("084071D2")),
            loss(T0 + 150, "LHR-BBBB2222", Some("084071D2")),
            loss(T0 + 200, "LHR-CCCC3333", Some("084071D2")),
        ]);
        assert_eq!(v[0].cause, Cause::BaseStandbyOrPowerdown("084071D2".into()));
        assert_eq!(v[0].confidence, Confidence::High);
    }

    #[test]
    fn reacquire_correction_is_imu_drift() {
        let v = run(vec![
            flap_start(T0, DEV),
            loss(T0 + 200, DEV, None),
            flap_end(T0 + 1500, DEV),
            ev(
                T0 + 1500,
                Source::Api,
                Some(DEV),
                Kind::Drift {
                    meters: 0.2,
                    secs: 1.5,
                },
                "drift 0.2m",
            ),
        ]);
        assert_eq!(v.len(), 1, "{v:?}");
        assert_eq!(v[0].cause, Cause::ImuDriftDuringDropout);
        assert_eq!(v[0].confidence, Confidence::High);
        assert_eq!(v[0].duration_ms, Some(1500));
    }

    #[test]
    fn drift_after_short_flap_still_reported() {
        let v = run(vec![
            flap_start(T0, DEV),
            flap_end(T0 + 90, DEV),
            ev(
                T0 + 90,
                Source::Api,
                Some(DEV),
                Kind::Drift {
                    meters: 0.15,
                    secs: 0.09,
                },
                "drift 0.15m",
            ),
        ]);
        assert_eq!(v.len(), 1, "{v:?}");
        assert_eq!(v[0].cause, Cause::ImuDriftDuringDropout);
        assert_eq!(v[0].confidence, Confidence::Medium);
    }

    #[test]
    fn healthy_jump_is_reflection_at_medium() {
        let v = run(vec![
            ev(
                T0 - 2000,
                Source::Log,
                Some(DEV),
                Kind::BackFacingHits { count: 172 },
                "Dropped 172 back-facing hits",
            ),
            jump(T0, DEV),
        ]);
        assert_eq!(v[0].cause, Cause::ReflectionJump);
        assert_eq!(v[0].confidence, Confidence::Medium);
        assert!(v[0].evidence.iter().any(|e| e.contains("back-facing")));
        assert_eq!(v[0].duration_ms, None);
        assert_eq!(v[0].pose_valid, Some(true));
    }

    #[test]
    fn playspace_shift_coalesces_to_one_verdict() {
        let v = run(vec![
            jump(T0, DEV),
            jump(T0 + 50, "LHR-BBBB2222"),
            jump(T0 + 90, "LHR-CCCC3333"),
        ]);
        let shifts: Vec<&Verdict> = v
            .iter()
            .filter(|x| x.cause == Cause::PlayspaceShift)
            .collect();
        assert_eq!(shifts.len(), 1, "{v:?}");
        assert!(shifts[0].evidence[0].contains(DEV));
    }

    #[test]
    fn hmd_plus_one_device_is_playspace_shift() {
        let mut c = warmed();
        c.set_hmd("QUEST-HMD".into());
        let v = run_with(&mut c, vec![jump(T0, "QUEST-HMD"), jump(T0 + 50, DEV)]);
        assert!(v.iter().any(|x| x.cause == Cause::PlayspaceShift), "{v:?}");
        assert!(
            !v.iter().any(|x| x.device == "QUEST-HMD"),
            "hmd never gets its own case: {v:?}"
        );
    }

    #[test]
    fn recenter_near_jump_is_tagged_playspace_shift() {
        let v = run(vec![
            log(
                T0,
                Kind::OvrRecenter,
                "oculus: OVR runtime requested recenter",
            ),
            jump(T0 + 1200, DEV),
        ]);
        assert_eq!(v.len(), 1, "{v:?}");
        assert_eq!(v[0].cause, Cause::PlayspaceShift);
        assert_eq!(v[0].confidence, Confidence::High);
        assert_eq!(v[0].tags, vec![Tag::OvrRecenter]);
        assert!(v[0].evidence.iter().any(|e| e.contains("recenter")));
    }

    #[test]
    fn recenter_far_from_jump_is_ignored() {
        let v = run(vec![
            log(
                T0,
                Kind::OvrRecenter,
                "oculus: OVR runtime requested recenter",
            ),
            jump(T0 + 2500, DEV),
        ]);
        assert_eq!(v[0].cause, Cause::ReflectionJump);
        assert!(v[0].tags.is_empty(), "{:?}", v[0].tags);
    }

    #[test]
    fn base_move_for_tracking_tags_jump() {
        let v = run(vec![
            log(
                T0,
                Kind::BaseMoved {
                    base: "9D8DEA7B".into(),
                    mm: 74,
                    deg: 1.0,
                },
                "lighthouse: Moving base 9D8DEA7B 74mm and 1.0 deg",
            ),
            log(
                T0,
                Kind::BaseMovedForTracking,
                "lighthouse: ... Moving the base for tracking too, which might cause a pop",
            ),
            jump(T0 + 300, DEV),
        ]);
        assert_eq!(v[0].cause, Cause::PlayspaceShift);
        assert_eq!(v[0].confidence, Confidence::Medium);
        assert_eq!(v[0].tags, vec![Tag::BaseMoved]);
        assert!(v[0].evidence.iter().any(|e| e.contains("74mm")));
    }

    #[test]
    fn frozen_pose_is_driver_stall() {
        let v = run(vec![ev(
            T0,
            Source::Api,
            Some(DEV),
            Kind::PoseFrozen { ms: 400 },
            "pose frozen for 400ms",
        )]);
        assert_eq!(v[0].cause, Cause::DriverStall);
    }

    #[test]
    fn never_acquired_after_activation() {
        let v = run(vec![
            pose(T0 - 100, DEV, false),
            state_drop(T0, DEV),
            ev(
                T0 + 500,
                Source::Log,
                Some(DEV),
                Kind::BootstrapFail,
                "Samples didn't yield successful bootstrap pose",
            ),
            loss(T0 + 600, DEV, Some("084071D2")),
        ]);
        assert_eq!(v[0].cause, Cause::NeverAcquiredBase);
    }

    #[test]
    fn base_laser_fault_deduped() {
        let mut c = warmed();
        let fault = |t| {
            log(
                t,
                Kind::BaseLaserFault {
                    base: "084071D2".into(),
                },
                "[PROBLEM] Basestation 084071D2 sending strong signals from one laser and not the other.",
            )
        };
        let v1 = run_with(&mut c, vec![fault(T0)]);
        let v2 = run_with(&mut c, vec![fault(T0 + 10_000)]);
        assert_eq!(v1.len(), 1);
        assert_eq!(v1[0].cause, Cause::BaseHardwareFault("084071D2".into()));
        assert_eq!(v1[0].device, "base 084071D2");
        assert!(v2.is_empty(), "{v2:?}");
    }

    #[test]
    fn boot_transitions_are_suppressed() {
        let mut c = Correlator::default();
        let v = run_with(
            &mut c,
            vec![
                state(
                    T0,
                    DEV,
                    TrackState::Uninitialized,
                    TrackState::CalibratingInProgress,
                ),
                state(
                    T0 + 500,
                    DEV,
                    TrackState::CalibratingInProgress,
                    TrackState::CalibratingOutOfRange,
                ),
                flap_end(T0 + 900, DEV),
            ],
        );
        assert!(v.is_empty(), "{v:?}");
    }

    #[test]
    fn sustained_incident_yields_one_verdict() {
        let mut c = warmed();
        let mut all = Vec::new();
        c.ingest(&state_drop(T0, DEV));
        for i in 0..6u64 {
            c.ingest(&loss(T0 + i * 1000, DEV, Some("084071D2")));
            c.ingest(&state_drop(T0 + 100 + i * 1000, DEV));
            all.extend(c.tick(T0 + 2500 + i * 1000));
        }
        c.ingest(&state(
            T0 + 7000,
            DEV,
            TrackState::RunningOutOfRange,
            TrackState::RunningOk,
        ));
        all.extend(c.tick(T0 + 60_000));
        let occl: Vec<&Verdict> = all
            .iter()
            .filter(|v| matches!(v.cause, Cause::OcclusionOneBase(_)))
            .collect();
        assert_eq!(occl.len(), 1, "{all:?}");
        assert_eq!(occl[0].duration_ms, Some(7000));
    }

    #[test]
    fn short_valid_flap_is_counted_not_cased() {
        let mut c = warmed();
        let v = run_with(
            &mut c,
            vec![
                pose(T0 - 1000, DEV, true),
                flap_start(T0, DEV),
                flap_end(T0 + 91, DEV),
                flap_start(T0 + 5000, DEV),
                flap_end(T0 + 5400, DEV),
            ],
        );
        assert!(v.is_empty(), "{v:?}");
        let s = c.flap_summary();
        assert_eq!(s.threshold_ms, 500);
        assert_eq!(
            s.devices[DEV],
            FlapStats {
                count: 2,
                total_ms: 491,
                max_ms: 400
            }
        );
        assert_eq!(c.flap_count(DEV), 2);
    }

    #[test]
    fn longer_valid_flap_gets_duration() {
        let v = run(vec![
            pose(T0 - 1000, DEV, true),
            flap_start(T0, DEV),
            flap_end(T0 + 600, DEV),
        ]);
        assert_eq!(v.len(), 1, "{v:?}");
        assert_eq!(v[0].cause, Cause::Unknown);
        assert_eq!(v[0].duration_ms, Some(600));
        assert_eq!(v[0].pose_valid, Some(true));
    }

    #[test]
    fn short_flap_with_lost_pose_opens_case() {
        let v = run(vec![
            pose(T0 - 1000, DEV, true),
            flap_start(T0, DEV),
            pose(T0 + 20, DEV, false),
            pose(T0 + 150, DEV, true),
            flap_end(T0 + 200, DEV),
        ]);
        assert_eq!(v.len(), 1, "{v:?}");
        assert_eq!(v[0].duration_ms, Some(200));
        assert_eq!(v[0].pose_valid, Some(false));
    }

    #[test]
    fn flap_noise_does_not_swallow_real_outage() {
        let mut c = warmed();
        let start = T0 + 3_000;
        let v = drive(
            &mut c,
            vec![
                pose(T0 - 1000, DEV, true),
                flap_start(T0, DEV),
                flap_end(T0 + 600, DEV),
                flap_start(start, DEV),
                pose(start + 1755, DEV, false),
                pose(start + 74_358, DEV, true),
                flap_end(start + 74_358, DEV),
            ],
            start + 80_000,
        );
        assert_eq!(v.len(), 2, "{v:?}");
        assert_eq!(v[0].duration_ms, Some(600));
        assert_eq!(v[0].pose_valid, Some(true));
        assert_eq!(v[1].t_start_ms, start);
        assert_eq!(v[1].duration_ms, Some(74_358));
        assert_eq!(v[1].pose_valid, Some(false));
        assert_eq!(v[1].t_end_ms, start + 74_358);
    }

    #[test]
    fn real_outage_still_suppresses_repeat_noise() {
        let mut c = warmed();
        let v = drive(
            &mut c,
            vec![
                flap_start(T0, DEV),
                pose(T0 + 100, DEV, false),
                pose(T0 + 9_000, DEV, true),
                flap_end(T0 + 9_000, DEV),
                flap_start(T0 + 12_000, DEV),
                flap_end(T0 + 12_700, DEV),
            ],
            T0 + 20_000,
        );
        assert_eq!(v.len(), 1, "{v:?}");
        assert_eq!(v[0].duration_ms, Some(9_000));
    }

    #[test]
    fn unended_outage_is_reported_after_hold_cap() {
        let mut c = warmed();
        let v = drive(
            &mut c,
            vec![flap_start(T0, DEV), pose(T0 + 1000, DEV, false)],
            T0 + 130_000,
        );
        assert_eq!(v.len(), 1, "{v:?}");
        assert_eq!(v[0].duration_ms, Some(120_000));
        assert!(v[0].evidence.iter().any(|e| e.contains("still ongoing")));
    }

    #[test]
    fn standby_marks_overlapping_and_trailing_verdicts() {
        let mut c = warmed();
        let v = drive(
            &mut c,
            vec![
                ev(T0, Source::Api, Some("HMD"), Kind::StandbyStart, "standby"),
                log(T0 + 4, Kind::StandbyStart, "0 - entering standby"),
                flap_start(T0 + 2_000, DEV),
                pose(T0 + 3_755, DEV, false),
                ev(
                    T0 + 33_000,
                    Source::Api,
                    Some("HMD"),
                    Kind::StandbyEnd,
                    "end",
                ),
                flap_end(T0 + 40_000, DEV),
                jump(T0 + 36_000, "LHR-BBBB2222"),
                jump(T0 + 60_000, "LHR-CCCC3333"),
            ],
            T0 + 70_000,
        );
        let tagged = |d: &str| {
            v.iter()
                .find(|x| x.device == d)
                .map(|x| x.tags.contains(&Tag::Standby))
        };
        assert_eq!(tagged(DEV), Some(true), "{v:?}");
        assert_eq!(tagged("LHR-BBBB2222"), Some(true), "{v:?}");
        assert_eq!(tagged("LHR-CCCC3333"), Some(false), "{v:?}");
        let mine = v.iter().find(|x| x.device == DEV).unwrap();
        assert!(mine.evidence.iter().any(|e| e.contains("HMD standby")));
    }

    #[test]
    fn device_power_off_is_standby_related() {
        let v = run(vec![
            ev(
                T0,
                Source::Log,
                Some(DEV),
                Kind::DevicePowerOff,
                "lighthouse: Device LHR-AAAA1111 powering off upon entering standby.",
            ),
            ev(
                T0 + 50,
                Source::Api,
                Some(DEV),
                Kind::DeviceDeactivated,
                "gone",
            ),
        ]);
        assert_eq!(v[0].tags, vec![Tag::Standby]);
    }

    #[test]
    fn parked_device_opens_no_cases() {
        let mut c = warmed();
        c.set_parked(DEV, true);
        let v = run_with(
            &mut c,
            vec![
                state_drop(T0, DEV),
                pose(T0 + 10, DEV, false),
                jump(T0 + 20, DEV),
            ],
        );
        assert!(v.is_empty(), "{v:?}");
    }

    #[test]
    fn old_verdict_lines_still_load() {
        let line = r#"{"device":"LHR-A","t_start_ms":1,"t_end_ms":2,"cause":"Unknown","confidence":"Low","evidence":[],"alternates":[]}"#;
        let v: Verdict = serde_json::from_str(line).unwrap();
        assert_eq!(v.duration_ms, None);
        assert!(v.tags.is_empty());
    }
}

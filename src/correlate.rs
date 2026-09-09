use crate::event::{Kind, SignalEvent, Source, TrackState};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
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
            Self::OcclusionOneBase(b) => format!("occlusion: lost sight of base station {b} (other base kept tracking)"),
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

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Verdict {
    pub device: String,
    pub t_start_ms: u64,
    pub t_end_ms: u64,
    pub cause: Cause,
    pub confidence: Confidence,
    pub evidence: Vec<String>,
    pub alternates: Vec<Cause>,
}

struct Case {
    device: String,
    t_open: u64,
}

pub struct Correlator {
    ring: VecDeque<SignalEvent>,
    cases: Vec<Case>,
    pending: Vec<Verdict>,
    hmd: Option<String>,
    suppress_until: u64,
    cooldown: HashMap<(String, Discriminant<Cause>), u64>,
    laser_seen: HashMap<String, u64>,
    playspace_until: u64,
    retention_ms: u64,
    lookback_ms: u64,
    lookforward_ms: u64,
    cooldown_ms: u64,
    startup_grace_ms: u64,
}

impl Default for Correlator {
    fn default() -> Self {
        Self {
            ring: VecDeque::new(),
            cases: Vec::new(),
            pending: Vec::new(),
            hmd: None,
            suppress_until: 0,
            cooldown: HashMap::new(),
            laser_seen: HashMap::new(),
            playspace_until: 0,
            retention_ms: 30_000,
            lookback_ms: 10_000,
            lookforward_ms: 2_000,
            cooldown_ms: 10_000,
            startup_grace_ms: 10_000,
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
        Kind::TrackingState { from, to } => {
            *from == TrackState::RunningOk && *to != TrackState::RunningOk
        }
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
    let off = ev.t_ms as i64 - t_open as i64;
    format!("[{src}] {:+.1}s: {}", off as f32 / 1000.0, ev.detail)
}

impl Correlator {
    pub fn set_hmd(&mut self, serial: String) {
        self.hmd = Some(serial);
    }

    pub fn ingest(&mut self, ev: &SignalEvent) {
        if self.suppress_until == 0 {
            self.suppress_until = ev.t_ms + self.startup_grace_ms;
        }
        if ev.kind == Kind::LogRotated {
            self.suppress_until = ev.t_ms + self.startup_grace_ms;
        }
        self.ring.push_back(ev.clone());

        if let Kind::BaseLaserFault { base } = &ev.kind {
            let last = self.laser_seen.get(base).copied().unwrap_or(0);
            if ev.t_ms.saturating_sub(last) > 300_000 {
                self.laser_seen.insert(base.clone(), ev.t_ms);
                self.pending.push(Verdict {
                    device: format!("base {base}"),
                    t_start_ms: ev.t_ms,
                    t_end_ms: ev.t_ms,
                    cause: Cause::BaseHardwareFault(base.clone()),
                    confidence: Confidence::High,
                    evidence: vec![fmt_evidence(ev, ev.t_ms)],
                    alternates: vec![],
                });
            }
            return;
        }

        if !is_anomaly(ev) || ev.t_ms < self.suppress_until {
            return;
        }
        let Some(device) = ev.device.clone() else {
            return;
        };
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
        let open_exists = self
            .cases
            .iter()
            .any(|c| c.device == device && ev.t_ms.saturating_sub(c.t_open) < self.lookforward_ms);
        if !open_exists {
            self.cases.push(Case {
                device,
                t_open: ev.t_ms,
            });
        }
    }

    pub fn tick(&mut self, now_ms: u64) -> Vec<Verdict> {
        while let Some(front) = self.ring.front() {
            if now_ms.saturating_sub(front.t_ms) > self.retention_ms {
                self.ring.pop_front();
            } else {
                break;
            }
        }
        let mut out = std::mem::take(&mut self.pending);
        let mut remaining = Vec::new();
        for case in self.cases.drain(..) {
            if now_ms < case.t_open + self.lookforward_ms {
                remaining.push(case);
                continue;
            }
            let v = classify(
                &case,
                &self.ring,
                self.hmd.as_deref(),
                self.lookback_ms,
                self.lookforward_ms,
            );
            if v.cause == Cause::PlayspaceShift {
                if now_ms < self.playspace_until {
                    continue;
                }
                self.playspace_until = v.t_end_ms + 5_000;
            }
            let key = (v.device.clone(), std::mem::discriminant(&v.cause));
            let until = self.cooldown.get(&key).copied().unwrap_or(0);
            if v.t_start_ms < until {
                continue;
            }
            self.cooldown.insert(key, v.t_end_ms + self.cooldown_ms);
            out.push(v);
        }
        self.cases = remaining;
        out
    }
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
}

fn classify(
    case: &Case,
    ring: &VecDeque<SignalEvent>,
    hmd: Option<&str>,
    lookback_ms: u64,
    lookforward_ms: u64,
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

    let mut v = decide(case, &w, hmd, hi);
    v.evidence.extend(stray_usb);
    v
}

fn my_dongle(w: &Window) -> Option<String> {
    w.mine.iter().find_map(|e| match &e.kind {
        Kind::UsbRemove { dongle, .. } | Kind::UsbAttach { dongle, .. } => dongle.clone(),
        _ => None,
    })
}

fn decide(case: &Case, w: &Window, hmd: Option<&str>, hi: u64) -> Verdict {
    let verdict = |cause: Cause,
                   confidence: Confidence,
                   evidence: Vec<String>,
                   alternates: Vec<Cause>| Verdict {
        device: case.device.clone(),
        t_start_ms: case.t_open,
        t_end_ms: hi,
        cause,
        confidence,
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
        let malformed = w.evidence(|e| matches!(e.kind, Kind::MalformedPacket));
        let conf = if malformed.is_empty() {
            Confidence::Medium
        } else {
            Confidence::High
        };
        evidence.extend(malformed);
        return verdict(Cause::RfDropout, conf, evidence, vec![]);
    }

    let my_bases: HashSet<String> = w
        .mine
        .iter()
        .filter_map(|e| match &e.kind {
            Kind::OpticalLoss { base, .. } => Some(base.clone()),
            _ => None,
        })
        .collect();

    if !my_bases.is_empty() {
        for base in &my_bases {
            let other_devices: HashSet<&str> = w
                .all
                .iter()
                .filter(|e| matches!(&e.kind, Kind::OpticalLoss { base: b, .. } if b == base))
                .filter_map(|e| e.device.as_deref())
                .filter(|d| *d != case.device)
                .collect();
            if !other_devices.is_empty() {
                let evidence = w.evidence(|e| {
                    matches!(&e.kind, Kind::OpticalLoss { base: b, .. } if b == base)
                        || matches!(
                            e.kind,
                            Kind::StandbyStart | Kind::LeavingStandby | Kind::OotxSelected
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

        let never_valid = w.mine_has(|k| matches!(k, Kind::BootstrapFail))
            && !w.mine_has(|k| *k == Kind::PoseValid(true));
        if never_valid {
            let evidence = w.my_evidence(|k| {
                matches!(
                    k,
                    Kind::BootstrapFail
                        | Kind::OpticalLoss { .. }
                        | Kind::DeviceActivated
                        | Kind::OotxSelected
                )
            });
            return verdict(Cause::NeverAcquiredBase, Confidence::High, evidence, vec![]);
        }

        let recovered = w.mine_has(|k| matches!(k, Kind::SyncAcquired));
        let drifted = w.mine_has(|k| matches!(k, Kind::Drift { .. } | Kind::SnapBack { .. }));
        let evidence = w.my_evidence(|k| {
            matches!(
                k,
                Kind::OpticalLoss { .. }
                    | Kind::SyncAcquired
                    | Kind::NoOpticalFrames
                    | Kind::ImuOffScale
                    | Kind::Drift { .. }
                    | Kind::SnapBack { .. }
                    | Kind::TrackingState { .. }
            )
        });
        if drifted && recovered {
            return verdict(
                Cause::ImuDriftDuringDropout,
                Confidence::High,
                evidence,
                vec![Cause::OcclusionBothBases],
            );
        }
        if my_bases.len() >= 2 || w.mine_has(|k| matches!(k, Kind::NoOpticalFrames)) {
            return verdict(
                Cause::OcclusionBothBases,
                Confidence::High,
                evidence,
                vec![Cause::ImuDriftDuringDropout],
            );
        }
        let base = my_bases.into_iter().next().unwrap();
        return verdict(
            Cause::OcclusionOneBase(base),
            Confidence::High,
            evidence,
            vec![Cause::ReflectionJump],
        );
    }

    let jumpers: HashSet<&str> = w
        .all
        .iter()
        .filter(|e| matches!(e.kind, Kind::Jump { .. }))
        .filter_map(|e| e.device.as_deref())
        .collect();
    if jumpers.len() >= 3 || (jumpers.len() >= 2 && hmd.is_some_and(|h| jumpers.contains(h))) {
        let mut devices: Vec<&str> = jumpers.into_iter().collect();
        devices.sort_unstable();
        let mut evidence = vec![format!("coherent jumps on: {}", devices.join(", "))];
        evidence.extend(w.evidence(|e| matches!(e.kind, Kind::Jump { .. })));
        return verdict(
            Cause::PlayspaceShift,
            Confidence::Medium,
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

    if w.mine_has(|k| matches!(k, Kind::Jump { .. } | Kind::OrientationJump { .. })) {
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
        let mut c = Correlator::default();
        c.suppress_until = 1;
        c
    }

    fn run_with(c: &mut Correlator, events: Vec<SignalEvent>) -> Vec<Verdict> {
        let last = events.last().map(|e| e.t_ms).unwrap_or(0);
        for e in &events {
            c.ingest(e);
        }
        c.tick(last + 3000)
    }

    fn run(events: Vec<SignalEvent>) -> Vec<Verdict> {
        run_with(&mut warmed(), events)
    }

    fn state_drop(t: u64, device: &str) -> SignalEvent {
        ev(
            t,
            Source::Api,
            Some(device),
            Kind::TrackingState {
                from: TrackState::RunningOk,
                to: TrackState::RunningOutOfRange,
            },
            "tracking RunningOk -> RunningOutOfRange",
        )
    }

    #[test]
    fn own_dongle_removal_is_usb_reset() {
        let v = run(vec![
            ev(
                T0,
                Source::Api,
                Some("LHR-AAAA1111"),
                Kind::DeviceDeactivated,
                "device 4 gone",
            ),
            ev(
                T0 + 100,
                Source::Usb,
                Some("LHR-AAAA1111"),
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
            state_drop(T0, "LHR-AAAA1111"),
            ev(
                T0 + 300,
                Source::Log,
                Some("LHR-AAAA1111"),
                Kind::OpticalLoss {
                    base: "084071D2".into(),
                    outage_ms: 900,
                },
                "no optical samples from base 084071D2 for 900ms",
            ),
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
        let mine: Vec<&Verdict> = v.iter().filter(|x| x.device == "LHR-AAAA1111").collect();
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
                Some("LHR-AAAA1111"),
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
            ev(
                T0 - 500,
                Source::Log,
                None,
                Kind::MalformedPacket,
                "WARNING: Malformed wireless packet",
            ),
            ev(
                T0,
                Source::Api,
                Some("LHR-AAAA1111"),
                Kind::WirelessDisconnect,
                "device 4 wireless disconnect",
            ),
        ]);
        assert_eq!(v[0].cause, Cause::RfDropout);
        assert_eq!(v[0].confidence, Confidence::High);
    }

    #[test]
    fn one_base_loss_is_occlusion() {
        let v = run(vec![
            state_drop(T0, "LHR-AAAA1111"),
            ev(
                T0 + 400,
                Source::Log,
                Some("LHR-AAAA1111"),
                Kind::OpticalLoss {
                    base: "084071D2".into(),
                    outage_ms: 2539,
                },
                "no optical samples from base 084071D2 for 2539ms",
            ),
        ]);
        assert_eq!(v[0].cause, Cause::OcclusionOneBase("084071D2".into()));
    }

    #[test]
    fn two_base_loss_is_full_occlusion() {
        let v = run(vec![
            state_drop(T0, "LHR-AAAA1111"),
            ev(
                T0 + 300,
                Source::Log,
                Some("LHR-AAAA1111"),
                Kind::OpticalLoss {
                    base: "084071D2".into(),
                    outage_ms: 900,
                },
                "loss a",
            ),
            ev(
                T0 + 500,
                Source::Log,
                Some("LHR-AAAA1111"),
                Kind::OpticalLoss {
                    base: "1D2B44AA".into(),
                    outage_ms: 800,
                },
                "loss b",
            ),
        ]);
        assert_eq!(v[0].cause, Cause::OcclusionBothBases);
    }

    #[test]
    fn fleet_wide_base_loss_is_base_standby() {
        let v = run(vec![
            state_drop(T0, "LHR-AAAA1111"),
            ev(
                T0 + 100,
                Source::Log,
                Some("LHR-AAAA1111"),
                Kind::OpticalLoss {
                    base: "084071D2".into(),
                    outage_ms: 900,
                },
                "loss",
            ),
            ev(
                T0 + 150,
                Source::Log,
                Some("LHR-BBBB2222"),
                Kind::OpticalLoss {
                    base: "084071D2".into(),
                    outage_ms: 900,
                },
                "loss",
            ),
            ev(
                T0 + 200,
                Source::Log,
                Some("LHR-CCCC3333"),
                Kind::OpticalLoss {
                    base: "084071D2".into(),
                    outage_ms: 900,
                },
                "loss",
            ),
        ]);
        assert_eq!(v[0].cause, Cause::BaseStandbyOrPowerdown("084071D2".into()));
        assert_eq!(v[0].confidence, Confidence::High);
    }

    #[test]
    fn drift_sandwich_is_imu_drift() {
        let v = run(vec![
            state_drop(T0, "LHR-AAAA1111"),
            ev(
                T0 + 200,
                Source::Log,
                Some("LHR-AAAA1111"),
                Kind::OpticalLoss {
                    base: "084071D2".into(),
                    outage_ms: 900,
                },
                "loss",
            ),
            ev(
                T0 + 900,
                Source::Api,
                Some("LHR-AAAA1111"),
                Kind::Drift {
                    meters: 0.2,
                    secs: 0.7,
                },
                "drift 0.2m",
            ),
            ev(
                T0 + 1500,
                Source::Log,
                Some("LHR-AAAA1111"),
                Kind::SyncAcquired,
                "tdm sync acquired",
            ),
        ]);
        assert_eq!(v[0].cause, Cause::ImuDriftDuringDropout);
    }

    #[test]
    fn healthy_jump_is_reflection_at_medium() {
        let v = run(vec![
            ev(
                T0 - 2000,
                Source::Log,
                Some("LHR-AAAA1111"),
                Kind::BackFacingHits { count: 172 },
                "Dropped 172 back-facing hits",
            ),
            ev(
                T0,
                Source::Api,
                Some("LHR-AAAA1111"),
                Kind::Jump {
                    meters: 0.12,
                    accel_mps2: 500.0,
                },
                "jump 0.12m",
            ),
        ]);
        assert_eq!(v[0].cause, Cause::ReflectionJump);
        assert_eq!(v[0].confidence, Confidence::Medium);
        assert!(v[0].evidence.iter().any(|e| e.contains("back-facing")));
    }

    #[test]
    fn playspace_shift_coalesces_to_one_verdict() {
        let v = run(vec![
            ev(
                T0,
                Source::Api,
                Some("LHR-AAAA1111"),
                Kind::Jump {
                    meters: 0.3,
                    accel_mps2: 400.0,
                },
                "jump",
            ),
            ev(
                T0 + 50,
                Source::Api,
                Some("LHR-BBBB2222"),
                Kind::Jump {
                    meters: 0.3,
                    accel_mps2: 400.0,
                },
                "jump",
            ),
            ev(
                T0 + 90,
                Source::Api,
                Some("LHR-CCCC3333"),
                Kind::Jump {
                    meters: 0.3,
                    accel_mps2: 400.0,
                },
                "jump",
            ),
        ]);
        let shifts: Vec<&Verdict> = v
            .iter()
            .filter(|x| x.cause == Cause::PlayspaceShift)
            .collect();
        assert_eq!(shifts.len(), 1, "{v:?}");
        assert!(shifts[0].evidence[0].contains("LHR-AAAA1111"));
    }

    #[test]
    fn hmd_plus_one_device_is_playspace_shift() {
        let mut c = warmed();
        c.set_hmd("QUEST-HMD".into());
        let v = run_with(
            &mut c,
            vec![
                ev(
                    T0,
                    Source::Api,
                    Some("QUEST-HMD"),
                    Kind::Jump {
                        meters: 0.3,
                        accel_mps2: 400.0,
                    },
                    "hmd jump",
                ),
                ev(
                    T0 + 50,
                    Source::Api,
                    Some("LHR-AAAA1111"),
                    Kind::Jump {
                        meters: 0.3,
                        accel_mps2: 400.0,
                    },
                    "jump",
                ),
            ],
        );
        assert!(v.iter().any(|x| x.cause == Cause::PlayspaceShift), "{v:?}");
        assert!(
            !v.iter().any(|x| x.device == "QUEST-HMD"),
            "hmd never gets its own case: {v:?}"
        );
    }

    #[test]
    fn frozen_pose_is_driver_stall() {
        let v = run(vec![ev(
            T0,
            Source::Api,
            Some("LHR-AAAA1111"),
            Kind::PoseFrozen { ms: 400 },
            "pose frozen for 400ms",
        )]);
        assert_eq!(v[0].cause, Cause::DriverStall);
    }

    #[test]
    fn never_acquired_after_activation() {
        let v = run(vec![
            state_drop(T0, "LHR-AAAA1111"),
            ev(
                T0 + 500,
                Source::Log,
                Some("LHR-AAAA1111"),
                Kind::BootstrapFail,
                "did not successfully get a bootstrap pose",
            ),
            ev(
                T0 + 600,
                Source::Log,
                Some("LHR-AAAA1111"),
                Kind::OpticalLoss {
                    base: "084071D2".into(),
                    outage_ms: 5000,
                },
                "loss",
            ),
        ]);
        assert_eq!(v[0].cause, Cause::NeverAcquiredBase);
    }

    #[test]
    fn base_laser_fault_deduped() {
        let mut c = warmed();
        let fault = |t| {
            ev(
                t,
                Source::Log,
                None,
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
        let boot = |t, from, to| {
            ev(
                t,
                Source::Api,
                Some("LHR-AAAA1111"),
                Kind::TrackingState { from, to },
                "boot",
            )
        };
        let v = run_with(
            &mut c,
            vec![
                boot(
                    T0,
                    TrackState::Uninitialized,
                    TrackState::CalibratingInProgress,
                ),
                boot(
                    T0 + 500,
                    TrackState::CalibratingInProgress,
                    TrackState::CalibratingOutOfRange,
                ),
                boot(
                    T0 + 900,
                    TrackState::CalibratingOutOfRange,
                    TrackState::RunningOk,
                ),
            ],
        );
        assert!(v.is_empty(), "{v:?}");
    }

    #[test]
    fn sustained_incident_yields_one_verdict() {
        let mut c = warmed();
        let mut all = Vec::new();
        c.ingest(&state_drop(T0, "LHR-AAAA1111"));
        for i in 0..6u64 {
            c.ingest(&ev(
                T0 + i * 1000,
                Source::Log,
                Some("LHR-AAAA1111"),
                Kind::OpticalLoss {
                    base: "084071D2".into(),
                    outage_ms: 900,
                },
                "loss",
            ));
            c.ingest(&state_drop(T0 + 100 + i * 1000, "LHR-AAAA1111"));
            all.extend(c.tick(T0 + 2500 + i * 1000));
        }
        all.extend(c.tick(T0 + 60_000));
        let occl: Vec<&Verdict> = all
            .iter()
            .filter(|v| matches!(v.cause, Cause::OcclusionOneBase(_)))
            .collect();
        assert_eq!(occl.len(), 1, "{all:?}");
    }
}

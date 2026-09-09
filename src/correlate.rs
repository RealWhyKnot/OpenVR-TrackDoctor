use crate::event::{Kind, SignalEvent, Source};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

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
    Unknown,
}

impl Cause {
    pub fn describe(&self) -> String {
        match self {
            Self::UsbReset => "USB reset: the dongle re-enumerated on the USB bus".into(),
            Self::UsbBandwidthSuspect => "USB trouble across multiple dongles at once: suspect shared hub/controller bandwidth".into(),
            Self::RfDropout => "RF dropout: device lost the wireless link to its dongle (USB stayed fine)".into(),
            Self::OcclusionOneBase(b) => format!("occlusion: lost sight of base station {b} (other base kept tracking)"),
            Self::OcclusionBothBases => "occlusion: lost sight of all visible base stations".into(),
            Self::NeverAcquiredBase => "never acquired: device could not bootstrap a pose from any base station".into(),
            Self::ReflectionJump => "pose jump while everything reported healthy: reflection or solver glitch (probabilistic)".into(),
            Self::ImuDriftDuringDropout => "IMU dead-reckoning drift during an optical dropout, corrected on reacquire".into(),
            Self::BaseStandbyOrPowerdown(b) => format!("base station {b} went to standby or powered down (all devices affected)"),
            Self::BaseHardwareFault(b) => format!("base station {b} hardware fault reported by the lighthouse driver"),
            Self::PlayspaceShift => "coherent shift across devices: playspace/calibration moved (e.g. HMD relocalization), not a device fault".into(),
            Self::Unknown => "anomaly with no matching signal pattern".into(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd)]
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
    retention_ms: u64,
    lookback_ms: u64,
    lookforward_ms: u64,
}

impl Default for Correlator {
    fn default() -> Self {
        Self { ring: VecDeque::new(), cases: Vec::new(), pending: Vec::new(), retention_ms: 30_000, lookback_ms: 10_000, lookforward_ms: 2_000 }
    }
}

fn is_anomaly(ev: &SignalEvent) -> bool {
    matches!(
        ev.kind,
        Kind::Jump { .. }
            | Kind::OrientationJump { .. }
            | Kind::Drift { .. }
            | Kind::SnapBack { .. }
            | Kind::DeviceDeactivated
            | Kind::WirelessDisconnect
    ) || matches!(&ev.kind, Kind::TrackingState { to, .. } if to != "RunningOk")
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
    pub fn ingest(&mut self, ev: &SignalEvent) {
        self.ring.push_back(ev.clone());
        while let Some(front) = self.ring.front() {
            if ev.t_ms.saturating_sub(front.t_ms) > self.retention_ms {
                self.ring.pop_front();
            } else {
                break;
            }
        }
        if let Kind::BaseLaserFault { base } = &ev.kind {
            self.pending.push(Verdict {
                device: ev.device.clone().unwrap_or_else(|| format!("base {base}")),
                t_start_ms: ev.t_ms,
                t_end_ms: ev.t_ms,
                cause: Cause::BaseHardwareFault(base.clone()),
                confidence: Confidence::High,
                evidence: vec![fmt_evidence(ev, ev.t_ms)],
                alternates: vec![],
            });
            return;
        }
        if is_anomaly(ev) {
            let device = ev.device.clone().unwrap_or_else(|| "unknown".into());
            let open_exists = self
                .cases
                .iter()
                .any(|c| c.device == device && ev.t_ms.saturating_sub(c.t_open) < self.lookforward_ms);
            if !open_exists {
                self.cases.push(Case { device, t_open: ev.t_ms });
            }
        }
    }

    pub fn tick(&mut self, now_ms: u64) -> Vec<Verdict> {
        let mut out = std::mem::take(&mut self.pending);
        let mut remaining = Vec::new();
        for case in self.cases.drain(..) {
            if now_ms >= case.t_open + self.lookforward_ms {
                out.push(classify(&case, &self.ring, self.lookback_ms, self.lookforward_ms));
            } else {
                remaining.push(case);
            }
        }
        self.cases = remaining;
        out
    }
}

fn classify(case: &Case, ring: &VecDeque<SignalEvent>, lookback_ms: u64, lookforward_ms: u64) -> Verdict {
    let lo = case.t_open.saturating_sub(lookback_ms);
    let hi = case.t_open + lookforward_ms;
    let window: Vec<&SignalEvent> = ring.iter().filter(|e| e.t_ms >= lo && e.t_ms <= hi).collect();
    let mine: Vec<&&SignalEvent> = window.iter().filter(|e| e.device.as_deref() == Some(case.device.as_str())).collect();
    let ev = |pred: &dyn Fn(&SignalEvent) -> bool| -> Vec<String> {
        window.iter().filter(|e| pred(e)).map(|e| fmt_evidence(e, case.t_open)).collect()
    };

    let verdict = |cause: Cause, confidence: Confidence, evidence: Vec<String>, alternates: Vec<Cause>| Verdict {
        device: case.device.clone(),
        t_start_ms: case.t_open,
        t_end_ms: hi,
        cause,
        confidence,
        evidence,
        alternates,
    };

    let removals: Vec<&&SignalEvent> = window.iter().filter(|e| matches!(e.kind, Kind::UsbRemove { .. })).collect();
    if !removals.is_empty() {
        let mut evidence = ev(&|e| matches!(e.kind, Kind::UsbRemove { .. } | Kind::UsbAttach { .. }));
        evidence.extend(mine.iter().filter(|e| is_anomaly(e)).map(|e| fmt_evidence(e, case.t_open)));
        let ports: std::collections::HashSet<&str> = removals
            .iter()
            .filter_map(|e| match &e.kind {
                Kind::UsbRemove { port } => Some(port.as_str()),
                _ => None,
            })
            .collect();
        return if ports.len() > 1 {
            verdict(Cause::UsbBandwidthSuspect, Confidence::Medium, evidence, vec![Cause::UsbReset])
        } else {
            verdict(Cause::UsbReset, Confidence::High, evidence, vec![])
        };
    }

    if mine.iter().any(|e| e.kind == Kind::WirelessDisconnect) {
        let mut evidence = ev(&|e| {
            matches!(e.kind, Kind::MalformedPacket)
                || (e.device.as_deref() == Some(case.device.as_str())
                    && matches!(e.kind, Kind::WirelessDisconnect | Kind::WirelessReconnect | Kind::DongleBind { .. }))
        });
        let malformed = evidence.iter().any(|s| s.contains("alformed"));
        evidence.extend(mine.iter().filter(|e| matches!(e.kind, Kind::DeviceDeactivated)).map(|e| fmt_evidence(e, case.t_open)));
        let conf = if malformed { Confidence::High } else { Confidence::Medium };
        return verdict(Cause::RfDropout, conf, evidence, vec![]);
    }

    let my_losses: Vec<(&str, u64)> = mine
        .iter()
        .filter_map(|e| match &e.kind {
            Kind::OpticalLoss { base, outage_ms } => Some((base.as_str(), *outage_ms)),
            _ => None,
        })
        .collect();
    let my_bases: std::collections::HashSet<&str> = my_losses.iter().map(|(b, _)| *b).collect();

    if !my_bases.is_empty() {
        for base in &my_bases {
            let other_devices: std::collections::HashSet<&str> = window
                .iter()
                .filter(|e| matches!(&e.kind, Kind::OpticalLoss { base: b, .. } if b == base))
                .filter_map(|e| e.device.as_deref())
                .filter(|d| *d != case.device)
                .collect();
            if !other_devices.is_empty() {
                let evidence = ev(&|e| matches!(&e.kind, Kind::OpticalLoss { base: b, .. } if b == *base) || matches!(e.kind, Kind::StandbyStart));
                return verdict(
                    Cause::BaseStandbyOrPowerdown(base.to_string()),
                    if other_devices.len() >= 2 { Confidence::High } else { Confidence::Medium },
                    evidence,
                    vec![Cause::OcclusionOneBase(base.to_string())],
                );
            }
        }

        let never_valid = mine.iter().any(|e| matches!(e.kind, Kind::BootstrapFail))
            && !mine.iter().any(|e| e.kind == Kind::PoseValid(true));
        if never_valid {
            let evidence = ev(&|e| {
                e.device.as_deref() == Some(case.device.as_str())
                    && matches!(e.kind, Kind::BootstrapFail | Kind::OpticalLoss { .. } | Kind::DeviceActivated)
            });
            return verdict(Cause::NeverAcquiredBase, Confidence::High, evidence, vec![]);
        }

        let recovered = mine.iter().any(|e| matches!(e.kind, Kind::SyncAcquired));
        let drifted = mine.iter().any(|e| matches!(e.kind, Kind::Drift { .. } | Kind::SnapBack { .. }));
        let mut evidence = ev(&|e| {
            e.device.as_deref() == Some(case.device.as_str())
                && matches!(
                    e.kind,
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
            return verdict(Cause::ImuDriftDuringDropout, Confidence::High, evidence, vec![Cause::OcclusionBothBases]);
        }
        if my_bases.len() >= 2 || mine.iter().any(|e| matches!(e.kind, Kind::NoOpticalFrames)) {
            return verdict(Cause::OcclusionBothBases, Confidence::High, evidence, vec![Cause::ImuDriftDuringDropout]);
        }
        let base = my_bases.iter().next().unwrap().to_string();
        evidence.retain(|s| !s.is_empty());
        return verdict(Cause::OcclusionOneBase(base.clone()), Confidence::High, evidence, vec![Cause::ReflectionJump]);
    }

    let jumpers: std::collections::HashSet<&str> = window
        .iter()
        .filter(|e| matches!(e.kind, Kind::Jump { .. }))
        .filter_map(|e| e.device.as_deref())
        .collect();
    if jumpers.len() >= 3 {
        let evidence = ev(&|e| matches!(e.kind, Kind::Jump { .. }));
        return verdict(Cause::PlayspaceShift, Confidence::Medium, evidence, vec![Cause::ReflectionJump]);
    }

    let jumped = mine.iter().any(|e| matches!(e.kind, Kind::Jump { .. } | Kind::OrientationJump { .. }));
    if jumped {
        let evidence = ev(&|e| {
            (e.device.as_deref() == Some(case.device.as_str())
                && matches!(e.kind, Kind::Jump { .. } | Kind::OrientationJump { .. } | Kind::SnapBack { .. }))
                || matches!(e.kind, Kind::BackFacingHits { .. })
        });
        return verdict(Cause::ReflectionJump, Confidence::Medium, evidence, vec![Cause::OcclusionOneBase("?".into())]);
    }

    let evidence = mine.iter().map(|e| fmt_evidence(e, case.t_open)).collect();
    verdict(Cause::Unknown, Confidence::Low, evidence, vec![])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Source, now_ms};

    fn ev(t_ms: u64, source: Source, device: Option<&str>, kind: Kind, detail: &str) -> SignalEvent {
        SignalEvent { t_ms, source, device: device.map(String::from), kind, detail: detail.into() }
    }

    fn run(events: Vec<SignalEvent>) -> Vec<Verdict> {
        let mut c = Correlator::default();
        let last = events.last().map(|e| e.t_ms).unwrap_or(0);
        for e in &events {
            c.ingest(e);
        }
        c.tick(last + 3000)
    }

    #[test]
    fn usb_reset_wins_over_everything() {
        let t = now_ms();
        let v = run(vec![
            ev(t, Source::Usb, None, Kind::UsbRemove { port: "USB1:3.2".into() }, "28de:2101 Watchman Dongle"),
            ev(t + 100, Source::Api, Some("LHR-AAAA1111"), Kind::DeviceDeactivated, "device 4 gone"),
        ]);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].cause, Cause::UsbReset);
        assert_eq!(v[0].confidence, Confidence::High);
    }

    #[test]
    fn multi_dongle_usb_is_bandwidth_suspect() {
        let t = now_ms();
        let v = run(vec![
            ev(t, Source::Usb, None, Kind::UsbRemove { port: "USB1:3.1".into() }, "dongle a"),
            ev(t + 200, Source::Usb, None, Kind::UsbRemove { port: "USB1:3.4".into() }, "dongle b"),
            ev(t + 300, Source::Api, Some("LHR-AAAA1111"), Kind::DeviceDeactivated, "device 4 gone"),
        ]);
        assert_eq!(v[0].cause, Cause::UsbBandwidthSuspect);
    }

    #[test]
    fn wireless_without_usb_is_rf() {
        let t = now_ms();
        let v = run(vec![
            ev(t - 500, Source::Log, None, Kind::MalformedPacket, "WARNING: Malformed wireless packet"),
            ev(t, Source::Api, Some("LHR-AAAA1111"), Kind::WirelessDisconnect, "device 4 wireless disconnect"),
        ]);
        assert_eq!(v[0].cause, Cause::RfDropout);
        assert_eq!(v[0].confidence, Confidence::High);
    }

    #[test]
    fn one_base_loss_is_occlusion() {
        let t = now_ms();
        let v = run(vec![
            ev(t, Source::Api, Some("LHR-AAAA1111"), Kind::TrackingState { from: "RunningOk".into(), to: "RunningOutOfRange".into() }, "state change"),
            ev(t + 400, Source::Log, Some("LHR-AAAA1111"), Kind::OpticalLoss { base: "084071D2".into(), outage_ms: 2539 }, "no optical samples from base 084071D2 for 2539ms"),
        ]);
        assert_eq!(v[0].cause, Cause::OcclusionOneBase("084071D2".into()));
    }

    #[test]
    fn two_base_loss_is_full_occlusion() {
        let t = now_ms();
        let v = run(vec![
            ev(t, Source::Api, Some("LHR-AAAA1111"), Kind::TrackingState { from: "RunningOk".into(), to: "FallbackRotationOnly".into() }, "state change"),
            ev(t + 300, Source::Log, Some("LHR-AAAA1111"), Kind::OpticalLoss { base: "084071D2".into(), outage_ms: 900 }, "loss a"),
            ev(t + 500, Source::Log, Some("LHR-AAAA1111"), Kind::OpticalLoss { base: "1D2B44AA".into(), outage_ms: 800 }, "loss b"),
        ]);
        assert_eq!(v[0].cause, Cause::OcclusionBothBases);
    }

    #[test]
    fn fleet_wide_base_loss_is_base_standby() {
        let t = now_ms();
        let v = run(vec![
            ev(t, Source::Api, Some("LHR-AAAA1111"), Kind::TrackingState { from: "RunningOk".into(), to: "RunningOutOfRange".into() }, "state change"),
            ev(t + 100, Source::Log, Some("LHR-AAAA1111"), Kind::OpticalLoss { base: "084071D2".into(), outage_ms: 900 }, "loss"),
            ev(t + 150, Source::Log, Some("LHR-BBBB2222"), Kind::OpticalLoss { base: "084071D2".into(), outage_ms: 900 }, "loss"),
            ev(t + 200, Source::Log, Some("LHR-CCCC3333"), Kind::OpticalLoss { base: "084071D2".into(), outage_ms: 900 }, "loss"),
        ]);
        assert_eq!(v[0].cause, Cause::BaseStandbyOrPowerdown("084071D2".into()));
        assert_eq!(v[0].confidence, Confidence::High);
    }

    #[test]
    fn drift_sandwich_is_imu_drift() {
        let t = now_ms();
        let v = run(vec![
            ev(t, Source::Api, Some("LHR-AAAA1111"), Kind::TrackingState { from: "RunningOk".into(), to: "FallbackRotationOnly".into() }, "state change"),
            ev(t + 200, Source::Log, Some("LHR-AAAA1111"), Kind::OpticalLoss { base: "084071D2".into(), outage_ms: 900 }, "loss"),
            ev(t + 900, Source::Api, Some("LHR-AAAA1111"), Kind::Drift { meters: 0.2, secs: 0.7 }, "drift 0.2m"),
            ev(t + 1500, Source::Log, Some("LHR-AAAA1111"), Kind::SyncAcquired, "tdm sync acquired"),
            ev(t + 1600, Source::Api, Some("LHR-AAAA1111"), Kind::SnapBack { meters: 0.19 }, "snap back"),
        ]);
        assert_eq!(v[0].cause, Cause::ImuDriftDuringDropout);
    }

    #[test]
    fn healthy_jump_is_reflection_at_medium() {
        let t = now_ms();
        let v = run(vec![
            ev(t - 2000, Source::Log, Some("LHR-AAAA1111"), Kind::BackFacingHits { count: 172 }, "Dropped 172 back-facing hits"),
            ev(t, Source::Api, Some("LHR-AAAA1111"), Kind::Jump { meters: 0.12, accel_mps2: 500.0 }, "jump 0.12m"),
        ]);
        assert_eq!(v[0].cause, Cause::ReflectionJump);
        assert_eq!(v[0].confidence, Confidence::Medium);
        assert!(v[0].evidence.iter().any(|e| e.contains("back-facing")));
    }

    #[test]
    fn coherent_multi_device_jump_is_playspace_shift() {
        let t = now_ms();
        let v = run(vec![
            ev(t, Source::Api, Some("LHR-AAAA1111"), Kind::Jump { meters: 0.3, accel_mps2: 400.0 }, "jump"),
            ev(t + 50, Source::Api, Some("LHR-BBBB2222"), Kind::Jump { meters: 0.3, accel_mps2: 400.0 }, "jump"),
            ev(t + 90, Source::Api, Some("LHR-CCCC3333"), Kind::Jump { meters: 0.3, accel_mps2: 400.0 }, "jump"),
        ]);
        assert!(v.iter().all(|x| x.cause == Cause::PlayspaceShift), "{v:?}");
    }

    #[test]
    fn never_acquired_after_activation() {
        let t = now_ms();
        let v = run(vec![
            ev(t, Source::Api, Some("LHR-AAAA1111"), Kind::TrackingState { from: "Uninitialized".into(), to: "CalibratingOutOfRange".into() }, "state change"),
            ev(t + 500, Source::Log, Some("LHR-AAAA1111"), Kind::BootstrapFail, "did not successfully get a bootstrap pose"),
            ev(t + 600, Source::Log, Some("LHR-AAAA1111"), Kind::OpticalLoss { base: "084071D2".into(), outage_ms: 5000 }, "loss"),
        ]);
        assert_eq!(v[0].cause, Cause::NeverAcquiredBase);
    }

    #[test]
    fn base_laser_fault_direct_verdict() {
        let t = now_ms();
        let v = run(vec![ev(
            t,
            Source::Log,
            None,
            Kind::BaseLaserFault { base: "084071D2".into() },
            "[PROBLEM] Basestation 084071D2 sending strong signals from one laser and not the other.",
        )]);
        assert_eq!(v[0].cause, Cause::BaseHardwareFault("084071D2".into()));
    }
}

use serde::Serialize;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Serialize, Clone, Debug)]
pub struct SignalEvent {
    pub t_ms: u64,
    pub source: Source,
    pub device: Option<String>,
    pub kind: Kind,
    pub detail: String,
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Api,
    Log,
    Usb,
    #[allow(dead_code)]
    Probe,
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceClass {
    Hmd,
    Controller,
    GenericTracker,
    TrackingReference,
    Other,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub enum Kind {
    DeviceActivated,
    DeviceDeactivated,
    TrackingState {
        from: TrackState,
        to: TrackState,
    },
    PoseValid(bool),
    WirelessDisconnect,
    WirelessReconnect,
    StandbyStart,
    StandbyEnd,
    OpticalLoss {
        base: String,
        outage_ms: u64,
    },
    SyncAcquired,
    BackFacingHits {
        count: u64,
    },
    DongleBind {
        dongle: String,
    },
    MalformedPacket,
    ImuOffScale,
    BootstrapFail,
    OotxSelected,
    LeavingStandby,
    BaseLaserFault {
        base: String,
    },
    NoOpticalFrames,
    LogRotated,
    UsbAttach {
        port: String,
        dongle: Option<String>,
    },
    UsbRemove {
        port: String,
        dongle: Option<String>,
    },
    Jump {
        meters: f32,
        accel_mps2: f32,
    },
    OrientationJump {
        rad_per_s: f32,
    },
    Drift {
        meters: f32,
        secs: f32,
    },
    SnapBack {
        meters: f32,
    },
    PoseFrozen {
        ms: u64,
    },
    BatteryLevel {
        pct: f32,
    },
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Kind::Jump { meters, accel_mps2 } => write!(
                f,
                "pose jump {meters:.3}m (implied accel {accel_mps2:.0} m/s^2)"
            ),
            Kind::OrientationJump { rad_per_s } => {
                write!(f, "orientation jump {rad_per_s:.0} rad/s")
            }
            Kind::Drift { meters, secs } => {
                write!(f, "dead-reckoning drift {meters:.3}m over {secs:.1}s")
            }
            Kind::SnapBack { meters } => write!(f, "snap-back correction {meters:.3}m"),
            Kind::PoseFrozen { ms } => write!(f, "pose frozen for {ms}ms while reported valid"),
            Kind::BatteryLevel { pct } => write!(f, "battery {pct:.0}%"),
            Kind::TrackingState { from, to } => write!(f, "tracking {from:?} -> {to:?}"),
            k => write!(f, "{k:?}"),
        }
    }
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackState {
    Uninitialized,
    CalibratingInProgress,
    CalibratingOutOfRange,
    RunningOk,
    RunningOutOfRange,
    FallbackRotationOnly,
    Other(i32),
}

impl TrackState {
    pub fn from_raw(v: i32) -> Self {
        if v == openvr_sys::ETrackingResult_TrackingResult_Uninitialized {
            Self::Uninitialized
        } else if v == openvr_sys::ETrackingResult_TrackingResult_Calibrating_InProgress {
            Self::CalibratingInProgress
        } else if v == openvr_sys::ETrackingResult_TrackingResult_Calibrating_OutOfRange {
            Self::CalibratingOutOfRange
        } else if v == openvr_sys::ETrackingResult_TrackingResult_Running_OK {
            Self::RunningOk
        } else if v == openvr_sys::ETrackingResult_TrackingResult_Running_OutOfRange {
            Self::RunningOutOfRange
        } else if v == openvr_sys::ETrackingResult_TrackingResult_Fallback_RotationOnly {
            Self::FallbackRotationOnly
        } else {
            Self::Other(v)
        }
    }

    pub fn is_calibrating(self) -> bool {
        matches!(
            self,
            Self::CalibratingInProgress | Self::CalibratingOutOfRange
        )
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl SignalEvent {
    pub fn new(
        source: Source,
        device: Option<String>,
        kind: Kind,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            t_ms: now_ms(),
            source,
            device,
            kind,
            detail: detail.into(),
        }
    }
}

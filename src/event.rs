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

#[derive(Serialize, Clone, Debug, PartialEq)]
pub enum Kind {
    DeviceActivated,
    DeviceDeactivated,
    TrackingState { from: String, to: String },
    PoseValid(bool),
    WirelessDisconnect,
    WirelessReconnect,
    StandbyStart,
    StandbyEnd,
    OpticalLoss { base: String, outage_ms: u64 },
    SyncAcquired,
    BackFacingHits { count: u64 },
    DongleBind { dongle: String },
    MalformedPacket,
    ImuOffScale,
    BootstrapFail,
    OotxSelected,
    LeavingStandby,
    BaseLaserFault { base: String },
    NoOpticalFrames,
    UsbAttach { port: String },
    UsbRemove { port: String },
    Jump { meters: f32, accel_mps2: f32 },
    OrientationJump { rad_per_s: f32 },
    Drift { meters: f32, secs: f32 },
    SnapBack { meters: f32 },
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
        matches!(self, Self::CalibratingInProgress | Self::CalibratingOutOfRange)
    }

    pub fn label(self) -> String {
        match self {
            Self::Other(v) => format!("Other({v})"),
            s => format!("{s:?}"),
        }
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl SignalEvent {
    pub fn new(source: Source, device: Option<String>, kind: Kind, detail: impl Into<String>) -> Self {
        Self { t_ms: now_ms(), source, device, kind, detail: detail.into() }
    }
}

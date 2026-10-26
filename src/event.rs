use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SignalEvent {
    pub t_ms: u64,
    pub source: Source,
    pub device: Option<String>,
    pub kind: Kind,
    pub detail: String,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Api,
    Log,
    Usb,
    #[allow(dead_code)]
    Probe,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceClass {
    Hmd,
    Controller,
    GenericTracker,
    TrackingReference,
    Other,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
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
        base: Option<String>,
        outage_ms: u64,
    },
    CalibrationFailed {
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
    DevicePowerOff,
    ImuHidError,
    BaseMoved {
        base: String,
        mm: u32,
        deg: f32,
    },
    BaseMovedForTracking,
    OvrRecenter,
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
    RadioGap {
        dongle: String,
        ms: u64,
    },
    Parked(bool),
    BaseReset {
        base: String,
    },
    BoardUnreachable {
        dongle: String,
    },
    DeviceConfigFail {
        dongle: String,
    },
    ImuMissing,
    SyncMismatch,
    DbDisallowed,
    ChannelConflict {
        ch: u32,
    },
    RotorPhase,
    BaseRelation {
        a: String,
        b: String,
        mm: u32,
        deg: f32,
    },
    BaseSight {
        ch: u32,
        gained: bool,
        seeing: Vec<u32>,
        ids: Vec<(u32, String)>,
    },
    RoomCheck {
        code: u32,
    },
    UpdateAvailable {
        manual: bool,
        waiting: bool,
    },
    Driver {
        name: String,
        status: DriverStatus,
    },
    StillJitter {
        mm: f32,
        secs: f32,
        bases: Option<u32>,
        base_m: Option<f32>,
    },
    RestartFailed {
        base: String,
        hits: Option<(u32, u32)>,
    },
    PositionOutOfBounds {
        x: f32,
        y: f32,
        z: f32,
    },
    ImuMisaligned,
    BaseLayout {
        bases: Vec<crate::check_layout::LayoutBase>,
        arc_deg: Option<f32>,
        samples: usize,
    },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum DriverStatus {
    Loaded(String),
    Disabled,
    Failed(String),
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Kind::Jump { meters, accel_mps2 } => write!(
                f,
                "jumped {meters:.2} m in one frame ({accel_mps2:.0} m/s\u{b2})"
            ),
            Kind::OrientationJump { rad_per_s } => {
                write!(
                    f,
                    "snapped round at {:.0}\u{b0} a second",
                    rad_per_s.to_degrees()
                )
            }
            Kind::Drift { meters, secs } => {
                write!(
                    f,
                    "drifted {meters:.2} m in {secs:.1} s on its motion sensor alone"
                )
            }
            Kind::SnapBack { meters } => {
                write!(f, "snapped back {meters:.2} m when the bases found it")
            }
            Kind::PoseFrozen { ms } => write!(
                f,
                "position stopped updating for {ms} ms while SteamVR still called it valid"
            ),
            Kind::BatteryLevel { pct } => write!(f, "battery {pct:.0}%"),
            Kind::TrackingState { from, to } => {
                write!(f, "went from {} to {}", from.plain(), to.plain())
            }
            Kind::RadioGap { dongle, ms } => {
                write!(f, "no radio packets for {ms} ms on dongle {dongle}")
            }
            Kind::Parked(true) => write!(f, "sitting far outside the play space, so it's left out"),
            Kind::Parked(false) => write!(f, "back inside the play space"),
            Kind::DeviceActivated => write!(f, "connected"),
            Kind::DeviceDeactivated => write!(f, "disconnected or turned off"),
            Kind::PoseValid(true) => write!(f, "has a valid position again"),
            Kind::PoseValid(false) => write!(f, "lost its position"),
            Kind::StandbyStart => write!(f, "headset went to standby"),
            Kind::StandbyEnd => write!(f, "headset woke up"),
            Kind::BaseSight {
                ch, gained, seeing, ..
            } => write!(
                f,
                "{} the base on channel {ch}, now sees {}",
                if *gained { "can see" } else { "lost sight of" },
                plural_bases(seeing.len())
            ),
            Kind::RoomCheck { code } => write!(f, "{}", room_check_text(*code)),
            Kind::UpdateAvailable { waiting: false, .. } => {
                write!(f, "no longer has a firmware update waiting")
            }
            Kind::UpdateAvailable { manual: false, .. } => {
                write!(f, "has a firmware update waiting in SteamVR")
            }
            Kind::UpdateAvailable { manual: true, .. } => {
                write!(f, "has a firmware update that has to be installed by hand")
            }
            Kind::Driver { name, status } => match status {
                DriverStatus::Loaded(_) => write!(f, "SteamVR loaded the {name} driver"),
                DriverStatus::Disabled => write!(f, "the {name} driver is turned off in SteamVR"),
                DriverStatus::Failed(e) => {
                    write!(f, "SteamVR couldn't load the {name} driver: {e}")
                }
            },
            Kind::StillJitter {
                mm,
                secs,
                bases,
                base_m,
            } => write!(
                f,
                "{}",
                crate::check_jitter::still_text(&crate::check_jitter::Still {
                    mm: *mm,
                    secs: *secs,
                    bases: *bases,
                    base_m: *base_m,
                })
            ),
            Kind::RestartFailed { base, .. } => write!(
                f,
                "couldn't start tracking again from base {base} because too few of its sensors saw that base"
            ),
            Kind::ImuMisaligned => {
                write!(
                    f,
                    "had its tracking reset because its motion and light sensors disagreed about which way it faced"
                )
            }
            Kind::PositionOutOfBounds { .. } => {
                write!(f, "SteamVR rejected its position as out of bounds")
            }
            k => write!(f, "{k:?}"),
        }
    }
}

fn plural_bases(n: usize) -> String {
    match n {
        0 => "no bases".into(),
        1 => "1 base".into(),
        n => format!("{n} bases"),
    }
}

pub fn room_check_text(code: u32) -> &'static str {
    match code {
        1 => "SteamVR's room setup check is fine again",
        101 => "SteamVR thinks a base station may have moved since room setup",
        102 => "SteamVR thinks a base station was removed since room setup",
        103 => "SteamVR says the seated position is no longer valid",
        201 => "SteamVR says its base stations aren't set up yet",
        202 => "SteamVR found two base stations it can't tell apart",
        203 => "SteamVR says the play area is invalid",
        204 => "SteamVR says the play area boundary is invalid",
        100..=199 => "SteamVR's room setup check gave a warning",
        _ => "SteamVR's room setup check gave an error",
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
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

    pub fn plain(self) -> String {
        match self {
            Self::Uninitialized => "starting".into(),
            Self::CalibratingInProgress => "calibrating".into(),
            Self::CalibratingOutOfRange => "searching for bases".into(),
            Self::RunningOk => "tracking".into(),
            Self::RunningOutOfRange => "out of range".into(),
            Self::FallbackRotationOnly => "rotation only".into(),
            Self::Other(v) => format!("state {v}"),
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
    pub fn new(
        source: Source,
        device: Option<String>,
        kind: Kind,
        detail: impl Into<String>,
    ) -> Self {
        Self::at(now_ms(), source, device, kind, detail)
    }

    pub fn at(
        t_ms: u64,
        source: Source,
        device: Option<String>,
        kind: Kind,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            t_ms,
            source,
            device,
            kind,
            detail: detail.into(),
        }
    }
}

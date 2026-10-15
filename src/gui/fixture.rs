use crate::engine::{DevLive, Engine, Msg};
use crate::event::{DeviceClass, Kind, SignalEvent, Source, TrackState};
use crate::names::Names;
use crate::report::SessionWriter;
use crate::signals::openvr::DeviceMeta;
use crate::signals::usb::UsbDongle;

pub const T0: u64 = 1_000_000;

pub fn look(pos: [f32; 3], target: [f32; 3]) -> [[f32; 4]; 3] {
    let norm = |v: [f32; 3]| {
        let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        [v[0] / l, v[1] / l, v[2] / l]
    };
    let cross = |a: [f32; 3], b: [f32; 3]| {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    };
    let f = norm([target[0] - pos[0], target[1] - pos[1], target[2] - pos[2]]);
    let z = [-f[0], -f[1], -f[2]];
    let x = norm(cross([0.0, 1.0, 0.0], z));
    let y = cross(z, x);
    [
        [x[0], y[0], z[0], pos[0]],
        [x[1], y[1], z[1], pos[1]],
        [x[2], y[2], z[2], pos[2]],
    ]
}

pub fn fake_engine() -> Engine {
    let mut e = Engine::new(
        SessionWriter::memory(std::path::PathBuf::from("session-test")),
        Names::default(),
    );
    let devs = [
        (0, "HMD-1", "Quest", DeviceClass::Hmd, "", [0.4, 1.6, 0.6]),
        (
            1,
            "LHB-9417E0B3",
            "Valve SR Imp",
            DeviceClass::TrackingReference,
            "",
            [-1.2, 2.2, -1.8],
        ),
        (
            2,
            "LHB-9D8DEA7B",
            "Valve SR Imp",
            DeviceClass::TrackingReference,
            "",
            [1.2, 2.2, 1.8],
        ),
        (
            3,
            "LHR-7E902102",
            "Knuckles Left",
            DeviceClass::Controller,
            "0C58EE58E6",
            [-0.3, 1.1, -0.2],
        ),
        (
            4,
            "LHR-905BD201",
            "VIVE Tracker 3.0 MV",
            DeviceClass::GenericTracker,
            "E1B9096D38",
            [0.1, 0.1, 0.1],
        ),
        (
            5,
            "LHR-617D30D7",
            "VIVE Tracker 3.0 MV",
            DeviceClass::GenericTracker,
            "D373DE2627",
            [0.0, 1.0, 0.0],
        ),
    ];
    for (idx, serial, model, class, dongle, pos) in devs {
        e.handle(Msg::Device(DeviceMeta {
            idx,
            serial: serial.into(),
            model: model.into(),
            class,
            dongle: dongle.into(),
            battery_pct: Some(80.0),
            is_lighthouse: class != DeviceClass::Hmd,
        }));
        e.live.insert(
            idx,
            DevLive {
                state: if idx == 4 {
                    TrackState::CalibratingOutOfRange
                } else {
                    TrackState::RunningOk
                },
                valid: true,
                connected: true,
                parked: false,
                pos,
                rot: if class == DeviceClass::TrackingReference {
                    look(pos, [0.0, 0.0, 0.0])
                } else {
                    look(pos, [pos[0], pos[1], pos[2] - 1.0])
                },
            },
        );
    }
    e.handle(Msg::Usb(vec![
        UsbDongle {
            serial: Some("0C58EE58E6".into()),
            product: "Watchman Dongle".into(),
            controller: "Renesas USB controller (PCI 0102.0002.0600.0000)".into(),
            bus: "r".into(),
            ports: vec![8],
            hubs: vec![],
        },
        UsbDongle {
            serial: Some("E1B9096D38".into()),
            product: "Watchman Dongle".into(),
            controller: "AMD USB controller (PCI 0801.0003)".into(),
            bus: "a".into(),
            ports: vec![3],
            hubs: vec![],
        },
    ]));
    let state = |t: u64, dev: &str, from: TrackState, to: TrackState| {
        Msg::Event(SignalEvent::at(
            t,
            Source::Api,
            Some(dev.into()),
            Kind::TrackingState { from, to },
            "s",
        ))
    };
    e.handle(state(
        T0,
        "LHR-7E902102",
        TrackState::Uninitialized,
        TrackState::RunningOk,
    ));
    for i in 0..5 {
        let t = T0 + 20_000 + i * 3_000;
        e.handle(state(
            t,
            "LHR-905BD201",
            TrackState::RunningOk,
            TrackState::CalibratingOutOfRange,
        ));
        e.handle(state(
            t + 80,
            "LHR-905BD201",
            TrackState::CalibratingOutOfRange,
            TrackState::RunningOk,
        ));
    }
    let t = T0 + 40_000;
    e.handle(state(
        t,
        "LHR-7E902102",
        TrackState::RunningOk,
        TrackState::CalibratingOutOfRange,
    ));
    e.handle(Msg::Event(SignalEvent::at(
        t + 5,
        Source::Api,
        Some("LHR-7E902102".into()),
        Kind::PoseValid(false),
        "lost",
    )));
    e.handle(state(
        t + 6_000,
        "LHR-7E902102",
        TrackState::CalibratingOutOfRange,
        TrackState::RunningOk,
    ));
    let sight = |t: u64, dev: &str, ch: u32, gained: bool| {
        Msg::Event(SignalEvent::at(
            t,
            Source::Log,
            Some(dev.into()),
            Kind::BaseSight {
                ch,
                gained,
                seeing: [1, 10, 15]
                    .into_iter()
                    .filter(|c| gained || *c != ch)
                    .collect(),
                ids: vec![
                    (1, "9417E0B3".into()),
                    (10, "9D8DEA7B".into()),
                    (15, "B394A63C".into()),
                ],
            },
            "sob",
        ))
    };
    let seers = ["LHR-7E902102", "LHR-905BD201", "LHR-617D30D7"];
    for dev in seers {
        for ch in [1, 10, 15] {
            e.handle(sight(T0 + 500, dev, ch, true));
        }
    }
    for i in 0..24u64 {
        let ch = match i % 8 {
            0 | 4 => 1,
            6 => 15,
            _ => 10,
        };
        for dev in seers {
            let t = T0 + 1_000 + i * 2_000;
            e.handle(sight(t, dev, ch, false));
            e.handle(sight(t + 700, dev, ch, true));
        }
    }
    let mut now = T0;
    while now < T0 + 60_000 {
        now += 100;
        e.tick(now);
    }
    e
}

pub const RECORDED_T0: u64 = 1_791_428_644_270;
pub const RECORDED_MS: u64 = 120_000;
pub const LOST_AT: u64 = 40_000;
pub const LOST_MS: u64 = 6_000;

fn body(serial: &str, s: f32) -> [f32; 3] {
    let (cx, cz) = ((s * 0.3).cos() * 1.2, (s * 0.3).sin() * 0.9);
    let swing = (s * 2.2).sin();
    match serial {
        "HMD-1" => [cx, 1.65 + (s * 3.1).sin() * 0.02, cz],
        "LHR-7E902102" => [cx - 0.25, 1.05 + swing * 0.12, cz + swing * 0.2],
        "LHR-905BD201" => [cx + 0.12, 0.12 + swing.max(0.0) * 0.1, cz - swing * 0.25],
        _ => [cx, 0.95, cz],
    }
}

pub fn record_session(root: &std::path::Path) -> std::path::PathBuf {
    use crate::signals::openvr::FrameSample;
    let dir = root.join(format!("session-{RECORDED_T0}"));
    let session = SessionWriter::in_dir(dir.clone(), false).unwrap();
    let mut e = Engine::new(session, Names::default());
    let devs = [
        (0, "HMD-1", "Quest", DeviceClass::Hmd, ""),
        (
            1,
            "LHB-9417E0B3",
            "Valve SR Imp",
            DeviceClass::TrackingReference,
            "",
        ),
        (
            2,
            "LHB-9D8DEA7B",
            "Valve SR Imp",
            DeviceClass::TrackingReference,
            "",
        ),
        (
            3,
            "LHR-7E902102",
            "Knuckles Left",
            DeviceClass::Controller,
            "0C58EE58E6",
        ),
        (
            4,
            "LHR-905BD201",
            "VIVE Tracker 3.0 MV",
            DeviceClass::GenericTracker,
            "E1B9096D38",
        ),
        (
            5,
            "LHR-617D30D7",
            "VIVE Tracker 3.0 MV",
            DeviceClass::GenericTracker,
            "D373DE2627",
        ),
    ];
    let bases = [[-2.0, 2.3, -1.9], [2.1, 2.2, 1.8]];
    for (idx, serial, model, class, dongle) in devs {
        e.handle(Msg::Device(DeviceMeta {
            idx,
            serial: serial.into(),
            model: model.into(),
            class,
            dongle: dongle.into(),
            battery_pct: Some(80.0),
            is_lighthouse: class != DeviceClass::Hmd,
        }));
    }
    let mut state = vec![(TrackState::Uninitialized, false); devs.len()];
    let mut t = RECORDED_T0;
    let mut tick = 0u32;
    while t < RECORDED_T0 + RECORDED_MS {
        let rel = t - RECORDED_T0;
        let s = rel as f32 / 1000.0;
        for (idx, serial, _, class, _) in devs {
            let base = class == DeviceClass::TrackingReference;
            if base && !tick.is_multiple_of(90) {
                continue;
            }
            let lost = serial == "LHR-905BD201" && (LOST_AT..LOST_AT + LOST_MS).contains(&rel);
            let (now, valid) = if lost {
                (TrackState::CalibratingOutOfRange, rel < LOST_AT + 300)
            } else {
                (TrackState::RunningOk, true)
            };
            let (was, was_valid) = state[idx];
            if was != now && lost {
                e.handle(Msg::Event(SignalEvent::at(
                    t + 2,
                    Source::Log,
                    Some(serial.into()),
                    Kind::OpticalLoss {
                        base: None,
                        outage_ms: LOST_MS,
                    },
                    "Tue Oct 13 2026 23:45:24.280 [Info] - lighthouse: LHR-905BD201 H: Optical tracking lost, no sensors visible",
                )));
            }
            if was != now {
                e.handle(Msg::Event(SignalEvent::at(
                    t,
                    Source::Api,
                    Some(serial.into()),
                    Kind::TrackingState { from: was, to: now },
                    "",
                )));
            }
            if was_valid != valid {
                e.handle(Msg::Event(SignalEvent::at(
                    t,
                    Source::Api,
                    Some(serial.into()),
                    Kind::PoseValid(valid),
                    "",
                )));
            }
            state[idx] = (now, valid);
            let pos = if base {
                bases[idx - 1]
            } else if lost {
                let p = body(serial, (LOST_AT as f32) / 1000.0);
                let drift = (rel - LOST_AT) as f32 / 1000.0;
                [p[0] + drift * 0.05, p[1], p[2] + drift * 0.03]
            } else {
                body(serial, s)
            };
            let rot = if base {
                look(pos, [0.0, 0.0, 0.0])
            } else {
                let ahead = body(serial, s + 0.5);
                look(pos, [ahead[0], pos[1] - 0.2, ahead[2]])
            };
            e.handle(Msg::Frame(FrameSample {
                idx,
                t_ms: t,
                pos,
                vel: [0.0; 3],
                rot,
                ang_speed: 0.5,
                state: now,
                valid,
            }));
        }
        if tick.is_multiple_of(9) {
            e.tick(t);
        }
        tick += 1;
        t += 11;
    }
    e.tick(t + 3_000);
    e.finish().unwrap();
    e.session.close();
    crate::store::pack(&dir).unwrap();
    dir
}

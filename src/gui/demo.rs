use super::fixture::look;
use super::shots::{Frame, render, setup};
use super::*;
use crate::event::{DeviceClass, Kind, SignalEvent, Source, TrackState};
use crate::signals::openvr::{DeviceMeta, FrameSample};
use std::io::Write;

const FPS: u64 = 20;
const STEP_MS: u64 = 1000 / FPS;
const T0: u64 = 1_792_029_600_000;
const LENGTH_MS: u64 = 90_000;
const FOOT_LOST_AT: u64 = 34_000;
const FOOT_LOST_MS: u64 = 3_500;
const HAND_FLICKER_AT: u64 = 61_000;
const LIVE_FROM: u64 = 24_000;
const LIVE_TO: u64 = 42_000;

const AIM: [f32; 3] = [0.0, 1.0, 0.0];

const BASES: [(&str, [f32; 3]); 4] = [
    ("LHB-0B0C0D01", [-2.1, 2.3, -1.9]),
    ("LHB-0B0C0D02", [2.0, 2.2, -2.0]),
    ("LHB-0B0C0D03", [2.1, 2.3, 1.9]),
    ("LHB-0B0C0D04", [-1.9, 2.2, 2.1]),
];

const HEADSET: &str = "2G0YC1ZF8B0001";
const LEFT_HAND: &str = "LHR-1A2B3C01";
const RIGHT_HAND: &str = "LHR-1A2B3C02";
const WAIST: &str = "LHR-1A2B3C03";
const LEFT_FOOT: &str = "LHR-1A2B3C04";
const RIGHT_FOOT: &str = "LHR-1A2B3C05";
const CHEST: &str = "LHR-1A2B3C06";

const DEVICES: [(&str, &str, DeviceClass, &str); 7] = [
    (HEADSET, "Meta Quest 3", DeviceClass::Hmd, ""),
    (
        LEFT_HAND,
        "Knuckles Left",
        DeviceClass::Controller,
        "0A1B2C3D01",
    ),
    (
        RIGHT_HAND,
        "Knuckles Right",
        DeviceClass::Controller,
        "0A1B2C3D02",
    ),
    (
        WAIST,
        "VIVE Tracker 3.0 MV",
        DeviceClass::GenericTracker,
        "0A1B2C3D03",
    ),
    (
        LEFT_FOOT,
        "VIVE Tracker 3.0 MV",
        DeviceClass::GenericTracker,
        "0A1B2C3D04",
    ),
    (
        RIGHT_FOOT,
        "VIVE Tracker 3.0 MV",
        DeviceClass::GenericTracker,
        "0A1B2C3D05",
    ),
    (
        CHEST,
        "VIVE Tracker 3.0 MV",
        DeviceClass::GenericTracker,
        "0A1B2C3D06",
    ),
];

fn add(a: [f32; 3], b: [f32; 3], k: f32) -> [f32; 3] {
    [a[0] + b[0] * k, a[1] + b[1] * k, a[2] + b[2] * k]
}

fn body(serial: &str, ms: u64) -> ([f32; 3], [f32; 3]) {
    let s = ms as f32 / 1000.0;
    let a = s * 0.22;
    let c = [a.cos() * 1.0, 0.0, a.sin() * 0.8];
    let f = {
        let d = [-a.sin() * 1.0, 0.0, a.cos() * 0.8];
        let l = (d[0] * d[0] + d[2] * d[2]).sqrt();
        [d[0] / l, 0.0, d[2] / l]
    };
    let r = [-f[2], 0.0, f[0]];
    let up = [0.0, 1.0, 0.0];
    let step = (s * std::f32::consts::TAU * 0.9).sin();
    let pos = match serial {
        HEADSET => add(add(c, up, 1.62 + step.abs() * 0.015), f, 0.05),
        CHEST => add(add(c, up, 1.32), f, 0.1),
        WAIST => add(add(c, up, 1.0), f, 0.08),
        LEFT_FOOT => add(
            add(add(c, r, -0.12), f, 0.18 * step),
            up,
            0.08 + 0.07 * step.max(0.0),
        ),
        RIGHT_FOOT => add(
            add(add(c, r, 0.12), f, -0.18 * step),
            up,
            0.08 + 0.07 * (-step).max(0.0),
        ),
        LEFT_HAND => add(add(add(c, r, -0.27), f, 0.22 - 0.12 * step), up, 1.08),
        _ => add(add(add(c, r, 0.27), f, 0.22 + 0.12 * step), up, 1.08),
    };
    let target = add(
        add(pos, f, 1.0),
        up,
        if serial == HEADSET { -0.15 } else { -0.3 },
    );
    (pos, target)
}

fn state_at(serial: &str, rel: u64) -> (TrackState, bool) {
    let foot = serial == RIGHT_FOOT && (FOOT_LOST_AT..FOOT_LOST_AT + FOOT_LOST_MS).contains(&rel);
    let hand = serial == RIGHT_HAND && (HAND_FLICKER_AT..HAND_FLICKER_AT + 300).contains(&rel);
    match (foot, hand) {
        (true, _) => (TrackState::CalibratingOutOfRange, rel < FOOT_LOST_AT + 300),
        (_, true) => (TrackState::CalibratingOutOfRange, true),
        _ => (TrackState::RunningOk, true),
    }
}

struct Video {
    ffmpeg: std::process::Child,
    frames: u64,
}

impl Video {
    fn start(path: &std::path::Path) -> Video {
        let exe = std::env::var("TD_FFMPEG").expect("set TD_FFMPEG to an ffmpeg.exe");
        let ffmpeg = std::process::Command::new(exe)
            .args([
                "-y",
                "-loglevel",
                "error",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "rgb24",
                "-s",
                "1040x700",
                "-r",
            ])
            .arg(FPS.to_string())
            .args([
                "-i",
                "-",
                "-c:v",
                "libx264",
                "-preset",
                "slow",
                "-crf",
                "20",
                "-pix_fmt",
                "yuv420p",
                "-movflags",
                "+faststart",
            ])
            .arg(path)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        Video { ffmpeg, frames: 0 }
    }

    fn push(&mut self, frame: &Frame, times: u64) {
        let bytes: Vec<u8> = frame
            .pixels
            .iter()
            .flat_map(|p| [p.red, p.green, p.blue])
            .collect();
        let stdin = self.ffmpeg.stdin.as_mut().unwrap();
        for _ in 0..times {
            stdin.write_all(&bytes).unwrap();
            self.frames += 1;
        }
    }

    fn hold(&mut self, secs: f32) {
        let frame = render();
        self.push(&frame, (secs * FPS as f32) as u64);
    }

    fn finish(mut self) -> u64 {
        drop(self.ffmpeg.stdin.take());
        assert!(self.ffmpeg.wait().unwrap().success());
        self.frames
    }
}

fn live_updates(e: &mut Engine, now: u64, feed: &mut Vec<ui::Line>) -> Vec<Update> {
    let verdicts = e.tick(now);
    e.snapshot(now);
    for v in &verdicts {
        let mut lines = present::verdict_lines(v, &e.label(&v.device), T0);
        lines.append(feed);
        *feed = lines;
    }
    let summary = e.summary();
    vec![
        Update::Devices(present::devices(e)),
        Update::Report(present::report(e, &summary, false)),
        Update::Room(present::live_room(e, &summary)),
        Update::Feed(feed.clone()),
        Update::Status {
            state: "Recording".into(),
            tone: present::GOOD,
            detail: format!(
                "{} so far, {}, {:.0} KB saved",
                crate::summary::fmt_dur(now - T0),
                crate::summary::plural(e.session.verdicts().len(), "incident"),
                e.session.bytes_written() as f64 / 1024.0
            ),
        },
    ]
}

#[test]
#[ignore]
fn demo_video() {
    let root = std::env::temp_dir().join(format!("trackdoctor-demo-{}", std::process::id()));
    let dir = root.join(format!("session-{T0}"));
    let out =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/shots/trackdoctor-demo.mp4");
    std::fs::create_dir_all(out.parent().unwrap()).unwrap();
    let app = setup();
    let mut video = Video::start(&out);

    let session = SessionWriter::in_dir(dir.clone(), false).unwrap();
    let mut e = Engine::new(session, Names::default());
    let mut idx = 0;
    for (serial, _) in BASES {
        e.handle(Msg::Device(DeviceMeta {
            idx,
            serial: serial.into(),
            model: "Valve SR Imp".into(),
            class: DeviceClass::TrackingReference,
            dongle: String::new(),
            battery_pct: None,
            is_lighthouse: true,
        }));
        idx += 1;
    }
    for (serial, model, class, dongle) in DEVICES {
        e.handle(Msg::Device(DeviceMeta {
            idx,
            serial: serial.into(),
            model: model.into(),
            class,
            dongle: dongle.into(),
            battery_pct: Some(if class == DeviceClass::Hmd {
                74.0
            } else {
                88.0
            }),
            is_lighthouse: class != DeviceClass::Hmd,
        }));
        idx += 1;
    }
    with(|w, s| {
        s.recording = Some(dir.clone());
        apply(
            w,
            s,
            vec![Update::Feed(vec![present::line(
                "SteamVR is running. Recording a new session.",
                present::GOOD,
            )])],
        );
    });
    let mut feed = vec![present::line(
        "SteamVR is running. Recording a new session.",
        present::GOOD,
    )];
    let mut state = vec![(TrackState::Uninitialized, false); DEVICES.len()];
    let mut frozen = [0.0f32; 3];
    let mut last_frame: Option<Frame> = None;
    let mut next_tick = T0;
    let mut next_frame = LIVE_FROM;
    let mut t = T0;
    let mut n = 0u64;
    while t < T0 + LENGTH_MS {
        let rel = t - T0;
        if n.is_multiple_of(90) {
            for (i, (_, p)) in BASES.iter().enumerate() {
                e.handle(Msg::Frame(FrameSample {
                    idx: i,
                    t_ms: t,
                    pos: *p,
                    vel: [0.0; 3],
                    rot: look(*p, AIM),
                    ang_speed: 0.0,
                    state: TrackState::RunningOk,
                    valid: true,
                }));
            }
        }
        for (k, (serial, _, _, _)) in DEVICES.iter().enumerate() {
            let (now, valid) = state_at(serial, rel);
            let (was, was_valid) = state[k];
            if was != now && *serial == RIGHT_FOOT && now != TrackState::RunningOk {
                e.handle(Msg::Event(SignalEvent::at(
                    t + 2,
                    Source::Log,
                    Some(serial.to_string()),
                    Kind::OpticalLoss { base: None, outage_ms: FOOT_LOST_MS },
                    "Wed Oct 14 2026 21:00:34.002 [Info] - lighthouse: LHR-1A2B3C05 H: Optical tracking lost, no sensors visible",
                )));
                frozen = body(serial, rel).0;
            }
            if was != now {
                e.handle(Msg::Event(SignalEvent::at(
                    t,
                    Source::Api,
                    Some(serial.to_string()),
                    Kind::TrackingState { from: was, to: now },
                    "",
                )));
            }
            if was_valid != valid {
                e.handle(Msg::Event(SignalEvent::at(
                    t,
                    Source::Api,
                    Some(serial.to_string()),
                    Kind::PoseValid(valid),
                    "",
                )));
            }
            state[k] = (now, valid);
            let (mut pos, target) = body(serial, rel);
            if *serial == RIGHT_FOOT && now != TrackState::RunningOk {
                let drift = (rel - FOOT_LOST_AT) as f32 / 1000.0;
                pos = [
                    frozen[0] + drift * 0.06,
                    frozen[1] - drift * 0.01,
                    frozen[2] + drift * 0.04,
                ];
            }
            e.handle(Msg::Frame(FrameSample {
                idx: BASES.len() + k,
                t_ms: t,
                pos,
                vel: [0.0; 3],
                rot: look(pos, target),
                ang_speed: 0.5,
                state: now,
                valid,
            }));
        }
        if t >= next_tick {
            next_tick += 250;
            let updates = live_updates(&mut e, t, &mut feed);
            with(|w, s| apply(w, s, updates)).unwrap();
            if (LIVE_FROM..LIVE_TO).contains(&rel) {
                let page = if rel < LIVE_FROM + 3_000 { 0 } else { 3 };
                app.global::<Navigation>().set_page(page);
                last_frame = Some(render());
            }
        }
        while next_frame <= rel && next_frame < LIVE_TO {
            next_frame += STEP_MS;
            if let Some(f) = &last_frame {
                video.push(f, 1);
            }
        }
        n += 1;
        t += 11;
    }
    e.tick(t + 3_000);
    e.finish().unwrap();
    e.session.close();
    crate::store::pack(&dir).unwrap();
    println!("live part: {} frames", video.frames);

    let entries = sessions::list_in(&root, None);
    for en in &entries {
        println!("history: {} {}", en.title, en.detail);
    }
    with(|w, s| {
        s.recording = None;
        back_to_live(w, s);
        s.sessions = entries.clone();
        let g = w.global::<Sessions>();
        g.set_items(model(sessions::items(&s.sessions)));
        g.set_totals(sessions::totals(&s.sessions).into());
        apply(
            w,
            s,
            vec![Update::Status {
                state: "SteamVR closed".into(),
                tone: present::DIM,
                detail: "The session is saved. A new one starts when SteamVR runs again.".into(),
            }],
        );
    });
    app.global::<Navigation>().set_page(5);
    video.hold(3.0);

    let i = entries.iter().position(|en| en.dir == dir).unwrap();
    let shown = sessions::past(&dir, Names::default()).unwrap();
    with(|w, s| opened(w, s, &entries[i], ROOM_PAGE, shown)).unwrap();
    with(|w, s| {
        s.play.speed = 4;
        w.global::<Room>().set_speed(1);
        s.play.playing = true;
        w.global::<Room>().set_playing(true);
    });
    let play = |video: &mut Video, from: u32, to: u32| {
        let mut t = from;
        while t <= to {
            with(|w, s| {
                s.play.t = t;
                show_moment(w, s);
            });
            video.push(&render(), 1);
            t += (STEP_MS * 4) as u32;
        }
    };
    play(&mut video, 0, 14_000);
    let jump = with(|_, s| {
        let r = s.past.as_ref().unwrap().1.replay.clone().unwrap();
        r.seek(FOOT_LOST_AT as f32 / r.length as f32 + 0.01, 0.05)
    })
    .unwrap();
    println!("seek snapped to {jump} ms");
    play(&mut video, jump, jump + 12_000);
    with(|w, s| {
        s.play.playing = false;
        w.global::<Room>().set_playing(false);
        s.play.t = FOOT_LOST_AT as u32 + 1_500;
        show_moment(w, s);
    });
    video.hold(2.5);

    app.global::<Navigation>().set_page(0);
    app.global::<Live>().set_mode(1);
    video.hold(3.0);
    app.global::<Navigation>().set_page(1);
    video.hold(3.0);

    let frames = video.finish();
    println!(
        "{frames} frames, {:.1} s -> {}",
        frames as f32 / FPS as f32,
        out.display()
    );
    for en in entries {
        crate::store::delete(&en.dir).unwrap();
    }
    std::fs::remove_dir(&root).unwrap();
}

#[test]
#[ignore]
fn live_cost() {
    let root = std::env::temp_dir().join(format!("trackdoctor-cost-{}", std::process::id()));
    let app = setup();
    let secs = 60u64;
    for (name, tick_ms, page) in [
        ("room-166", 166u64, 3),
        ("live-250", 250, 0),
        ("live-1000", 1000, 0),
        ("summary-1000", 1000, 1),
        ("history-1000", 1000, 5),
    ] {
        let dir = root.join(name);
        let d2 = dir.clone();
        let factory: recorder::Factory = Box::new(move |_| {
            (
                Engine::new(
                    SessionWriter::in_dir(d2.clone(), false).unwrap(),
                    Names::default(),
                ),
                None,
            )
        });
        let mut rec = Recorder::new(factory, Arc::new(AtomicBool::new(false)));
        app.global::<Navigation>().set_page(page);
        with(|w, s| {
            s.room_shown = rec.room_shown();
            s.recording = Some(dir.clone());
            back_to_live(w, s);
        });
        let mut idx = 0;
        for (serial, _) in BASES {
            rec.handle(
                Msg::Device(DeviceMeta {
                    idx,
                    serial: serial.into(),
                    model: "Valve SR Imp".into(),
                    class: DeviceClass::TrackingReference,
                    dongle: String::new(),
                    battery_pct: None,
                    is_lighthouse: true,
                }),
                T0,
            );
            idx += 1;
        }
        for (serial, model, class, dongle) in DEVICES {
            rec.handle(
                Msg::Device(DeviceMeta {
                    idx,
                    serial: serial.into(),
                    model: model.into(),
                    class,
                    dongle: dongle.into(),
                    battery_pct: Some(80.0),
                    is_lighthouse: class != DeviceClass::Hmd,
                }),
                T0,
            );
            idx += 1;
        }
        let (mut h, mut k, mut a, mut r) = (
            Duration::ZERO,
            Duration::ZERO,
            Duration::ZERO,
            Duration::ZERO,
        );
        let (mut ticks, mut frames) = (0u64, 0u64);
        let mut next_tick = T0;
        let mut t = T0;
        let mut n = 0u64;
        while t < T0 + secs * 1000 {
            let rel = t - T0;
            let mut msgs = Vec::new();
            if n.is_multiple_of(90) {
                for (i, (_, p)) in BASES.iter().enumerate() {
                    msgs.push(Msg::Frame(FrameSample {
                        idx: i,
                        t_ms: t,
                        pos: *p,
                        vel: [0.0; 3],
                        rot: look(*p, AIM),
                        ang_speed: 0.0,
                        state: TrackState::RunningOk,
                        valid: true,
                    }));
                }
            }
            for (k, (serial, _, _, _)) in DEVICES.iter().enumerate() {
                let (pos, target) = body(serial, rel);
                msgs.push(Msg::Frame(FrameSample {
                    idx: BASES.len() + k,
                    t_ms: t,
                    pos,
                    vel: [0.0; 3],
                    rot: look(pos, target),
                    ang_speed: 0.5,
                    state: TrackState::RunningOk,
                    valid: true,
                }));
            }
            let s0 = Instant::now();
            for m in msgs {
                rec.handle(m, t);
            }
            h += s0.elapsed();
            if t >= next_tick {
                next_tick += tick_ms;
                ticks += 1;
                let s1 = Instant::now();
                let updates: Vec<Update> = rec
                    .tick(t)
                    .into_iter()
                    .filter(|u| !matches!(u, Update::Recording(_) | Update::SessionEnded))
                    .collect();
                k += s1.elapsed();
                let s2 = Instant::now();
                with(|w, s| apply(w, s, updates)).unwrap();
                a += s2.elapsed();
                let s3 = Instant::now();
                let _ = render();
                r += s3.elapsed();
                frames += 1;
            }
            n += 1;
            t += 11;
        }
        let per = |d: Duration| d.as_secs_f64() * 1000.0 / secs as f64;
        println!(
            "{name}: ticks={ticks} frames={frames} | ms per second of play: frames-in {:.2}, tick {:.2}, apply {:.2}, repaint {:.2} | total {:.2} ms/s = {:.2}% of a core",
            per(h),
            per(k),
            per(a),
            per(r) / 3.0,
            per(h + k + a) + per(r) / 3.0,
            (per(h + k + a) + per(r) / 3.0) / 10.0
        );
        rec.finish(t);
    }
    with(|_, s| s.recording = None);
    let _ = std::fs::remove_dir_all(&root);
}

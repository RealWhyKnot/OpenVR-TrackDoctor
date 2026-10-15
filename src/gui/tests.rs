use super::fixture::{T0, fake_engine};
use super::present;
use super::recorder::{Recorder, Update};
use crate::engine::{Engine, Msg};
use crate::event::{DeviceClass, Kind, SignalEvent, Source};
use crate::names::Names;
use crate::report::SessionWriter;
use crate::signals::openvr::DeviceMeta;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

fn joined(lines: &[super::ui::Line]) -> String {
    lines
        .iter()
        .map(|l| l.text.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn presenter_shows_what_the_terminal_view_showed() {
    let e = fake_engine();
    let devices = present::devices(&e);
    let row = |serial: &str| devices.iter().find(|d| d.serial == serial).unwrap().clone();
    assert_eq!(row("LHR-7E902102").name, "left controller");
    assert_eq!(row("LHR-905BD201").state, "searching for bases");
    assert_eq!(row("LHR-905BD201").tone, present::BAD);
    assert_eq!(row("LHB-9417E0B3").state, "on");
    assert_eq!(row("LHR-7E902102").incidents, "1");
    assert!(devices.last().unwrap().serial.starts_with("LHB"));

    let feed: Vec<_> = e
        .session
        .verdicts()
        .iter()
        .flat_map(|v| present::verdict_lines(v, &e.label(&v.device), T0))
        .collect();
    let feed = joined(&feed);
    assert!(feed.contains("dropout, cause unclear"), "{feed}");
    assert!(feed.contains("Lost for 6.0s, no position"), "{feed}");
    assert!(!feed.contains("(unsure)"), "{feed}");

    let s = e.summary();
    let r = present::report(&e, &s, false);
    assert_eq!(r.ranks[0].serial, "LHR-7E902102");
    assert_eq!(r.ranks[0].rank, "1");
    assert_eq!(r.ranks[0].tone, present::BAD);
    assert!(r.ranks[0].share > 0.0 && r.ranks[0].share <= 1.0);
    assert!(r.heading.starts_with("So far:"));
    assert!(r.findings.iter().any(|c| !c.steps.is_empty()));
    assert!(
        joined(&r.bases).contains(" 9417E0B3"),
        "{}",
        joined(&r.bases)
    );

    let usb = joined(&present::usb(&e, &s).tree);
    assert!(
        usb.contains("port 8  dongle 0C58EE58E6 -> LHR-7E902102 left controller  #1"),
        "{usb}"
    );

    let room = present::live_room(&e, &s);
    assert_eq!(room.scene.bases.len(), 2);
    assert_eq!(room.scene.devices.len(), 4);
    let key = |serial: &str| {
        room.legend
            .iter()
            .find(|k| k.serial == serial)
            .unwrap()
            .clone()
    };
    assert_eq!(key("LHR-905BD201").mark, "2?");
    assert!(key("LHR-905BD201").detail.starts_with("not tracking now"));
    assert_eq!(key("HMD-1").mark, "H");
    assert_eq!(key("LHB-9417E0B3").mark, "B1");
    assert_eq!(room.legend[0].serial, "LHR-7E902102");
    let tracker = room
        .scene
        .devices
        .iter()
        .find(|d| d.label.contains("LHR-905BD201") || d.label.contains("tracker"))
        .unwrap();
    assert!(
        tracker.label.starts_with("#2 ") && tracker.label.ends_with(" ?"),
        "{}",
        tracker.label
    );
    assert!(!tracker.tracking);
}

fn memory_recorder() -> Recorder {
    Recorder::new(
        Box::new(|_| {
            (
                Engine::new(
                    SessionWriter::memory(std::path::PathBuf::from("session-test")),
                    Names::default(),
                ),
                None,
            )
        }),
        Arc::new(AtomicBool::new(false)),
    )
}

fn log(t: u64, kind: Kind) -> Msg {
    Msg::Event(SignalEvent::at(t, Source::Log, None, kind, "log"))
}

fn device(idx: usize, serial: &str, class: DeviceClass) -> Msg {
    Msg::Device(DeviceMeta {
        idx,
        serial: serial.into(),
        model: "VIVE Tracker 3.0 MV".into(),
        class,
        dongle: String::new(),
        battery_pct: None,
        is_lighthouse: true,
    })
}

#[test]
fn recorder_waits_for_steamvr_then_records_and_closes_the_session() {
    let mut r = memory_recorder();
    let out = r.tick(T0);
    assert!(
        out.iter()
            .any(|u| matches!(u, Update::Status { state, .. } if state == "Waiting for SteamVR"))
    );
    r.handle(log(T0 + 1, Kind::LogRotated), T0 + 1);
    r.handle(log(T0 + 2, Kind::LogRotated), T0 + 2);
    assert!(!r.recording());
    let out = r.handle(device(1, "LHR-1", DeviceClass::GenericTracker), T0 + 10);
    assert!(r.recording());
    assert!(out.iter().any(|u| matches!(u, Update::Recording(Some(_)))));
    let out = r.tick(T0 + 300);
    let feed = out
        .iter()
        .find_map(|u| match u {
            Update::Feed(f) => Some(joined(f)),
            _ => None,
        })
        .unwrap();
    assert!(feed.starts_with("SteamVR is running"), "{feed}");
    assert!(
        out.iter()
            .any(|u| matches!(u, Update::Status { state, .. } if state == "Recording"))
    );
    assert!(
        out.iter()
            .any(|u| matches!(u, Update::Devices(d) if d.len() == 1))
    );

    let out = r.handle(Msg::SteamVrExited, T0 + 5_000);
    assert!(!r.recording());
    assert!(out.iter().any(|u| matches!(u, Update::SessionEnded)));
    assert!(
        out.iter()
            .any(|u| matches!(u, Update::Report(rep) if rep.heading.starts_with("Session length")))
    );
    let feed = out
        .iter()
        .find_map(|u| match u {
            Update::Feed(f) => Some(joined(f)),
            _ => None,
        })
        .unwrap();
    assert!(feed.starts_with("SteamVR closed"), "{feed}");
    assert!(
        out.iter()
            .any(|u| matches!(u, Update::Status { state, .. } if state == "SteamVR closed"))
    );

    r.handle(device(1, "LHR-1", DeviceClass::GenericTracker), T0 + 9_000);
    assert!(r.recording());
}

#[test]
fn recorder_builds_the_room_only_while_it_is_showing() {
    let mut r = memory_recorder();
    let shown = r.room_shown();
    r.handle(device(1, "LHR-1", DeviceClass::GenericTracker), T0);
    let room = |out: &[Update]| out.iter().any(|u| matches!(u, Update::Room(_)));
    assert!(!room(&r.tick(T0 + 100)));
    shown.store(true, Ordering::Relaxed);
    assert!(room(&r.tick(T0 + 200)));
    shown.store(false, Ordering::Relaxed);
    assert!(!room(&r.tick(T0 + 300)));
    let out = r.handle(Msg::SteamVrExited, T0 + 400);
    assert!(room(&out));
}

#[test]
fn recorder_replays_the_current_steamvr_log_into_a_new_session() {
    let seen = Arc::new(Mutex::new(0usize));
    let counter = seen.clone();
    let mut r = Recorder::new(
        Box::new(move |_| {
            *counter.lock().unwrap() += 1;
            (
                Engine::new(
                    SessionWriter::memory(std::path::PathBuf::from("session-test")),
                    Names::default(),
                ),
                None,
            )
        }),
        Arc::new(AtomicBool::new(false)),
    );
    r.handle(log(T0, Kind::LogRotated), T0);
    r.handle(Msg::Usb(Vec::new()), T0);
    r.handle(device(0, "HMD-1", DeviceClass::Hmd), T0 + 1);
    r.handle(device(1, "LHR-1", DeviceClass::GenericTracker), T0 + 2);
    assert_eq!(*seen.lock().unwrap(), 1);
    r.finish(T0 + 100);
    r.handle(device(1, "LHR-1", DeviceClass::GenericTracker), T0 + 200);
    assert_eq!(*seen.lock().unwrap(), 2);
}

#[test]
fn coverage_heights_match_the_menu() {
    assert_eq!(super::coverage_height(0), None);
    assert_eq!(super::coverage_height(1), Some(0.0));
    assert_eq!(super::coverage_height(2), Some(1.0));
    assert_eq!(super::coverage_height(3), Some(1.6));
}

#[test]
fn session_titles_use_the_start_time() {
    let dir = std::path::Path::new("session-1790989030647");
    assert_eq!(super::sessions::start_ms(dir), Some(1_790_989_030_647));
    let t = super::sessions::title(dir);
    assert_eq!(t.len(), 16, "{t}");
    assert!(t.starts_with("2026-10-"), "{t}");
    assert_eq!(super::sessions::title(std::path::Path::new("odd")), "odd");
}

#[test]
fn dev_packets_land_in_the_session_and_are_packed_and_deleted_with_it() {
    use crate::signals::packets::{Batch, Gap, Line, Second, Totals};
    let dir = std::env::temp_dir().join(format!("trackdoctor-packets-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let session_dir = dir.clone();
    let mut r = Recorder::new(
        Box::new(move |poses| {
            (
                Engine::new(
                    SessionWriter::in_dir(session_dir.clone(), poses).unwrap(),
                    Names::default(),
                ),
                None,
            )
        }),
        Arc::new(AtomicBool::new(false)),
    );
    r.handle(
        Msg::Device(DeviceMeta {
            idx: 1,
            serial: "LHR-1".into(),
            model: "VIVE Tracker 3.0 MV".into(),
            class: DeviceClass::GenericTracker,
            dongle: "D1".into(),
            battery_pct: None,
            is_lighthouse: true,
        }),
        T0,
    );
    r.handle(log(T0 + 1, Kind::LogRotated), T0 + 1);
    let second = |t_ms: u64, packets: u32| {
        Line::Second(Second {
            t_ms,
            dongle: "D1".into(),
            reports: packets,
            packets,
            ..Second::default()
        })
    };
    let first = Batch {
        lines: vec![
            Line::Note {
                t_ms: T0 + 2,
                dongle: "D1".into(),
                text: "listening (read-only)".into(),
            },
            Line::Gap(Gap {
                t_ms: T0 + 900,
                dongle: "D1".into(),
                host_ms: 45,
                air_ms: Some(44),
                before: Some(0x80),
                after: Some(0x90),
            }),
            second(T0 + 1_000, 900),
        ],
    };
    r.handle(Msg::Packets(first), T0 + 1_000);
    r.handle(
        Msg::Packets(Batch {
            lines: vec![second(T0 + 2_000, 950)],
        }),
        T0 + 2_000,
    );
    let feed = r
        .tick(T0 + 2_100)
        .into_iter()
        .find_map(|u| match u {
            Update::Feed(f) => Some(joined(&f)),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        feed.matches("Dev build: recording radio packets too. 1 dongle sending so far.")
            .count(),
        1,
        "{feed}"
    );

    r.handle(Msg::SteamVrExited, T0 + 5_000);
    assert!(!dir.join("packets.jsonl").exists());
    let lines = crate::signals::packets::parse_lines(
        &crate::store::read_text(&dir, "packets.jsonl").unwrap(),
    );
    assert_eq!(lines.len(), 4);
    let totals: Vec<Totals> = crate::report::read_json(&dir.join("packets.json")).unwrap();
    assert_eq!(totals.len(), 1);
    assert_eq!(totals[0].device.as_deref(), Some("LHR-1"));
    assert_eq!((totals[0].packets, totals[0].seconds), (1_850, 2));
    assert_eq!(totals[0].worst.len(), 1);

    crate::store::delete(&dir).unwrap();
    assert!(!dir.exists());
}

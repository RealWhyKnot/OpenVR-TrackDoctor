use super::fixture::{T0, fake_engine};
use super::recorder::Update;
use super::*;
use i_slint_backend_testing::ElementHandle;
use slint::platform::software_renderer::{
    MinimalSoftwareWindow, PremultipliedRgbaColor as Pixel, RepaintBufferType,
};
use slint::platform::{Platform, WindowAdapter};
use slint::{PhysicalSize, PlatformError};
use std::rc::Rc;

const WIDTH: u32 = 1040;
const HEIGHT: u32 = 700;

thread_local! {
    static WINDOW: Rc<MinimalSoftwareWindow> = {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        let _ = slint::platform::set_platform(Box::new(TestPlatform { window: window.clone() }));
        window
    };
}

struct TestPlatform {
    window: Rc<MinimalSoftwareWindow>,
}

impl Platform for TestPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.window.clone())
    }
}

pub struct Frame {
    pub pixels: Vec<Pixel>,
}

impl Frame {
    pub fn at(&self, x: u32, y: u32) -> [u8; 3] {
        let p = self.pixels[(y * WIDTH + x) as usize];
        [p.red, p.green, p.blue]
    }

    pub fn save(&self, name: &str) -> std::path::PathBuf {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/shots");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{name}.png"));
        let file = std::fs::File::create(&path).unwrap();
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), WIDTH, HEIGHT);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        let bytes: Vec<u8> = self
            .pixels
            .iter()
            .flat_map(|p| [p.red, p.green, p.blue])
            .collect();
        writer.write_image_data(&bytes).unwrap();
        path
    }
}

pub(super) fn render() -> Frame {
    let window = WINDOW.with(Rc::clone);
    let mut pixels = vec![Pixel::default(); (WIDTH * HEIGHT) as usize];
    for _ in 0..3 {
        slint::platform::update_timers_and_animations();
        window.request_redraw();
        window.draw_if_needed(|renderer| {
            renderer.render(&mut pixels, WIDTH as usize);
        });
    }
    Frame { pixels }
}

pub(super) fn setup() -> AppWindow {
    let window = WINDOW.with(Rc::clone);
    let app = AppWindow::new().unwrap();
    window.set_size(PhysicalSize::new(WIDTH, HEIGHT));
    app.show().unwrap();
    SHELL.with(|cell| {
        *cell.borrow_mut() = Some(Shell {
            window: app.as_weak(),
            live: Shown::default(),
            past: None,
            room: RoomState::default(),
            recording: None,
            sessions: Vec::new(),
            release: None,
            installed: None,
            config: Config::default(),
            poses: Arc::new(AtomicBool::new(false)),
            room_shown: Arc::new(AtomicBool::new(false)),
            steamvr_synced: true,
            play: Play {
                speed: 1,
                ..Play::default()
            },
            log_page: 0,
            started_ms: 0,
            asking: None,
            listing: 0,
        })
    });
    app.global::<Status>().set_version("2026.10.15.0".into());
    let settings = app.global::<Settings>();
    settings.set_data_dir("C:\\Users\\you\\AppData\\Local\\trackdoctor".into());
    app.global::<Report>()
        .set_empty("No trackers or controllers yet.".into());
    set_coverage_note(&app);
    bind(&app);
    app
}

fn fill() {
    let e = fake_engine();
    let s = e.summary();
    let mut feed: Vec<ui::Line> = Vec::new();
    for v in e.session.verdicts().iter().rev() {
        feed.extend(present::verdict_lines(v, &e.label(&v.device), T0));
    }
    feed.push(present::line(
        "SteamVR is running. Recording a new session.",
        present::GOOD,
    ));
    let updates = vec![
        Update::Status {
            state: "Recording".into(),
            tone: present::GOOD,
            detail: "2m 05s so far, 1 incident, 48 KB saved".into(),
        },
        Update::Devices(present::devices(&e)),
        Update::Feed(feed),
        Update::Report(present::report(&e, &s, false)),
        Update::Usb(present::usb(&e, &s)),
        Update::Room(present::live_room(&e, &s)),
        Update::Names(present::name_rows(&e)),
    ];
    with(|w, sh| apply(w, sh, updates)).unwrap();
    let entries = vec![
        Entry {
            dir: "session-1791505301553".into(),
            title: "2026-10-14 21:03".into(),
            detail: "recording now".into(),
            tone: present::BASE,
            size: 180_000,
            track: false,
            live: true,
        },
        Entry {
            dir: "session-1791428644270".into(),
            title: "2026-10-13 23:44".into(),
            detail: "2h 10m, 14 incidents, 3.1 MB".into(),
            tone: present::WARN,
            size: 3_250_000,
            track: true,
            live: false,
        },
        Entry {
            dir: "session-1791369969812".into(),
            title: "2026-10-13 07:26".into(),
            detail: "35m, 0 incidents, 84 KB, no movement saved".into(),
            tone: present::GOOD,
            size: 86_000,
            track: false,
            live: false,
        },
    ];
    with(|w, sh| {
        w.global::<Sessions>()
            .set_items(model(sessions::items(&entries)));
        w.global::<Sessions>()
            .set_totals(sessions::totals(&entries).into());
        sh.sessions = entries;
        w.global::<Bases>().set_lines(model(present::bases_lines(
            "SteamVR base moves per session, oldest first:\n session       LHB-9417E0B3  LHB-9D8DEA7B\n 10-13 07:26          - \u{20}       1 / 4mm \n 10-13 23:44   2 / 38mm*            - \n",
        )));
    });
}

fn has_text(app: &AppWindow, text: &str) -> bool {
    ElementHandle::find_by_accessible_label(app, text).any(|e| e.size().width > 0.0)
}

const PAGES: [&str; 7] = [
    "live", "summary", "usb", "room", "bases", "sessions", "settings",
];

#[test]
fn every_page_renders_offscreen() {
    let app = setup();
    fill();
    let mut saved = Vec::new();
    for (i, name) in PAGES.iter().enumerate() {
        app.global::<Navigation>().set_page(i as i32);
        let frame = render();
        saved.push(frame.save(name));
    }
    app.global::<Navigation>().set_page(0);
    render();
    assert!(has_text(&app, "LHR-905BD201"));
    assert!(has_text(&app, "searching for bases"));
    app.global::<Navigation>().set_page(1);
    render();
    assert!(has_text(&app, "LHR-7E902102"));
    assert!(has_text(&app, "What to try"));
    app.global::<Navigation>().set_page(3);
    let frame = render();
    let labels = app.global::<Room>().get_labels();
    assert!(slint::Model::row_count(&labels) >= 6);
    let background = frame.at(WIDTH - 300, HEIGHT - 60);
    let lit = (0..HEIGHT)
        .step_by(4)
        .flat_map(|y| (0..WIDTH - 280).step_by(4).map(move |x| (x, y)))
        .filter(|&(x, y)| frame.at(x, y) != background)
        .count();
    assert!(lit > 2_000, "room picture looks empty ({lit} lit samples)");
    for p in saved {
        assert!(p.exists());
    }

    let room_global = app.global::<Room>();
    for (preset, name) in [
        (0, "room-top"),
        (1, "room-front"),
        (2, "room-side"),
        (3, "room"),
    ] {
        room_global.set_preset(preset);
        room_global.invoke_pick_preset(preset);
        render().save(name);
        let model = room_global.get_labels();
        let labels: Vec<crate::room::Label> = slint::Model::iter(&model)
            .map(|l| crate::room::Label {
                x: l.x,
                y: l.y,
                text: l.text.to_string(),
                tone: crate::room::Tone::Unknown,
                kind: match l.kind {
                    0 => crate::room::LabelKind::Base,
                    1 => crate::room::LabelKind::Device,
                    2 => crate::room::LabelKind::Grid,
                    _ => crate::room::LabelKind::Note,
                },
            })
            .collect();
        let rects: Vec<_> = labels
            .iter()
            .map(|l| crate::room::chip_rect(l, 1.0))
            .collect();
        for (i, a) in labels.iter().enumerate() {
            if a.kind != crate::room::LabelKind::Device {
                continue;
            }
            for (j, b) in labels.iter().enumerate() {
                if i == j || b.kind == crate::room::LabelKind::Grid {
                    continue;
                }
                assert!(
                    !crate::room::overlaps(&rects[i], &rects[j], 0.0),
                    "{name}: '{}' overlaps '{}'",
                    a.text,
                    b.text
                );
            }
        }
    }
}

fn labels_now(app: &AppWindow) -> Vec<String> {
    slint::Model::iter(&app.global::<Room>().get_labels())
        .map(|l| l.text.to_string())
        .collect()
}

#[test]
fn history_log_and_replay_render_offscreen() {
    use super::fixture::{LOST_AT, record_session};
    let root =
        std::env::temp_dir().join(format!("trackdoctor-history-shots-{}", std::process::id()));
    let dir = record_session(&root);
    println!(
        "verdicts: {}",
        crate::store::read_text(&dir, "verdicts.jsonl").unwrap()
    );
    for name in [
        "events.jsonl",
        "verdicts.jsonl",
        "report.txt",
        crate::track::RAW_FILE,
    ] {
        assert!(!dir.join(name).exists(), "{name} left unpacked");
    }
    assert!(dir.join(crate::track::PACKED_FILE).is_file());
    let entries = sessions::list_in(&root, None);
    assert_eq!(entries.len(), 1);
    assert!(entries[0].track && !entries[0].live);
    println!(
        "recorded session: {} ({} bytes)",
        entries[0].detail, entries[0].size
    );
    for f in crate::store::FILES {
        if let Ok(m) = std::fs::metadata(dir.join(f)) {
            println!("  {f}: {} bytes", m.len());
        }
    }

    let app = setup();
    fill();
    with(|w, s| {
        let mut all = s.sessions.clone();
        all.insert(1, entries[0].clone());
        w.global::<Sessions>()
            .set_items(model(sessions::items(&all)));
        w.global::<Sessions>()
            .set_totals(sessions::totals(&all).into());
        s.sessions = all;
    });
    app.global::<Navigation>().set_page(5);
    render().save("history");
    app.global::<Sessions>().invoke_delete(1);
    render().save("history-delete");
    assert!(has_text(&app, "Delete this session?"));
    app.global::<Sessions>().invoke_cancel_delete();
    assert!(dir.exists());

    let shown = sessions::past(&dir, Names::default()).unwrap();
    assert!(shown.log.len() > 10, "{}", shown.log.len());
    let entry = entries[0].clone();
    with(|w, s| opened(w, s, &entry, 0, shown)).unwrap();
    app.global::<Live>().set_mode(1);
    render().save("live-log");
    assert!(has_text(&app, "Every event"));

    app.global::<Navigation>().set_page(3);
    render();
    let room = app.global::<Room>();
    assert!(room.get_replay());
    assert!(slint::Model::row_count(&room.get_ticks()) >= 1);
    for (secs, name) in [
        (12u32, "replay-12s"),
        (LOST_AT as u32 / 1000 + 2, "replay-lost"),
        (90, "replay-90s"),
    ] {
        let started = Instant::now();
        with(|w, s| {
            s.play.t = secs * 1000;
            show_moment(w, s);
        });
        println!("{name}: scene + render {:?}", started.elapsed());
        render().save(name);
        let labels = labels_now(&app);
        let lost = labels.iter().filter(|l| l.ends_with('?')).count() == 1
            && labels
                .iter()
                .any(|l| l.starts_with("#1 ") && l.ends_with('?'));
        assert_eq!(lost, name == "replay-lost", "{name}: {labels:?}");
        println!(
            "  clock {} near {}",
            room.get_clock(),
            slint::Model::row_count(&room.get_near())
        );
    }

    with(|w, s| {
        s.play.t = 0;
        s.play.speed = 60;
        toggle_play(w, s);
        assert!(s.play.playing);
    });
    std::thread::sleep(Duration::from_millis(100));
    with(tick);
    let t = with(|_, s| s.play.t).unwrap();
    assert!(
        (5_000..20_000).contains(&t),
        "60x for 100 ms moved to {t} ms"
    );
    with(|w, s| {
        w.global::<Navigation>().set_page(1);
        tick(w, s);
        assert!(!s.play.playing, "playback keeps going off the Room page");
        back_to_live(w, s);
    });
    assert!(!room.get_replay());

    crate::store::delete(&dir).unwrap();
    std::fs::remove_dir(&root).unwrap();
}

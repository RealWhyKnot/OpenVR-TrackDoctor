pub mod config;
pub mod present;
pub mod recorder;
pub mod replay;
pub mod sessions;

#[cfg(test)]
mod demo;
#[cfg(test)]
mod fixture;
#[cfg(test)]
mod shots;
#[cfg(test)]
mod tests;

#[allow(clippy::all)]
pub mod ui {
    slint::include_modules!();
}

use crate::engine::{Engine, Msg};
use crate::names::{Names, data_dir};
use crate::report::SessionWriter;
use crate::room::{self, Camera, Preset};
use crate::{app, autostart, resetmap, update};
use config::Config;
use recorder::{Recorder, Update};
use sessions::{Entry, Shown};
use slint::{ComponentHandle, Image, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel};
use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use ui::{
    AppWindow, Bases, Live, Navigation, Report, ResetMap, Room, RoomLabel, Sessions, Settings,
    Status, Updates, Usb,
};

pub const TITLE: &str = "TrackDoctor";
const SUMMARY_PAGE: i32 = 1;
const ROOM_PAGE: i32 = 3;
const LOG_PAGE: usize = 250;
const SPEEDS: [u32; 4] = [1, 4, 16, 60];
const KEEP_DAYS: [u32; 4] = [0, 365, 91, 30];
const KEEP_NAMES: [&str; 4] = ["ever", "a year", "3 months", "a month"];

fn model<T: Clone + 'static>(items: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(items))
}

#[derive(Default)]
struct RoomState {
    cam: Camera,
    width: f32,
    height: f32,
    moved: bool,
    backdrop: Option<room::Backdrop>,
}

#[derive(Default)]
struct Play {
    t: u32,
    playing: bool,
    speed: u32,
    last: Option<Instant>,
    timer: slint::Timer,
}

struct Shell {
    window: slint::Weak<AppWindow>,
    live: Shown,
    past: Option<(usize, Shown)>,
    room: RoomState,
    recording: Option<PathBuf>,
    sessions: Vec<Entry>,
    release: Option<update::Release>,
    installed: Option<update::Installed>,
    config: Config,
    poses: Arc<AtomicBool>,
    room_shown: Arc<AtomicBool>,
    steamvr_synced: bool,
    play: Play,
    log_page: usize,
    started_ms: u64,
    asking: Option<Asking>,
    listing: u64,
}

#[derive(Clone, Debug)]
enum Asking {
    Delete(PathBuf),
    Keep { pick: usize, was: usize },
}

thread_local! {
    static SHELL: RefCell<Option<Shell>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&AppWindow, &mut Shell) -> R) -> Option<R> {
    SHELL.with(|cell| {
        let mut borrow = cell.try_borrow_mut().ok()?;
        let shell = borrow.as_mut()?;
        let window = shell.window.upgrade()?;
        Some(f(&window, shell))
    })
}

fn post(f: impl FnOnce(&AppWindow, &mut Shell) + Send + 'static) {
    let _ = slint::invoke_from_event_loop(move || {
        with(f);
    });
}

fn has(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

fn select_backend(active: bool) {
    let _ = slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("software".into())
        .with_winit_window_attributes_hook(move |a| a.with_active(active))
        .select();
}

fn new_engine(poses: bool) -> (Engine, Option<String>) {
    let names = Names::load_default();
    match SessionWriter::new(poses) {
        Ok(session) => (Engine::new(session, names), None),
        Err(e) => (
            Engine::new(SessionWriter::memory(crate::report::sessions_dir()), names),
            Some(format!(
                "Couldn't create a session folder, so this session won't be saved: {e}"
            )),
        ),
    }
}

pub fn run() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(setup) = autostart::Setup::from_args(&args) {
        return autostart::setup_command(setup);
    }
    let launched = has(&args, autostart::LAUNCH_FLAG);
    update::cleanup_old();
    let Some(lock) = app::try_lock() else {
        if !launched {
            crate::win::focus_existing(TITLE);
        }
        return 0;
    };
    crate::debug::init();
    autostart::refresh_manifest();
    select_backend(!launched);
    let window = match AppWindow::new() {
        Ok(w) => w,
        Err(_) => return 1,
    };
    let config = Config::load();
    let poses = Arc::new(AtomicBool::new(config.save_poses));
    let recorder = Recorder::new(Box::new(new_engine), poses.clone());
    SHELL.with(|cell| {
        *cell.borrow_mut() = Some(Shell {
            window: window.as_weak(),
            live: Shown::default(),
            past: None,
            room: RoomState::default(),
            recording: None,
            sessions: Vec::new(),
            release: None,
            installed: None,
            config: config.clone(),
            poses,
            room_shown: recorder.room_shown(),
            steamvr_synced: launched && config.open_with_steamvr,
            play: Play {
                speed: SPEEDS[0],
                ..Play::default()
            },
            log_page: 0,
            started_ms: crate::event::now_ms(),
            asking: None,
            listing: 0,
        })
    });
    load(&window, &config);
    bind(&window);

    let (tx, rx) = std::sync::mpsc::channel::<Msg>();
    app::spawn_collectors(tx);
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = stop.clone();
    let worker = std::thread::Builder::new()
        .name("recorder".into())
        .spawn(move || {
            recorder::run(rx, recorder, thread_stop, |updates| {
                post(move |w, s| apply(w, s, updates));
            })
        })
        .ok();

    refresh_sessions();
    check_updates(true);
    let _ = window.run();
    stop.store(true, Ordering::Relaxed);
    if let Some(worker) = worker {
        let _ = worker.join();
    }
    let restart =
        with(|_, s| matches!(s.installed, Some(update::Installed::Restart(_)))).unwrap_or(false);
    SHELL.with(|cell| cell.borrow_mut().take());
    drop(window);
    drop(lock);
    if restart && let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).spawn();
    }
    0
}

fn load(w: &AppWindow, config: &Config) {
    w.global::<Status>().set_version(crate::VERSION.into());
    let settings = w.global::<Settings>();
    settings.set_open_with_steamvr(config.open_with_steamvr);
    settings.set_save_poses(config.save_poses);
    settings.set_data_dir(data_dir().display().to_string().into());
    w.global::<Sessions>()
        .set_keep(keep_index(config.keep_days) as i32);
    settings.set_names(model(present::name_rows_for(
        &sessions::last_roster(),
        &Names::load_default(),
    )));
    w.global::<Live>()
        .set_empty("Start SteamVR. This fills in once it is running.".into());
    w.global::<Report>()
        .set_empty("No trackers or controllers yet.".into());
    w.global::<Room>()
        .set_caption("Waiting for SteamVR to report positions.".into());
    set_coverage_note(w);
}

fn bind(w: &AppWindow) {
    let status = w.global::<Status>();
    status.on_dismiss_notice(|| {
        with(|w, _| w.global::<Status>().set_notice("".into()));
    });
    status.on_back_to_live(|| {
        with(back_to_live);
    });

    let room = w.global::<Room>();
    room.on_shown(|_| {
        with(sync_room_shown);
    });
    room.on_resized(|width, height| {
        with(|w, s| {
            s.room.width = width;
            s.room.height = height;
            render_room(w, s);
        });
    });
    room.on_drag(|dx, dy, pan| {
        with(|w, s| {
            if pan {
                s.room.cam.pan(dx, dy, s.room.height);
            } else {
                s.room.cam.orbit(dx, dy);
            }
            s.room.moved = true;
            render_room(w, s);
        });
    });
    room.on_zoom(|steps| {
        with(|w, s| {
            s.room.cam.zoom(steps);
            s.room.moved = true;
            render_room(w, s);
        });
    });
    room.on_pick_preset(|i| {
        with(|w, s| {
            let preset = match i {
                0 => Preset::Top,
                1 => Preset::Front,
                2 => Preset::Side,
                _ => Preset::Angled,
            };
            s.room.cam.preset(preset);
            s.room.moved = false;
            render_room(w, s);
        });
    });
    room.on_options_changed(|| {
        with(|w, s| {
            set_coverage_note(w);
            render_room(w, s);
        });
    });
    room.on_play(|| {
        with(toggle_play);
    });
    room.on_pick_speed(|i| {
        with(|_, s| s.play.speed = SPEEDS[i.clamp(0, 3) as usize]);
    });
    room.on_seek(|fraction, snap| {
        with(|w, s| {
            if let Some(r) = replaying(s) {
                s.play.t = r.seek(fraction, snap);
                s.play.last = Some(Instant::now());
                show_moment(w, s);
            }
        });
    });

    w.global::<Live>().on_log_page(|step| {
        with(|w, s| {
            s.log_page = s.log_page.saturating_add_signed(step as isize);
            show_log(w, s);
        });
    });

    let sessions = w.global::<Sessions>();
    sessions.on_refresh(refresh_sessions);
    sessions.on_open(|i| open_session(i, SUMMARY_PAGE));
    sessions.on_replay(|i| open_session(i, ROOM_PAGE));
    sessions.on_open_folder(|i| {
        with(|_, s| {
            if let Some(e) = s.sessions.get(i as usize) {
                crate::win::open_folder(&e.dir);
            }
        });
    });
    sessions.on_delete(|i| {
        with(|w, s| {
            let Some(e) = s.sessions.get(i as usize).filter(|e| !e.live) else {
                return;
            };
            let what = format!(
                "The session from {} ({}). Its log, summary and movement are removed from {}. This can't be undone.",
                e.title,
                e.detail,
                e.dir.display()
            );
            s.asking = Some(Asking::Delete(e.dir.clone()));
            ask(w, "Delete this session?", &what, "Delete");
        });
    });
    sessions.on_cancel_delete(|| {
        with(|w, s| {
            if let Some(Asking::Keep { was, .. }) = s.asking.take() {
                w.global::<Sessions>().set_keep(was as i32);
            }
            w.global::<Sessions>().set_asking(-1);
        });
    });
    sessions.on_confirm_delete(|| {
        let Some(dir) = with(|w, s| {
            w.global::<Sessions>().set_asking(-1);
            match s.asking.take()? {
                Asking::Keep { pick, .. } => {
                    keep_for(w, s, pick);
                    None
                }
                Asking::Delete(dir) => {
                    if s.recording.as_ref() == Some(&dir) {
                        return None;
                    }
                    let open = s.past.as_ref().and_then(|(at, _)| s.sessions.get(*at));
                    if open.is_some_and(|e| e.dir == dir) {
                        back_to_live(w, s);
                    }
                    Some(dir)
                }
            }
        })
        .flatten() else {
            return;
        };
        std::thread::spawn(move || {
            let result = crate::store::delete(&dir);
            post(move |w, s| {
                list_sessions(s);
                if let Err(e) = result {
                    w.global::<Sessions>().set_note(
                        format!(
                            "Couldn't remove all of {}: {e}. TrackDoctor only deletes files it wrote, so anything else in that folder is still there.",
                            dir.display()
                        )
                        .into(),
                    );
                }
            });
        });
    });
    sessions.on_keep_changed(|i| {
        with(|w, s| {
            let pick = (i.clamp(0, 3)) as usize;
            let was = keep_index(s.config.keep_days);
            let days = KEEP_DAYS[pick];
            let cutoff = crate::event::now_ms().saturating_sub(days as u64 * 86_400_000);
            let old: Vec<&Entry> = s
                .sessions
                .iter()
                .filter(|e| !e.live && days > 0)
                .filter(|e| sessions::start_ms(&e.dir).is_some_and(|t| t < cutoff))
                .collect();
            if old.is_empty() {
                keep_for(w, s, pick);
                return;
            }
            let what = format!(
                "Keeping sessions for {} removes {} you already have, using {}. This can't be undone.",
                KEEP_NAMES[pick],
                crate::summary::plural(old.len(), "older session"),
                crate::store::fmt_size(old.iter().map(|e| e.size).sum())
            );
            s.asking = Some(Asking::Keep { pick, was });
            ask(w, "Remove older sessions?", &what, "Remove");
        });
    });

    let settings = w.global::<Settings>();
    settings.on_steamvr_toggled(steamvr_toggled);
    settings.on_poses_toggled(|on| {
        with(|w, s| {
            s.config.save_poses = on;
            s.poses.store(on, Ordering::Relaxed);
            save_config(w, &s.config);
        });
    });
    settings.on_rename(|serial, name| {
        let mut names = Names::load_default();
        let result = names.set(&serial, &name);
        with(|w, _| match result {
            Ok(()) => {
                let settings = w.global::<Settings>();
                let rows: Vec<ui::NameRow> = slint::Model::iter(&settings.get_names())
                    .map(|mut r| {
                        if r.serial == serial {
                            r.name = name.trim().into();
                        }
                        r
                    })
                    .collect();
                settings.set_names(model(rows));
            }
            Err(e) => w.global::<Status>().set_notice(
                format!("Couldn't save the name to {}: {e}", names.path().display()).into(),
            ),
        });
    });
    settings.on_open_data(|| crate::win::open_folder(&data_dir()));
    settings.on_check_updates(|| check_updates(false));

    let updates = w.global::<Updates>();
    updates.on_update_now(update_now);
    updates.on_update_later(|| {
        with(|w, _| w.global::<Updates>().set_shown(false));
    });
    updates.on_update_skip(|| {
        with(|w, s| {
            if let Some(r) = &s.release {
                let _ = update::skip(r);
            }
            w.global::<Updates>().set_shown(false);
        });
    });
    updates.on_finish_update(|| {
        with(|w, _| {
            let _ = w.hide();
        });
        let _ = slint::quit_event_loop();
    });

    w.global::<Report>().on_reset_map(|| {
        std::thread::spawn(|| {
            let explain = resetmap::explain(resetmap::steamvr_running());
            post(move |w, _| {
                let r = w.global::<ResetMap>();
                r.set_explain(explain.into());
                r.set_progress("".into());
                r.set_busy(false);
                r.set_done(false);
                r.set_shown(true);
            });
        });
    });
    let reset = w.global::<ResetMap>();
    reset.on_cancel(|| {
        with(|w, _| w.global::<ResetMap>().set_shown(false));
    });
    reset.on_confirm(|| {
        with(|w, _| {
            let r = w.global::<ResetMap>();
            r.set_busy(true);
            r.set_progress("Working...".into());
        });
        std::thread::spawn(|| {
            let result = resetmap::run(&mut |line| {
                post(move |w, _| w.global::<ResetMap>().set_progress(line.into()));
            });
            post(move |w, _| {
                let r = w.global::<ResetMap>();
                r.set_busy(false);
                r.set_done(true);
                if let Err(e) = result {
                    r.set_progress(format!("That didn't work: {e}").into());
                }
            });
        });
    });
}

fn save_config(w: &AppWindow, config: &Config) {
    if let Err(e) = config.save() {
        w.global::<Status>()
            .set_notice(format!("Couldn't save settings: {e}").into());
    }
}

fn steamvr_toggled(on: bool) {
    with(|w, s| {
        s.config.open_with_steamvr = on;
        save_config(w, &s.config);
        let settings = w.global::<Settings>();
        settings.set_steamvr_busy(true);
        settings.set_steamvr_note("".into());
    });
    std::thread::spawn(move || {
        let code = steamvr_child(if on {
            autostart::Setup::Enable
        } else {
            autostart::Setup::Disable
        });
        let note = match (code, on) {
            (Some(0), true) => "TrackDoctor now opens when SteamVR starts.".to_string(),
            (Some(0), false) => "SteamVR no longer opens TrackDoctor.".to_string(),
            (Some(autostart::NOT_AVAILABLE), true) => {
                "SteamVR isn't running or couldn't be reached. The setting is saved, and TrackDoctor adds itself once it sees SteamVR running.".to_string()
            }
            (Some(autostart::NOT_AVAILABLE), false) => {
                "SteamVR isn't running or couldn't be reached. The setting is saved, and TrackDoctor takes itself off SteamVR's list once it sees SteamVR running.".to_string()
            }
            (Some(c), _) => format!("Couldn't change SteamVR's startup list (code {c})."),
            (None, _) => "Couldn't change SteamVR's startup list.".to_string(),
        };
        post(move |w, _| {
            let settings = w.global::<Settings>();
            settings.set_steamvr_busy(false);
            settings.set_steamvr_note(note.into());
        });
    });
}

fn steamvr_child(setup: autostart::Setup) -> Option<i32> {
    let exe = std::env::current_exe().ok()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg(setup.flag());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd.status().ok()?.code()
}

fn release_version(r: &update::Release) -> String {
    r.version().map_or(r.tag_name.clone(), |v| v.to_string())
}

fn check_updates(quiet: bool) {
    if !quiet {
        with(|w, _| w.global::<Settings>().set_update_note("Checking...".into()));
    }
    std::thread::spawn(move || {
        let found = update::find(quiet);
        post(move |w, s| {
            let note = match found {
                Ok(Some(release)) => {
                    let version = release_version(&release);
                    let u = w.global::<Updates>();
                    u.set_version(version.clone().into());
                    if s.installed.is_none() {
                        u.set_state("".into());
                        u.set_shown(true);
                    }
                    s.release = Some(release);
                    format!("TrackDoctor {version} is available.")
                }
                Ok(None) => "You have the latest version.".into(),
                Err(e) => format!("Couldn't check for updates: {e}"),
            };
            if !quiet {
                w.global::<Settings>().set_update_note(note.into());
            }
        });
    });
}

fn update_now() {
    let Some(release) = with(|_, s| s.release.clone()).flatten() else {
        return;
    };
    with(|w, _| {
        let u = w.global::<Updates>();
        u.set_busy(true);
        u.set_state(format!("Downloading TrackDoctor {}...", release_version(&release)).into());
    });
    std::thread::spawn(move || {
        let result = update::install(&release);
        post(move |w, s| {
            let u = w.global::<Updates>();
            u.set_busy(false);
            match result {
                Ok(installed) => {
                    u.set_state(installed.message().to_string().into());
                    u.set_ready(true);
                    s.installed = Some(installed);
                }
                Err(e) => u.set_state(format!("The update didn't work: {e}").into()),
            }
        });
    });
}

fn keep_index(days: u32) -> usize {
    KEEP_DAYS.iter().position(|d| *d == days).unwrap_or(0)
}

fn ask(w: &AppWindow, title: &str, what: &str, button: &str) {
    let g = w.global::<Sessions>();
    g.set_asking_title(title.into());
    g.set_asking_what(what.into());
    g.set_asking_button(button.into());
    g.set_asking(0);
}

fn keep_for(w: &AppWindow, s: &mut Shell, pick: usize) {
    s.config.keep_days = KEEP_DAYS[pick];
    w.global::<Sessions>().set_keep(pick as i32);
    save_config(w, &s.config);
    list_sessions(s);
}

fn refresh_sessions() {
    with(|_, s| list_sessions(s));
}

fn list_sessions(s: &mut Shell) {
    s.listing += 1;
    let listing = s.listing;
    let recording = s.recording.clone();
    let keep_days = s.config.keep_days;
    let started = s.started_ms;
    std::thread::spawn(move || {
        let root = crate::report::sessions_dir();
        let removed = sessions::prune(
            &root,
            keep_days,
            recording.as_deref(),
            crate::event::now_ms(),
        );
        sessions::pack_finished(&root, recording.as_deref(), started);
        let entries = sessions::list(recording.as_deref());
        let bases = sessions::bases_lines();
        post(move |w, s| {
            if s.listing != listing {
                return;
            }
            let current = s
                .past
                .as_ref()
                .and_then(|(i, _)| s.sessions.get(*i))
                .map(|e| e.dir.clone());
            s.sessions = entries;
            let index = current
                .as_ref()
                .and_then(|d| s.sessions.iter().position(|e| &e.dir == d));
            match (index, s.past.as_mut()) {
                (Some(i), Some((at, _))) => *at = i,
                (None, Some(_)) => back_to_live(w, s),
                _ => {}
            }
            let g = w.global::<Sessions>();
            g.set_items(model(sessions::items(&s.sessions)));
            g.set_current(index.map_or(-1, |i| i as i32));
            g.set_totals(sessions::totals(&s.sessions).into());
            if !removed.is_empty() {
                g.set_note(
                    format!(
                        "Removed {} older than you keep them.",
                        crate::summary::plural(removed.len(), "session")
                    )
                    .into(),
                );
            } else if g.get_note().starts_with("Removed ") {
                g.set_note("".into());
            }
            w.global::<Bases>().set_lines(model(bases));
        });
    });
}

fn open_session(i: i32, page: i32) {
    let Some(entry) = with(|_, s| s.sessions.get(i as usize).cloned()).flatten() else {
        return;
    };
    if entry.live {
        with(|w, s| {
            back_to_live(w, s);
            w.global::<Navigation>().set_page(SUMMARY_PAGE);
        });
        return;
    }
    if with(|w, s| {
        let open = s.past.as_ref().is_some_and(|(at, _)| *at == i as usize);
        if open {
            w.global::<Navigation>().set_page(page);
        }
        open
    }) == Some(true)
    {
        return;
    }
    with(|w, _| {
        w.global::<Sessions>()
            .set_note(format!("Reading the session from {}...", entry.title).into())
    });
    std::thread::spawn(move || {
        let result = sessions::past(&entry.dir, Names::load_default());
        post(move |w, s| match result {
            Ok(shown) => opened(w, s, &entry, page, shown),
            Err(e) => w
                .global::<Sessions>()
                .set_note(format!("Couldn't read that session: {e}").into()),
        });
    });
}

fn opened(w: &AppWindow, s: &mut Shell, entry: &Entry, page: i32, shown: Shown) {
    let g = w.global::<Sessions>();
    g.set_note("".into());
    let Some(index) = s.sessions.iter().position(|e| e.dir == entry.dir) else {
        return;
    };
    g.set_current(index as i32);
    w.global::<Status>()
        .set_viewing(format!("the session from {}", entry.title).into());
    s.room.moved = false;
    s.log_page = 0;
    stop_play(w, s);
    s.play.t = 0;
    s.past = Some((index, shown.clone()));
    show(w, s, &shown, true);
    w.global::<Navigation>().set_page(page);
    sync_room_shown(w, s);
}

fn replaying(s: &Shell) -> Option<Arc<replay::Replay>> {
    s.past.as_ref().and_then(|(_, p)| p.replay.clone())
}

fn stop_play(w: &AppWindow, s: &mut Shell) {
    s.play.playing = false;
    s.play.last = None;
    s.play.timer.stop();
    w.global::<Room>().set_playing(false);
}

fn toggle_play(w: &AppWindow, s: &mut Shell) {
    let Some(r) = replaying(s) else {
        return;
    };
    if s.play.playing {
        stop_play(w, s);
        return;
    }
    if s.play.t >= r.length {
        s.play.t = 0;
    }
    s.play.playing = true;
    s.play.last = Some(Instant::now());
    s.play.timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(33),
        || {
            with(tick);
        },
    );
    w.global::<Room>().set_playing(true);
}

fn tick(w: &AppWindow, s: &mut Shell) {
    let Some(r) = replaying(s) else {
        stop_play(w, s);
        return;
    };
    if !s.play.playing || w.global::<Navigation>().get_page() != ROOM_PAGE {
        stop_play(w, s);
        return;
    }
    let now = Instant::now();
    let dt = s
        .play
        .last
        .map_or(0, |l| now.duration_since(l).as_millis() as u64);
    s.play.last = Some(now);
    let t = s.play.t as u64 + dt * s.play.speed as u64;
    s.play.t = t.min(r.length as u64) as u32;
    if s.play.t >= r.length {
        stop_play(w, s);
    }
    show_moment(w, s);
}

fn show_moment(w: &AppWindow, s: &mut Shell) {
    let g = w.global::<Room>();
    match replaying(s) {
        Some(r) => {
            g.set_position(s.play.t as f32 / r.length as f32);
            g.set_clock(r.clock(s.play.t).into());
            g.set_near(model(r.near(s.play.t, 12, 4)));
        }
        None => g.set_near(model(Vec::new())),
    }
    render_room(w, s);
}

fn show_log(w: &AppWindow, s: &mut Shell) {
    let g = w.global::<Live>();
    let log = s
        .past
        .as_ref()
        .map(|(_, p)| p.log.as_slice())
        .unwrap_or(&[]);
    let pages = log.len().div_ceil(LOG_PAGE).max(1);
    s.log_page = s.log_page.min(pages - 1);
    let from = s.log_page * LOG_PAGE;
    let to = (from + LOG_PAGE).min(log.len());
    g.set_log(model(log[from..to].to_vec()));
    g.set_log_where(if log.is_empty() {
        "This session has no events saved.".into()
    } else {
        format!(
            "Events {} to {} of {}, oldest first",
            from + 1,
            to,
            log.len()
        )
        .into()
    });
    g.set_log_older(s.log_page > 0);
    g.set_log_newer(s.log_page + 1 < pages);
}

fn back_to_live(w: &AppWindow, s: &mut Shell) {
    stop_play(w, s);
    s.past = None;
    s.room.moved = false;
    w.global::<Status>().set_viewing("".into());
    w.global::<Sessions>().set_current(-1);
    let shown = s.live.clone();
    show(w, s, &shown, false);
    sync_room_shown(w, s);
}

fn sync_room_shown(w: &AppWindow, s: &mut Shell) {
    let on = s.past.is_none()
        && w.global::<Navigation>().get_page() == ROOM_PAGE
        && !w.window().is_minimized();
    s.room_shown.store(on, Ordering::Relaxed);
}

fn show(w: &AppWindow, s: &mut Shell, shown: &Shown, past: bool) {
    show_devices(w, shown);
    show_feed(w, shown);
    show_report(w, shown);
    show_usb(w, shown);
    let live = w.global::<Live>();
    live.set_past(past);
    live.set_mode(0);
    show_log(w, s);
    let room = w.global::<Room>();
    room.set_replay(shown.replay.is_some());
    room.set_ticks(model(
        shown
            .replay
            .as_ref()
            .map(|r| {
                r.marks
                    .iter()
                    .map(|m| ui::Tick {
                        at: m.t as f32 / r.length as f32,
                        tone: m.tone,
                    })
                    .collect()
            })
            .unwrap_or_default(),
    ));
    let g = w.global::<Room>();
    g.set_legend(model(shown.room.legend.clone()));
    g.set_caption(shown.room.caption.clone().into());
    show_moment(w, s);
}

fn show_devices(w: &AppWindow, shown: &Shown) {
    w.global::<Live>().set_devices(model(shown.devices.clone()));
}

fn show_feed(w: &AppWindow, shown: &Shown) {
    w.global::<Live>().set_feed(model(shown.feed.clone()));
}

fn show_report(w: &AppWindow, shown: &Shown) {
    let r = &shown.report;
    let g = w.global::<Report>();
    g.set_findings(model(r.findings.iter().map(|c| c.to_ui()).collect()));
    g.set_heading(r.heading.clone().into());
    g.set_ranks(model(r.ranks.clone()));
    g.set_notes(model(r.notes.clone()));
    g.set_bases(model(r.bases.clone()));
    g.set_empty(r.empty.clone().into());
}

fn show_usb(w: &AppWindow, shown: &Shown) {
    let g = w.global::<Usb>();
    g.set_tree(model(shown.usb.tree.clone()));
    g.set_warnings(model(shown.usb.warnings.clone()));
}

fn show_room(w: &AppWindow, s: &mut Shell, view: &present::RoomView) {
    let g = w.global::<Room>();
    g.set_legend(model(view.legend.clone()));
    g.set_caption(view.caption.clone().into());
    render_scene(w, &mut s.room, &view.scene);
}

fn render_room(w: &AppWindow, s: &mut Shell) {
    let scene = match &s.past {
        Some((_, p)) => match &p.replay {
            Some(r) => r.scene(s.play.t),
            None => p.room.scene.clone(),
        },
        None => s.live.room.scene.clone(),
    };
    render_scene(w, &mut s.room, &scene);
}

pub fn coverage_height(index: i32) -> Option<f32> {
    match index {
        1 => Some(0.0),
        2 => Some(1.0),
        3 => Some(1.6),
        _ => None,
    }
}

fn set_coverage_note(w: &AppWindow) {
    let g = w.global::<Room>();
    let note = match g.get_coverage() {
        1 => "Each spot on the floor is shaded by how many base stations can see it.",
        2 => {
            "Each floor spot is shaded by how many base stations can see the point 1 m above it, about waist height."
        }
        3 => {
            "Each floor spot is shaded by how many base stations can see the point 1.6 m above it, about head height."
        }
        _ => "Shading is off. Pick a height above to see where the base stations reach.",
    };
    g.set_coverage_note(
        format!("{note} Two or more make tracking much steadier, since one can be blocked.").into(),
    );
}

fn render_scene(w: &AppWindow, state: &mut RoomState, scene: &room::Scene) {
    if w.global::<Navigation>().get_page() != ROOM_PAGE {
        return;
    }
    let scale = w.window().scale_factor();
    let pw = (state.width * scale).round() as u32;
    let ph = (state.height * scale).round() as u32;
    if pw < 2 || ph < 2 {
        return;
    }
    let g = w.global::<Room>();
    if !state.moved {
        state.cam.fit(&scene.bounds(), state.width, state.height);
        if g.get_preset() == 3 {
            let bases: Vec<room::V3> = scene.bases.iter().map(|b| b.pos).collect();
            state.cam.yaw = room::pick_yaw(&bases, state.cam.target);
        }
    }
    let opts = room::Options {
        coverage_height: coverage_height(g.get_coverage()),
        coverage_on_floor: true,
        cones: g.get_cones(),
        drops: g.get_drops(),
        scale,
    };
    let (pixmap, labels) = room::render_with(scene, &state.cam, &opts, pw, ph, &mut state.backdrop);
    let buffer = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(pixmap.data(), pw, ph);
    g.set_picture(Image::from_rgba8_premultiplied(buffer));
    g.set_labels(model(
        labels
            .into_iter()
            .map(|l| RoomLabel {
                x: l.x / scale,
                y: l.y / scale,
                text: l.text.into(),
                tone: present::room_tone(l.tone),
                kind: match l.kind {
                    room::LabelKind::Base => 0,
                    room::LabelKind::Device => 1,
                    room::LabelKind::Grid => 2,
                    room::LabelKind::Note => 3,
                },
            })
            .collect(),
    ));
}

fn apply(w: &AppWindow, s: &mut Shell, updates: Vec<Update>) {
    let live = s.past.is_none();
    let mut room_changed = false;
    for u in updates {
        match u {
            Update::Status {
                state,
                tone,
                detail,
            } => {
                let g = w.global::<Status>();
                g.set_state(state.into());
                g.set_tone(tone);
                g.set_detail(detail.into());
            }
            Update::Devices(d) => {
                s.live.devices = d;
                if live {
                    show_devices(w, &s.live);
                }
            }
            Update::Feed(f) => {
                s.live.feed = f;
                if live {
                    show_feed(w, &s.live);
                }
            }
            Update::Report(r) => {
                s.live.report = r;
                if live {
                    show_report(w, &s.live);
                }
            }
            Update::Usb(u) => {
                s.live.usb = u;
                if live {
                    show_usb(w, &s.live);
                }
            }
            Update::Room(r) => {
                s.live.room = r;
                room_changed = true;
            }
            Update::Names(n) => {
                let settings = w.global::<Settings>();
                let shown: Vec<ui::NameRow> = slint::Model::iter(&settings.get_names()).collect();
                if shown != n {
                    settings.set_names(model(n));
                }
            }
            Update::Recording(dir) => {
                if dir.is_some() && !s.steamvr_synced {
                    s.steamvr_synced = true;
                    let setup = if s.config.open_with_steamvr {
                        autostart::Setup::Sync
                    } else {
                        autostart::Setup::Disable
                    };
                    w.global::<Settings>().set_steamvr_busy(true);
                    std::thread::spawn(move || {
                        let code = steamvr_child(setup);
                        post(move |w, s| {
                            let settings = w.global::<Settings>();
                            settings.set_steamvr_busy(false);
                            if code == Some(0) {
                                let config = Config::load();
                                s.config.open_with_steamvr = config.open_with_steamvr;
                                settings.set_open_with_steamvr(config.open_with_steamvr);
                            }
                        });
                    });
                }
                s.recording = dir;
                list_sessions(s);
            }
            Update::SessionEnded => {
                let nav = w.global::<Navigation>();
                if nav.get_page() == 0 {
                    nav.set_page(1);
                }
            }
        }
    }
    if room_changed && live {
        let view = s.live.room.clone();
        show_room(w, s, &view);
    }
    sync_room_shown(w, s);
}

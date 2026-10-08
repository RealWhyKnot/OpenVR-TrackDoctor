pub mod app;
pub mod autostart;
pub mod correlate;
pub mod detect;
pub mod engine;
pub mod event;
pub mod names;
pub mod report;
pub mod signals;
pub mod summary;
pub mod tui;
pub mod update;
pub mod win;

pub const VERSION: &str = match option_env!("TRACKDOCTOR_VERSION") {
    Some(v) => v,
    None => "dev",
};

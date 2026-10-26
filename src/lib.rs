pub mod app;
pub mod autostart;
pub mod bases;
pub mod channel;
pub mod check_battery;
pub mod check_device;
pub mod check_jitter;
pub mod check_layout;
pub mod check_radio;
pub mod check_restart;
pub mod check_software;
pub mod check_spots;
pub mod check_usb;
pub mod correlate;
pub mod debug;
pub mod detect;
pub mod diagnose;
pub mod engine;
pub mod event;
pub mod gui;
pub mod names;
pub mod report;
pub mod resetmap;
pub mod room;
pub mod sight;
pub mod signals;
pub mod store;
pub mod summary;
pub mod track;
pub mod update;
pub mod win;

pub const VERSION: &str = match option_env!("TRACKDOCTOR_VERSION") {
    Some(v) => v,
    None => "dev",
};

pub const DEV: bool = matches!(env!("TRACKDOCTOR_DEV_BUILD").as_bytes(), b"1");

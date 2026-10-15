#![windows_subsystem = "windows"]

fn main() {
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
    {
        let _ = std::process::Command::new(dir.join("trackdoctor.exe"))
            .arg("--steamvr")
            .spawn();
    }
}

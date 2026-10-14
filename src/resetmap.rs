use crate::update::system_tool;
use anyhow::{Context, bail};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const STEAMVR_URL: &str = "steam://rungameid/250820";
const CLOSE_TIMEOUT: Duration = Duration::from_secs(30);

fn quiet(cmd: &mut std::process::Command) -> &mut std::process::Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

pub fn listed(tasklist: &str, image: &str) -> bool {
    let want = format!("\"{}\"", image.to_ascii_lowercase());
    tasklist
        .lines()
        .any(|l| l.to_ascii_lowercase().starts_with(&want))
}

pub fn steamvr_running() -> bool {
    quiet(system_tool("tasklist.exe").args([
        "/FI",
        "IMAGENAME eq vrserver.exe",
        "/NH",
        "/FO",
        "CSV",
    ]))
    .output()
    .map(|o| listed(&String::from_utf8_lossy(&o.stdout), "vrserver.exe"))
    .unwrap_or(false)
}

fn close_steamvr() -> anyhow::Result<()> {
    quiet(
        system_tool("WindowsPowerShell\\v1.0\\powershell.exe").args([
            "-NoProfile",
            "-Command",
            "$m = Get-Process vrmonitor -ErrorAction SilentlyContinue; if ($m) { [void]$m.CloseMainWindow() }",
        ]),
    )
    .status()
    .context("could not ask SteamVR to close")?;
    let start = Instant::now();
    while steamvr_running() {
        if start.elapsed() > CLOSE_TIMEOUT {
            bail!(
                "SteamVR is still running after {} seconds. Close it yourself, then run trackdoctor resetmap again.",
                CLOSE_TIMEOUT.as_secs()
            );
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Ok(())
}

fn start_steamvr() -> anyhow::Result<()> {
    quiet(system_tool("cmd.exe").args(["/C", "start", "", STEAMVR_URL]))
        .status()
        .context("could not start SteamVR")?;
    Ok(())
}

pub fn backup_path(db: &Path, stamp: &str) -> PathBuf {
    let dir = db.parent().unwrap_or(Path::new("."));
    let mut path = dir.join(format!("lighthousedb.backup-{stamp}.json"));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("lighthousedb.backup-{stamp}-{n}.json"));
        n += 1;
    }
    path
}

pub fn back_up(db: &Path, stamp: &str) -> anyhow::Result<PathBuf> {
    let to = backup_path(db, stamp);
    std::fs::rename(db, &to)
        .with_context(|| format!("could not rename {} to {}", db.display(), to.display()))?;
    Ok(to)
}

pub fn confirm(prompt: &str) -> bool {
    print!("{prompt} [y/N]: ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

pub fn explain(running: bool) -> String {
    let steps = if running {
        "This closes SteamVR and whatever game is running in it, renames SteamVR's saved base station map so a backup is kept, and starts SteamVR again."
    } else {
        "This renames SteamVR's saved base station map so a backup is kept. SteamVR builds a new one the next time it starts."
    };
    format!(
        "{steps} SteamVR then works out where the bases are from scratch, which takes a minute or two with your trackers in view of the bases. Do this after every base is in its final place."
    )
}

pub fn run(asked: bool) -> anyhow::Result<()> {
    let db = crate::signals::logtail::lighthouse_db_path()
        .context("cannot find SteamVR's config folder")?;
    if !db.exists() {
        println!(
            "There's no saved base station map at {}. SteamVR builds one when it starts.",
            db.display()
        );
        return Ok(());
    }
    let running = steamvr_running();
    if !asked {
        println!("{}", explain(running));
        if !confirm("Reset the base station map now?") {
            println!("Left the map as it is.");
            return Ok(());
        }
    }
    if running {
        println!("Closing SteamVR...");
        close_steamvr()?;
    }
    let backup = back_up(&db, &crate::win::local_stamp())?;
    println!("Moved the old map to {}.", backup.display());
    if running {
        start_steamvr()?;
        println!("Starting SteamVR again.");
    }
    println!(
        "To undo, close SteamVR and rename {} back to lighthousedb.json.",
        backup
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    );
    Ok(())
}

pub fn command() -> anyhow::Result<()> {
    let result = run(false);
    if let Err(e) = &result {
        eprintln!("{e:#}");
    }
    crate::win::pause_if_own_console();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tasklist_csv_is_matched_by_image_name() {
        assert!(listed(
            "\"vrserver.exe\",\"77016\",\"Console\",\"1\",\"224,264 K\"\r\n",
            "vrserver.exe"
        ));
        assert!(!listed(
            "INFO: No tasks are running which match the specified criteria.\r\n",
            "vrserver.exe"
        ));
        assert!(!listed(
            "\"vrserver_helper.exe\",\"1\",\"Console\",\"1\",\"1 K\"\r\n",
            "vrserver.exe"
        ));
    }

    #[test]
    fn backup_keeps_the_old_map_and_never_overwrites_one() {
        let dir = std::env::temp_dir().join(format!("trackdoctor-resetmap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("lighthousedb.json");
        std::fs::write(&db, "first").unwrap();
        let a = back_up(&db, "2026-10-14-0153").unwrap();
        assert_eq!(
            a.file_name().unwrap(),
            "lighthousedb.backup-2026-10-14-0153.json"
        );
        assert!(!db.exists());
        std::fs::write(&db, "second").unwrap();
        let b = back_up(&db, "2026-10-14-0153").unwrap();
        assert_eq!(
            b.file_name().unwrap(),
            "lighthousedb.backup-2026-10-14-0153-2.json"
        );
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "first");
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "second");
        std::fs::remove_file(&a).unwrap();
        std::fs::remove_file(&b).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }
}

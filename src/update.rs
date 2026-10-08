use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

const RELEASES_URL: &str =
    "https://api.github.com/repos/RealWhyKnot/OpenVR-TrackDoctor/releases?per_page=20";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Channel {
    Dev,
    Beta,
    Stable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub nums: [u32; 4],
    pub channel: Channel,
}

impl Version {
    pub fn parse(text: &str) -> Option<Version> {
        let text = text.trim();
        let text = text.split('+').next()?;
        let text = text.strip_prefix(['v', 'V']).unwrap_or(text);
        let (numeric, suffix) = text.split_once('-').unwrap_or((text, ""));
        let mut parts = numeric.split('.');
        let mut nums = [0u32; 4];
        for n in &mut nums {
            let part = parts.next()?;
            if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            *n = part.parse().ok()?;
        }
        if parts.next().is_some() {
            return None;
        }
        let channel = match suffix {
            "" => Channel::Stable,
            s if s.eq_ignore_ascii_case("beta") => Channel::Beta,
            _ => Channel::Dev,
        };
        Some(Version { nums, channel })
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let [a, b, c, d] = self.nums;
        write!(f, "{a}.{b}.{c}.{d}")?;
        if self.channel == Channel::Beta {
            write!(f, "-beta")?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Release {
    pub tag_name: String,
    #[serde(default)]
    pub html_url: String,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub assets: Vec<Asset>,
}

impl Release {
    pub fn version(&self) -> Option<Version> {
        Version::parse(&self.tag_name)
    }

    fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|a| a.name == name)
    }
}

pub fn select(releases: &[Release], current: Version) -> Option<&Release> {
    if current.channel == Channel::Dev {
        return None;
    }
    let mut best: Option<(&Release, Version)> = None;
    for release in releases {
        if release.draft || (current.channel == Channel::Stable && release.prerelease) {
            continue;
        }
        let Some(version) = release.version() else {
            continue;
        };
        if version.channel == Channel::Dev || version <= best.map_or(current, |(_, v)| v) {
            continue;
        }
        best = Some((release, version));
    }
    best.map(|(r, _)| r)
}

fn bare(tag: &str) -> &str {
    tag.strip_prefix(['v', 'V']).unwrap_or(tag)
}

pub fn setup_name(tag: &str) -> String {
    format!("OpenVR-TrackDoctor-Setup-{}.exe", bare(tag))
}

pub fn zip_name(tag: &str) -> String {
    format!("OpenVR-TrackDoctor-{}.zip", bare(tag))
}

pub fn parse_sidecar(text: &str, name: &str) -> Option<String> {
    let mut words = text.split_whitespace();
    let hash = words.next()?;
    let named = words.next()?.trim_start_matches('*');
    if named != name || hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(hash.to_ascii_lowercase())
}

fn ps_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

pub fn setup_script(
    pid: u32,
    setup: &Path,
    staging: &Path,
    install_dir: &Path,
    log: &Path,
) -> String {
    let setup = ps_quote(&setup.display().to_string());
    let args = ps_quote(&format!("/S /D={}", install_dir.display()));
    let staging = ps_quote(&staging.display().to_string());
    let log = ps_quote(&log.display().to_string());
    [
        "$ErrorActionPreference = 'Stop'".to_string(),
        "try {".to_string(),
        format!("    Wait-Process -Id {pid} -Timeout 300 -ErrorAction SilentlyContinue"),
        "    while ($true) {".to_string(),
        "        $busy = @(Get-Process -Name 'trackdoctor', 'trackdoctor-bg' -ErrorAction SilentlyContinue)".to_string(),
        "        if ($busy.Count -gt 0) { $busy | Wait-Process -ErrorAction SilentlyContinue; continue }".to_string(),
        format!("        $setup = Start-Process -FilePath {setup} -ArgumentList {args} -Wait -PassThru"),
        "        if ($setup.ExitCode -eq 5 -or $setup.ExitCode -eq 6) { Start-Sleep -Seconds 5; continue }".to_string(),
        "        if ($setup.ExitCode -ne 0) { throw \"setup exited with code $($setup.ExitCode)\" }".to_string(),
        "        break".to_string(),
        "    }".to_string(),
        "} catch {".to_string(),
        format!("    \"$(Get-Date -Format s) update failed\" | Add-Content -LiteralPath {log}"),
        format!("    $_ | Out-String | Add-Content -LiteralPath {log}"),
        "}".to_string(),
        format!("Remove-Item -LiteralPath {staging} -Recurse -Force -ErrorAction SilentlyContinue"),
        String::new(),
    ]
    .join("\r\n")
}

static NOTICE: Mutex<Option<Release>> = Mutex::new(None);

fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()?
        .parent()
        .map(Path::to_path_buf)
}

pub fn installed() -> bool {
    exe_dir().is_some_and(|d| d.join("Uninstall.exe").exists())
}

fn staging_dir() -> PathBuf {
    crate::names::data_dir().join("update")
}

fn skip_file() -> PathBuf {
    crate::names::data_dir().join("skipped-update.txt")
}

pub fn current() -> Option<Version> {
    Version::parse(crate::VERSION)
}

pub fn cleanup_old() {
    if let Some(dir) = exe_dir() {
        for exe in ["trackdoctor.exe.old", "trackdoctor-bg.exe.old"] {
            let _ = std::fs::remove_file(dir.join(exe));
        }
    }
}

#[cfg(windows)]
fn system_tool(exe: &str) -> Command {
    let path = match std::env::var_os("SystemRoot") {
        Some(root) => PathBuf::from(root).join("System32").join(exe),
        None => PathBuf::from(exe),
    };
    Command::new(path)
}

#[cfg(not(windows))]
fn system_tool(exe: &str) -> Command {
    Command::new(exe)
}

fn fetch_text(url: &str) -> anyhow::Result<String> {
    let out = system_tool("curl.exe")
        .args([
            "-fsSL",
            "--max-time",
            "30",
            "-H",
            "Accept: application/vnd.github+json",
            "-A",
        ])
        .arg(format!("TrackDoctor/{}", crate::VERSION))
        .arg(url)
        .output()?;
    if !out.status.success() {
        anyhow::bail!("curl exited with {} for {url}", out.status);
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn find(respect_skip: bool) -> anyhow::Result<Option<Release>> {
    let Some(current) = current() else {
        return Ok(None);
    };
    let url =
        std::env::var("TRACKDOCTOR_RELEASES_URL").unwrap_or_else(|_| RELEASES_URL.to_string());
    let releases: Vec<Release> = serde_json::from_str(&fetch_text(&url)?)?;
    let Some(release) = select(&releases, current).cloned() else {
        return Ok(None);
    };
    let skipped = std::fs::read_to_string(skip_file()).unwrap_or_default();
    if respect_skip && skipped.trim() == release.tag_name {
        return Ok(None);
    }
    let name = if installed() {
        setup_name(&release.tag_name)
    } else {
        zip_name(&release.tag_name)
    };
    let complete =
        release.asset(&name).is_some() && release.asset(&format!("{name}.sha256")).is_some();
    Ok(complete.then_some(release))
}

pub fn spawn_check() {
    if current().is_none_or(|v| v.channel == Channel::Dev) {
        return;
    }
    std::thread::spawn(|| {
        if let Ok(Some(release)) = find(true) {
            *NOTICE.lock().unwrap() = Some(release);
        }
    });
}

pub fn found() -> Option<Release> {
    NOTICE.lock().unwrap().clone()
}

pub fn skip(release: &Release) -> anyhow::Result<()> {
    std::fs::create_dir_all(crate::names::data_dir())?;
    std::fs::write(skip_file(), &release.tag_name)?;
    Ok(())
}

fn sha256(path: &Path) -> Option<String> {
    let out = system_tool("certutil.exe")
        .arg("-hashfile")
        .arg(path)
        .arg("SHA256")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .find(|t| t.len() == 64 && t.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_ascii_lowercase)
}

fn download(release: &Release) -> anyhow::Result<PathBuf> {
    let name = if installed() {
        setup_name(&release.tag_name)
    } else {
        zip_name(&release.tag_name)
    };
    let asset = release
        .asset(&name)
        .ok_or_else(|| anyhow::anyhow!("{} has no {name}", release.tag_name))?;
    let sidecar = release
        .asset(&format!("{name}.sha256"))
        .ok_or_else(|| anyhow::anyhow!("{} has no {name}.sha256", release.tag_name))?;
    let expected = parse_sidecar(&fetch_text(&sidecar.browser_download_url)?, &name)
        .ok_or_else(|| anyhow::anyhow!("{name}.sha256 does not hold a checksum for {name}"))?;

    let staging = staging_dir();
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;
    let path = staging.join(&name);
    let status = system_tool("curl.exe")
        .args(["-fL", "--progress-bar", "--max-time", "900", "-A"])
        .arg(format!("TrackDoctor/{}", crate::VERSION))
        .arg("-o")
        .arg(&path)
        .arg(&asset.browser_download_url)
        .status()?;
    if !status.success() {
        anyhow::bail!("the download failed (curl exited with {status})");
    }
    if sha256(&path).as_deref() != Some(expected.as_str()) {
        anyhow::bail!("{name} does not match its published checksum");
    }
    Ok(path)
}

fn swap_in(dir: &Path, fresh: &Path) -> anyhow::Result<()> {
    let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
    let result = (|| -> anyhow::Result<()> {
        for exe in ["trackdoctor.exe", "trackdoctor-bg.exe"] {
            let target = dir.join(exe);
            let old = dir.join(format!("{exe}.old"));
            let _ = std::fs::remove_file(&old);
            if target.exists() {
                std::fs::rename(&target, &old)?;
                moved.push((old, target.clone()));
            }
            std::fs::copy(fresh.join(exe), &target)?;
        }
        for doc in ["README.md", "LICENSE"] {
            if fresh.join(doc).exists() {
                std::fs::copy(fresh.join(doc), dir.join(doc))?;
            }
        }
        Ok(())
    })();
    if result.is_err() {
        for (old, target) in moved.into_iter().rev() {
            let _ = std::fs::remove_file(&target);
            let _ = std::fs::rename(&old, &target);
        }
    }
    result
}

pub fn install(release: &Release) -> anyhow::Result<String> {
    let dir = exe_dir().ok_or_else(|| anyhow::anyhow!("cannot find the TrackDoctor folder"))?;
    let path = download(release)?;
    let staging = staging_dir();
    let version = release
        .version()
        .map_or(release.tag_name.clone(), |v| v.to_string());
    if installed() {
        let script = staging.join("apply.ps1");
        let log = crate::names::data_dir().join("update.log");
        std::fs::write(
            &script,
            setup_script(std::process::id(), &path, &staging, &dir, &log),
        )?;
        let mut cmd = system_tool("WindowsPowerShell\\v1.0\\powershell.exe");
        cmd.args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-WindowStyle",
            "Hidden",
            "-File",
        ])
        .arg(&script);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        cmd.spawn()?;
        return Ok(if crate::app::recorder_running() {
            format!(
                "TrackDoctor {version} installs once SteamVR closes, because the recorder is running."
            )
        } else {
            format!("TrackDoctor {version} installs as soon as this window closes.")
        });
    }
    let extracted = staging.join("extracted");
    std::fs::create_dir_all(&extracted)?;
    let status = system_tool("tar.exe")
        .arg("-xf")
        .arg(&path)
        .arg("-C")
        .arg(&extracted)
        .status()?;
    if !status.success() {
        anyhow::bail!(
            "could not unpack {} (tar exited with {status})",
            path.display()
        );
    }
    swap_in(&dir, &extracted)?;
    let _ = std::fs::remove_dir_all(&staging);
    Ok(if crate::app::recorder_running() {
        format!(
            "Updated to TrackDoctor {version}. The recorder switches over the next time SteamVR starts."
        )
    } else {
        format!("Updated to TrackDoctor {version}.")
    })
}

pub fn ask(release: &Release, offer_skip: bool) -> Answer {
    let version = release
        .version()
        .map_or(release.tag_name.clone(), |v| v.to_string());
    println!("\nTrackDoctor {version} is available: {}", release.html_url);
    if offer_skip {
        print!("Update now? [Y]es, [n]o, [s]kip this version: ");
    } else {
        print!("Update now? [Y]es, [n]o: ");
    }
    use std::io::Write;
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return Answer::No;
    }
    match line.trim().to_ascii_lowercase().as_str() {
        "" | "y" | "yes" => Answer::Yes,
        "s" | "skip" if offer_skip => Answer::Skip,
        _ => Answer::No,
    }
}

pub fn command() -> anyhow::Result<()> {
    match current() {
        Some(v) if v.channel != Channel::Dev => {
            println!("TrackDoctor {v}. Checking for a newer release...");
            match find(false)? {
                Some(release) => offer(&release, false),
                None => println!("You have the latest version."),
            }
        }
        _ => println!(
            "This is a development build ({}), so it doesn't update itself.",
            crate::VERSION
        ),
    }
    crate::win::pause_if_own_console();
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
pub enum Answer {
    Yes,
    No,
    Skip,
}

pub fn offer(release: &Release, offer_skip: bool) {
    match ask(release, offer_skip) {
        Answer::Yes => match install(release) {
            Ok(msg) => println!("{msg}"),
            Err(e) => println!(
                "The update didn't finish: {e}. You're still on {}.",
                crate::VERSION
            ),
        },
        Answer::Skip => match skip(release) {
            Ok(()) => println!("Skipped. `trackdoctor update` still offers it."),
            Err(e) => println!("Could not save the skip: {e}"),
        },
        Answer::No => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    fn release(tag: &str, prerelease: bool, draft: bool) -> Release {
        Release {
            tag_name: tag.into(),
            html_url: String::new(),
            prerelease,
            draft,
            assets: Vec::new(),
        }
    }

    #[test]
    fn channel_comes_from_the_suffix() {
        assert_eq!(v("v2026.10.8.0").channel, Channel::Stable);
        assert_eq!(v("2026.10.8.0+abc").channel, Channel::Stable);
        assert_eq!(v("v2026.10.8.1-beta").channel, Channel::Beta);
        assert_eq!(v("2026.10.7.0-8F2D").channel, Channel::Dev);
        for bad in [
            "",
            "dev",
            "0.0.0",
            "v2026.10.8",
            "2026.10.8.x",
            "2026.10.8.0.1",
        ] {
            assert!(Version::parse(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn numbers_compare_first_then_stable_beats_beta() {
        assert!(v("2026.10.9.0") > v("2026.10.8.5"));
        assert!(v("2026.11.1.0") > v("2026.10.31.9"));
        assert!(v("2026.10.8.0") > v("2026.10.8.0-beta"));
        assert_eq!(v("v2026.10.8.0").to_string(), "2026.10.8.0");
        assert_eq!(v("v2026.10.8.0-beta").to_string(), "2026.10.8.0-beta");
    }

    #[test]
    fn stable_skips_betas_and_drafts_and_beta_takes_either() {
        let releases = vec![
            release("v2026.10.9.0-beta", true, false),
            release("v2026.10.10.0", false, true),
            release("v2026.10.8.1", false, false),
            release("v2026.10.7.0", false, false),
            release("not-a-version", false, false),
        ];
        assert_eq!(
            select(&releases, v("2026.10.7.0")).map(|r| r.tag_name.as_str()),
            Some("v2026.10.8.1")
        );
        assert_eq!(
            select(&releases, v("2026.10.8.0-beta")).map(|r| r.tag_name.as_str()),
            Some("v2026.10.9.0-beta")
        );
        assert!(select(&releases, v("2026.10.8.1")).is_none());
        assert!(select(&releases, v("2026.10.7.0-8F2D")).is_none());
    }

    #[test]
    fn asset_names_match_the_release_workflow() {
        assert_eq!(
            setup_name("v2026.10.7.0"),
            "OpenVR-TrackDoctor-Setup-2026.10.7.0.exe"
        );
        assert_eq!(
            zip_name("v2026.10.7.0"),
            "OpenVR-TrackDoctor-2026.10.7.0.zip"
        );
        assert_eq!(
            setup_name("v2026.10.8.0-beta"),
            "OpenVR-TrackDoctor-Setup-2026.10.8.0-beta.exe"
        );
    }

    #[test]
    fn sidecar_must_name_the_asset_and_hold_a_hash() {
        let hash = "fddbb4de15dd180e37c2c8c36880d9f6a666f5744195bd612937fb1af0107052";
        let name = "OpenVR-TrackDoctor-2026.10.7.0.zip";
        assert_eq!(
            parse_sidecar(&format!("{}  {name}\n", hash.to_uppercase()), name).as_deref(),
            Some(hash)
        );
        assert!(parse_sidecar(&format!("{hash}  other.zip\n"), name).is_none());
        assert!(parse_sidecar(&format!("abc  {name}\n"), name).is_none());
        assert!(parse_sidecar("", name).is_none());
    }

    #[test]
    fn setup_script_waits_for_both_exes_and_quotes_paths() {
        let script = setup_script(
            4242,
            Path::new(r"C:\Users\O'Neil\update\Setup.exe"),
            Path::new(r"C:\Users\O'Neil\update"),
            Path::new(r"C:\Users\O'Neil\AppData\Local\Programs\OpenVR-TrackDoctor"),
            Path::new(r"C:\Users\O'Neil\update.log"),
        );
        assert!(script.contains("Wait-Process -Id 4242 "));
        assert!(script.contains("Get-Process -Name 'trackdoctor', 'trackdoctor-bg'"));
        assert!(script.contains(r"-ArgumentList '/S /D=C:\Users\O''Neil\AppData\Local\Programs\OpenVR-TrackDoctor' -Wait"));
        assert!(script.contains("ExitCode -eq 5 -or $setup.ExitCode -eq 6"));
        assert!(!script.contains("O'N"));
    }
}

use crate::gui::config::Config;
use anyhow::{Context, anyhow, bail};
use std::ffi::CString;
use std::path::{Path, PathBuf};

pub const APP_KEY: &str = "whyknot.TrackDoctor";
pub const MANIFEST: &str = "manifest.vrmanifest";
pub const EXE: &str = "trackdoctor.exe";
pub const LAUNCH_FLAG: &str = "--steamvr";
pub const REGISTER_FLAG: &str = "--register-steamvr";
pub const ENABLE_FLAG: &str = "--enable-steamvr";
const OLD_EXE: &str = "trackdoctor-bg.exe";
pub const UNREGISTER_FLAG: &str = "--unregister-steamvr";
pub const NOT_AVAILABLE: i32 = 3;

pub fn manifest_json() -> String {
    format!(
        r#"{{
	"source" : "builtin",
	"applications": [{{
		"app_key": "{APP_KEY}",
		"launch_type": "binary",
		"binary_path_windows": "{EXE}",
		"arguments": "{LAUNCH_FLAG}",
		"is_dashboard_overlay": true,

		"strings": {{
			"en_us": {{
				"name": "TrackDoctor",
				"description": "Shows why trackers glitch while you are in VR"
			}}
		}}
	}}]
}}
"#
    )
}

pub fn install_dir() -> anyhow::Result<PathBuf> {
    let exe = std::env::current_exe().context("locate trackdoctor.exe")?;
    exe.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow!("trackdoctor.exe has no parent folder"))
}

struct Apps {
    table: &'static openvr_sys::VR_IVRApplications_FnTable,
    _ctx: openvr::Context,
}

impl Apps {
    fn open() -> anyhow::Result<Self> {
        let ctx = unsafe { openvr::init(openvr::ApplicationType::Utility) }
            .map_err(|e| anyhow!("{UNAVAILABLE} ({e:?})"))?;
        let mut name = b"FnTable:".to_vec();
        name.extend_from_slice(openvr_sys::IVRApplications_Version);
        let mut err = openvr_sys::EVRInitError_VRInitError_None;
        let ptr = unsafe { openvr_sys::VR_GetGenericInterface(name.as_ptr().cast(), &mut err) };
        if err != openvr_sys::EVRInitError_VRInitError_None || ptr == 0 {
            bail!("SteamVR's application interface is unavailable ({err})");
        }
        let table = unsafe { &*(ptr as *const openvr_sys::VR_IVRApplications_FnTable) };
        Ok(Self { table, _ctx: ctx })
    }

    fn installed(&self, key: &CString) -> bool {
        self.table
            .IsApplicationInstalled
            .is_some_and(|f| unsafe { f(key.as_ptr().cast_mut()) })
    }

    fn auto_launch(&self, key: &CString) -> bool {
        self.table
            .GetApplicationAutoLaunch
            .is_some_and(|f| unsafe { f(key.as_ptr().cast_mut()) })
    }

    fn check(err: openvr_sys::EVRApplicationError, what: &str) -> anyhow::Result<()> {
        if err == openvr_sys::EVRApplicationError_VRApplicationError_None {
            Ok(())
        } else {
            Err(anyhow!("SteamVR refused to {what} (error {err})"))
        }
    }

    fn set_auto_launch(&self, key: &CString, on: bool) -> anyhow::Result<()> {
        let f = self
            .table
            .SetApplicationAutoLaunch
            .ok_or_else(|| anyhow!("SetApplicationAutoLaunch missing"))?;
        Self::check(
            unsafe { f(key.as_ptr().cast_mut(), on) },
            "change auto-launch",
        )
    }

    fn add(&self, manifest: &CString) -> anyhow::Result<()> {
        let f = self
            .table
            .AddApplicationManifest
            .ok_or_else(|| anyhow!("AddApplicationManifest missing"))?;
        Self::check(
            unsafe { f(manifest.as_ptr().cast_mut(), false) },
            "add the manifest",
        )
    }

    fn remove(&self, manifest: &CString) -> anyhow::Result<()> {
        let f = self
            .table
            .RemoveApplicationManifest
            .ok_or_else(|| anyhow!("RemoveApplicationManifest missing"))?;
        Self::check(
            unsafe { f(manifest.as_ptr().cast_mut()) },
            "remove the manifest",
        )
    }
}

fn cstr(s: &str) -> anyhow::Result<CString> {
    CString::new(s).map_err(|_| anyhow!("path contains a NUL byte"))
}

pub fn enable() -> anyhow::Result<String> {
    let dir = install_dir()?;
    let manifest = dir.join(MANIFEST);
    write_manifest(&manifest)?;
    let apps = Apps::open()?;
    let key = cstr(APP_KEY)?;
    apps.add(&cstr(&manifest.to_string_lossy())?)?;
    apps.set_auto_launch(&key, true)?;
    if !apps.auto_launch(&key) {
        bail!("SteamVR accepted the manifest but did not keep auto-launch on");
    }
    Ok("TrackDoctor opens with SteamVR.".into())
}

fn write_manifest(manifest: &Path) -> anyhow::Result<()> {
    if std::fs::read_to_string(manifest).ok().as_deref() != Some(manifest_json().as_str()) {
        std::fs::write(manifest, manifest_json())
            .with_context(|| format!("write {}", manifest.display()))?;
    }
    Ok(())
}

pub fn refresh_manifest() {
    if let Ok(dir) = install_dir() {
        let manifest = dir.join(MANIFEST);
        if manifest.is_file() {
            note_old_manifest(&manifest);
            let _ = write_manifest(&manifest);
        }
    }
}

fn note_old_manifest(manifest: &Path) {
    if std::fs::read_to_string(manifest).is_ok_and(|t| t.contains(OLD_EXE)) {
        let mut config = Config::load();
        if !config.old_manifest {
            config.old_manifest = true;
            let _ = config.save();
        }
    }
}

pub fn sync() -> anyhow::Result<String> {
    if !Config::load().open_with_steamvr {
        return Ok(String::new());
    }
    let dir = install_dir()?;
    let manifest = dir.join(MANIFEST);
    note_old_manifest(&manifest);
    write_manifest(&manifest)?;
    let apps = Apps::open()?;
    let key = cstr(APP_KEY)?;
    let known = apps.installed(&key);
    let mut config = Config::load();
    if config.old_manifest {
        config.old_manifest = false;
        config.open_with_steamvr = known;
        config.save()?;
    }
    if !config.open_with_steamvr {
        return Ok(String::new());
    }
    apps.add(&cstr(&manifest.to_string_lossy())?)?;
    if !known {
        apps.set_auto_launch(&key, true)?;
    }
    Ok("TrackDoctor opens with SteamVR.".into())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Setup {
    Sync,
    Enable,
    Disable,
}

impl Setup {
    pub fn from_args(args: &[String]) -> Option<Self> {
        [
            (REGISTER_FLAG, Self::Sync),
            (ENABLE_FLAG, Self::Enable),
            (UNREGISTER_FLAG, Self::Disable),
        ]
        .into_iter()
        .find(|(flag, _)| args.iter().any(|a| a == flag))
        .map(|(_, s)| s)
    }

    pub fn flag(self) -> &'static str {
        match self {
            Self::Sync => REGISTER_FLAG,
            Self::Enable => ENABLE_FLAG,
            Self::Disable => UNREGISTER_FLAG,
        }
    }
}

pub fn setup_command(setup: Setup) -> i32 {
    let result = match setup {
        Setup::Sync => sync(),
        Setup::Enable => enable(),
        Setup::Disable => disable(),
    };
    match result {
        Ok(_) => 0,
        Err(e) if e.to_string().starts_with(UNAVAILABLE) => NOT_AVAILABLE,
        Err(_) => 1,
    }
}

const UNAVAILABLE: &str = "SteamVR is not available";

pub fn disable() -> anyhow::Result<String> {
    let dir = install_dir()?;
    let manifest = dir.join(MANIFEST);
    let apps = Apps::open()?;
    let key = cstr(APP_KEY)?;
    if apps.installed(&key) {
        let _ = apps.set_auto_launch(&key, false);
        if manifest.is_file() {
            apps.remove(&cstr(&manifest.to_string_lossy())?)?;
        }
    }
    Ok("TrackDoctor no longer opens with SteamVR.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_flags_round_trip() {
        for s in [Setup::Sync, Setup::Enable, Setup::Disable] {
            assert_eq!(Setup::from_args(&[s.flag().to_string()]), Some(s));
        }
        assert_eq!(Setup::from_args(&[LAUNCH_FLAG.to_string()]), None);
    }

    #[test]
    fn manifest_is_valid_json_with_overlay_flag() {
        let v: serde_json::Value = serde_json::from_str(&manifest_json()).unwrap();
        let app = &v["applications"][0];
        assert_eq!(app["app_key"], APP_KEY);
        assert_eq!(app["binary_path_windows"], EXE);
        assert_eq!(app["arguments"], LAUNCH_FLAG);
        assert_eq!(app["is_dashboard_overlay"], true);
        assert_eq!(app["launch_type"], "binary");
    }
}

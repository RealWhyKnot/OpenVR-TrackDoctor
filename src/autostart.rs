use anyhow::{Context, anyhow, bail};
use std::ffi::CString;
use std::path::{Path, PathBuf};

pub const APP_KEY: &str = "whyknot.TrackDoctor";
pub const MANIFEST: &str = "manifest.vrmanifest";
pub const BG_EXE: &str = "trackdoctor-bg.exe";

pub fn manifest_json() -> String {
    format!(
        r#"{{
	"source" : "builtin",
	"applications": [{{
		"app_key": "{APP_KEY}",
		"launch_type": "binary",
		"binary_path_windows": "{BG_EXE}",
		"is_dashboard_overlay": true,

		"strings": {{
			"en_us": {{
				"name": "TrackDoctor",
				"description": "Records why trackers glitch while you are in VR"
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
            .map_err(|e| anyhow!("SteamVR is not available ({e:?})"))?;
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
    if !dir.join(BG_EXE).is_file() {
        bail!(
            "{BG_EXE} is missing next to trackdoctor.exe in {}",
            dir.display()
        );
    }
    let manifest = dir.join(MANIFEST);
    if std::fs::read_to_string(&manifest).ok().as_deref() != Some(manifest_json().as_str()) {
        std::fs::write(&manifest, manifest_json())
            .with_context(|| format!("write {}", manifest.display()))?;
    }
    let apps = Apps::open()?;
    let key = cstr(APP_KEY)?;
    apps.add(&cstr(&manifest.to_string_lossy())?)?;
    apps.set_auto_launch(&key, true)?;
    if !apps.auto_launch(&key) {
        bail!("SteamVR accepted the manifest but did not keep auto-launch on");
    }
    Ok(format!(
        "TrackDoctor will start with SteamVR and record in the background.\nManifest: {}",
        manifest.display()
    ))
}

pub fn disable() -> anyhow::Result<String> {
    let dir = install_dir()?;
    let manifest = dir.join(MANIFEST);
    let apps = Apps::open()?;
    let key = cstr(APP_KEY)?;
    if apps.installed(&key) {
        let _ = apps.set_auto_launch(&key, false);
    }
    if manifest.is_file() {
        apps.remove(&cstr(&manifest.to_string_lossy())?)?;
    }
    Ok("TrackDoctor no longer starts with SteamVR.".into())
}

pub fn status() -> anyhow::Result<String> {
    let apps = Apps::open()?;
    let key = cstr(APP_KEY)?;
    Ok(if !apps.installed(&key) {
        "Not registered with SteamVR. Run: trackdoctor autostart on".into()
    } else if apps.auto_launch(&key) {
        "Registered with SteamVR; starts automatically with VR.".into()
    } else {
        "Registered with SteamVR, but auto-start is off. Run: trackdoctor autostart on".into()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_is_valid_json_with_overlay_flag() {
        let v: serde_json::Value = serde_json::from_str(&manifest_json()).unwrap();
        let app = &v["applications"][0];
        assert_eq!(app["app_key"], APP_KEY);
        assert_eq!(app["binary_path_windows"], BG_EXE);
        assert_eq!(app["is_dashboard_overlay"], true);
        assert_eq!(app["launch_type"], "binary");
    }
}

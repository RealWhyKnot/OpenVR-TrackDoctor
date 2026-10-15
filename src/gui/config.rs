use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub open_with_steamvr: bool,
    pub save_poses: bool,
    pub keep_days: u32,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub old_manifest: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            open_with_steamvr: true,
            save_poses: false,
            keep_days: 0,
            old_manifest: false,
        }
    }
}

fn path() -> PathBuf {
    crate::names::data_dir().join("settings.json")
}

impl Config {
    pub fn load() -> Self {
        Self::load_from(&path())
    }

    pub fn load_from(path: &std::path::Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        self.save_to(&path())
    }

    pub fn save_to(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_or_partial_settings_fall_back_to_defaults() {
        let dir = std::env::temp_dir().join(format!("trackdoctor-config-{}", std::process::id()));
        let file = dir.join("settings.json");
        assert_eq!(Config::load_from(&file), Config::default());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&file, "{\"save_poses\": true}").unwrap();
        let c = Config::load_from(&file);
        assert!(c.open_with_steamvr && c.save_poses);
        let off = Config {
            open_with_steamvr: false,
            save_poses: false,
            keep_days: 30,
            old_manifest: false,
        };
        off.save_to(&file).unwrap();
        assert_eq!(Config::load_from(&file), off);
        assert!(
            !std::fs::read_to_string(&file)
                .unwrap()
                .contains("old_manifest")
        );
        let old = Config {
            old_manifest: true,
            ..off.clone()
        };
        old.save_to(&file).unwrap();
        assert_eq!(Config::load_from(&file), old);
        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_dir(&dir);
    }
}

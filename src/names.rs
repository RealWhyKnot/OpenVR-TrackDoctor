use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub fn data_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("trackdoctor")
}

#[derive(Default)]
pub struct Names {
    path: PathBuf,
    map: BTreeMap<String, String>,
    stamp: Option<SystemTime>,
}

pub fn parse(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .filter(|(k, v)| !k.is_empty() && !v.is_empty())
        .collect()
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

impl Names {
    pub fn load_default() -> Self {
        Self::load(data_dir().join("names.txt"))
    }

    pub fn load(path: PathBuf) -> Self {
        let mut n = Self {
            path,
            ..Default::default()
        };
        n.reload();
        n
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn get(&self, serial: &str) -> Option<&str> {
        self.map.get(serial).map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    fn reload(&mut self) {
        self.stamp = modified(&self.path);
        self.map = std::fs::read_to_string(&self.path)
            .map(|t| parse(&t))
            .unwrap_or_default();
    }

    pub fn refresh(&mut self) -> bool {
        if modified(&self.path) == self.stamp {
            return false;
        }
        self.reload();
        true
    }

    pub fn add_missing(&self, devices: &[(String, String)]) -> std::io::Result<()> {
        let mut text = std::fs::read_to_string(&self.path).unwrap_or_else(|_| {
            "# Give each device a name you will recognise, for example:\n\
             # LHR-617D30D7 = left foot\n\
             # Save this file and TrackDoctor picks the names up right away.\n\n"
                .to_string()
        });
        for (serial, hint) in devices {
            let listed = text
                .lines()
                .filter(|l| !l.trim_start().starts_with('#'))
                .any(|l| l.split('=').next().map(str::trim) == Some(serial.as_str()));
            if !listed {
                if !text.ends_with('\n') && !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&format!("# {hint}\n{serial} =\n"));
            }
        }
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&self.path, text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_skips_comments_and_blank_names() {
        let m = parse(
            "# LHR-AAAA = commented\nLHR-617D30D7 = left foot \n LHR-905BD201=\nnot a line\n",
        );
        assert_eq!(m.len(), 1);
        assert_eq!(m["LHR-617D30D7"], "left foot");
    }

    #[test]
    fn add_missing_keeps_user_names_and_appends_new_serials() {
        let path =
            std::env::temp_dir().join(format!("trackdoctor-names-{}.txt", std::process::id()));
        std::fs::write(&path, "LHR-1 = waist\n").unwrap();
        let mut names = Names::load(path.clone());
        assert_eq!(names.get("LHR-1"), Some("waist"));
        let devices = vec![
            ("LHR-1".to_string(), "tracker".to_string()),
            ("LHR-2".to_string(), "left controller".to_string()),
        ];
        names.add_missing(&devices).unwrap();
        names.add_missing(&devices).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text, "LHR-1 = waist\n# left controller\nLHR-2 =\n");
        std::fs::write(&path, "LHR-1 = waist\nLHR-2 = right hand\n").unwrap();
        let _ = names.refresh();
        assert_eq!(names.get("LHR-2"), Some("right hand"));
        let _ = std::fs::remove_file(&path);
    }
}

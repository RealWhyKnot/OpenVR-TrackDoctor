use crate::track;
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub const PACKED_TEXT: [&str; 6] = [
    "events.jsonl",
    "verdicts.jsonl",
    "report.txt",
    "poses.csv",
    "packets.jsonl",
    "vrserver.txt",
];

pub const FILES: [&str; 20] = [
    "events.jsonl",
    "events.jsonl.gz",
    "verdicts.jsonl",
    "verdicts.jsonl.gz",
    "report.txt",
    "report.txt.gz",
    "poses.csv",
    "poses.csv.gz",
    track::RAW_FILE,
    track::PACKED_FILE,
    "devices.json",
    "usb.json",
    "bases.json",
    "flaps.json",
    "room.json",
    "packets.jsonl",
    "packets.jsonl.gz",
    "packets.json",
    "vrserver.txt",
    "vrserver.txt.gz",
];

fn gz_name(name: &str) -> String {
    if name == track::RAW_FILE {
        track::PACKED_FILE.to_string()
    } else {
        format!("{name}.gz")
    }
}

fn gunzip(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    GzDecoder::new(bytes).read_to_end(&mut out).ok()?;
    Some(out)
}

fn gzip(bytes: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut e = GzEncoder::new(Vec::new(), Compression::best());
    e.write_all(bytes)?;
    e.finish()
}

pub fn has(dir: &Path, name: &str) -> bool {
    dir.join(name).is_file() || dir.join(gz_name(name)).is_file()
}

pub fn read_bytes(dir: &Path, name: &str) -> Option<Vec<u8>> {
    match std::fs::read(dir.join(name)) {
        Ok(b) => Some(b),
        Err(_) => gunzip(&std::fs::read(dir.join(gz_name(name))).ok()?),
    }
}

pub fn read_text(dir: &Path, name: &str) -> Option<String> {
    String::from_utf8(read_bytes(dir, name)?).ok()
}

pub fn read_track(dir: &Path) -> Option<track::Track> {
    if let Ok(raw) = std::fs::read(dir.join(track::RAW_FILE)) {
        return track::read_raw(&raw);
    }
    track::unpack(&gunzip(&std::fs::read(dir.join(track::PACKED_FILE)).ok()?)?)
}

fn replace(dir: &Path, name: &str, packed: &[u8]) -> std::io::Result<()> {
    let target = dir.join(gz_name(name));
    let tmp = dir.join(format!("{}.tmp", gz_name(name)));
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(packed)?;
    f.sync_all()?;
    drop(f);
    if std::fs::read(&tmp)? != packed {
        let _ = std::fs::remove_file(&tmp);
        return Err(std::io::Error::other(format!(
            "{} didn't read back the same",
            gz_name(name)
        )));
    }
    std::fs::rename(&tmp, &target)?;
    std::fs::remove_file(dir.join(name))
}

pub fn needs_pack(dir: &Path) -> bool {
    PACKED_TEXT
        .iter()
        .chain([&track::RAW_FILE])
        .any(|n| dir.join(n).is_file())
}

pub fn pack(dir: &Path) -> std::io::Result<()> {
    for name in PACKED_TEXT {
        let Ok(plain) = std::fs::read(dir.join(name)) else {
            continue;
        };
        let packed = gzip(&plain)?;
        if gunzip(&packed).as_deref() != Some(plain.as_slice()) {
            return Err(std::io::Error::other(format!(
                "{name} didn't survive packing"
            )));
        }
        replace(dir, name, &packed)?;
    }
    if let Ok(raw) = std::fs::read(dir.join(track::RAW_FILE))
        && let Some(t) = track::read_raw(&raw)
    {
        let packed = gzip(&track::pack(&t))?;
        let back = gunzip(&packed).and_then(|b| track::unpack(&b));
        if back.as_ref() != Some(&t) {
            return Err(std::io::Error::other(
                "the movement track didn't survive packing",
            ));
        }
        replace(dir, track::RAW_FILE, &packed)?;
    }
    Ok(())
}

pub fn size(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|r| {
            r.filter_map(Result::ok)
                .filter_map(|e| e.metadata().ok())
                .filter(|m| m.is_file())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0)
}

pub fn delete(dir: &Path) -> std::io::Result<()> {
    for name in FILES {
        for p in [dir.join(name), dir.join(format!("{}.tmp", gz_name(name)))] {
            match std::fs::remove_file(&p) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
    }
    std::fs::remove_dir(dir)
}

pub fn fmt_size(bytes: u64) -> String {
    let kb = bytes as f64 / 1024.0;
    if kb < 1.0 {
        format!("{bytes} bytes")
    } else if kb < 1024.0 {
        format!("{kb:.0} KB")
    } else {
        format!("{:.1} MB", kb / 1024.0)
    }
}

pub fn session_dirs(root: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(root)
        .map(|r| {
            r.filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| {
                    p.is_dir()
                        && p.file_name()
                            .and_then(|n| n.to_str())
                            .is_some_and(|n| n.starts_with("session-"))
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("trackdoctor-store-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn packing_keeps_every_byte_and_reads_the_same() {
        let dir = scratch("pack");
        let events = "{\"t_ms\":1}\n{\"t_ms\":2}\n".repeat(500);
        std::fs::write(dir.join("events.jsonl"), &events).unwrap();
        std::fs::write(dir.join("devices.json"), "[]").unwrap();
        let mut w = track::TrackWriter::create(&dir.join(track::RAW_FILE), 10).unwrap();
        for i in 0..200u32 {
            let s = track::Sample::new(
                0,
                [i as f32 * 0.01, 1.0, 0.0],
                [1.0, 0.0, 0.0, 0.0],
                crate::event::TrackState::RunningOk,
                true,
            );
            w.frame(
                "LHR-A",
                crate::event::DeviceClass::GenericTracker,
                10 + i as u64 * 11,
                s,
            );
        }
        w.finish();
        drop(w);
        let before = read_track(&dir).unwrap();
        let plain_size = size(&dir);
        assert!(needs_pack(&dir));
        pack(&dir).unwrap();
        assert!(!needs_pack(&dir));
        assert!(!dir.join("events.jsonl").exists());
        assert!(dir.join("events.jsonl.gz").is_file());
        assert!(dir.join("devices.json").is_file());
        assert_eq!(read_text(&dir, "events.jsonl").unwrap(), events);
        assert_eq!(read_track(&dir).unwrap(), before);
        assert!(
            size(&dir) < plain_size / 5,
            "{} vs {plain_size}",
            size(&dir)
        );
        assert!(has(&dir, "events.jsonl"));
        pack(&dir).unwrap();
        delete(&dir).unwrap();
        assert!(!dir.exists());
    }

    #[test]
    fn delete_leaves_a_folder_holding_files_it_did_not_write() {
        let dir = scratch("keep");
        std::fs::write(dir.join("events.jsonl"), "x").unwrap();
        std::fs::write(dir.join("my notes.txt"), "mine").unwrap();
        assert!(delete(&dir).is_err());
        assert!(dir.join("my notes.txt").is_file());
        assert!(!dir.join("events.jsonl").exists());
        std::fs::remove_file(dir.join("my notes.txt")).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }
}

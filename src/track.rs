use crate::event::{DeviceClass, TrackState};
use std::collections::HashMap;
use std::io::{BufWriter, Write};
use std::path::Path;

pub const RAW_FILE: &str = "track.bin";
pub const PACKED_FILE: &str = "track.gz";
const RAW_MAGIC: &[u8; 4] = b"TDT1";
const PACKED_MAGIC: &[u8; 4] = b"TDC1";
const ROW_DEVICE: u8 = 0xFF;
const ROW_SAMPLE: u8 = 0x01;
pub const POS_TOL_M: f32 = 0.005;
pub const ROT_TOL_DEG: f32 = 2.0;
pub const MAX_GAP_MS: u32 = 2_000;
const HOLD_GAP_MS: u32 = 500;
const SILENT_MS: u32 = MAX_GAP_MS + HOLD_GAP_MS;
const MM: f32 = 1000.0;
const Q_SCALE: f32 = 1024.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    pub t: u32,
    pub pos: [f32; 3],
    pub q: [f32; 4],
    pub flags: u8,
}

impl Sample {
    pub fn new(t: u32, pos: [f32; 3], q_wxyz: [f32; 4], state: TrackState, valid: bool) -> Self {
        let [w, x, y, z] = q_wxyz;
        let q = if w < 0.0 {
            [-x, -y, -z, -w]
        } else {
            [x, y, z, w]
        };
        Self {
            t,
            pos,
            q,
            flags: flags(state, valid),
        }
    }

    pub fn valid(&self) -> bool {
        self.flags & 1 != 0
    }

    pub fn state(&self) -> TrackState {
        match self.flags >> 1 {
            0 => TrackState::Uninitialized,
            1 => TrackState::CalibratingInProgress,
            2 => TrackState::CalibratingOutOfRange,
            3 => TrackState::RunningOk,
            4 => TrackState::RunningOutOfRange,
            5 => TrackState::FallbackRotationOnly,
            _ => TrackState::Other(-1),
        }
    }

    pub fn tracking(&self) -> bool {
        self.valid() && self.state() == TrackState::RunningOk
    }

    fn quantized(&self) -> [i32; 6] {
        [
            (self.pos[0] * MM).round() as i32,
            (self.pos[1] * MM).round() as i32,
            (self.pos[2] * MM).round() as i32,
            (self.q[0] * Q_SCALE).round() as i32,
            (self.q[1] * Q_SCALE).round() as i32,
            (self.q[2] * Q_SCALE).round() as i32,
        ]
    }

    fn from_quantized(t: u32, flags: u8, v: [i32; 6]) -> Self {
        let (x, y, z) = (
            v[3] as f32 / Q_SCALE,
            v[4] as f32 / Q_SCALE,
            v[5] as f32 / Q_SCALE,
        );
        let w = (1.0 - x * x - y * y - z * z).max(0.0).sqrt();
        Self {
            t,
            pos: [v[0] as f32 / MM, v[1] as f32 / MM, v[2] as f32 / MM],
            q: [x, y, z, w],
            flags,
        }
    }
}

fn flags(state: TrackState, valid: bool) -> u8 {
    let code = match state {
        TrackState::Uninitialized => 0,
        TrackState::CalibratingInProgress => 1,
        TrackState::CalibratingOutOfRange => 2,
        TrackState::RunningOk => 3,
        TrackState::RunningOutOfRange => 4,
        TrackState::FallbackRotationOnly => 5,
        TrackState::Other(_) => 6,
    };
    (code << 1) | valid as u8
}

fn lerp3(a: [f32; 3], b: [f32; 3], f: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * f,
        a[1] + (b[1] - a[1]) * f,
        a[2] + (b[2] - a[2]) * f,
    ]
}

fn qdot(a: [f32; 4], b: [f32; 4]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3]
}

pub fn nlerp(a: [f32; 4], b: [f32; 4], f: f32) -> [f32; 4] {
    let s = if qdot(a, b) < 0.0 { -1.0 } else { 1.0 };
    let q = [
        a[0] + (b[0] * s - a[0]) * f,
        a[1] + (b[1] * s - a[1]) * f,
        a[2] + (b[2] * s - a[2]) * f,
        a[3] + (b[3] * s - a[3]) * f,
    ];
    let n = qdot(q, q).sqrt().max(1e-6);
    [q[0] / n, q[1] / n, q[2] / n, q[3] / n]
}

pub fn angle_deg(a: [f32; 4], b: [f32; 4]) -> f32 {
    2.0 * qdot(a, b).abs().min(1.0).acos().to_degrees()
}

fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    let d = [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
}

pub fn between(a: &Sample, b: &Sample, t: u32) -> Sample {
    if b.t <= a.t || a.flags != b.flags || b.t - a.t > SILENT_MS {
        return Sample { t, ..*a };
    }
    let f = (t.saturating_sub(a.t)) as f32 / (b.t - a.t) as f32;
    let f = f.clamp(0.0, 1.0);
    Sample {
        t,
        pos: lerp3(a.pos, b.pos, f),
        q: nlerp(a.q, b.q, f),
        flags: a.flags,
    }
}

#[derive(Default)]
pub struct Thinner {
    last: Option<Sample>,
    pending: Vec<Sample>,
}

impl Thinner {
    fn fits(&self, last: &Sample, s: &Sample) -> bool {
        s.t.saturating_sub(last.t) <= MAX_GAP_MS
            && self.pending.iter().all(|m| {
                let guess = between(last, s, m.t);
                dist(guess.pos, m.pos) <= POS_TOL_M && angle_deg(guess.q, m.q) <= ROT_TOL_DEG
            })
    }

    fn keep(&mut self, s: Sample, out: &mut Vec<Sample>) {
        out.push(s);
        self.last = Some(s);
    }

    pub fn push(&mut self, s: Sample, out: &mut Vec<Sample>) {
        let Some(last) = self.last else {
            self.keep(s, out);
            return;
        };
        let quiet = self.pending.last().map_or(last.t, |p| p.t);
        if s.flags != last.flags || s.t.saturating_sub(quiet) > HOLD_GAP_MS {
            self.flush(out);
            self.keep(s, out);
            return;
        }
        if self.fits(&last, &s) {
            self.pending.push(s);
            return;
        }
        match self.pending.pop() {
            Some(p) => {
                self.keep(p, out);
                self.pending.clear();
                self.pending.push(s);
            }
            None => self.keep(s, out),
        }
    }

    pub fn flush(&mut self, out: &mut Vec<Sample>) {
        if let Some(s) = self.pending.pop() {
            self.keep(s, out);
        }
        self.pending.clear();
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TrackDevice {
    pub serial: String,
    pub class: DeviceClass,
    pub samples: Vec<Sample>,
}

impl TrackDevice {
    pub fn at(&self, t: u32) -> Option<Sample> {
        let i = self.samples.partition_point(|s| s.t <= t);
        if i == 0 {
            return None;
        }
        let a = &self.samples[i - 1];
        match self.samples.get(i) {
            Some(b) if b.t - a.t <= SILENT_MS => Some(between(a, b, t)),
            _ if t - a.t <= SILENT_MS => Some(Sample { t, ..*a }),
            _ => None,
        }
    }

    pub fn path(&self, from: u32, to: u32) -> Vec<[f32; 3]> {
        let start = self.samples.partition_point(|s| s.t <= from);
        let end = self.samples.partition_point(|s| s.t < to).max(start);
        let mut kept: Vec<Sample> = self.at(from).into_iter().collect();
        kept.extend_from_slice(&self.samples[start..end]);
        kept.extend(self.at(to));
        let mut pts: Vec<[f32; 3]> = Vec::new();
        let mut last: Option<u32> = None;
        for s in kept {
            if !s.tracking() || last.is_some_and(|l| s.t - l > SILENT_MS) {
                pts.clear();
            }
            if s.tracking() {
                pts.push(s.pos);
            }
            last = Some(s.t);
        }
        pts
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Track {
    pub t0: u64,
    pub devices: Vec<TrackDevice>,
}

impl Track {
    pub fn end(&self) -> u32 {
        self.devices
            .iter()
            .filter_map(|d| d.samples.last().map(|s| s.t))
            .max()
            .unwrap_or(0)
    }

    pub fn device(&self, serial: &str) -> Option<&TrackDevice> {
        self.devices.iter().find(|d| d.serial == serial)
    }
}

fn class_code(c: DeviceClass) -> u8 {
    match c {
        DeviceClass::Hmd => 0,
        DeviceClass::Controller => 1,
        DeviceClass::GenericTracker => 2,
        DeviceClass::TrackingReference => 3,
        DeviceClass::Other => 4,
    }
}

fn class_from(c: u8) -> DeviceClass {
    match c {
        0 => DeviceClass::Hmd,
        1 => DeviceClass::Controller,
        2 => DeviceClass::GenericTracker,
        3 => DeviceClass::TrackingReference,
        _ => DeviceClass::Other,
    }
}

pub struct TrackWriter {
    file: BufWriter<std::fs::File>,
    t0: u64,
    ids: HashMap<String, u8>,
    thin: Vec<Thinner>,
    last: Vec<u32>,
    out: Vec<Sample>,
    bytes: u64,
}

impl TrackWriter {
    pub const HEADER: u64 = 12;

    pub fn create(path: &Path, t0: u64) -> std::io::Result<Self> {
        let mut file = BufWriter::new(std::fs::File::create(path)?);
        file.write_all(RAW_MAGIC)?;
        file.write_all(&t0.to_le_bytes())?;
        Ok(Self {
            file,
            t0,
            ids: HashMap::new(),
            thin: Vec::new(),
            last: Vec::new(),
            out: Vec::new(),
            bytes: Self::HEADER,
        })
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    pub fn frame(&mut self, serial: &str, class: DeviceClass, t_ms: u64, s: Sample) {
        let id = match self.ids.get(serial) {
            Some(id) => *id,
            None => {
                if self.ids.len() >= ROW_DEVICE as usize {
                    return;
                }
                let id = self.ids.len() as u8;
                self.ids.insert(serial.to_string(), id);
                self.thin.push(Thinner::default());
                self.last.push(0);
                let name = serial.as_bytes();
                let len = name.len().min(255);
                let mut row = vec![ROW_DEVICE, id, class_code(class), len as u8];
                row.extend_from_slice(&name[..len]);
                self.write(&row);
                id
            }
        };
        let t =
            (t_ms.saturating_sub(self.t0).min(u32::MAX as u64) as u32).max(self.last[id as usize]);
        self.last[id as usize] = t;
        self.thin[id as usize].push(Sample { t, ..s }, &mut self.out);
        self.drain(id);
    }

    fn drain(&mut self, id: u8) {
        let out = std::mem::take(&mut self.out);
        for s in &out {
            let mut row = Vec::with_capacity(22);
            row.push(ROW_SAMPLE);
            row.push(id);
            row.extend_from_slice(&s.t.to_le_bytes());
            row.push(s.flags);
            for v in s.quantized() {
                row.extend_from_slice(&v.to_le_bytes());
            }
            self.write(&row);
        }
        self.out = out;
        self.out.clear();
    }

    fn write(&mut self, row: &[u8]) {
        if self.file.write_all(row).is_ok() {
            self.bytes += row.len() as u64;
        }
    }

    pub fn flush(&mut self) {
        let _ = self.file.flush();
    }

    pub fn finish(&mut self) {
        for id in 0..self.thin.len() {
            let mut out = Vec::new();
            self.thin[id].flush(&mut out);
            self.out = out;
            self.drain(id as u8);
        }
        self.flush();
    }
}

pub fn read_raw(bytes: &[u8]) -> Option<Track> {
    let rest = bytes.strip_prefix(RAW_MAGIC)?;
    let t0 = u64::from_le_bytes(rest.get(..8)?.try_into().ok()?);
    let mut p = &rest[8..];
    let mut track = Track {
        t0,
        devices: Vec::new(),
    };
    let mut by_id: HashMap<u8, usize> = HashMap::new();
    while let Some((&tag, after)) = p.split_first() {
        match tag {
            ROW_DEVICE => {
                let Some(&[id, class, len]) = after.get(..3) else {
                    break;
                };
                let Some(name) = after.get(3..3 + len as usize) else {
                    break;
                };
                by_id.insert(id, track.devices.len());
                track.devices.push(TrackDevice {
                    serial: String::from_utf8_lossy(name).into_owned(),
                    class: class_from(class),
                    samples: Vec::new(),
                });
                p = &after[3 + len as usize..];
            }
            ROW_SAMPLE => {
                let Some(row) = after.get(..30) else {
                    break;
                };
                let id = row[0];
                let t = u32::from_le_bytes(row[1..5].try_into().ok()?);
                let flags = row[5];
                let mut v = [0i32; 6];
                for (i, x) in v.iter_mut().enumerate() {
                    *x = i32::from_le_bytes(row[6 + i * 4..10 + i * 4].try_into().ok()?);
                }
                if let Some(&d) = by_id.get(&id) {
                    track.devices[d]
                        .samples
                        .push(Sample::from_quantized(t, flags, v));
                }
                p = &after[30..];
            }
            _ => break,
        }
    }
    Some(track)
}

fn put_varint(mut n: u64, out: &mut Vec<u8>) {
    loop {
        let b = (n & 0x7F) as u8;
        n >>= 7;
        if n == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn get_varint(p: &mut &[u8]) -> Option<u64> {
    let mut n = 0u64;
    let mut shift = 0;
    loop {
        let (&b, rest) = p.split_first()?;
        *p = rest;
        n |= ((b & 0x7F) as u64) << shift;
        if b & 0x80 == 0 {
            return Some(n);
        }
        shift += 7;
        if shift > 63 {
            return None;
        }
    }
}

fn zig(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

fn zag(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

fn predict(hist: &[(u32, [i32; 6])], t: u32) -> [i32; 6] {
    match hist {
        [(t1, a), (t2, b)] => {
            let f =
                (t.saturating_sub(*t2) as f64 / (t2.saturating_sub(*t1)).max(1) as f64).min(2.0);
            let mut out = [0; 6];
            for i in 0..6 {
                out[i] = b[i] + ((b[i] - a[i]) as f64 * f).round() as i32;
            }
            out
        }
        [.., (_, b)] => *b,
        [] => [0; 6],
    }
}

pub fn pack(track: &Track) -> Vec<u8> {
    let mut rows: Vec<(u32, usize, &Sample)> = track
        .devices
        .iter()
        .enumerate()
        .flat_map(|(d, dev)| dev.samples.iter().map(move |s| (s.t, d, s)))
        .collect();
    rows.sort_by_key(|(t, d, _)| (*t, *d));
    let mut cols: Vec<Vec<u8>> = vec![Vec::new(); 9];
    let mut hist: Vec<Vec<(u32, [i32; 6])>> = vec![Vec::new(); track.devices.len()];
    let mut prev_t = 0;
    for (t, d, s) in rows {
        cols[0].push(d as u8);
        put_varint((t - prev_t) as u64, &mut cols[1]);
        prev_t = t;
        cols[2].push(s.flags);
        let v = s.quantized();
        let guess = predict(&hist[d], t);
        for i in 0..6 {
            put_varint(zig(v[i] as i64 - guess[i] as i64), &mut cols[3 + i]);
        }
        let h = &mut hist[d];
        h.push((t, v));
        if h.len() > 2 {
            h.remove(0);
        }
    }
    let mut out = PACKED_MAGIC.to_vec();
    out.extend_from_slice(&track.t0.to_le_bytes());
    out.push(track.devices.len() as u8);
    for dev in &track.devices {
        let name = dev.serial.as_bytes();
        let len = name.len().min(255);
        out.push(class_code(dev.class));
        out.push(len as u8);
        out.extend_from_slice(&name[..len]);
    }
    put_varint(cols[0].len() as u64, &mut out);
    for c in &cols {
        put_varint(c.len() as u64, &mut out);
        out.extend_from_slice(c);
    }
    out
}

pub fn unpack(bytes: &[u8]) -> Option<Track> {
    let mut p = bytes.strip_prefix(PACKED_MAGIC)?;
    let t0 = u64::from_le_bytes(p.get(..8)?.try_into().ok()?);
    p = &p[8..];
    let (&n, rest) = p.split_first()?;
    p = rest;
    let mut devices = Vec::new();
    for _ in 0..n {
        let [class, len] = p.get(..2)?.try_into().ok()?;
        let name = p.get(2..2 + len as usize)?;
        devices.push(TrackDevice {
            serial: String::from_utf8_lossy(name).into_owned(),
            class: class_from(class),
            samples: Vec::new(),
        });
        p = &p[2 + len as usize..];
    }
    let count = get_varint(&mut p)? as usize;
    let mut cols: Vec<&[u8]> = Vec::with_capacity(9);
    for _ in 0..9 {
        let len = get_varint(&mut p)? as usize;
        cols.push(p.get(..len)?);
        p = &p[len..];
    }
    let mut hist: Vec<Vec<(u32, [i32; 6])>> = vec![Vec::new(); devices.len()];
    let mut t = 0u32;
    for i in 0..count {
        let d = *cols[0].get(i)? as usize;
        t += get_varint(&mut cols[1])? as u32;
        let flags = *cols[2].get(i)?;
        let guess = predict(hist.get(d)?, t);
        let mut v = [0i32; 6];
        for k in 0..6 {
            v[k] = (guess[k] as i64 + zag(get_varint(&mut cols[3 + k])?)) as i32;
        }
        devices
            .get_mut(d)?
            .samples
            .push(Sample::from_quantized(t, flags, v));
        let h = &mut hist[d];
        h.push((t, v));
        if h.len() > 2 {
            h.remove(0);
        }
    }
    Some(Track { t0, devices })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn walk(n: u32, jump_at: Option<u32>) -> Vec<Sample> {
        (0..n)
            .map(|i| {
                let t = i * 11;
                let s = t as f32 / 1000.0;
                let mut pos = [
                    s.sin() * 0.8,
                    1.0 + (s * 2.3).sin() * 0.1,
                    (s * 0.7).cos() * 0.6,
                ];
                if Some(i) == jump_at {
                    pos[0] += 0.4;
                }
                let yaw = (s * 0.9).sin() * 1.5;
                let q = [(yaw / 2.0).cos(), 0.0, (yaw / 2.0).sin(), 0.0];
                Sample::new(t, pos, q, TrackState::RunningOk, true)
            })
            .collect()
    }

    fn thin(frames: &[Sample]) -> Vec<Sample> {
        let mut t = Thinner::default();
        let mut out = Vec::new();
        for f in frames {
            t.push(*f, &mut out);
        }
        t.flush(&mut out);
        out
    }

    #[test]
    fn thinned_track_stays_within_tolerance_and_keeps_a_one_frame_jump() {
        let frames = walk(9_000, Some(4_000));
        let kept = thin(&frames);
        assert!(
            kept.len() < frames.len() / 5,
            "{} of {}",
            kept.len(),
            frames.len()
        );
        let dev = TrackDevice {
            serial: "LHR-A".into(),
            class: DeviceClass::GenericTracker,
            samples: kept.clone(),
        };
        for f in &frames {
            let g = dev.at(f.t).unwrap();
            assert!(
                dist(g.pos, f.pos) <= POS_TOL_M + 1e-4,
                "t {} off by {}",
                f.t,
                dist(g.pos, f.pos)
            );
            assert!(angle_deg(g.q, f.q) <= ROT_TOL_DEG + 0.05, "t {}", f.t);
        }
        let jump = frames[4_000];
        assert!(
            kept.iter()
                .any(|k| k.t == jump.t && dist(k.pos, jump.pos) < 1e-6)
        );
    }

    #[test]
    fn state_changes_are_kept_exactly() {
        let mut frames = walk(500, None);
        for f in &mut frames[200..260] {
            f.flags = flags(TrackState::CalibratingOutOfRange, false);
        }
        let dev = TrackDevice {
            serial: "LHR-A".into(),
            class: DeviceClass::Controller,
            samples: thin(&frames),
        };
        for f in &frames {
            assert_eq!(dev.at(f.t).unwrap().flags, f.flags, "t {}", f.t);
        }
    }

    #[test]
    fn raw_file_and_packed_form_round_trip() {
        let dir = std::env::temp_dir().join(format!("trackdoctor-track-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(RAW_FILE);
        let t0 = 1_792_000_000_000;
        let mut w = TrackWriter::create(&path, t0).unwrap();
        let a = walk(3_000, Some(1_000));
        let b = walk(3_000, None);
        for (fa, fb) in a.iter().zip(&b) {
            w.frame("LHR-A", DeviceClass::GenericTracker, t0 + fa.t as u64, *fa);
            w.frame(
                "HMD-1",
                DeviceClass::Hmd,
                t0 + fb.t as u64,
                Sample {
                    pos: [fb.pos[2], 1.7, fb.pos[0]],
                    ..*fb
                },
            );
        }
        w.finish();
        drop(w);
        let raw = read_raw(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(raw.t0, t0);
        assert_eq!(raw.devices.len(), 2);
        assert_eq!(raw.devices[0].serial, "LHR-A");
        assert_eq!(raw.devices[1].class, DeviceClass::Hmd);
        let last = a.last().unwrap();
        let end = raw.devices[0].samples.last().unwrap();
        assert_eq!(end.t, last.t);
        let packed = pack(&raw);
        let back = unpack(&packed).unwrap();
        assert_eq!(back, raw);
        for f in &a {
            let g = back.devices[0].at(f.t).unwrap();
            assert!(dist(g.pos, f.pos) <= POS_TOL_M + 0.002, "t {}", f.t);
        }
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn a_device_that_goes_quiet_disappears_and_its_trail_restarts() {
        let mut frames = walk(300, None);
        frames.extend(walk(600, None).into_iter().skip(500).map(|s| Sample {
            t: s.t + 3_000,
            ..s
        }));
        let d = TrackDevice {
            serial: "LHR-A".into(),
            class: DeviceClass::GenericTracker,
            samples: thin(&frames),
        };
        let off = frames[299].t;
        let back = frames[300].t;
        assert!(d.at(off + 1_000).is_some());
        assert!(d.at(off + SILENT_MS + 1).is_none());
        assert!(d.at(back + 5).is_some());
        assert!(d.at(frames.last().unwrap().t + SILENT_MS + 1).is_none());
        let trail = d.path(off - 500, back + 200);
        let first = trail.first().unwrap();
        assert!(dist(*first, frames[300].pos) < 0.02, "{first:?}");
        assert!(d.path(0, 50).len() >= 2);
    }

    #[test]
    fn a_cut_off_raw_file_reads_up_to_the_last_whole_row() {
        let mut bytes = RAW_MAGIC.to_vec();
        bytes.extend_from_slice(&5u64.to_le_bytes());
        bytes.extend_from_slice(&[ROW_DEVICE, 0, 2, 1, b'A']);
        let s = Sample::new(
            7,
            [1.0, 2.0, 3.0],
            [1.0, 0.0, 0.0, 0.0],
            TrackState::RunningOk,
            true,
        );
        bytes.extend_from_slice(&[ROW_SAMPLE, 0]);
        bytes.extend_from_slice(&s.t.to_le_bytes());
        bytes.push(s.flags);
        for v in s.quantized() {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        let whole = bytes.len();
        bytes.extend_from_slice(&[ROW_SAMPLE, 0, 1, 2]);
        let t = read_raw(&bytes).unwrap();
        assert_eq!(t.devices[0].samples.len(), 1);
        assert_eq!(t.devices[0].samples[0].pos, [1.0, 2.0, 3.0]);
        bytes.truncate(whole);
        bytes.extend_from_slice(&[ROW_DEVICE, 1, 2, 9, b'L', b'H']);
        let t = read_raw(&bytes).unwrap();
        assert_eq!(t.devices.len(), 1);
        assert_eq!(t.devices[0].samples.len(), 1);
    }

    #[test]
    fn every_orientation_packs_exactly_and_time_never_runs_backwards() {
        let dir =
            std::env::temp_dir().join(format!("trackdoctor-track-turns-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(RAW_FILE);
        let mut w = TrackWriter::create(&path, 0).unwrap();
        let mut seed = 0x2545_f491_u32;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as f32 / u32::MAX as f32 * 2.0 - 1.0
        };
        for i in 0..20_000u32 {
            let near_flip = i % 2 == 0;
            let q = [
                if near_flip { next() * 0.01 } else { next() },
                next(),
                next(),
                next(),
            ];
            let n = q.iter().map(|v| v * v).sum::<f32>().sqrt().max(1e-3);
            let q = q.map(|v| v / n);
            let t = if i == 10_000 { 5 } else { i as u64 * 11 };
            let s = Sample::new(
                0,
                [i as f32 * 0.01, 1.0, 0.0],
                q,
                TrackState::RunningOk,
                true,
            );
            w.frame("LHR-A", DeviceClass::GenericTracker, t, s);
        }
        w.finish();
        drop(w);
        let raw = read_raw(&std::fs::read(&path).unwrap()).unwrap();
        let times: Vec<u32> = raw.devices[0].samples.iter().map(|s| s.t).collect();
        assert!(times.windows(2).all(|p| p[0] <= p[1]));
        assert!(raw.devices[0].samples.len() > 5_000);
        assert_eq!(unpack(&pack(&raw)).unwrap(), raw);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }
}

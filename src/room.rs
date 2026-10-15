use tiny_skia::{
    Color, FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Stroke, Transform,
};

pub type V3 = [f32; 3];
pub type Rot = [[f32; 3]; 3];

pub const IDENTITY: Rot = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn scale(a: V3, s: f32) -> V3 {
    [a[0] * s, a[1] * s, a[2] * s]
}

fn dot(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn len(a: V3) -> f32 {
    dot(a, a).sqrt()
}

fn rotate(r: &Rot, v: V3) -> V3 {
    [dot(r[0], v), dot(r[1], v), dot(r[2], v)]
}

fn unrotate(r: &Rot, v: V3) -> V3 {
    [
        r[0][0] * v[0] + r[1][0] * v[1] + r[2][0] * v[2],
        r[0][1] * v[0] + r[1][1] * v[1] + r[2][1] * v[2],
        r[0][2] * v[0] + r[1][2] * v[1] + r[2][2] * v[2],
    ]
}

pub fn rot_from_matrix(m: &[[f32; 4]; 3]) -> Rot {
    [
        [m[0][0], m[0][1], m[0][2]],
        [m[1][0], m[1][1], m[1][2]],
        [m[2][0], m[2][1], m[2][2]],
    ]
}

pub fn rot_from_quat(x: f32, y: f32, z: f32, w: f32) -> Rot {
    [
        [
            1.0 - 2.0 * (y * y + z * z),
            2.0 * (x * y - z * w),
            2.0 * (x * z + y * w),
        ],
        [
            2.0 * (x * y + z * w),
            1.0 - 2.0 * (x * x + z * z),
            2.0 * (y * z - x * w),
        ],
        [
            2.0 * (x * z - y * w),
            2.0 * (y * z + x * w),
            1.0 - 2.0 * (x * x + y * y),
        ],
    ]
}

pub fn forward(r: &Rot) -> V3 {
    rotate(r, [0.0, 0.0, -1.0])
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fov {
    pub h_deg: f32,
    pub v_deg: f32,
    pub range_m: f32,
}

pub const FOV_V2: Fov = Fov {
    h_deg: 160.0,
    v_deg: 115.0,
    range_m: 7.0,
};

pub const FOV_V1: Fov = Fov {
    h_deg: 120.0,
    v_deg: 120.0,
    range_m: 5.0,
};

pub fn fov_for_model(model: &str) -> Fov {
    if model.to_ascii_lowercase().contains("htc v2") {
        FOV_V1
    } else {
        FOV_V2
    }
}

pub fn base_sees(pos: V3, rot: &Rot, fov: Fov, p: V3) -> bool {
    let d = sub(p, pos);
    if len(d) > fov.range_m {
        return false;
    }
    let l = unrotate(rot, d);
    let ahead = -l[2];
    let az = l[0].atan2(ahead).to_degrees();
    let el = l[1]
        .atan2((l[0] * l[0] + ahead * ahead).sqrt())
        .to_degrees();
    az.abs() <= fov.h_deg / 2.0 && el.abs() <= fov.v_deg / 2.0
}

pub fn fov_dir(az_deg: f32, el_deg: f32) -> V3 {
    let (az, el) = (az_deg.to_radians(), el_deg.to_radians());
    [el.cos() * az.sin(), el.sin(), -el.cos() * az.cos()]
}

pub fn aim_on_floor(pos: V3, rot: &Rot) -> Option<V3> {
    let f = forward(rot);
    if f[1] >= -1e-3 || pos[1] <= 0.0 {
        return None;
    }
    let t = -pos[1] / f[1];
    Some(add(pos, scale(f, t)))
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub yaw: f32,
    pub pitch: f32,
    pub dist: f32,
    pub target: V3,
    pub fov_y: f32,
    pub flat: bool,
}

pub const PITCH_MIN: f32 = -0.17;
pub const PITCH_MAX: f32 = std::f32::consts::FRAC_PI_2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    Top,
    Front,
    Side,
    Angled,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            yaw: 0.6,
            pitch: 0.6,
            dist: 6.0,
            target: [0.0, 0.8, 0.0],
            fov_y: 45f32.to_radians(),
            flat: false,
        }
    }
}

pub struct View {
    eye: V3,
    right: V3,
    up: V3,
    fwd: V3,
    focal: f32,
    flat_scale: f32,
    flat: bool,
    cx: f32,
    cy: f32,
}

pub const NEAR: f32 = 0.05;

impl Camera {
    pub fn eye(&self) -> V3 {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        add(self.target, scale([cp * sy, sp, cp * cy], self.dist))
    }

    pub fn view(&self, w: f32, h: f32) -> View {
        let eye = self.eye();
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        let fwd = [-cp * sy, -sp, -cp * cy];
        let right = [cy, 0.0, -sy];
        let up = cross(right, fwd);
        let focal = (h / 2.0) / (self.fov_y / 2.0).tan();
        View {
            eye,
            right,
            up,
            fwd,
            focal,
            flat_scale: focal / self.dist,
            flat: self.flat,
            cx: w / 2.0,
            cy: h / 2.0,
        }
    }

    pub fn orbit(&mut self, dx_px: f32, dy_px: f32) {
        self.yaw -= dx_px * 0.008;
        self.pitch = (self.pitch + dy_px * 0.008).clamp(PITCH_MIN, PITCH_MAX);
        self.flat = false;
    }

    pub fn zoom(&mut self, steps: f32) {
        self.dist = (self.dist * 0.9f32.powf(steps)).clamp(0.5, 60.0);
    }

    pub fn pan(&mut self, dx_px: f32, dy_px: f32, h: f32) {
        let v = self.view(1.0, h);
        let per_px = self.dist / v.focal;
        let ground_fwd = [-self.yaw.sin(), 0.0, -self.yaw.cos()];
        let up_move = if self.pitch > 1.2 {
            ground_fwd
        } else {
            [0.0, 1.0, 0.0]
        };
        self.target = add(
            self.target,
            add(
                scale(v.right, -dx_px * per_px),
                scale(up_move, dy_px * per_px),
            ),
        );
    }

    pub fn preset(&mut self, p: Preset) {
        let (yaw, pitch, flat) = match p {
            Preset::Top => (0.0, PITCH_MAX, true),
            Preset::Front => (0.0, 0.0, true),
            Preset::Side => (std::f32::consts::FRAC_PI_2, 0.0, true),
            Preset::Angled => (0.6, 0.6, false),
        };
        self.yaw = yaw;
        self.pitch = pitch;
        self.flat = flat;
    }

    pub fn fit(&mut self, b: &Bounds, w: f32, h: f32) {
        self.target = [
            (b.min[0] + b.max[0]) / 2.0,
            (b.min[1] + b.max[1]) / 2.0,
            (b.min[2] + b.max[2]) / 2.0,
        ];
        let radius = len(sub(b.max, b.min)) / 2.0;
        let half_v = self.fov_y / 2.0;
        let half_h = ((w / h.max(1.0)) * half_v.tan()).atan();
        let half = half_v.min(half_h);
        self.dist = (radius / half.sin()).clamp(1.0, 60.0);
    }
}

impl View {
    pub fn camera_space(&self, p: V3) -> V3 {
        let c = sub(p, self.eye);
        [dot(c, self.right), dot(c, self.up), dot(c, self.fwd)]
    }

    fn screen(&self, c: V3) -> Option<(f32, f32)> {
        if self.flat {
            return Some((
                self.cx + c[0] * self.flat_scale,
                self.cy - c[1] * self.flat_scale,
            ));
        }
        if c[2] < NEAR {
            return None;
        }
        Some((
            self.cx + self.focal * c[0] / c[2],
            self.cy - self.focal * c[1] / c[2],
        ))
    }

    pub fn project(&self, p: V3) -> Option<(f32, f32)> {
        self.screen(self.camera_space(p))
    }

    pub fn depth(&self, p: V3) -> f32 {
        self.camera_space(p)[2]
    }

    pub fn segment(&self, a: V3, b: V3) -> Option<((f32, f32), (f32, f32))> {
        let (mut ca, mut cb) = (self.camera_space(a), self.camera_space(b));
        if !self.flat {
            if ca[2] < NEAR && cb[2] < NEAR {
                return None;
            }
            if ca[2] < NEAR || cb[2] < NEAR {
                let t = (NEAR - ca[2]) / (cb[2] - ca[2]);
                let mut hit = add(ca, scale(sub(cb, ca), t));
                hit[2] = NEAR;
                if ca[2] < NEAR {
                    ca = hit;
                } else {
                    cb = hit;
                }
            }
        }
        Some((self.screen(ca)?, self.screen(cb)?))
    }

    pub fn px_per_m(&self, p: V3) -> f32 {
        if self.flat {
            return self.flat_scale;
        }
        let z = self.depth(p).max(NEAR);
        self.focal / z
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub min: V3,
    pub max: V3,
}

impl Bounds {
    pub fn around(points: &[V3]) -> Bounds {
        if points.is_empty() {
            return Bounds {
                min: [-2.0, 0.0, -2.0],
                max: [2.0, 2.0, 2.0],
            };
        }
        let mut min = points[0];
        let mut max = points[0];
        for p in points {
            for i in 0..3 {
                min[i] = min[i].min(p[i]);
                max[i] = max[i].max(p[i]);
            }
        }
        min[1] = min[1].min(0.0);
        max[1] = max[1].max(2.0);
        for i in [0, 2] {
            min[i] = (min[i] - 0.5).floor();
            max[i] = (max[i] + 0.5).ceil();
            while max[i] - min[i] < 4.0 {
                min[i] -= 1.0;
                max[i] += 1.0;
            }
        }
        Bounds { min, max }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Base,
    Headset,
    Clean,
    Some,
    Worst,
    Unknown,
}

impl Tone {
    pub fn rgb(self) -> (u8, u8, u8) {
        match self {
            Tone::Base => (0x22, 0xd3, 0xee),
            Tone::Headset => (0xe5, 0xe7, 0xeb),
            Tone::Clean => (0x84, 0xcc, 0x16),
            Tone::Some => (0xfb, 0xbf, 0x24),
            Tone::Worst => (0xf8, 0x71, 0x71),
            Tone::Unknown => (0x9c, 0xa3, 0xaf),
        }
    }
}

#[derive(Clone, Debug)]
pub struct BaseView {
    pub pos: V3,
    pub rot: Rot,
    pub fov: Fov,
    pub label: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    Headset,
    Controller,
    Tracker,
}

#[derive(Clone, Debug)]
pub struct DeviceView {
    pub pos: V3,
    pub rot: Rot,
    pub shape: Shape,
    pub tone: Tone,
    pub label: String,
    pub tracking: bool,
    pub trail: Vec<V3>,
}

#[derive(Clone, Debug)]
pub struct DropView {
    pub pos: V3,
    pub tone: Tone,
}

#[derive(Clone, Debug, Default)]
pub struct Scene {
    pub bases: Vec<BaseView>,
    pub devices: Vec<DeviceView>,
    pub drops: Vec<DropView>,
    pub fixed: Option<Bounds>,
}

impl Scene {
    pub fn bounds(&self) -> Bounds {
        if let Some(b) = self.fixed {
            return b;
        }
        let mut pts: Vec<V3> = self.bases.iter().map(|b| b.pos).collect();
        pts.extend(self.devices.iter().map(|d| d.pos));
        pts.extend(self.drops.iter().map(|d| d.pos));
        Bounds::around(&pts)
    }

    pub fn coverage(&self, p: V3) -> usize {
        self.bases
            .iter()
            .filter(|b| base_sees(b.pos, &b.rot, b.fov, p))
            .count()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub coverage_height: Option<f32>,
    pub coverage_on_floor: bool,
    pub cones: bool,
    pub drops: bool,
    pub scale: f32,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            coverage_height: Some(1.0),
            coverage_on_floor: true,
            cones: false,
            drops: true,
            scale: 1.0,
        }
    }
}

pub const DEFAULT_YAW: f32 = 0.6;
const CLEAR_YAW: f32 = 0.52;

fn wrap(a: f32) -> f32 {
    let tau = std::f32::consts::TAU;
    let mut a = (a + std::f32::consts::PI).rem_euclid(tau) - std::f32::consts::PI;
    if a <= -std::f32::consts::PI {
        a += tau;
    }
    a
}

pub fn pick_yaw(bases: &[V3], target: V3) -> f32 {
    let dirs: Vec<f32> = bases
        .iter()
        .filter_map(|b| {
            let (dx, dz) = (b[0] - target[0], b[2] - target[2]);
            (dx.hypot(dz) > 0.3).then(|| dx.atan2(dz))
        })
        .collect();
    if dirs.is_empty() {
        return DEFAULT_YAW;
    }
    let gap = |yaw: f32| {
        dirs.iter()
            .map(|d| wrap(yaw - d).abs())
            .fold(std::f32::consts::PI, f32::min)
    };
    let candidates = (0..72).map(|i| wrap((i as f32 * 5.0).to_radians()));
    let from_default = |y: f32| wrap(y - DEFAULT_YAW).abs();
    candidates
        .clone()
        .filter(|&y| gap(y) >= CLEAR_YAW)
        .min_by(|a, b| from_default(*a).total_cmp(&from_default(*b)))
        .unwrap_or_else(|| {
            candidates
                .max_by(|a, b| gap(*a).total_cmp(&gap(*b)))
                .unwrap_or(DEFAULT_YAW)
        })
}

pub const CHIP_DX: f32 = 10.0;
pub const CHIP_DY: f32 = -22.0;
const CHIP_H: f32 = 18.0;
const CHAR_W: f32 = 7.4;

pub(crate) fn chip_rect(l: &Label, s: f32) -> [f32; 4] {
    let w = (l.text.chars().count() as f32 * CHAR_W + 10.0) * s;
    match l.kind {
        LabelKind::Base => {
            let w = l.text.chars().count() as f32 * CHAR_W * s;
            [l.x - w / 2.0, l.y - 34.0 * s, w, 16.0 * s]
        }
        LabelKind::Note | LabelKind::Grid => {
            let w = l.text.chars().count() as f32 * CHAR_W * s;
            [l.x + 4.0 * s, l.y - 16.0 * s, w, 16.0 * s]
        }
        LabelKind::Device => [l.x + CHIP_DX * s, l.y + CHIP_DY * s, w, CHIP_H * s],
    }
}

pub(crate) fn overlaps(a: &[f32; 4], b: &[f32; 4], pad: f32) -> bool {
    a[0] < b[0] + b[2] + pad
        && b[0] < a[0] + a[2] + pad
        && a[1] < b[1] + b[3] + pad
        && b[1] < a[1] + a[3] + pad
}

const MIN_ARROW_PX: f32 = 24.0;

fn readable_axis(axis: Vec<Label>, s: f32) -> Vec<Label> {
    let (Some(a), Some(b)) = (axis.first(), axis.last()) else {
        return axis;
    };
    let gaps = axis.len().saturating_sub(1).max(1) as f32;
    if (a.x - b.x).hypot(a.y - b.y) / gaps < MIN_ARROW_PX * s {
        return Vec::new();
    }
    axis
}

pub fn thin_grid_labels(labels: &mut Vec<Label>, s: f32) {
    let mut placed: Vec<[f32; 4]> = labels
        .iter()
        .filter(|l| matches!(l.kind, LabelKind::Base | LabelKind::Note))
        .map(|l| chip_rect(l, s))
        .collect();
    labels.retain(|l| {
        if l.kind != LabelKind::Grid {
            return true;
        }
        let r = chip_rect(l, s);
        if placed.iter().any(|p| overlaps(&r, p, 2.0 * s)) {
            return false;
        }
        placed.push(r);
        true
    });
}

pub fn spread_labels(labels: &mut [Label], s: f32) -> Vec<((f32, f32), (f32, f32))> {
    let mut placed: Vec<[f32; 4]> = labels
        .iter()
        .filter(|l| matches!(l.kind, LabelKind::Base | LabelKind::Note))
        .map(|l| chip_rect(l, s))
        .collect();
    let mut order: Vec<usize> = (0..labels.len())
        .filter(|&i| labels[i].kind == LabelKind::Device)
        .collect();
    order.sort_by(|&a, &b| labels[a].y.total_cmp(&labels[b].y));
    let mut leaders = Vec::new();
    for i in order {
        let anchor = (labels[i].x, labels[i].y);
        let base = chip_rect(&labels[i], s);
        let step = (CHIP_H + 3.0) * s;
        let shift = [0.0f32, -1.0, 1.0, -2.0, 2.0, -3.0, 3.0, -4.0, 4.0]
            .iter()
            .map(|k| k * step)
            .find(|dy| {
                let r = [base[0], base[1] + dy, base[2], base[3]];
                !placed.iter().any(|p| overlaps(&r, p, 2.0 * s))
            })
            .unwrap_or(0.0);
        placed.push([base[0], base[1] + shift, base[2], base[3]]);
        if shift != 0.0 {
            labels[i].y += shift;
            leaders.push((anchor, (base[0], base[1] + shift + base[3] / 2.0)));
        }
    }
    leaders
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LabelKind {
    Base,
    Device,
    Grid,
    Note,
}

#[derive(Clone, Debug)]
pub struct Label {
    pub x: f32,
    pub y: f32,
    pub text: String,
    pub tone: Tone,
    pub kind: LabelKind,
}

pub const BACKGROUND: (u8, u8, u8) = (0x11, 0x18, 0x27);
const GRID: (u8, u8, u8, u8) = (0x4b, 0x55, 0x63, 0xff);

pub fn coverage_rgba(count: usize) -> (u8, u8, u8, u8) {
    match count {
        0 => (0xf8, 0x71, 0x71, 0x38),
        1 => (0xfb, 0xbf, 0x24, 0x40),
        2 => (0x84, 0xcc, 0x16, 0x40),
        _ => (0x22, 0xc5, 0x5e, 0x58),
    }
}

fn paint(rgba: (u8, u8, u8, u8)) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(Color::from_rgba8(rgba.0, rgba.1, rgba.2, rgba.3));
    p.anti_alias = true;
    p
}

fn tone_rgba(t: Tone, a: u8) -> (u8, u8, u8, u8) {
    let (r, g, b) = t.rgb();
    (r, g, b, a)
}

struct Canvas<'a> {
    pix: &'a mut Pixmap,
    view: &'a View,
    s: f32,
}

impl Canvas<'_> {
    fn line(&mut self, a: V3, b: V3, rgba: (u8, u8, u8, u8), width: f32) {
        let Some((pa, pb)) = self.view.segment(a, b) else {
            return;
        };
        let mut pb2 = PathBuilder::new();
        pb2.move_to(pa.0, pa.1);
        pb2.line_to(pb.0, pb.1);
        if let Some(path) = pb2.finish() {
            let stroke = Stroke {
                width: width * self.s,
                line_cap: LineCap::Round,
                line_join: LineJoin::Round,
                ..Stroke::default()
            };
            self.pix
                .stroke_path(&path, &paint(rgba), &stroke, Transform::identity(), None);
        }
    }

    fn polyline(&mut self, pts: &[V3], rgba: (u8, u8, u8, u8), width: f32) {
        for w in pts.windows(2) {
            self.line(w[0], w[1], rgba, width);
        }
    }

    fn poly(&mut self, pts: &[V3], rgba: (u8, u8, u8, u8)) {
        let mut pb = PathBuilder::new();
        for (i, p) in pts.iter().enumerate() {
            let Some((x, y)) = self.view.project(*p) else {
                return;
            };
            if i == 0 {
                pb.move_to(x, y);
            } else {
                pb.line_to(x, y);
            }
        }
        pb.close();
        if let Some(path) = pb.finish() {
            self.pix.fill_path(
                &path,
                &paint(rgba),
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
    }

    fn dot(
        &mut self,
        p: V3,
        radius_px: f32,
        fill: (u8, u8, u8, u8),
        ring: Option<(u8, u8, u8, u8)>,
    ) {
        let Some((x, y)) = self.view.project(p) else {
            return;
        };
        let r = radius_px * self.s;
        if let Some(path) = PathBuilder::from_circle(x, y, r) {
            self.pix.fill_path(
                &path,
                &paint(fill),
                FillRule::Winding,
                Transform::identity(),
                None,
            );
            if let Some(ring) = ring {
                let stroke = Stroke {
                    width: 1.5 * self.s,
                    ..Stroke::default()
                };
                self.pix
                    .stroke_path(&path, &paint(ring), &stroke, Transform::identity(), None);
            }
        }
    }

    fn cross_mark(&mut self, p: V3, size_px: f32, rgba: (u8, u8, u8, u8)) {
        let Some((x, y)) = self.view.project(p) else {
            return;
        };
        let r = size_px * self.s;
        let mut pb = PathBuilder::new();
        pb.move_to(x - r, y - r);
        pb.line_to(x + r, y + r);
        pb.move_to(x - r, y + r);
        pb.line_to(x + r, y - r);
        if let Some(path) = pb.finish() {
            let stroke = Stroke {
                width: 2.0 * self.s,
                line_cap: LineCap::Round,
                ..Stroke::default()
            };
            self.pix
                .stroke_path(&path, &paint(rgba), &stroke, Transform::identity(), None);
        }
    }
}

fn box_corners(pos: V3, rot: &Rot, half: V3) -> [V3; 8] {
    let mut out = [[0.0; 3]; 8];
    for (i, c) in out.iter_mut().enumerate() {
        let l = [
            if i & 1 == 0 { -half[0] } else { half[0] },
            if i & 2 == 0 { -half[1] } else { half[1] },
            if i & 4 == 0 { -half[2] } else { half[2] },
        ];
        *c = add(pos, rotate(rot, l));
    }
    out
}

const BOX_FACES: [[usize; 4]; 6] = [
    [0, 1, 3, 2],
    [4, 5, 7, 6],
    [0, 1, 5, 4],
    [2, 3, 7, 6],
    [0, 2, 6, 4],
    [1, 3, 7, 5],
];

fn draw_box(cv: &mut Canvas, pos: V3, rot: &Rot, half: V3, tone: Tone, front: Option<usize>) {
    let corners = box_corners(pos, rot, half);
    let mut faces: Vec<(f32, usize)> = BOX_FACES
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let c = f.iter().fold([0.0; 3], |acc, &k| add(acc, corners[k]));
            (cv.view.depth(scale(c, 0.25)), i)
        })
        .collect();
    faces.sort_by(|a, b| b.0.total_cmp(&a.0));
    for (_, i) in faces.iter().skip(3) {
        let f = BOX_FACES[*i];
        let pts: Vec<V3> = f.iter().map(|&k| corners[k]).collect();
        let shade = if Some(*i) == front { 0xff } else { 0x90 };
        let (r, g, b) = tone.rgb();
        let dim = |c: u8| ((c as u16 * shade as u16) / 0xff) as u8;
        cv.poly(&pts, (dim(r), dim(g), dim(b), 0xff));
        let mut ring = pts.clone();
        ring.push(pts[0]);
        cv.polyline(&ring, (0x11, 0x18, 0x27, 0xff), 1.0);
    }
}

fn draw_wedge(cv: &mut Canvas, b: &BaseView, dist: f32, rgba: (u8, u8, u8, u8), width: f32) {
    let (h, v) = (b.fov.h_deg / 2.0, b.fov.v_deg / 2.0);
    let world = |az: f32, el: f32| add(b.pos, rotate(&b.rot, scale(fov_dir(az, el), dist)));
    let steps = 24;
    let mut ring = Vec::new();
    for i in 0..=steps {
        ring.push(world(-h + 2.0 * h * i as f32 / steps as f32, v));
    }
    for i in 0..=steps {
        ring.push(world(h, v - 2.0 * v * i as f32 / steps as f32));
    }
    for i in 0..=steps {
        ring.push(world(h - 2.0 * h * i as f32 / steps as f32, -v));
    }
    for i in 0..=steps {
        ring.push(world(-h, -v + 2.0 * v * i as f32 / steps as f32));
    }
    cv.polyline(&ring, rgba, width);
    for (az, el) in [(-h, v), (h, v), (h, -v), (-h, -v)] {
        cv.line(b.pos, world(az, el), rgba, width);
    }
}

fn grid_step(px_per_m: f32) -> f32 {
    if px_per_m < 12.0 { 2.0 } else { 1.0 }
}

pub struct Backdrop {
    key: BackdropKey,
    pix: Pixmap,
    labels: Vec<Label>,
}

#[derive(Clone, Debug, PartialEq)]
struct BackdropKey {
    cam: Camera,
    size: (u32, u32),
    coverage_height: Option<f32>,
    coverage_on_floor: bool,
    scale: f32,
    bounds: Bounds,
    bases: Vec<(V3, Rot, Fov)>,
}

fn draw_backdrop(
    pix: &mut Pixmap,
    scene: &Scene,
    cam: &Camera,
    view: &View,
    bounds: &Bounds,
    opts: &Options,
) -> Vec<Label> {
    let mut labels = Vec::new();
    let mut cv = Canvas {
        pix,
        view,
        s: opts.scale,
    };

    let (x0, x1, z0, z1) = (bounds.min[0], bounds.max[0], bounds.min[2], bounds.max[2]);
    cv.poly(
        &[[x0, 0.0, z0], [x1, 0.0, z0], [x1, 0.0, z1], [x0, 0.0, z1]],
        (0x1f, 0x29, 0x37, 0xff),
    );

    if let Some(hgt) = opts.coverage_height
        && !scene.bases.is_empty()
    {
        let cell = 0.1;
        let nx = ((x1 - x0) / cell).round() as usize;
        let nz = ((z1 - z0) / cell).round() as usize;
        for j in 0..nz {
            let za = z0 + j as f32 * cell;
            let zb = za + cell;
            let mut i = 0;
            while i < nx {
                let at = |i: usize| [x0 + (i as f32 + 0.5) * cell, hgt, za + cell / 2.0];
                let count = scene.coverage(at(i));
                let mut k = i + 1;
                while k < nx && scene.coverage(at(k)) == count {
                    k += 1;
                }
                let (xa, xb) = (x0 + i as f32 * cell, x0 + k as f32 * cell);
                let y = if opts.coverage_on_floor { 0.0 } else { hgt };
                cv.poly(
                    &[[xa, y, za], [xb, y, za], [xb, y, zb], [xa, y, zb]],
                    coverage_rgba(count),
                );
                i = k;
            }
        }
    }

    let step = grid_step(view.px_per_m(cam.target));
    let grid_label = |p: [f32; 3], v: f32| {
        view.project(p).map(|(sx, sy)| Label {
            x: sx,
            y: sy,
            text: format!("{v:.0} m"),
            tone: Tone::Unknown,
            kind: LabelKind::Grid,
        })
    };
    let mut axis = Vec::new();
    let mut x = x0;
    while x <= x1 + 1e-3 {
        cv.line([x, 0.0, z0], [x, 0.0, z1], GRID, 1.0);
        axis.extend(grid_label([x, 0.0, z1], x));
        x += step;
    }
    let mut x_axis = readable_axis(axis, opts.scale);
    let mut axis = Vec::new();
    let mut z = z0;
    while z <= z1 + 1e-3 {
        cv.line([x0, 0.0, z], [x1, 0.0, z], GRID, 1.0);
        axis.extend(grid_label([x1, 0.0, z], z));
        z += step;
    }
    let mut z_axis = readable_axis(axis, opts.scale);
    if !x_axis.is_empty() && !z_axis.is_empty() {
        x_axis.pop();
        z_axis.pop();
    }
    labels.extend(x_axis);
    labels.extend(z_axis);

    let arrow = (0xe5, 0xe7, 0xeb, 0xc0);
    cv.line([0.0, 0.0, 0.3], [0.0, 0.0, -0.6], arrow, 2.0);
    cv.line([0.0, 0.0, -0.6], [-0.15, 0.0, -0.42], arrow, 2.0);
    cv.line([0.0, 0.0, -0.6], [0.15, 0.0, -0.42], arrow, 2.0);
    let arrow_px = match (
        view.project([0.0, 0.0, 0.3]),
        view.project([0.0, 0.0, -0.6]),
    ) {
        (Some(a), Some(b)) => (a.0 - b.0).hypot(a.1 - b.1),
        _ => 0.0,
    };
    if arrow_px >= MIN_ARROW_PX * opts.scale
        && let Some((sx, sy)) = view.project([0.0, 0.0, -0.7])
    {
        labels.push(Label {
            x: sx,
            y: sy,
            text: "forward".into(),
            tone: Tone::Headset,
            kind: LabelKind::Note,
        });
    }

    for b in &scene.bases {
        if let Some(aim) = aim_on_floor(b.pos, &b.rot) {
            let c = tone_rgba(Tone::Base, 0x90);
            cv.line(b.pos, aim, c, 1.5);
            cv.dot(aim, 4.0, tone_rgba(Tone::Base, 0xc0), None);
        }
    }
    labels
}

pub fn render(scene: &Scene, cam: &Camera, opts: &Options, w: u32, h: u32) -> (Pixmap, Vec<Label>) {
    render_with(scene, cam, opts, w, h, &mut None)
}

pub fn render_with(
    scene: &Scene,
    cam: &Camera,
    opts: &Options,
    w: u32,
    h: u32,
    cache: &mut Option<Backdrop>,
) -> (Pixmap, Vec<Label>) {
    let (w, h) = (w.max(1), h.max(1));
    let view = cam.view(w as f32, h as f32);
    let bounds = scene.bounds();
    let key = BackdropKey {
        cam: *cam,
        size: (w, h),
        coverage_height: opts.coverage_height,
        coverage_on_floor: opts.coverage_on_floor,
        scale: opts.scale,
        bounds,
        bases: scene.bases.iter().map(|b| (b.pos, b.rot, b.fov)).collect(),
    };
    let (mut pix, mut labels) = match cache {
        Some(b) if b.key == key => (b.pix.clone(), b.labels.clone()),
        _ => {
            let mut pix = Pixmap::new(w, h).expect("pixmap size");
            pix.fill(Color::from_rgba8(
                BACKGROUND.0,
                BACKGROUND.1,
                BACKGROUND.2,
                0xff,
            ));
            let labels = draw_backdrop(&mut pix, scene, cam, &view, &bounds, opts);
            *cache = Some(Backdrop {
                key,
                pix: pix.clone(),
                labels: labels.clone(),
            });
            (pix, labels)
        }
    };
    let mut cv = Canvas {
        pix: &mut pix,
        view: &view,
        s: opts.scale,
    };

    if opts.drops {
        for d in &scene.drops {
            cv.line(
                d.pos,
                [d.pos[0], 0.0, d.pos[2]],
                tone_rgba(d.tone, 0x50),
                1.0,
            );
            cv.cross_mark(d.pos, 4.5, tone_rgba(d.tone, 0xe0));
        }
    }

    for d in &scene.devices {
        let n = d.trail.len();
        for (k, w) in d.trail.windows(2).enumerate() {
            let a = 0x30 + (0xb0 * (k + 1) / n.max(2)) as u8;
            cv.line(w[0], w[1], tone_rgba(d.tone, a), 2.0);
        }
    }

    enum Item<'a> {
        Base(&'a BaseView),
        Device(&'a DeviceView),
    }
    let mut items: Vec<(f32, Item)> = scene
        .bases
        .iter()
        .map(|b| (view.depth(b.pos), Item::Base(b)))
        .chain(
            scene
                .devices
                .iter()
                .map(|d| (view.depth(d.pos), Item::Device(d))),
        )
        .collect();
    items.sort_by(|a, b| b.0.total_cmp(&a.0));

    for (_, item) in &items {
        match item {
            Item::Base(b) => {
                let floor = [b.pos[0], 0.0, b.pos[2]];
                cv.line(b.pos, floor, (0x6b, 0x72, 0x80, 0xa0), 1.0);
                cv.dot(floor, 2.5, (0x6b, 0x72, 0x80, 0xff), None);
                if opts.cones {
                    draw_wedge(&mut cv, b, b.fov.range_m, tone_rgba(Tone::Base, 0x50), 1.0);
                }
                draw_wedge(&mut cv, b, 0.45, tone_rgba(Tone::Base, 0xd0), 1.5);
                draw_box(
                    &mut cv,
                    b.pos,
                    &b.rot,
                    [0.07, 0.07, 0.05],
                    Tone::Base,
                    Some(0),
                );
            }
            Item::Device(d) => {
                let floor = [d.pos[0], 0.0, d.pos[2]];
                cv.line(d.pos, floor, (0x6b, 0x72, 0x80, 0x90), 1.0);
                cv.dot(floor, 2.0, (0x6b, 0x72, 0x80, 0xff), None);
                let a = if d.tracking { 0xff } else { 0x70 };
                match d.shape {
                    Shape::Headset => {
                        draw_box(&mut cv, d.pos, &d.rot, [0.09, 0.05, 0.06], d.tone, Some(0));
                        let tip = add(d.pos, scale(forward(&d.rot), 0.3));
                        cv.line(d.pos, tip, tone_rgba(d.tone, a), 2.0);
                    }
                    Shape::Controller => {
                        let tip = add(d.pos, scale(forward(&d.rot), 0.18));
                        cv.line(d.pos, tip, tone_rgba(d.tone, a), 3.0);
                        cv.dot(
                            d.pos,
                            6.0,
                            tone_rgba(d.tone, a),
                            Some((0x11, 0x18, 0x27, 0xff)),
                        );
                    }
                    Shape::Tracker => {
                        cv.dot(
                            d.pos,
                            7.0,
                            tone_rgba(d.tone, a),
                            Some((0x11, 0x18, 0x27, 0xff)),
                        );
                    }
                }
            }
        }
    }

    for b in &scene.bases {
        if let Some((sx, sy)) = view.project(b.pos) {
            labels.push(Label {
                x: sx,
                y: sy,
                text: b.label.clone(),
                tone: Tone::Base,
                kind: LabelKind::Base,
            });
        }
    }
    for d in &scene.devices {
        if let Some((sx, sy)) = view.project(d.pos) {
            labels.push(Label {
                x: sx,
                y: sy,
                text: d.label.clone(),
                tone: d.tone,
                kind: LabelKind::Device,
            });
        }
    }
    thin_grid_labels(&mut labels, opts.scale);
    let leaders = spread_labels(&mut labels, opts.scale);
    for (a, b) in leaders {
        let mut pb = PathBuilder::new();
        pb.move_to(a.0, a.1);
        pb.line_to(b.0, b.1);
        if let Some(path) = pb.finish() {
            let stroke = Stroke {
                width: 1.0 * opts.scale,
                ..Stroke::default()
            };
            cv.pix.stroke_path(
                &path,
                &paint((0x9c, 0xa3, 0xaf, 0xb0)),
                &stroke,
                Transform::identity(),
                None,
            );
        }
    }
    (pix, labels)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() <= tol
    }

    fn yaw_pitch(yaw_deg: f32, pitch_down_deg: f32) -> Rot {
        let (y, p) = (yaw_deg.to_radians(), pitch_down_deg.to_radians());
        let ry: Rot = [
            [y.cos(), 0.0, y.sin()],
            [0.0, 1.0, 0.0],
            [-y.sin(), 0.0, y.cos()],
        ];
        let rx: Rot = [
            [1.0, 0.0, 0.0],
            [0.0, p.cos(), p.sin()],
            [0.0, -p.sin(), p.cos()],
        ];
        let mut out = [[0.0; 3]; 3];
        for i in 0..3 {
            for j in 0..3 {
                out[i][j] = (0..3).map(|k| ry[i][k] * rx[k][j]).sum();
            }
        }
        out
    }

    #[test]
    fn top_view_puts_right_on_the_right_and_forward_up() {
        let mut cam = Camera::default();
        cam.preset(Preset::Top);
        cam.target = [0.0, 0.0, 0.0];
        cam.dist = 5.0;
        let v = cam.view(800.0, 600.0);
        let (cx, cy) = v.project([0.0, 0.0, 0.0]).unwrap();
        assert!(close(cx, 400.0, 0.01) && close(cy, 300.0, 0.01));
        let (rx, ry) = v.project([1.0, 0.0, 0.0]).unwrap();
        assert!(rx > 400.0 && close(ry, 300.0, 0.01));
        let (fx, fy) = v.project([0.0, 0.0, -1.0]).unwrap();
        assert!(close(fx, 400.0, 0.01) && fy < 300.0);
        let (_, hy) = v.project([0.0, 2.0, 0.0]).unwrap();
        assert!(close(hy, 300.0, 0.01), "a flat top view ignores height");
        assert!(
            close(rx - 400.0, 300.0 - fy, 0.01),
            "one metre is the same on both axes"
        );
    }

    #[test]
    fn flat_scale_matches_perspective_at_the_target() {
        let mut cam = Camera {
            yaw: 0.3,
            pitch: 0.4,
            dist: 4.0,
            target: [0.5, 1.0, -0.5],
            ..Camera::default()
        };
        let persp = cam.view(1000.0, 700.0).px_per_m(cam.target);
        cam.flat = true;
        let flat = cam.view(1000.0, 700.0).px_per_m(cam.target);
        assert!(close(persp, flat, 1e-3));
    }

    #[test]
    fn perspective_shrinks_with_distance_and_clips_behind() {
        let cam = Camera {
            yaw: 0.0,
            pitch: 0.0,
            dist: 3.0,
            target: [0.0, 1.0, 0.0],
            ..Camera::default()
        };
        let v = cam.view(800.0, 600.0);
        let near = v.project([1.0, 1.0, 0.0]).unwrap().0 - 400.0;
        let far = v.project([1.0, 1.0, -3.0]).unwrap().0 - 400.0;
        assert!(close(near / far, 2.0, 1e-3));
        assert!(v.project([0.0, 1.0, 4.0]).is_none());
        let (a, b) = v.segment([0.0, 1.0, 5.0], [0.0, 1.0, -5.0]).unwrap();
        assert!(a.1.is_finite() && b.1.is_finite());
    }

    #[test]
    fn side_view_looks_from_the_right() {
        let mut cam = Camera {
            target: [0.0, 0.0, 0.0],
            ..Camera::default()
        };
        cam.preset(Preset::Side);
        let v = cam.view(800.0, 600.0);
        let (fx, _) = v.project([0.0, 0.0, -1.0]).unwrap();
        let (_, uy) = v.project([0.0, 1.0, 0.0]).unwrap();
        assert!(
            fx > 400.0,
            "forward is to the right when looking from the right side"
        );
        assert!(uy < 300.0);
    }

    #[test]
    fn base_field_of_view_edges_and_range() {
        let rot = yaw_pitch(0.0, 0.0);
        let pos = [0.0, 0.0, 0.0];
        let at = |az: f32, el: f32, d: f32| scale(fov_dir(az, el), d);
        assert!(base_sees(pos, &rot, FOV_V2, at(0.0, 0.0, 3.0)));
        assert!(base_sees(pos, &rot, FOV_V2, at(79.0, 0.0, 3.0)));
        assert!(!base_sees(pos, &rot, FOV_V2, at(81.0, 0.0, 3.0)));
        assert!(base_sees(pos, &rot, FOV_V2, at(0.0, 57.0, 3.0)));
        assert!(!base_sees(pos, &rot, FOV_V2, at(0.0, 58.0, 3.0)));
        assert!(!base_sees(pos, &rot, FOV_V2, at(0.0, 0.0, 7.1)));
        assert!(!base_sees(pos, &rot, FOV_V2, [0.0, 0.0, 1.0]));
        assert!(!base_sees(pos, &rot, FOV_V1, at(61.0, 0.0, 3.0)));
        assert!(base_sees(pos, &rot, FOV_V1, at(59.0, 0.0, 3.0)));
    }

    #[test]
    fn turned_base_sees_where_it_points() {
        let rot = yaw_pitch(90.0, 0.0);
        let f = forward(&rot);
        assert!(close(f[0], -1.0, 1e-5) && close(f[2], 0.0, 1e-5));
        assert!(base_sees([0.0; 3], &rot, FOV_V2, [-3.0, 0.0, 0.0]));
        assert!(!base_sees([0.0; 3], &rot, FOV_V2, [3.0, 0.0, 0.0]));
    }

    #[test]
    fn aim_point_matches_height_and_tilt() {
        let rot = yaw_pitch(0.0, 26.0);
        let aim = aim_on_floor([0.0, 2.15, 0.0], &rot).unwrap();
        let expect = 2.15 / 26f32.to_radians().tan();
        assert!(close(aim[1], 0.0, 1e-4));
        assert!(close(-aim[2], expect, 1e-3), "{aim:?} vs {expect}");
        assert!(aim_on_floor([0.0, 2.0, 0.0], &yaw_pitch(0.0, -5.0)).is_none());
    }

    #[test]
    fn quaternion_matches_axis_angle() {
        let half = 45f32.to_radians();
        let r = rot_from_quat(0.0, half.sin(), 0.0, half.cos());
        let f = forward(&r);
        assert!(close(f[0], -1.0, 1e-5) && close(f[2], 0.0, 1e-5));
    }

    #[test]
    fn bounds_are_whole_metres_with_margin() {
        let b = Bounds::around(&[[0.47, 2.15, -3.7], [-1.64, 1.43, -2.7], [2.2, 0.31, -0.89]]);
        assert_eq!(b.min[0], -3.0);
        assert_eq!(b.max[0], 3.0);
        assert_eq!(b.min[2], -5.0);
        assert_eq!(b.max[2], 0.0);
        assert_eq!(b.min[1], 0.0);
    }

    #[test]
    fn fit_keeps_the_whole_room_on_screen() {
        let b = Bounds::around(&[[0.47, 2.15, -3.7], [-1.64, 1.43, -2.7], [2.2, 0.31, -0.89]]);
        for preset in [Preset::Top, Preset::Front, Preset::Side, Preset::Angled] {
            let mut cam = Camera::default();
            cam.preset(preset);
            cam.fit(&b, 900.0, 600.0);
            let v = cam.view(900.0, 600.0);
            for i in 0..8 {
                let c = [
                    if i & 1 == 0 { b.min[0] } else { b.max[0] },
                    if i & 2 == 0 { b.min[1] } else { b.max[1] },
                    if i & 4 == 0 { b.min[2] } else { b.max[2] },
                ];
                let (x, y) = v.project(c).unwrap();
                assert!(
                    (0.0..=900.0).contains(&x) && (0.0..=600.0).contains(&y),
                    "{preset:?} corner {c:?} at {x},{y}"
                );
            }
        }
    }

    #[test]
    fn coverage_counts_bases() {
        let scene = Scene {
            bases: vec![
                BaseView {
                    pos: [-2.0, 2.0, -2.0],
                    rot: yaw_pitch(-135.0, 30.0),
                    fov: FOV_V2,
                    label: "A".into(),
                },
                BaseView {
                    pos: [2.0, 2.0, 2.0],
                    rot: yaw_pitch(45.0, 30.0),
                    fov: FOV_V2,
                    label: "B".into(),
                },
            ],
            ..Scene::default()
        };
        assert_eq!(scene.coverage([0.0, 1.0, 0.0]), 2);
        assert_eq!(scene.coverage([-3.0, 1.0, -3.0]), 0);
    }

    #[test]
    fn reused_floor_draws_the_same_picture_and_redraws_when_the_view_changes() {
        let base = |pos: V3, yaw: f32| BaseView {
            pos,
            rot: yaw_pitch(yaw, 30.0),
            fov: FOV_V2,
            label: "A".into(),
        };
        let device = |x: f32| DeviceView {
            pos: [x, 1.0, 0.0],
            rot: IDENTITY,
            shape: Shape::Tracker,
            tone: Tone::Clean,
            label: "tracker".into(),
            tracking: true,
            trail: vec![[x - 0.3, 1.0, 0.0], [x, 1.0, 0.0]],
        };
        let mut scene = Scene {
            bases: vec![base([-2.0, 2.0, -2.0], -135.0), base([2.0, 2.0, 2.0], 45.0)],
            devices: vec![device(0.0)],
            ..Scene::default()
        };
        let mut cam = Camera::default();
        let opts = Options::default();
        let mut cache = None;
        let same = |scene: &Scene, cam: &Camera, cache: &mut Option<Backdrop>| {
            let (a, la) = render_with(scene, cam, &opts, 400, 300, cache);
            let (b, lb) = render(scene, cam, &opts, 400, 300);
            assert!(a.data() == b.data());
            let key = |l: &Label| (l.text.clone(), l.x.to_bits(), l.y.to_bits());
            assert_eq!(
                la.iter().map(key).collect::<Vec<_>>(),
                lb.iter().map(key).collect::<Vec<_>>()
            );
            a
        };
        let first = same(&scene, &cam, &mut cache);
        let kept = cache.as_ref().unwrap().pix.data().as_ptr();
        scene.devices = vec![device(0.5)];
        let moved = same(&scene, &cam, &mut cache);
        assert_eq!(cache.as_ref().unwrap().pix.data().as_ptr(), kept);
        assert!(moved.data() != first.data());
        scene.bases[0].pos = [-2.5, 2.0, -2.0];
        same(&scene, &cam, &mut cache);
        assert_ne!(cache.as_ref().unwrap().pix.data().as_ptr(), kept);
        cam.orbit(40.0, 0.0);
        same(&scene, &cam, &mut cache);
        let opts = Options {
            coverage_height: None,
            ..opts
        };
        let (a, _) = render_with(&scene, &cam, &opts, 400, 300, &mut cache);
        assert!(a.data() == render(&scene, &cam, &opts, 400, 300).0.data());
    }

    #[test]
    fn picked_yaw_keeps_bases_out_of_the_line_of_sight() {
        let target = [0.0, 0.8, 0.0];
        let bases = [[-1.2, 2.2, -1.8], [1.2, 2.2, 1.8]];
        let yaw = pick_yaw(&bases, target);
        for b in bases {
            let dir = (b[0] - target[0]).atan2(b[2] - target[2]);
            assert!(wrap(yaw - dir).abs() >= CLEAR_YAW - 1e-4, "{yaw} vs {dir}");
        }
        assert!(wrap(yaw - DEFAULT_YAW).abs() <= 0.6, "{yaw}");
        assert_eq!(pick_yaw(&[], target), DEFAULT_YAW);
        let far = [[3.0, 2.0, -3.0], [-3.0, 2.0, 3.0]];
        assert!((pick_yaw(&far, target) - DEFAULT_YAW).abs() < 0.05);
        for a in [-7.0f32, -3.2, 0.0, 3.2, 7.0] {
            let w = wrap(a);
            assert!(w > -std::f32::consts::PI && w <= std::f32::consts::PI);
            let k = (a - w) / std::f32::consts::TAU;
            assert!((k - k.round()).abs() < 1e-4, "{a} -> {w}");
        }
    }

    #[test]
    fn edge_on_views_keep_grid_labels_apart_and_hide_the_flat_forward_arrow() {
        let scene = Scene::default();
        for preset in [Preset::Top, Preset::Front, Preset::Side, Preset::Angled] {
            let mut cam = Camera::default();
            cam.preset(preset);
            let (_, labels) = render(&scene, &cam, &Options::default(), 600, 400);
            let grid: Vec<_> = labels
                .iter()
                .filter(|l| l.kind == LabelKind::Grid)
                .collect();
            assert!(!grid.is_empty(), "{preset:?}");
            if matches!(preset, Preset::Front | Preset::Side) {
                let texts: std::collections::HashSet<_> = grid.iter().map(|l| &l.text).collect();
                assert_eq!(
                    texts.len(),
                    grid.len(),
                    "{preset:?}: an edge-on axis left its labels"
                );
            }
            if preset == Preset::Top {
                let right = grid.iter().map(|l| l.x).fold(f32::MIN, f32::max);
                let bottom = grid.iter().map(|l| l.y).fold(f32::MIN, f32::max);
                assert!(
                    !grid.iter().any(|l| l.x > right - 1.0 && l.y > bottom - 1.0),
                    "Top: a label sits on the corner both axes share"
                );
            }
            for (i, a) in grid.iter().enumerate() {
                for b in &grid[i + 1..] {
                    assert!(
                        !overlaps(&chip_rect(a, 1.0), &chip_rect(b, 1.0), 0.0),
                        "{preset:?}: '{}' overlaps '{}'",
                        a.text,
                        b.text
                    );
                }
            }
            let forward = labels.iter().any(|l| l.text == "forward");
            assert_eq!(forward, preset != Preset::Front, "{preset:?}");
        }
    }

    #[test]
    fn spread_labels_moves_overlapping_chips_apart_and_leads_back() {
        let chip = |x: f32, y: f32, text: &str| Label {
            x,
            y,
            text: text.into(),
            tone: Tone::Some,
            kind: LabelKind::Device,
        };
        for s in [1.0f32, 1.5] {
            let mut labels = vec![
                chip(100.0 * s, 100.0 * s, "#1 left controller"),
                chip(104.0 * s, 103.0 * s, "#2 tracker ?"),
                chip(300.0 * s, 100.0 * s, "waist"),
                Label {
                    x: 110.0 * s,
                    y: 140.0 * s,
                    text: "B1".into(),
                    tone: Tone::Base,
                    kind: LabelKind::Base,
                },
                Label {
                    x: 290.0 * s,
                    y: 82.0 * s,
                    text: "forward".into(),
                    tone: Tone::Headset,
                    kind: LabelKind::Note,
                },
            ];
            let leaders = spread_labels(&mut labels, s);
            let rects: Vec<[f32; 4]> = labels.iter().map(|l| chip_rect(l, s)).collect();
            for i in 0..rects.len() {
                for j in i + 1..rects.len() {
                    assert!(
                        !overlaps(&rects[i], &rects[j], 0.0),
                        "{i} {j} at scale {s}: {rects:?}"
                    );
                }
            }
            assert_eq!(leaders.len(), 2);
            assert_ne!(labels[2].y, 100.0 * s);
            let (a, b) = *leaders.iter().find(|(a, _)| a.0 == 104.0 * s).unwrap();
            assert_eq!(a, (104.0 * s, 103.0 * s));
            assert!((b.0 - rects[1][0]).abs() < 1e-3);
        }
    }
}

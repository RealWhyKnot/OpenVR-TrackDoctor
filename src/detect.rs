use crate::event::{Kind, TrackState};

#[derive(Clone, Debug)]
pub struct Thresholds {
    pub jump_m: f32,
    pub accel_mps2: f32,
    pub ori_rad_s: f32,
    pub ori_excess_rad_s: f32,
    pub ori_min_rad_s: f32,
    pub drift_m: f32,
    pub max_radius_m: f32,
    pub frozen_ms: u64,
    pub snap_min_ms: u64,
    pub snap_max_ms: u64,
    pub snap_cos: f32,
    pub snap_ratio_lo: f32,
    pub snap_ratio_hi: f32,
    pub suppress_ms: u64,
    pub min_dt_ms: u64,
    pub max_gap_ms: u64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            jump_m: 0.05,
            accel_mps2: 300.0,
            ori_rad_s: 100.0,
            ori_excess_rad_s: 20.0,
            ori_min_rad_s: 30.0,
            drift_m: 0.10,
            max_radius_m: 50.0,
            frozen_ms: 250,
            snap_min_ms: 200,
            snap_max_ms: 2000,
            snap_cos: -0.7,
            snap_ratio_lo: 0.5,
            snap_ratio_hi: 2.0,
            suppress_ms: 500,
            min_dt_ms: 5,
            max_gap_ms: 100,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Frame {
    pub t_ms: u64,
    pub pos: [f32; 3],
    pub vel: [f32; 3],
    pub rot: [[f32; 4]; 3],
    pub ang_speed: f32,
    pub state: TrackState,
    pub valid: bool,
}

#[derive(Default)]
pub struct DevDetect {
    last: Option<Frame>,
    activated_at: u64,
    dropout_since: Option<u64>,
    last_jump: Option<(u64, [f32; 3])>,
    frozen: Option<(u64, bool)>,
}

fn dead_reckoning(s: TrackState) -> bool {
    matches!(
        s,
        TrackState::CalibratingInProgress
            | TrackState::CalibratingOutOfRange
            | TrackState::FallbackRotationOnly
    )
}

pub fn beyond_radius(pos: [f32; 3], th: &Thresholds) -> bool {
    norm(pos) > th.max_radius_m
}

fn predict(last: &Frame, dt: f32) -> [f32; 3] {
    [
        last.pos[0] + last.vel[0] * dt,
        last.pos[1] + last.vel[1] * dt,
        last.pos[2] + last.vel[2] * dt,
    ]
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn norm(v: [f32; 3]) -> f32 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn rot_angle(a: &[[f32; 4]; 3], b: &[[f32; 4]; 3]) -> f32 {
    let mut trace = 0.0;
    for r in 0..3 {
        for c in 0..3 {
            trace += a[r][c] * b[r][c];
        }
    }
    ((trace - 1.0) / 2.0).clamp(-1.0, 1.0).acos()
}

impl DevDetect {
    pub fn on_activated(&mut self, t_ms: u64) {
        self.activated_at = t_ms;
        self.last = None;
        self.dropout_since = None;
        self.last_jump = None;
        self.frozen = None;
    }

    pub fn step(&mut self, f: &Frame, th: &Thresholds) -> Vec<Kind> {
        let mut out = Vec::new();

        if !f.valid || beyond_radius(f.pos, th) {
            self.last = None;
            self.dropout_since = None;
            self.frozen = None;
            return out;
        }
        if f.t_ms < self.activated_at + th.suppress_ms {
            self.last = Some(*f);
            return out;
        }
        if dead_reckoning(f.state) {
            if self.dropout_since.is_none() && self.last.is_some_and(|l| !dead_reckoning(l.state)) {
                self.dropout_since = Some(f.t_ms);
            }
            self.last = Some(*f);
            self.frozen = None;
            return out;
        }
        let Some(last) = self.last else {
            self.last = Some(*f);
            return out;
        };
        let dt_ms = f.t_ms.saturating_sub(last.t_ms);
        if dt_ms < th.min_dt_ms {
            return out;
        }
        if dead_reckoning(last.state) {
            if let Some(t0) = self.dropout_since.take()
                && dt_ms <= th.max_gap_ms
            {
                let correction = norm(sub(f.pos, predict(&last, dt_ms as f32 / 1000.0)));
                if correction > th.drift_m {
                    out.push(Kind::Drift {
                        meters: correction,
                        secs: f.t_ms.saturating_sub(t0) as f32 / 1000.0,
                    });
                }
            }
            self.dropout_since = None;
            self.last = Some(*f);
            return out;
        }
        if f.pos == last.pos && f.rot == last.rot {
            let (t0, emitted) = self.frozen.get_or_insert((last.t_ms, false));
            let held = f.t_ms.saturating_sub(*t0);
            if held >= th.frozen_ms && !*emitted {
                out.push(Kind::PoseFrozen { ms: held });
                *emitted = true;
            }
            self.last = Some(*f);
            return out;
        }
        self.frozen = None;
        if dt_ms > th.max_gap_ms {
            self.last = Some(*f);
            return out;
        }
        let dt = dt_ms as f32 / 1000.0;

        let jvec = sub(f.pos, predict(&last, dt));
        let residual = norm(jvec);
        let accel = norm(sub(f.vel, last.vel)) / dt;
        if residual > th.jump_m || accel > th.accel_mps2 {
            out.push(Kind::Jump {
                meters: residual,
                accel_mps2: accel,
            });
            if let Some((jt, jv)) = self.last_jump {
                let gap = f.t_ms.saturating_sub(jt);
                let (na, nb) = (norm(jv), residual);
                if gap >= th.snap_min_ms
                    && gap <= th.snap_max_ms
                    && na > 0.0
                    && nb > 0.0
                    && dot(jv, jvec) / (na * nb) < th.snap_cos
                    && (nb / na) > th.snap_ratio_lo
                    && (nb / na) < th.snap_ratio_hi
                {
                    out.push(Kind::SnapBack { meters: nb });
                    self.last_jump = None;
                } else {
                    self.last_jump = Some((f.t_ms, jvec));
                }
            } else {
                self.last_jump = Some((f.t_ms, jvec));
            }
        }

        let ori_rate = rot_angle(&last.rot, &f.rot) / dt;
        if ori_rate > th.ori_rad_s
            || (ori_rate > f.ang_speed + th.ori_excess_rad_s && ori_rate > th.ori_min_rad_s)
        {
            out.push(Kind::OrientationJump {
                rad_per_s: ori_rate,
            });
        }

        self.last = Some(*f);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(t_ms: u64, pos: [f32; 3], vel: [f32; 3]) -> Frame {
        Frame {
            t_ms,
            pos,
            vel,
            rot: [
                [1.0, 0.0, 0.0, pos[0]],
                [0.0, 1.0, 0.0, pos[1]],
                [0.0, 0.0, 1.0, pos[2]],
            ],
            ang_speed: 0.0,
            state: TrackState::RunningOk,
            valid: true,
        }
    }

    #[test]
    fn steady_motion_is_clean() {
        let th = Thresholds::default();
        let mut d = DevDetect::default();
        for i in 0..200u64 {
            let x = i as f32 * 0.011;
            let f = frame(1000 + i * 11, [x, 1.0, 0.0], [1.0, 0.0, 0.0]);
            assert!(d.step(&f, &th).is_empty(), "frame {i}");
        }
    }

    #[test]
    fn teleport_fires_jump() {
        let th = Thresholds::default();
        let mut d = DevDetect::default();
        d.step(&frame(1000, [0.0, 1.0, 0.0], [0.0; 3]), &th);
        d.step(&frame(1011, [0.0, 1.0, 0.0], [0.0; 3]), &th);
        let kinds = d.step(&frame(1022, [0.3, 1.0, 0.0], [0.0; 3]), &th);
        assert!(
            kinds.iter().any(|k| matches!(k, Kind::Jump { .. })),
            "{kinds:?}"
        );
    }

    #[test]
    fn snap_back_pairs_opposed_jumps() {
        let th = Thresholds::default();
        let mut d = DevDetect::default();
        d.step(&frame(1000, [0.0, 1.0, 0.0], [0.0; 3]), &th);
        d.step(&frame(1011, [0.2, 1.0, 0.0], [0.0; 3]), &th);
        for i in 0..40u64 {
            d.step(&frame(1022 + i * 11, [0.2, 1.0, 0.0], [0.0; 3]), &th);
        }
        let kinds = d.step(&frame(1473, [0.0, 1.0, 0.0], [0.0; 3]), &th);
        assert!(
            kinds.iter().any(|k| matches!(k, Kind::SnapBack { .. })),
            "{kinds:?}"
        );
    }

    fn dropout(t_ms: u64, pos: [f32; 3]) -> Frame {
        let mut f = frame(t_ms, pos, [0.0; 3]);
        f.state = TrackState::CalibratingOutOfRange;
        f
    }

    #[test]
    fn drift_fires_on_reacquire_after_valid_dropout() {
        let th = Thresholds::default();
        let mut d = DevDetect::default();
        d.step(&frame(1000, [0.0, 1.0, 0.0], [0.0; 3]), &th);
        d.step(&frame(1011, [0.0, 1.0, 0.0], [0.0; 3]), &th);
        let mut t = 1011;
        for i in 1..=60u64 {
            t += 11;
            let kinds = d.step(&dropout(t, [i as f32 * 0.004, 1.0, 0.0]), &th);
            assert!(kinds.is_empty(), "{kinds:?}");
        }
        let kinds = d.step(&frame(t + 11, [0.0, 1.0, 0.0], [0.0; 3]), &th);
        assert_eq!(kinds.len(), 1, "{kinds:?}");
        let Kind::Drift { meters, secs } = kinds[0] else {
            panic!("{kinds:?}");
        };
        assert!((meters - 0.24).abs() < 0.01, "{meters}");
        assert!((secs - 0.66).abs() < 0.001, "{secs}");
    }

    #[test]
    fn small_reacquire_correction_is_silent() {
        let th = Thresholds::default();
        let mut d = DevDetect::default();
        d.step(&frame(1000, [0.0, 1.0, 0.0], [0.0; 3]), &th);
        d.step(&frame(1011, [0.0, 1.0, 0.0], [0.0; 3]), &th);
        d.step(&dropout(1022, [0.01, 1.0, 0.0]), &th);
        let kinds = d.step(&frame(1033, [0.0, 1.0, 0.0], [0.0; 3]), &th);
        assert!(kinds.is_empty(), "{kinds:?}");
    }

    #[test]
    fn boot_calibration_is_not_a_dropout() {
        let th = Thresholds::default();
        let mut d = DevDetect::default();
        d.step(&dropout(1000, [3.0, 1.0, 0.0]), &th);
        d.step(&dropout(1011, [3.0, 1.0, 0.0]), &th);
        let kinds = d.step(&frame(1022, [0.0, 1.0, 0.0], [0.0; 3]), &th);
        assert!(kinds.is_empty(), "{kinds:?}");
    }

    #[test]
    fn running_out_of_range_still_detects_jumps() {
        let th = Thresholds::default();
        let mut d = DevDetect::default();
        let oor = |t: u64, x: f32| {
            let mut f = frame(t, [x, 1.0, 0.0], [0.0; 3]);
            f.state = TrackState::RunningOutOfRange;
            f
        };
        d.step(&oor(1000, 0.0), &th);
        d.step(&oor(1011, 0.0), &th);
        let kinds = d.step(&oor(1022, 0.3), &th);
        assert!(
            kinds.iter().any(|k| matches!(k, Kind::Jump { .. })),
            "{kinds:?}"
        );
    }

    #[test]
    fn parked_device_is_ignored() {
        let th = Thresholds::default();
        let mut d = DevDetect::default();
        d.step(&frame(1000, [0.0, 1.0, 0.0], [0.0; 3]), &th);
        d.step(&frame(1011, [0.0, 1.0, 0.0], [0.0; 3]), &th);
        for (i, pos) in [[9001.0, 9001.0, 9001.0], [8913.0, 9001.0, 9001.0]]
            .into_iter()
            .enumerate()
        {
            let kinds = d.step(&frame(1022 + i as u64 * 11, pos, [0.0; 3]), &th);
            assert!(kinds.is_empty(), "{kinds:?}");
        }
        let kinds = d.step(&frame(1044, [0.0, 1.0, 0.0], [0.0; 3]), &th);
        assert!(kinds.is_empty(), "{kinds:?}");
        let kinds = d.step(&frame(1055, [0.0, 1.0, 0.0], [0.0; 3]), &th);
        assert!(kinds.is_empty(), "{kinds:?}");
    }

    #[test]
    fn frozen_pose_fires_once() {
        let th = Thresholds::default();
        let mut d = DevDetect::default();
        d.step(&frame(1000, [0.5, 1.0, 0.0], [0.0; 3]), &th);
        d.step(&frame(1011, [0.51, 1.0, 0.0], [0.0; 3]), &th);
        let mut fired = 0;
        for i in 0..60u64 {
            let kinds = d.step(&frame(1022 + i * 11, [0.51, 1.0, 0.0], [0.0; 3]), &th);
            fired += kinds
                .iter()
                .filter(|k| matches!(k, Kind::PoseFrozen { .. }))
                .count();
        }
        assert_eq!(fired, 1);
        let kinds = d.step(&frame(1022 + 60 * 11, [0.52, 1.0, 0.0], [0.0; 3]), &th);
        assert!(
            !kinds.iter().any(|k| matches!(k, Kind::PoseFrozen { .. })),
            "{kinds:?}"
        );
    }

    #[test]
    fn activation_suppression_holds() {
        let th = Thresholds::default();
        let mut d = DevDetect::default();
        d.on_activated(1000);
        d.step(&frame(1100, [0.0, 1.0, 0.0], [0.0; 3]), &th);
        let kinds = d.step(&frame(1111, [0.4, 1.0, 0.0], [0.0; 3]), &th);
        assert!(kinds.is_empty(), "{kinds:?}");
    }
}

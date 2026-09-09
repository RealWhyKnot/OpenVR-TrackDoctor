use crate::event::{Kind, TrackState};

#[derive(Clone, Debug)]
pub struct Thresholds {
    pub jump_m: f32,
    pub accel_mps2: f32,
    pub ori_rad_s: f32,
    pub ori_excess_rad_s: f32,
    pub drift_m: f32,
    pub drift_min_ms: u64,
    pub drift_vel_mps: f32,
    pub snap_min_ms: u64,
    pub snap_max_ms: u64,
    pub snap_cos: f32,
    pub snap_ratio_lo: f32,
    pub snap_ratio_hi: f32,
    pub suppress_ms: u64,
    pub max_gap_ms: u64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            jump_m: 0.05,
            accel_mps2: 300.0,
            ori_rad_s: 100.0,
            ori_excess_rad_s: 20.0,
            drift_m: 0.10,
            drift_min_ms: 500,
            drift_vel_mps: 0.1,
            snap_min_ms: 200,
            snap_max_ms: 2000,
            snap_cos: -0.7,
            snap_ratio_lo: 0.5,
            snap_ratio_hi: 2.0,
            suppress_ms: 500,
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
    armed: Option<(u64, [f32; 3], bool)>,
    last_jump: Option<(u64, [f32; 3])>,
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
        self.armed = None;
        self.last_jump = None;
    }

    pub fn step(&mut self, f: &Frame, th: &Thresholds) -> Vec<Kind> {
        let mut out = Vec::new();

        if f.state != TrackState::RunningOk && f.valid {
            if self.armed.is_none() {
                self.armed = Some((f.t_ms, f.pos, false));
            }
        } else if f.state == TrackState::RunningOk {
            self.armed = None;
        }

        if !f.valid || f.state.is_calibrating() {
            self.last = None;
            return out;
        }
        if f.t_ms < self.activated_at + th.suppress_ms {
            self.last = Some(*f);
            return out;
        }
        let Some(last) = self.last else {
            self.last = Some(*f);
            return out;
        };
        let dt_ms = f.t_ms.saturating_sub(last.t_ms);
        if dt_ms == 0 || dt_ms > th.max_gap_ms {
            self.last = Some(*f);
            return out;
        }
        let dt = dt_ms as f32 / 1000.0;

        let predicted = [
            last.pos[0] + last.vel[0] * dt,
            last.pos[1] + last.vel[1] * dt,
            last.pos[2] + last.vel[2] * dt,
        ];
        let jvec = sub(f.pos, predicted);
        let residual = norm(jvec);
        let accel = norm(sub(f.vel, last.vel)) / dt;
        if residual > th.jump_m || accel > th.accel_mps2 {
            out.push(Kind::Jump { meters: residual, accel_mps2: accel });
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
        if ori_rate > th.ori_rad_s || ori_rate > f.ang_speed + th.ori_excess_rad_s && ori_rate > 30.0 {
            out.push(Kind::OrientationJump { rad_per_s: ori_rate });
        }

        if let Some((t0, p0, emitted)) = self.armed
            && !emitted {
                let disp = norm(sub(f.pos, p0));
                let held = f.t_ms.saturating_sub(t0);
                if disp > th.drift_m && held > th.drift_min_ms && norm(f.vel) < th.drift_vel_mps {
                    out.push(Kind::Drift { meters: disp, secs: held as f32 / 1000.0 });
                    self.armed = Some((t0, p0, true));
                }
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
            rot: [[1.0, 0.0, 0.0, pos[0]], [0.0, 1.0, 0.0, pos[1]], [0.0, 0.0, 1.0, pos[2]]],
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
        assert!(kinds.iter().any(|k| matches!(k, Kind::Jump { .. })), "{kinds:?}");
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
        assert!(kinds.iter().any(|k| matches!(k, Kind::SnapBack { .. })), "{kinds:?}");
    }

    #[test]
    fn drift_fires_during_optical_loss() {
        let th = Thresholds::default();
        let mut d = DevDetect::default();
        d.step(&frame(1000, [0.0, 1.0, 0.0], [0.0; 3]), &th);
        for i in 1..100u64 {
            let mut f = frame(1000 + i * 11, [i as f32 * 0.002, 1.0, 0.0], [0.0; 3]);
            f.state = TrackState::FallbackRotationOnly;
            let kinds = d.step(&f, &th);
            if kinds.iter().any(|k| matches!(k, Kind::Drift { .. })) {
                return;
            }
        }
        panic!("drift never fired");
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

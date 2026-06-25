use nalgebra::{Matrix3, Rotation3, RowVector3, Vector3};
use crate::calibration::transform::{TransformD, average_quaternions};
use crate::error::CalibrationError;

#[derive(Debug, Clone)]
pub struct PoseSample {
    pub source_position: Vector3<f64>,
    pub source_orientation: nalgebra::UnitQuaternion<f64>,
    pub target_position: Vector3<f64>,
    pub target_orientation: nalgebra::UnitQuaternion<f64>,
}

impl PoseSample {
    pub fn from_transforms(source: &TransformD, target: &TransformD) -> Self {
        Self {
            source_position: source.origin,
            source_orientation: source.rotation_quaternion(),
            target_position: target.origin,
            target_orientation: target.rotation_quaternion(),
        }
    }

    pub fn validate(&self) -> Result<(), CalibrationError> {
        if !self.source_position.iter().all(|&v| v.is_finite()) {
            return Err(CalibrationError::InvalidPoseData("Source position NaN/Inf".into()));
        }
        if !self.target_position.iter().all(|&v| v.is_finite()) {
            return Err(CalibrationError::InvalidPoseData("Target position NaN/Inf".into()));
        }
        let qnorm = |q: &nalgebra::UnitQuaternion<f64>| {
            (q.w * q.w + q.i * q.i + q.j * q.j + q.k * q.k).sqrt()
        };
        if (qnorm(&self.source_orientation) - 1.0).abs() > QUAT_NORM_TOLERANCE {
            return Err(CalibrationError::InvalidPoseData("Source quaternion not normalized".into()));
        }
        if (qnorm(&self.target_orientation) - 1.0).abs() > QUAT_NORM_TOLERANCE {
            return Err(CalibrationError::InvalidPoseData("Target quaternion not normalized".into()));
        }
        Ok(())
    }
}

#[derive(Clone)]
struct Poses {
    source: TransformD,
    target: TransformD,
}

impl Poses {
    fn from_sample(s: &PoseSample) -> Self {
        Self {
            source: TransformD::from_position_orientation(s.source_position, s.source_orientation),
            target: TransformD::from_position_orientation(s.target_position, s.target_orientation),
        }
    }
}

pub struct SampleCollector {
    samples: Vec<PoseSample>,
    target_count: u32,
}

impl SampleCollector {
    pub fn new(target_count: u32) -> Self {
        Self {
            samples: Vec::with_capacity(target_count as usize),
            target_count,
        }
    }

    pub fn add_sample(&mut self, sample: PoseSample) -> Result<(), CalibrationError> {
        sample.validate()?;
        self.samples.push(sample);
        Ok(())
    }

    #[cfg(test)]
    pub fn add_sample_unchecked(&mut self, sample: PoseSample) {
        self.samples.push(sample);
    }

    pub const fn sample_count(&self) -> u32 { self.samples.len() as u32 }
    pub const fn is_complete(&self) -> bool { self.samples.len() >= self.target_count as usize }
    pub const fn progress(&self) -> (u32, u32) { (self.sample_count(), self.target_count) }

    // Returns (offset, `median_error_degrees`, `axis_diversity`).
    // Needs >=3 samples of varied motion; a degenerate (collinear) set fails the solve.
    pub fn compute_calibration(&self) -> Result<(TransformD, f64, f64), CalibrationError> {
        if self.samples.len() < 3 {
            return Err(CalibrationError::InsufficientSamples {
                required: 3,
                collected: self.samples.len(),
            });
        }

        let poses: Vec<Poses> = self.samples.iter().map(Poses::from_sample).collect();

        let (rotation, axis_diversity) = solve_rotation(&poses)?;
        let translation = solve_translation(&poses, &rotation)?;

        let median_error = grip_consistency(&self.samples, &rotation);

        Ok((
            TransformD { basis: rotation, origin: translation },
            median_error,
            axis_diversity,
        ))
    }
}

// --- Rotation: Wahba on rotation-axis correspondences ---

// Wahba's SVD on rotation-axis pairs (no centroiding).
const MIN_ROTATION_RAD: f64 = 0.4; // ~23°, reject noise from small movements
const MIN_AXIS_NORM: f64 = 0.01; // reject degenerate near-180° rotations
const QUAT_NORM_TOLERANCE: f64 = 1e-6;

#[allow(clippy::many_single_char_names)]
fn solve_rotation(poses: &[Poses]) -> Result<(Rotation3<f64>, f64), CalibrationError> {
    let n = poses.len();
    let mut h = Matrix3::<f64>::zeros();
    let mut count = 0usize;

    for i in 0..n {
        for j in 0..i {
            let ds = poses[i].source.basis * poses[j].source.basis.transpose();
            let dt = poses[i].target.basis * poses[j].target.basis.transpose();

            let angle_s = rot_angle(ds.matrix());
            let angle_t = rot_angle(dt.matrix());
            let axis_s = rot_axis(ds.matrix());
            let axis_t = rot_axis(dt.matrix());

            if angle_s < MIN_ROTATION_RAD || angle_t < MIN_ROTATION_RAD
                || axis_s.norm() < MIN_AXIS_NORM || axis_t.norm() < MIN_AXIS_NORM
            {
                continue;
            }

            // Accumulate H = Σ source_dir · target_dir^T directly (stack-allocated 3×3)
            let s = Vector3::new(axis_s[0], axis_s[1], axis_s[2]).normalize();
            let t = Vector3::new(axis_t[0], axis_t[1], axis_t[2]).normalize();
            h += s * t.transpose();
            count += 1;
        }
    }

    if count == 0 {
        return Err(CalibrationError::InsufficientSamples { required: 1, collected: 0 });
    }

    let svd = h.svd(true, true);
    let u = svd.u.ok_or(CalibrationError::SvdFailed)?;
    let v = svd.v_t.ok_or(CalibrationError::SvdFailed)?.transpose();

    let svals = &svd.singular_values;
    let diversity = (svals[2] / svals[0].max(1e-12)).clamp(0.0, 1.0);

    // Reflection guard
    let d = (u * v.transpose()).determinant();
    let m = Matrix3::new(1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, d.signum());

    // R = U·M·V^T maps target→source (offset convention: O × T = S)
    let r = u * m * v.transpose();

    Ok((Rotation3::from_matrix_unchecked(r), diversity))
}

fn rot_axis(m: &Matrix3<f64>) -> RowVector3<f64> {
    RowVector3::new(m[(2,1)] - m[(1,2)], m[(0,2)] - m[(2,0)], m[(1,0)] - m[(0,1)])
}

fn rot_angle(m: &Matrix3<f64>) -> f64 {
    (((m[(0,0)] + m[(1,1)] + m[(2,2)] - 1.0) / 2.0).clamp(-1.0, 1.0)).acos()
}

// --- Translation: Cholesky on 3×3 normal equations ---

fn solve_translation(
    poses: &[Poses],
    rotation: &Rotation3<f64>,
) -> Result<Vector3<f64>, CalibrationError> {
    let mut ata = Matrix3::<f64>::zeros();
    let mut atb = Vector3::<f64>::zeros();
    let mut n = 0usize;

    for i in 0..poses.len() {
        let si = &poses[i];
        let ri = rotation * si.target.basis;
        let pi = rotation * si.target.origin;

        for sj in poses.iter().take(i) {
            let rj = rotation * sj.target.basis;
            let pj = rotation * sj.target.origin;

            let diff_i = si.source.origin - pi;
            let diff_j = sj.source.origin - pj;

            // Source-frame equations
            let inv_src_i = si.source.basis.transpose();
            let inv_src_j = sj.source.basis.transpose();
            let a: Matrix3<f64> = inv_src_j.matrix() - inv_src_i.matrix();
            let b = inv_src_j * diff_j - inv_src_i * diff_i;
            ata += a.transpose() * a;
            atb += a.transpose() * b;
            n += 3;

            // Target-frame equations
            let inv_tgt_i = ri.transpose();
            let inv_tgt_j = rj.transpose();
            let a: Matrix3<f64> = inv_tgt_j.matrix() - inv_tgt_i.matrix();
            let b = inv_tgt_j * diff_j - inv_tgt_i * diff_i;
            ata += a.transpose() * a;
            atb += a.transpose() * b;
            n += 3;
        }
    }

    if n == 0 {
        return Err(CalibrationError::InsufficientSamples { required: 1, collected: 0 });
    }

    nalgebra::Cholesky::new(ata)
        .map(|c| c.solve(&atb))
        .ok_or(CalibrationError::SvdFailed)
}

// --- Quality metric ---

fn grip_consistency(samples: &[PoseSample], rotation: &Rotation3<f64>) -> f64 {
    let q = nalgebra::UnitQuaternion::from_rotation_matrix(rotation);
    let offsets: Vec<_> = samples.iter()
        .map(|s| s.source_orientation.inverse() * q * s.target_orientation)
        .collect();

    let reference = average_quaternions(&offsets).unwrap_or(offsets[0]);
    let mut errors: Vec<f64> = offsets.iter()
        .map(|o| reference.rotation_to(o).angle().to_degrees())
        .collect();
    errors.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    errors[errors.len() / 2]
}

// --- Tests ---

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(sp: Vector3<f64>, so: nalgebra::UnitQuaternion<f64>,
              tp: Vector3<f64>, to: nalgebra::UnitQuaternion<f64>) -> PoseSample {
        PoseSample {
            source_position: sp, source_orientation: so,
            target_position: tp, target_orientation: to,
        }
    }

    #[test]
    fn test_identity_calibration() {
        let mut c = SampleCollector::new(10);
        for i in 0..10 {
            let r = nalgebra::UnitQuaternion::from_euler_angles(f64::from(i) * 0.5, 0.0, 0.0);
            let p = Vector3::new(f64::from(i) * 0.1, 0.0, 0.0);
            c.add_sample_unchecked(sample(p, r, p, r));
        }
        let (result, _, _) = c.compute_calibration().unwrap();
        assert!(result.origin.norm() < 0.1);
    }

    #[test]
    fn test_pure_rotation_offset() {
        let mut c = SampleCollector::new(20);
        let offset = nalgebra::UnitQuaternion::from_euler_angles(0.0, std::f64::consts::FRAC_PI_2, 0.0);
        let offset_r = Rotation3::from(offset);

        for i in 0..20 {
            let t = f64::from(i) * 0.2;
            let sr = nalgebra::UnitQuaternion::from_euler_angles(t.sin()*0.5, t.cos()*0.3, t*0.2);
            let sp = Vector3::new(t.cos()*0.5, t.sin()*0.3, t*0.1);
            c.add_sample_unchecked(sample(sp, sr, offset_r.inverse()*sp, offset.inverse()*sr));
        }
        let (result, _, _) = c.compute_calibration().unwrap();
        let dot = offset.quaternion().dot(
            nalgebra::UnitQuaternion::from_rotation_matrix(&result.basis).quaternion()
        ).abs();
        assert!(dot > 0.99, "dot: {dot}");
        assert!(result.origin.norm() < 0.1);
    }

    #[test]
    fn test_combined_offset() {
        let mut c = SampleCollector::new(30);
        let offset_r = nalgebra::UnitQuaternion::from_euler_angles(0.3, 0.5, 0.2);
        let offset_m = Rotation3::from(offset_r);
        let offset_t = Vector3::new(1.0, -0.5, 2.0);

        for i in 0..30 {
            let t = f64::from(i) * 0.15;
            let sr = nalgebra::UnitQuaternion::from_euler_angles(t.sin()*0.6, t.cos()*0.4, (t*1.5).sin()*0.3);
            let sp = Vector3::new(t.cos()*0.8, t.sin()*0.6+1.5, (t*0.5).cos()*0.4);
            let tr = offset_r.inverse() * sr;
            let tp = offset_m.inverse() * (sp - offset_t);
            c.add_sample_unchecked(sample(sp, sr, tp, tr));
        }
        let (result, _, _) = c.compute_calibration().unwrap();
        let dot = offset_r.quaternion().dot(
            nalgebra::UnitQuaternion::from_rotation_matrix(&result.basis).quaternion()
        ).abs();
        assert!(dot > 0.95, "dot: {dot}");
        assert!((result.origin - offset_t).norm() < 0.5);
    }

    #[test]
    fn test_insufficient_samples() {
        let mut c = SampleCollector::new(10);
        for i in 0..2 {
            let p = Vector3::new(f64::from(i), 0.0, 0.0);
            c.add_sample_unchecked(sample(p, nalgebra::UnitQuaternion::identity(), p, nalgebra::UnitQuaternion::identity()));
        }
        assert!(matches!(c.compute_calibration(), Err(CalibrationError::InsufficientSamples { .. })));
    }

    #[test]
    fn test_sample_validation() {
        let mut c = SampleCollector::new(1);
        let bad = PoseSample {
            source_position: Vector3::new(f64::NAN, 0.0, 0.0),
            source_orientation: nalgebra::UnitQuaternion::identity(),
            target_position: Vector3::zeros(),
            target_orientation: nalgebra::UnitQuaternion::identity(),
        };
        assert!(matches!(c.add_sample(bad), Err(CalibrationError::InvalidPoseData(_))));
    }

    #[test]
    fn test_progress_tracking() {
        let mut c = SampleCollector::new(5);
        assert_eq!(c.progress(), (0, 5));
        for i in 0..5 {
            let p = Vector3::new(f64::from(i), 0.0, 0.0);
            c.add_sample_unchecked(sample(p, nalgebra::UnitQuaternion::identity(), p, nalgebra::UnitQuaternion::identity()));
        }
        assert_eq!(c.progress(), (5, 5));
        assert!(c.is_complete());
    }
}

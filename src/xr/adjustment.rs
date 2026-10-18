use super::{MessageSender, StepError};
use crate::calibration::CalibrationMessage;
use crate::monado::ResetTarget;
use libmotoc::{CalibratorData, OffsetType, SavedCalibration, TransformD, UNIT};
use nalgebra::Rotation3;

pub(super) fn require_normal_profile(saved: Option<&SavedCalibration>) -> Result<(), StepError> {
    if saved.is_some_and(|saved| matches!(saved.offset_type, OffsetType::Device)) {
        Err(StepError::Backend(
            "Adjust is unavailable for continuous calibration; run a normal calibration first"
                .into(),
        ))
    } else {
        Ok(())
    }
}

pub(super) fn check_adjustment_profile(data: &CalibratorData<'_>) -> Result<(), StepError> {
    let saved = super::optional_profile(data.load_calibration(super::DEFAULT_PROFILE))?;
    require_normal_profile(saved.as_ref())
}

pub(super) fn save_adjustment(
    data: &CalibratorData<'_>,
    target: ResetTarget,
) -> Result<(), StepError> {
    let ResetTarget::TrackingOrigin(target_origin) = target else {
        return Err(StepError::Backend(
            "The motoc calibration format cannot save STAGE/LOCAL offsets".into(),
        ));
    };
    // Keep the reference from the existing calibration, independent of the
    // main window's current Source selection. Only the selected Target is edited.
    let saved = data
        .load_calibration(super::DEFAULT_PROFILE)
        .map_err(|error| {
            StepError::Backend(format!(
                "Cannot load the last calibration; calibrate before saving adjustments: {error}"
            ))
        })?;
    require_normal_profile(Some(&saved))?;
    let find_name = |name: &str| {
        data.tracking_origins
            .iter()
            .position(|origin| origin.name == name)
            .ok_or_else(|| StepError::Backend(format!("Saved tracking origin {name:?} not found")))
    };
    let dst = data
        .tracking_origins
        .iter()
        .position(|origin| origin.id == target_origin)
        .ok_or_else(|| StepError::Backend(format!("Tracking origin {target_origin} not found")))?;
    let src = find_name(&saved.src)?;
    let saved_dst = find_name(&saved.dst)?;
    if saved_dst != dst || src == dst {
        return Err(StepError::Backend(
            "The last calibration does not match this Target; calibrate it before saving adjustments".into(),
        ));
    }
    let source = TransformD::from(
        data.tracking_origins[src]
            .get_offset()
            .map_err(libmotoc::Error::from)?,
    );
    let target = TransformD::from(
        data.tracking_origins[dst]
            .get_offset()
            .map_err(libmotoc::Error::from)?,
    );
    // Match SampledMethod's normal calibration save path. save_calibration
    // expects vector indices, while commands and Monado expose origin IDs.
    data.save_calibration(
        super::DEFAULT_PROFILE,
        src,
        dst,
        saved_origin_offset(target, source),
        OffsetType::TrackingOrigin,
    )?;
    Ok(())
}

fn saved_origin_offset(target: TransformD, source: TransformD) -> TransformD {
    target * source.inverse()
}

// Match motoc tui's apply_adjust: world-axis translations and pre-multiplied
// rotations, with meters for position and degrees for angles.
fn apply_delta(mut offset: TransformD, axis: usize, delta: f64) -> Result<TransformD, StepError> {
    if !delta.is_finite() {
        return Err(StepError::Backend("Adjustment must be finite".into()));
    }
    match axis {
        0..=2 => offset.origin[axis] += delta,
        3..=5 => {
            let axis = match axis {
                3 => &UNIT.XU,
                4 => &UNIT.YU,
                _ => &UNIT.ZU,
            };
            offset.basis = Rotation3::from_axis_angle(axis, delta.to_radians()) * offset.basis;
        }
        _ => return Err(StepError::Backend("Invalid adjustment axis".into())),
    }
    Ok(offset)
}

pub(super) fn handle_adjustment<S: MessageSender>(
    data: &CalibratorData<'_>,
    target: ResetTarget,
    delta: Option<(usize, f64)>,
    msg_tx: &S,
) -> Result<(), StepError> {
    check_adjustment_profile(data)?;
    let origin = match target {
        ResetTarget::TrackingOrigin(id) => Some(
            data.tracking_origins
                .iter()
                .find(|origin| origin.id == id)
                .ok_or_else(|| StepError::Backend(format!("Tracking origin {id} not found")))?,
        ),
        _ => None,
    };
    let space = match target {
        ResetTarget::Local => libmonado::ReferenceSpaceType::Local,
        _ => libmonado::ReferenceSpaceType::Stage,
    };
    let read = || {
        if let Some(origin) = origin {
            origin.get_offset()
        } else {
            data.monado.get_reference_space_offset(space)
        }
        .map(TransformD::from)
        .map_err(libmotoc::Error::from)
        .map_err(StepError::from)
    };
    let mut offset = read()?;
    if let Some((axis, delta)) = delta {
        offset = apply_delta(offset, axis, delta)?;
        if let Some(origin) = origin {
            origin.set_offset(offset.into())
        } else {
            data.monado.set_reference_space_offset(space, offset.into())
        }
        .map_err(libmotoc::Error::from)?;
        offset = read()?;
    }
    let (pitch, yaw, roll) = offset.basis.euler_angles();
    let _ = msg_tx.send(CalibrationMessage::AdjustmentUpdated {
        target,
        values: [
            offset.origin.x,
            offset.origin.y,
            offset.origin.z,
            pitch.to_degrees(),
            yaw.to_degrees(),
            roll.to_degrees(),
        ],
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continuous_profiles_cannot_be_adjusted_or_converted_by_save() {
        let mut saved = SavedCalibration {
            offset_type: OffsetType::Device,
            src: "headset".into(),
            dst: "tracker".into(),
            offset: TransformD::default(),
        };
        assert!(require_normal_profile(Some(&saved)).is_err());
        saved.offset_type = OffsetType::TrackingOrigin;
        assert!(require_normal_profile(Some(&saved)).is_ok());
        assert!(require_normal_profile(None).is_ok());
    }

    #[test]
    fn saved_adjustment_restores_with_a_rotated_and_translated_source() {
        let source = apply_delta(TransformD::default(), 4, 57.0).unwrap();
        let source = apply_delta(source, 0, 1.2).unwrap();
        let source = apply_delta(source, 2, -0.7).unwrap();
        let target = apply_delta(TransformD::default(), 3, -23.0).unwrap();
        let target = apply_delta(target, 1, 0.03).unwrap();
        let target = apply_delta(target, 2, 2.0).unwrap();
        let saved = libmotoc::SavedCalibration {
            offset_type: OffsetType::TrackingOrigin,
            src: "source".into(),
            dst: "target".into(),
            offset: saved_origin_offset(target, source),
        };
        let restored = super::super::restored_origin_offset(&saved, source);
        assert!((restored.origin - target.origin).norm() < 1e-10);
        assert!((restored.basis.matrix() - target.basis.matrix()).norm() < 1e-10);

        // Reopening and saving without further edits must not accumulate offsets.
        let saved_again = saved_origin_offset(restored, source);
        assert!((saved_again.origin - saved.offset.origin).norm() < 1e-10);
        assert!((saved_again.basis.matrix() - saved.offset.basis.matrix()).norm() < 1e-10);
    }

    #[test]
    fn translation_preserves_rotation_and_other_axes() {
        let offset = apply_delta(TransformD::default(), 4, 45.0).unwrap();
        let adjusted = apply_delta(offset, 0, 0.01).unwrap();
        assert!((adjusted.origin.x - 0.01).abs() < 1e-10);
        assert_eq!(adjusted.origin.y, offset.origin.y);
        assert_eq!(adjusted.origin.z, offset.origin.z);
        assert_eq!(adjusted.basis, offset.basis);
    }

    #[test]
    fn rotation_uses_degrees_and_world_axes_without_rotating_translation() {
        let offset = apply_delta(TransformD::default(), 0, 2.0).unwrap();
        let offset = apply_delta(offset, 3, 90.0).unwrap();
        let adjusted = apply_delta(offset, 4, 90.0).unwrap();
        let expected = Rotation3::from_axis_angle(&UNIT.YU, std::f64::consts::FRAC_PI_2)
            * Rotation3::from_axis_angle(&UNIT.XU, std::f64::consts::FRAC_PI_2);
        assert!((adjusted.basis.matrix() - expected.matrix()).norm() < 1e-10);
        assert_eq!(adjusted.origin, offset.origin);
        let restored = apply_delta(adjusted, 4, -90.0).unwrap();
        assert!((restored.basis.matrix() - offset.basis.matrix()).norm() < 1e-10);
    }

    #[test]
    fn invalid_adjustments_are_rejected() {
        for (axis, delta) in [(6, 0.01), (0, f64::NAN), (3, f64::INFINITY)] {
            assert!(apply_delta(TransformD::default(), axis, delta).is_err());
        }
    }
}

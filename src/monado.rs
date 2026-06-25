use libmonado::{Monado, Pose, MndProperty};
use crate::error::MonadoError;
use crate::ui::{Device, Category};

pub struct MonadoConnection {
    monado: Monado,
}

impl MonadoConnection {
    pub fn connect() -> Result<Self, MonadoError> {
        match Monado::auto_connect() {
            Ok(monado) => Ok(Self { monado }),
            Err(e) => Err(MonadoError::ConnectionFailed(e)),
        }
    }

    pub fn enumerate_devices(&self) -> Result<Vec<Category>, MonadoError> {
        let origins: Vec<_> = self.monado.tracking_origins()
            .map_err(|e| MonadoError::EnumerationFailed(format!("{e:?}")))?
            .into_iter()
            .collect();

        let devices = self.monado.devices()
            .map_err(|e| MonadoError::EnumerationFailed(format!("{e:?}")))?;

        // Group devices by tracking origin
        let mut categories: Vec<Category> = Vec::new();

        for origin in &origins {
            categories.push(Category {
                index: origin.id,
                name: origin.name.clone(),
                devices: Vec::new(),
            });
        }

        if categories.is_empty() {
            categories.push(Category {
                index: 0,
                name: "Default".to_string(),
                devices: Vec::new(),
            });
        }

        for device in devices {
            let origin_index = device.get_info_u32(MndProperty::PropertyTrackingOriginU32)
                .unwrap_or(0);

            let cat_idx = categories.iter()
                .position(|c| c.index == origin_index)
                .unwrap_or(0);

            let serial = device.serial().unwrap_or_default();

            let (battery_charge, battery_charging) = match device.battery_status() {
                Ok(status) if status.present => (Some(status.charge), status.charging),
                _ => (None, false),
            };

            let dev = Device {
                name: device.name.clone(),
                serial,
                description: String::new(),
                category: categories[cat_idx].name.clone(),
                category_index: categories[cat_idx].index,
                device_index: device.index,
                battery_charge,
                battery_charging,
            };
            categories[cat_idx].devices.push(dev);
        }

        Ok(categories)
    }

    pub fn reset_tracking_origin(&self, origin_index: u32) -> Result<(), MonadoError> {
        let origins = self.monado.tracking_origins()
            .map_err(|e| MonadoError::TrackingOriginFailed(format!("{e:?}")))?;

        let origin = origins.into_iter()
            .find(|o| o.id == origin_index)
            .ok_or(MonadoError::InvalidDeviceId(origin_index))?;

        let identity_pose = Pose {
            position: mint::Vector3 { x: 0.0, y: 0.0, z: 0.0 },
            orientation: mint::Quaternion {
                v: mint::Vector3 { x: 0.0, y: 0.0, z: 0.0 },
                s: 1.0,
            },
        };

        origin.set_offset(identity_pose)
            .map_err(|e| MonadoError::ApplyOffsetFailed(format!("{e:?}")))
    }

    pub fn apply_offset(&self, origin_index: u32, position: [f64; 3], orientation: [f64; 4]) -> Result<(), MonadoError> {
        use crate::calibration::TransformD;
        use nalgebra::{UnitQuaternion, Quaternion, Vector3};

        let origins = self.monado.tracking_origins()
            .map_err(|e| MonadoError::TrackingOriginFailed(format!("{e:?}")))?;

        let origin = origins.into_iter()
            .find(|o| o.id == origin_index)
            .ok_or(MonadoError::InvalidDeviceId(origin_index))?;

        let current_offset = origin.get_offset()
            .map_err(|e| MonadoError::TrackingOriginFailed(format!("{e:?}")))?;

        let current = TransformD::from_position_orientation(
            Vector3::new(
                f64::from(current_offset.position.x),
                f64::from(current_offset.position.y),
                f64::from(current_offset.position.z),
            ),
            UnitQuaternion::from_quaternion(Quaternion::new(
                f64::from(current_offset.orientation.s),
                f64::from(current_offset.orientation.v.x),
                f64::from(current_offset.orientation.v.y),
                f64::from(current_offset.orientation.v.z),
            )),
        );

        // Convert new calibration offset to TransformD
        let new_offset = TransformD::from_position_orientation(
            Vector3::new(position[0], position[1], position[2]),
            UnitQuaternion::from_quaternion(Quaternion::new(
                orientation[3], // w
                orientation[0], // x
                orientation[1], // y
                orientation[2], // z
            )),
        );

        let full_offset = new_offset.mul(&current);

        let pose = Pose {
            position: mint::Vector3 {
                x: full_offset.origin.x as f32,
                y: full_offset.origin.y as f32,
                z: full_offset.origin.z as f32,
            },
            orientation: mint::Quaternion {
                v: mint::Vector3 {
                    x: full_offset.orientation_f64()[0] as f32,
                    y: full_offset.orientation_f64()[1] as f32,
                    z: full_offset.orientation_f64()[2] as f32,
                },
                s: full_offset.orientation_f64()[3] as f32,
            },
        };

        origin.set_offset(pose)
            .map_err(|e| MonadoError::ApplyOffsetFailed(format!("{e:?}")))
    }

    // STAGE offset for world-frame transform.
    pub fn get_stage_offset(&self) -> Result<([f64; 3], [f64; 4]), MonadoError> {
        let offset = self.monado.get_reference_space_offset(libmonado::ReferenceSpaceType::Stage)
            .map_err(|e| MonadoError::TrackingOriginFailed(format!("{e:?}")))?;

        let position = [
            f64::from(offset.position.x),
            f64::from(offset.position.y),
            f64::from(offset.position.z),
        ];
        let orientation = [
            f64::from(offset.orientation.v.x),
            f64::from(offset.orientation.v.y),
            f64::from(offset.orientation.v.z),
            f64::from(offset.orientation.s),
        ];

        Ok((position, orientation))
    }

    // Set floor level. Converts from STAGE to native coords for idempotency.
    pub fn set_floor_absolute(&self, measured_floor_y: f64) -> Result<(), MonadoError> {
        let current = self.monado.get_reference_space_offset(libmonado::ReferenceSpaceType::Stage)
            .map_err(|e| MonadoError::TrackingOriginFailed(format!("{e:?}")))?;

        // Convert measured floor (in STAGE coords) to native coords
        // native = stage + offset, so native_floor = measured + current_offset
        let floor_in_native = measured_floor_y + f64::from(current.position.y);

        let pose = Pose {
            position: mint::Vector3 {
                x: current.position.x,
                y: floor_in_native as f32,
                z: current.position.z,
            },
            orientation: current.orientation,
        };

        self.monado.set_reference_space_offset(libmonado::ReferenceSpaceType::Stage, pose)
            .map_err(|e| MonadoError::ApplyOffsetFailed(format!("{e:?}")))
    }

    pub fn reset_floor(&self) -> Result<(), MonadoError> {
        let current = self.monado.get_reference_space_offset(libmonado::ReferenceSpaceType::Stage)
            .map_err(|e| MonadoError::TrackingOriginFailed(format!("{e:?}")))?;

        let pose = Pose {
            position: mint::Vector3 {
                x: current.position.x,
                y: 0.0,  // Reset Y to 0
                z: current.position.z,
            },
            orientation: current.orientation,
        };

        self.monado.set_reference_space_offset(libmonado::ReferenceSpaceType::Stage, pose)
            .map_err(|e| MonadoError::ApplyOffsetFailed(format!("{e:?}")))
    }

    // Recenter STAGE origin to current HMD position/heading, preserving floor Y.
    pub fn apply_recenter_absolute(&self, hmd_position: [f32; 3], hmd_orientation: [f32; 4]) -> Result<(), MonadoError> {
        use nalgebra::{Rotation3, UnitQuaternion, Quaternion, Vector3};

        // Get current STAGE offset
        let current_pose = self.monado.get_reference_space_offset(libmonado::ReferenceSpaceType::Stage)
            .map_err(|e| MonadoError::TrackingOriginFailed(format!("{e:?}")))?;

        let current_trans = Vector3::new(
            f64::from(current_pose.position.x),
            f64::from(current_pose.position.y),
            f64::from(current_pose.position.z),
        );
        let current_rot = UnitQuaternion::from_quaternion(Quaternion::new(
            f64::from(current_pose.orientation.s),
            f64::from(current_pose.orientation.v.x),
            f64::from(current_pose.orientation.v.y),
            f64::from(current_pose.orientation.v.z),
        ));

        // HMD pose in current STAGE coords
        let stage_hmd_pos = Vector3::new(
            f64::from(hmd_position[0]),
            f64::from(hmd_position[1]),
            f64::from(hmd_position[2]),
        );
        let stage_hmd_ori = UnitQuaternion::from_quaternion(Quaternion::new(
            f64::from(hmd_orientation[3]),  // w
            f64::from(hmd_orientation[0]),  // x
            f64::from(hmd_orientation[1]),  // y
            f64::from(hmd_orientation[2]),  // z
        ));

        // Convert HMD pose from STAGE to native coords
        // If offset represents: native = offset * stage (i.e., stage = offset^-1 * native)
        let native_hmd_pos = current_rot * stage_hmd_pos + current_trans;
        let native_hmd_ori = current_rot * stage_hmd_ori;

        // Extract yaw from native HMD orientation
        // Forward is -Z in OpenXR
        let neg_z = Vector3::new(0.0, 0.0, -1.0);
        let fwd = native_hmd_ori * neg_z;
        let native_hmd_yaw = fwd.x.atan2(-fwd.z);

        // New offset: given native = offset * stage, we have stage = offset^-1 * native
        // To place native_hmd at stage origin facing -Z:
        //   stage_pos = R^-1 * (native_pos - t)
        //   We want stage_hmd = (0, floor, 0), so:
        //   (0, floor, 0) = R^-1 * (native_hmd - t)
        //   t = native_hmd - R * (0, floor, 0)
        //   Since R is yaw-only, R * (0, y, 0) = (0, y, 0)
        //   So t = (native_hmd.x, native_hmd.y - floor, native_hmd.z)
        //   But we preserve floor from current offset, so just use native_hmd x,z
        //
        // For rotation: native_ori = R * stage_ori, we want stage_ori = identity
        //   So R = native_hmd_yaw_rotation

        // Negate yaw because Monado's offset convention inverts rotation
        let new_rot = Rotation3::from_axis_angle(&Vector3::y_axis(), -native_hmd_yaw);

        // Translation is just native HMD position (x, z), preserving floor Y
        let final_trans = Vector3::new(native_hmd_pos.x, current_trans.y, native_hmd_pos.z);

        // Convert back to Pose
        let new_rot_quat = UnitQuaternion::from_rotation_matrix(&new_rot);
        let q = new_rot_quat.quaternion();
        let pose = Pose {
            position: mint::Vector3 {
                x: final_trans.x as f32,
                y: final_trans.y as f32,
                z: final_trans.z as f32,
            },
            orientation: mint::Quaternion {
                v: mint::Vector3 {
                    x: q.i as f32,
                    y: q.j as f32,
                    z: q.k as f32,
                },
                s: q.w as f32,
            },
        };

        self.monado.set_reference_space_offset(libmonado::ReferenceSpaceType::Stage, pose)
            .map_err(|e| MonadoError::ApplyOffsetFailed(format!("{e:?}")))
    }

    pub fn refresh_batteries(&self, categories: &mut [Category]) -> Result<(), MonadoError> {
        let devices = self.monado.devices()
            .map_err(|e| MonadoError::EnumerationFailed(format!("{e:?}")))?;

        let mut battery_map = std::collections::HashMap::new();
        for device in devices {
            if let Ok(status) = device.battery_status()
                && status.present
            {
                battery_map.insert(device.index, (status.charge, status.charging));
            }
        }

        for cat in categories.iter_mut() {
            for dev in &mut cat.devices {
                if let Some(&(charge, charging)) = battery_map.get(&dev.device_index) {
                    dev.battery_charge = Some(charge);
                    dev.battery_charging = charging;
                } else {
                    dev.battery_charge = None;
                    dev.battery_charging = false;
                }
            }
        }

        Ok(())
    }

    // Reset horizontal position and yaw, preserving floor Y.
    pub fn reset_center(&self) -> Result<(), MonadoError> {
        let current = self.monado.get_reference_space_offset(libmonado::ReferenceSpaceType::Stage)
            .map_err(|e| MonadoError::TrackingOriginFailed(format!("{e:?}")))?;

        // Reset X, Z position and rotation to identity, preserve Y (floor)
        let pose = Pose {
            position: mint::Vector3 {
                x: 0.0,
                y: current.position.y,
                z: 0.0,
            },
            orientation: mint::Quaternion {
                v: mint::Vector3 { x: 0.0, y: 0.0, z: 0.0 },
                s: 1.0,  // Identity rotation (no yaw)
            },
        };

        self.monado.set_reference_space_offset(libmonado::ReferenceSpaceType::Stage, pose)
            .map_err(|e| MonadoError::ApplyOffsetFailed(format!("{e:?}")))
    }
}

pub fn try_connect() -> Option<MonadoConnection> {
    MonadoConnection::connect().ok()
}

pub fn enumerate_devices() -> Vec<Category> {
    try_connect().map_or_else(Vec::new, |conn| conn.enumerate_devices().unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_connect() {
        match MonadoConnection::connect() {
            Ok(_conn) => println!("Successfully connected to Monado"),
            Err(e) => println!("Failed to connect to Monado: {e}"),
        }
    }

    #[test]
    fn test_enumerate() {
        if let Some(conn) = try_connect() {
            match conn.enumerate_devices() {
                Ok(categories) => {
                    println!("Found {} categories", categories.len());
                    for cat in categories {
                        println!("  Category: {} ({} devices)", cat.name, cat.devices.len());
                        for dev in cat.devices {
                            println!("    - {}", dev.name);
                        }
                    }
                }
                Err(e) => println!("Failed to enumerate devices: {e}"),
            }
        } else {
            println!("Could not connect to Monado");
        }
    }
}

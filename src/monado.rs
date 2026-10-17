use crate::error::MonadoError;
use crate::ui::{Category, Device};
use libmonado::{DeviceLogic, MndProperty, Monado};

#[derive(Debug, Clone, Copy)]
pub enum ResetTarget {
    TrackingOrigin(u32),
    Stage,
    Local,
}

// Direct offset writes here are restricted to explicit resets, following motoc CLI.
// Restoring saved tracking-origin offsets is the other explicit exception in xr.
// Calibration and recentering writes belong to libmotoc calibrators in xr.
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
        let origins: Vec<_> = self
            .monado
            .tracking_origins()
            .map_err(|e| MonadoError::EnumerationFailed(format!("{e:?}")))?
            .into_iter()
            .collect();

        let devices = self
            .monado
            .devices()
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
            let origin_index = device
                .get_info_u32(MndProperty::PropertyTrackingOriginU32)
                .unwrap_or(0);

            let cat_idx = categories
                .iter()
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

    pub fn reset(&self, target: ResetTarget) -> Result<(), MonadoError> {
        let identity = libmotoc::TransformD::default().into();
        match target {
            ResetTarget::TrackingOrigin(id) => {
                let origin = self
                    .monado
                    .tracking_origins()
                    .map_err(|e| MonadoError::Reset(format!("{e:?}")))?
                    .into_iter()
                    .find(|origin| origin.id == id)
                    .ok_or_else(|| MonadoError::Reset(format!("Tracking origin {id} not found")))?;
                origin
                    .set_offset(identity)
                    .map_err(|e| MonadoError::Reset(format!("{e:?}")))
            }
            ResetTarget::Stage | ResetTarget::Local => {
                let space = match target {
                    ResetTarget::Stage => libmonado::ReferenceSpaceType::Stage,
                    _ => libmonado::ReferenceSpaceType::Local,
                };
                self.monado
                    .set_reference_space_offset(space, identity)
                    .map_err(|e| MonadoError::Reset(format!("{e:?}")))
            }
        }
    }

    pub fn refresh_batteries(&self, categories: &mut [Category]) -> Result<(), MonadoError> {
        let devices = self
            .monado
            .devices()
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
}

pub fn try_connect() -> Option<MonadoConnection> {
    MonadoConnection::connect().ok()
}

pub fn enumerate_devices() -> Vec<Category> {
    try_connect().map_or_else(Vec::new, |conn| {
        conn.enumerate_devices().unwrap_or_default()
    })
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

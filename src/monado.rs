use crate::error::MonadoError;
use crate::ui::{Category, Device};
use libmonado::{DeviceLogic, MndProperty, Monado};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetTarget {
    TrackingOrigin(u32),
    Stage,
    Local,
}

// Direct offset writes here are restricted to explicit resets, following motoc CLI.
// Saved-offset restoration and the explicitly requested motoc tui Adjust UI
// are the other direct-write paths in xr.
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

    pub fn is_alive(&self) -> bool {
        // Device metadata is read from shared memory and can outlive the server.
        // Updating the client list makes an IPC request on this connection.
        self.monado.clients().is_ok()
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

pub(crate) fn refresh_connection<C>(
    connection: &mut Option<C>,
    connect: impl FnOnce() -> Option<C>,
    is_alive: impl FnOnce(&C) -> bool,
) -> bool {
    let was_connected = connection.is_some();
    if connection.is_none() {
        *connection = connect();
    }
    if connection.as_ref().is_some_and(|conn| !is_alive(conn)) {
        // Retry on the next poll so the disconnected state is also propagated.
        *connection = None;
    }
    was_connected != connection.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn connection_tracks_repeated_server_restarts() {
        let server_generation = Cell::new(None);
        let connect_attempts = Cell::new(0);
        let mut connection = None;
        let poll = |connection: &mut Option<u32>| {
            refresh_connection(
                connection,
                || {
                    connect_attempts.set(connect_attempts.get() + 1);
                    server_generation.get()
                },
                |generation| Some(*generation) == server_generation.get(),
            )
        };

        assert!(!poll(&mut connection));
        assert!(connection.is_none());
        for generation in 1..=4 {
            server_generation.set(Some(generation));
            assert!(poll(&mut connection));
            assert_eq!(connection, Some(generation));
            let attempts = connect_attempts.get();
            assert!(!poll(&mut connection));
            assert_eq!(connect_attempts.get(), attempts);

            server_generation.set(None);
            assert!(poll(&mut connection));
            assert!(connection.is_none());
            assert!(!poll(&mut connection));
            assert!(connection.is_none());
        }

        // A restart between polls must also invalidate the old connection.
        server_generation.set(Some(5));
        assert!(poll(&mut connection));
        server_generation.set(Some(6));
        assert!(poll(&mut connection));
        assert!(connection.is_none());
        assert!(poll(&mut connection));
        assert_eq!(connection, Some(6));
    }

    #[test]
    fn connection_lost_during_connect_stays_disconnected() {
        let mut connection = None;
        assert!(!refresh_connection(&mut connection, || Some(()), |()| false));
        assert!(connection.is_none());
        assert!(refresh_connection(&mut connection, || Some(()), |()| true));
        assert!(connection.is_some());
    }

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

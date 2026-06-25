use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;

use crate::calibration::DeviceMovement;
use crate::config::Config;
use crate::monado::{self, MonadoConnection};
use crate::ui::device_selector::{Category, Device};

#[derive(Debug, Clone)]
pub struct BatteryInfo {
    pub short_name: String,
    pub serial_suffix: String,
    pub charge: Option<f32>,
    pub charging: bool,
    pub online: bool,
    offline_since: Option<Instant>,
}


fn shorten_device_name(name: &str) -> String {
    let lower = name.to_lowercase();

    if lower.contains("knuckles") || lower.contains("index") {
        if lower.contains("left") {
            return "Knuckles L".to_string();
        } else if lower.contains("right") {
            return "Knuckles R".to_string();
        }
        return "Knuckles".to_string();
    }

    if lower.contains("controller") {
        if lower.contains("left") {
            return "Controller L".to_string();
        } else if lower.contains("right") {
            return "Controller R".to_string();
        }
        return "Controller".to_string();
    }

    if lower.contains("tracker") {
        return "Tracker".to_string();
    }

    if lower.contains("hmd") || lower.contains("headset") {
        return "HMD".to_string();
    }

    if lower.contains("gamepad") {
        return "Gamepad".to_string();
    }

    let words: Vec<&str> = name.split_whitespace().collect();
    if words.len() <= 2 {
        name.to_string()
    } else {
        words[..2].join(" ")
    }
}

pub struct AppState {
    connection: Option<MonadoConnection>,
    categories: Vec<Category>,
    source_id: Option<String>,
    target_id: Option<String>,
    movement_intensities: HashMap<String, f32>,
    // Tracks battery devices across disconnects
    known_battery_devices: HashMap<String, BatteryInfo>,
    hide_calibration_help: bool,
    sample_count: u32,
}

impl AppState {
    pub fn new() -> Self {
        let config = Config::load();
        let connection = monado::try_connect();
        let categories = if connection.is_some() {
            monado::enumerate_devices()
        } else {
            Vec::new()
        };

        let mut state = Self {
            connection,
            categories,
            source_id: if config.source.is_empty() { None } else { Some(config.source) },
            target_id: if config.target.is_empty() { None } else { Some(config.target) },
            movement_intensities: HashMap::new(),
            known_battery_devices: HashMap::new(),
            hide_calibration_help: config.hide_calibration_help,
            sample_count: config.sample_count,
        };
        state.update_known_battery_devices();
        state
    }

    pub const fn is_connected(&self) -> bool {
        self.connection.is_some()
    }

    pub const fn connection(&self) -> Option<&MonadoConnection> {
        self.connection.as_ref()
    }

    pub fn categories(&self) -> &[Category] {
        &self.categories
    }

    pub fn source_devices(&self) -> Vec<Device> {
        let exclude_category = self.selected_target().map(|d| d.category.clone());
        self.categories.iter()
            .flat_map(|c| c.devices.clone())
            .filter(|d| {
                if let Some(ref exc) = exclude_category {
                    &d.category != exc
                } else {
                    true
                }
            })
            .collect()
    }

    pub fn target_devices(&self) -> Vec<Device> {
        let exclude_category = self.selected_source().map(|d| d.category.clone());
        self.categories.iter()
            .flat_map(|c| c.devices.clone())
            .filter(|d| {
                if let Some(ref exc) = exclude_category {
                    &d.category != exc
                } else {
                    true
                }
            })
            .collect()
    }

    pub fn selected_source(&self) -> Option<&Device> {
        self.source_id.as_ref().and_then(|id| {
            self.categories.iter()
                .flat_map(|c| c.devices.iter())
                .find(|d| d.unique_id() == id)
        })
    }

    pub fn selected_target(&self) -> Option<&Device> {
        self.target_id.as_ref().and_then(|id| {
            self.categories.iter()
                .flat_map(|c| c.devices.iter())
                .find(|d| d.unique_id() == id)
        })
    }

    pub fn source_name(&self) -> Option<&str> {
        self.source_id.as_deref()
    }

    pub fn target_name(&self) -> Option<&str> {
        self.target_id.as_deref()
    }

    pub fn set_source(&mut self, id: Option<String>) {
        if self.source_id != id {
            self.source_id = id;
            self.save_config();
        }
    }

    pub fn set_target(&mut self, id: Option<String>) {
        if self.target_id != id {
            self.target_id = id;
            self.save_config();
        }
    }

    pub fn refresh_connection(&mut self) -> bool {
        let was_connected = self.is_connected();
        if !was_connected {
            self.connection = monado::try_connect();
        }

        let is_connected = self.is_connected();
        if is_connected && !was_connected {
            // Just connected
            if let Some(ref conn) = self.connection {
                self.categories = conn.enumerate_devices().unwrap_or_default();
            }
            self.update_known_battery_devices();
        } else if !is_connected {
            self.categories.clear();
        }

        was_connected != is_connected
    }

    pub fn force_refresh(&mut self) {
        self.connection = monado::try_connect();
        if let Some(ref conn) = self.connection {
            self.categories = conn.enumerate_devices().unwrap_or_default();
        } else {
            self.categories.clear();
        }
        self.update_known_battery_devices();
    }

    pub fn refresh_batteries(&mut self) {
        if let Some(ref conn) = self.connection
            && conn.refresh_batteries(&mut self.categories).is_err()
        {
            // Connection lost
            self.connection = None;
            self.categories.clear();
        }
        self.update_known_battery_devices();
    }

    fn update_known_battery_devices(&mut self) {
        let mut current_ids = std::collections::HashSet::new();

        for cat in &self.categories {
            for dev in &cat.devices {
                let id = dev.unique_id().to_string();
                current_ids.insert(id.clone());
                let serial_suffix = if !dev.serial.is_empty() && dev.serial != dev.name {
                    if dev.serial.len() > 8 {
                        dev.serial[dev.serial.len() - 8..].to_string()
                    } else {
                        dev.serial.clone()
                    }
                } else {
                    String::new()
                };
                self.known_battery_devices.insert(id, BatteryInfo {
                    short_name: shorten_device_name(&dev.name),
                    serial_suffix,
                    charge: dev.battery_charge,
                    charging: dev.battery_charging,
                    online: true,
                    offline_since: None,
                });
            }
        }

        let now = Instant::now();
        for (id, info) in &mut self.known_battery_devices {
            if !current_ids.contains(id) {
                if info.online {
                    info.offline_since = Some(now);
                }
                info.online = false;
                info.charge = None;
                info.charging = false;
            }
        }
        self.known_battery_devices.retain(|_, info| {
            info.online
                || info
                    .offline_since
                    .is_none_or(|t| now.duration_since(t).as_secs() < 300)
        });
    }

    pub fn battery_status_list(&self) -> Vec<&BatteryInfo> {
        let mut list: Vec<&BatteryInfo> = self.known_battery_devices.values().collect();
        list.sort_by(|a, b| a.short_name.cmp(&b.short_name).then(a.serial_suffix.cmp(&b.serial_suffix)));
        list
    }

    pub fn set_movement_intensities(&mut self, movements: Vec<DeviceMovement>) {
        self.movement_intensities.clear();
        for m in movements {
            self.movement_intensities.insert(m.device_id, m.intensity);
        }
    }

    pub const fn movement_intensities(&self) -> &HashMap<String, f32> {
        &self.movement_intensities
    }

    pub const fn hide_calibration_help(&self) -> bool {
        self.hide_calibration_help
    }

    pub fn set_hide_calibration_help(&mut self, hide: bool) {
        if self.hide_calibration_help != hide {
            self.hide_calibration_help = hide;
            self.save_config();
        }
    }

    pub const fn sample_count(&self) -> u32 {
        self.sample_count
    }

    pub fn set_sample_count(&mut self, count: u32) {
        if self.sample_count != count {
            self.sample_count = count;
            self.save_config();
        }
    }

    fn save_config(&self) {
        let config = Config {
            source: self.source_id.clone().unwrap_or_default(),
            target: self.target_id.clone().unwrap_or_default(),
            hide_calibration_help: self.hide_calibration_help,
            sample_count: self.sample_count,
        };
        if let Err(e) = config.save() {
            eprintln!("Warning: Failed to save config: {e}");
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

pub type SharedState = Rc<RefCell<AppState>>;

pub fn create_shared_state() -> SharedState {
    Rc::new(RefCell::new(AppState::new()))
}

use crate::error::ConfigError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
// UI preferences only. Calibration profiles are owned by libmotoc in its
// motoc config directory and must never be serialized into this file.
pub struct Config {
    pub source: String,
    pub target: String,
    #[serde(default)]
    pub hide_calibration_help: bool,
    #[serde(default = "default_sample_count")]
    pub sample_count: u32,
}

const fn default_sample_count() -> u32 {
    400
}

impl Default for Config {
    fn default() -> Self {
        Self {
            source: String::new(),
            target: String::new(),
            hide_calibration_help: false,
            sample_count: default_sample_count(),
        }
    }
}

impl Config {
    // Config file path. Falls back to current dir if data dir unavailable.
    pub fn path() -> std::path::PathBuf {
        dirs::data_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join("spacecal-for-monado")
            .join("config.json")
    }

    pub fn load() -> Self {
        let path = Self::path();
        if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(content) => match serde_json::from_str(&content) {
                    Ok(config) => config,
                    Err(e) => {
                        eprintln!("Warning: Failed to parse config file: {e}. Using defaults.");
                        Self::default()
                    }
                },
                Err(e) => {
                    eprintln!("Warning: Failed to read config file: {e}. Using defaults.");
                    Self::default()
                }
            }
        } else {
            Self::default()
        }
    }

    pub fn save(&self) -> Result<(), ConfigError> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ConfigError::DirectoryCreationFailed(e.to_string()))?;
        }
        let content = serde_json::to_string_pretty(self)
            .map_err(|e| ConfigError::SaveFailed(format!("Failed to serialize config: {e}")))?;
        std::fs::write(&path, content)
            .map_err(|e| ConfigError::SaveFailed(format!("Failed to write config file: {e}")))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_and_existing_configs_use_same_sample_count() {
        let existing: Config = serde_json::from_str(r#"{"source":"", "target":""}"#).unwrap();
        assert_eq!(Config::default().sample_count, 400);
        assert_eq!(existing.sample_count, Config::default().sample_count);
    }
}

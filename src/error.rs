use thiserror::Error;

#[derive(Error, Debug)]
pub enum MonadoError {
    #[error("Reset failed: {0}")]
    Reset(String),

    #[error("Failed to connect to Monado: {0}")]
    ConnectionFailed(String),

    #[error("Failed to enumerate devices: {0}")]
    EnumerationFailed(String),
}

#[derive(Error, Debug)]
pub enum XrError {
    #[error("Failed to create OpenXR instance: {0}")]
    InstanceCreationFailed(String),

    #[error("Failed to create OpenXR session: {0}")]
    SessionCreationFailed(String),

    #[error("OpenXR runtime error: {0}")]
    RuntimeError(String),
}

#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("Failed to load config: {0}")]
    LoadFailed(String),

    #[error("Failed to save config: {0}")]
    SaveFailed(String),

    #[error("Invalid config format: {0}")]
    InvalidFormat(String),

    #[error("Config directory creation failed: {0}")]
    DirectoryCreationFailed(String),
}

impl From<std::io::Error> for ConfigError {
    fn from(e: std::io::Error) -> Self {
        Self::LoadFailed(e.to_string())
    }
}

impl From<serde_json::Error> for ConfigError {
    fn from(e: serde_json::Error) -> Self {
        Self::InvalidFormat(e.to_string())
    }
}

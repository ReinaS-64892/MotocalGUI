#[derive(Debug, Clone)]
pub enum CalibrationCommand {
    StartSampled {
        source_serial: String,
        target_serial: String,
        sample_count: u32,
    },
    Reset {
        target: crate::monado::ResetTarget,
        label: String,
    },
    Recenter,
    StartMovementDetection,
    StopMovementDetection,
    Shutdown,
}

#[derive(Debug, Clone)]
pub struct DeviceMovement {
    pub device_id: String,
    pub intensity: f32,
}

#[derive(Debug, Clone)]
pub enum CalibrationMessage {
    Countdown { seconds: u32 },
    RecenterCountdown { seconds: u32 },
    Progress { collected: u32, total: u32 },
    SampledComplete,
    RecenterComplete,
    ResetComplete(String),
    ProfileRestored { continuous: bool },
    MovementUpdate { movements: Vec<DeviceMovement> },
    RuntimeDisconnected,
    Error(String),
}

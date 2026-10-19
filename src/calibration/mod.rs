#[derive(Debug, Clone)]
pub enum CalibrationCommand {
    StartSampled {
        source_serial: String,
        target_serial: String,
        sample_count: u32,
        continuous: bool,
    },
    Reset {
        target: crate::monado::ResetTarget,
        label: String,
    },
    StopContinuous,
    ReadAdjustment {
        target: crate::monado::ResetTarget,
    },
    Adjust {
        target: crate::monado::ResetTarget,
        axis: usize,
        delta: f64,
    },
    SaveAdjustment {
        target: crate::monado::ResetTarget,
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
    Countdown {
        seconds: u32,
    },
    RecenterCountdown {
        seconds: u32,
    },
    Progress {
        collected: u32,
        total: u32,
    },
    SampledComplete {
        continuous: bool,
    },
    ContinuousStopped,
    AdjustmentUpdated {
        target: crate::monado::ResetTarget,
        values: [f64; 6],
    },
    AdjustmentSaved,
    AdjustmentAvailability {
        continuous: bool,
    },
    AdjustmentSaveFailed(String),
    RecenterComplete,
    ResetComplete(String),
    ProfileRestored {
        continuous: bool,
    },
    MovementUpdate {
        movements: Vec<DeviceMovement>,
    },
    RuntimeDisconnected,
    Error(String),
}

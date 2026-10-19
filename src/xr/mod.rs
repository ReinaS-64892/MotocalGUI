use crate::calibration::{CalibrationCommand, CalibrationMessage, DeviceMovement};
mod adjustment;
use crate::error::XrError;
use adjustment::{check_adjustment_profile, handle_adjustment, save_adjustment};
use libmonado::DeviceLogic;
use libmotoc::{
    Calibrator, CalibratorData, CalibratorStatus, EffectiveSpaceVelocity, OffsetMethod, OffsetType,
    RecenterMethod, SampledMethod, SavedCalibration, StepResult, TransformD,
};
use openxr as xr;
use openxr_mndx_xdev_space::{SessionXDevExtensionMNDX, XDevList};
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub trait MessageSender: Send {
    fn send(&self, msg: CalibrationMessage) -> Result<(), String>;
}

impl MessageSender for mpsc::Sender<CalibrationMessage> {
    fn send(&self, msg: CalibrationMessage) -> Result<(), String> {
        Self::send(self, msg).map_err(|e| e.to_string())
    }
}

impl MessageSender for async_channel::Sender<CalibrationMessage> {
    fn send(&self, msg: CalibrationMessage) -> Result<(), String> {
        self.send_blocking(msg).map_err(|e| e.to_string())
    }
}

pub struct XrSession {
    instance: xr::Instance,
    session: xr::Session<xr::headless::Headless>,
    running: bool,
}

impl XrSession {
    pub fn new() -> Result<Self, XrError> {
        let (instance, system) =
            libmotoc::xr_init().map_err(|e| XrError::InstanceCreationFailed(e.to_string()))?;
        // SAFETY: MND_headless is enabled by libmotoc::xr_init.
        let (session, _, _) = unsafe {
            instance.create_session::<xr::headless::Headless>(
                system,
                &xr::headless::SessionCreateInfo {},
            )
        }
        .map_err(|e| XrError::SessionCreationFailed(e.to_string()))?;
        let mut result = Self {
            instance,
            session,
            running: false,
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while !result.running {
            result
                .poll_events()
                .map_err(|e| XrError::RuntimeError(e.user_message()))?;
            if Instant::now() >= deadline {
                return Err(XrError::RuntimeError(
                    "Timed out waiting for OpenXR session".into(),
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(result)
    }

    fn poll_events(&mut self) -> Result<(), StepError> {
        let mut buffer = xr::EventDataBuffer::new();
        while let Some(event) = self
            .instance
            .poll_event(&mut buffer)
            .map_err(StepError::from)?
        {
            match event {
                xr::Event::SessionStateChanged(event) => match event.state() {
                    xr::SessionState::READY => {
                        self.session
                            .begin(xr::ViewConfigurationType::PRIMARY_STEREO)
                            .map_err(StepError::from)?;
                        self.running = true;
                    }
                    xr::SessionState::STOPPING => {
                        self.running = false;
                        let _ = self.session.end();
                        return Err(StepError::RuntimeDisconnected);
                    }
                    xr::SessionState::EXITING | xr::SessionState::LOSS_PENDING => {
                        self.running = false;
                        return Err(StepError::RuntimeDisconnected);
                    }
                    _ => {}
                },
                xr::Event::InstanceLossPending(_) => {
                    self.running = false;
                    return Err(StepError::RuntimeDisconnected);
                }
                _ => {}
            }
        }
        Ok(())
    }

    pub fn now(&self) -> xr::Time {
        self.instance.now().unwrap_or(xr::Time::from_nanos(1))
    }
}

// Match the default profile of `motoc calibrate` and `motoc continue`.
const DEFAULT_PROFILE: &str = "last";

const MOVEMENT_LINEAR_THRESHOLD: f32 = 0.2;
const MOVEMENT_ANGULAR_THRESHOLD: f32 = 0.5; // rad/s (about 30 deg/s)
const MOVEMENT_FADE_DURATION: f32 = 2.0;

struct CachedDevice {
    id: String,
    space: xr::Space,
}

struct MovementCache {
    _list: XDevList,
    reference_space: xr::Space,
    devices: Vec<CachedDevice>,
}

fn build_movement_cache(session: &xr::Session<xr::headless::Headless>) -> Option<MovementCache> {
    let list = session.get_xdev_list().ok()?;
    let xdevs = list.enumerate_xdevs().ok()?;
    let reference_space = session
        .create_reference_space(xr::ReferenceSpaceType::LOCAL, xr::Posef::IDENTITY)
        .ok()?;

    let mut devices = Vec::with_capacity(xdevs.len());
    for d in &xdevs {
        if !d.can_create_space() {
            continue;
        }
        let id = {
            let s = d.serial();
            if s.is_empty() {
                d.name().to_string()
            } else {
                s.to_string()
            }
        };
        if let Ok(space) = d.create_space(xr::Posef::IDENTITY) {
            devices.push(CachedDevice { id, space });
        }
    }

    if devices.is_empty() {
        return None;
    }

    Some(MovementCache {
        _list: list,
        reference_space,
        devices,
    })
}

struct DeviceMovementState {
    device_id: String,
    last_moving_time: Option<Instant>,
}

#[allow(clippy::needless_pass_by_value)]
pub fn xr_event_loop<S: MessageSender>(cmd_rx: mpsc::Receiver<CalibrationCommand>, msg_tx: S) {
    let mut movement_detection_active = false;
    loop {
        match XrSession::new() {
            Ok(mut session) => {
                match libmonado::Monado::auto_connect()
                    .map_err(|error| {
                        eprintln!("Waiting for Monado: {error}");
                        StepError::RuntimeDisconnected
                    })
                    .and_then(|monado| {
                        let result = run_connected(
                            &mut session,
                            &monado,
                            &cmd_rx,
                            &msg_tx,
                            &mut movement_detection_active,
                        );
                        check_runtime_result(result, &monado)
                    }) {
                    Ok(()) => return,
                    Err(error) => report(&msg_tx, Err(error)),
                }
            }
            Err(error) => eprintln!("Waiting for OpenXR: {error}"),
        }
        let _ = msg_tx.send(CalibrationMessage::MovementUpdate { movements: vec![] });
        // Reset remains available without a headless OpenXR connection, as in motoc.
        match cmd_rx.recv_timeout(Duration::from_secs(2)) {
            Ok(CalibrationCommand::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
            Ok(CalibrationCommand::StartMovementDetection) => movement_detection_active = true,
            Ok(CalibrationCommand::StopMovementDetection) => movement_detection_active = false,
            Ok(CalibrationCommand::Reset { target, label }) => handle_reset(target, label, &msg_tx),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Ok(_) => report(
                &msg_tx,
                Err(StepError::NotConnected {
                    action: "calibration",
                }),
            ),
        }
    }
}

#[allow(clippy::too_many_lines)]
fn run_connected<S: MessageSender>(
    session: &mut XrSession,
    monado: &libmonado::Monado,
    cmd_rx: &mpsc::Receiver<CalibrationCommand>,
    msg_tx: &S,
    movement_detection_active: &mut bool,
) -> Result<(), StepError> {
    let mut data = load_calibrator_data(&session.session, monado)?;
    // A fresh runtime connection restores the shared profile exactly once.
    // Never reapply it on each timer tick (that would undo an explicit reset).
    let mut continuous = match load_default_profile(&mut data, msg_tx) {
        Ok(method) => method,
        Err(error) => {
            report_connected(msg_tx, Err(error), monado)?;
            None
        }
    };
    let mut movement_cache = None;
    let mut movement_state = Vec::new();
    let mut last_movement_update = Instant::now();
    loop {
        let interval = if continuous.is_some() { 33 } else { 100 };
        let command = cmd_rx.recv_timeout(Duration::from_millis(interval));
        session.poll_events()?;
        if !session.running {
            return Err(StepError::RuntimeDisconnected);
        }
        match command {
            Ok(CalibrationCommand::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                stop_continuous(&mut continuous, &mut data)?;
                return Ok(());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Ok(CalibrationCommand::StartMovementDetection) => {
                *movement_detection_active = true;
                movement_state.clear();
                movement_cache = None;
            }
            Ok(CalibrationCommand::StopMovementDetection) => {
                *movement_detection_active = false;
                movement_cache = None;
                movement_state.clear();
                let _ = msg_tx.send(CalibrationMessage::MovementUpdate { movements: vec![] });
            }
            Ok(CalibrationCommand::StartSampled {
                source_serial,
                target_serial,
                sample_count,
                continuous: maintain,
            }) => {
                stop_continuous(&mut continuous, &mut data)?;
                // Refresh the device list before a new calibration, keeping the
                // same data and device indices for its continuous successor.
                data = load_calibrator_data(&session.session, monado)?;
                let result = handle_start_sampled(
                    session,
                    &mut data,
                    msg_tx,
                    &source_serial,
                    &target_serial,
                    sample_count,
                    maintain,
                );
                match result {
                    Ok(method) => continuous = method,
                    Err(error) => {
                        let _ = msg_tx.send(CalibrationMessage::ContinuousStopped);
                        report_connected(msg_tx, Err(error), monado)?;
                    }
                }
            }
            Ok(CalibrationCommand::StopContinuous) => {
                stop_continuous(&mut continuous, &mut data)?;
                let _ = msg_tx.send(CalibrationMessage::ContinuousStopped);
            }
            Ok(CalibrationCommand::ReadAdjustment { target }) => {
                if let Err(error) = check_adjustment_profile(&data) {
                    report_connected(msg_tx, Err(error), monado)?;
                    continue;
                }
                stop_continuous(&mut continuous, &mut data)?;
                let _ = msg_tx.send(CalibrationMessage::ContinuousStopped);
                report_connected(
                    msg_tx,
                    handle_adjustment(&data, target, None, msg_tx),
                    monado,
                )?;
            }
            Ok(CalibrationCommand::Adjust {
                target,
                axis,
                delta,
            }) => {
                report_connected(
                    msg_tx,
                    handle_adjustment(&data, target, Some((axis, delta)), msg_tx),
                    monado,
                )?;
            }
            Ok(CalibrationCommand::Recenter) => {
                report_connected(msg_tx, handle_recenter(Some(session), msg_tx), monado)?;
            }
            Ok(CalibrationCommand::SaveAdjustment { target }) => {
                let result = check_runtime_result(save_adjustment(&data, target), monado);
                let message = match result {
                    Ok(()) => CalibrationMessage::AdjustmentSaved,
                    Err(StepError::RuntimeDisconnected) => {
                        return Err(StepError::RuntimeDisconnected);
                    }
                    Err(error) => CalibrationMessage::AdjustmentSaveFailed(error.user_message()),
                };
                let _ = msg_tx.send(message);
            }
            Ok(CalibrationCommand::Reset { target, label }) => {
                stop_continuous(&mut continuous, &mut data)?;
                let _ = msg_tx.send(CalibrationMessage::ContinuousStopped);
                handle_reset(target, label, msg_tx);
            }
        }
        if let Some(method) = continuous.as_mut() {
            data.now = session.instance.now().map_err(StepError::from)?;
            match method.step(&mut data) {
                Ok((StepResult::Continue, _)) => {}
                Ok((StepResult::End, _)) => {
                    stop_continuous(&mut continuous, &mut data)?;
                    let _ = msg_tx.send(CalibrationMessage::ContinuousStopped);
                }
                Ok((StepResult::Replace(mut next), _)) => {
                    method.finish(&mut data)?;
                    next.init(&mut data)?;
                    continuous = Some(next);
                }
                Err(error) => {
                    stop_continuous(&mut continuous, &mut data)?;
                    let _ = msg_tx.send(CalibrationMessage::ContinuousStopped);
                    report_connected(msg_tx, Err(error.into()), monado)?;
                }
            }
        }
        if *movement_detection_active
            && last_movement_update.elapsed() >= Duration::from_millis(100)
        {
            if movement_cache.is_none() {
                movement_cache = build_movement_cache(&session.session);
            }
            if let Some(cache) = movement_cache.as_ref() {
                process_movement_update(cache, session, &mut movement_state, msg_tx);
            }
            last_movement_update = Instant::now();
        }
    }
}

fn stop_continuous(
    method: &mut Option<Box<dyn Calibrator>>,
    data: &mut CalibratorData<'_>,
) -> Result<(), StepError> {
    if let Some(mut method) = method.take() {
        method.finish(data)?;
    }
    Ok(())
}

fn handle_reset<S: MessageSender>(target: crate::monado::ResetTarget, label: String, msg_tx: &S) {
    match crate::monado::MonadoConnection::connect().and_then(|connection| connection.reset(target))
    {
        Ok(()) => {
            let _ = msg_tx.send(CalibrationMessage::ResetComplete(label));
        }
        Err(error) => report(msg_tx, Err(StepError::Backend(error.to_string()))),
    }
}

fn load_default_profile<S: MessageSender>(
    data: &mut CalibratorData<'_>,
    msg_tx: &S,
) -> Result<Option<Box<dyn Calibrator>>, StepError> {
    let Some(saved) = optional_profile(data.load_calibration(DEFAULT_PROFILE))? else {
        let _ = msg_tx.send(CalibrationMessage::AdjustmentAvailability { continuous: false });
        return Ok(None);
    };
    let _ = msg_tx.send(CalibrationMessage::AdjustmentAvailability {
        continuous: matches!(saved.offset_type, OffsetType::Device),
    });
    let method = match saved.offset_type {
        OffsetType::TrackingOrigin => {
            // This is the same restore path as `motoc continue`. libmotoc owns
            // the file format, but exposes no one-shot apply method.
            let source = data
                .tracking_origins
                .iter()
                .find(|origin| origin.name == saved.src);
            let source_offset = if let Some(source) = source {
                TransformD::from(source.get_offset().map_err(libmotoc::Error::from)?)
            } else {
                eprintln!(
                    "Source origin {:?} not found; restoring with identity source, as motoc does",
                    saved.src
                );
                TransformD::default()
            };
            let destination = data
                .tracking_origins
                .iter()
                .find(|origin| origin.name == saved.dst)
                .ok_or_else(|| {
                    StepError::Backend(format!(
                        "Saved target tracking origin {:?} not found",
                        saved.dst
                    ))
                })?;
            destination
                .set_offset(restored_origin_offset(&saved, source_offset).into())
                .map_err(libmotoc::Error::from)?;
            None
        }
        OffsetType::Device => {
            let find = |serial: &str, role| {
                data.devices
                    .iter()
                    .position(|device| device.serial == serial)
                    .ok_or_else(|| StepError::DeviceNotFound {
                        role,
                        serial: serial.into(),
                    })
            };
            let src = find(&saved.src, "Source")?;
            let dst = find(&saved.dst, "Target")?;
            let mut method = OffsetMethod::new_internal(src, dst, saved.offset, 0.02);
            method.init(data)?;
            Some(Box::new(method) as Box<dyn Calibrator>)
        }
    };
    let _ = msg_tx.send(CalibrationMessage::ProfileRestored {
        continuous: method.is_some(),
    });
    Ok(method)
}

fn optional_profile(
    result: libmotoc::Result<SavedCalibration>,
) -> Result<Option<SavedCalibration>, StepError> {
    match result {
        Ok(saved) => Ok(Some(saved)),
        Err(libmotoc::Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(StepError::Backend(format!(
            "Cannot read saved calibration: {error}"
        ))),
    }
}

fn restored_origin_offset(saved: &SavedCalibration, source_offset: TransformD) -> TransformD {
    saved.offset * source_offset
}

fn process_movement_update<S: MessageSender>(
    cache: &MovementCache,
    xr_session: &XrSession,
    movement_state: &mut Vec<DeviceMovementState>,
    msg_tx: &S,
) {
    let time = xr_session.now();
    let now = Instant::now();

    for cached in &cache.devices {
        if let Ok((_location, velocity)) = cached.space.relate(&cache.reference_space, time) {
            let linear_moving = velocity.effective_linear().norm() > MOVEMENT_LINEAR_THRESHOLD;
            let angular_moving = velocity.effective_angular().norm() > MOVEMENT_ANGULAR_THRESHOLD;
            let is_moving = linear_moving || angular_moving;

            if let Some(s) = movement_state.iter_mut().find(|s| s.device_id == cached.id) {
                if is_moving {
                    s.last_moving_time = Some(now);
                }
            } else {
                movement_state.push(DeviceMovementState {
                    device_id: cached.id.clone(),
                    last_moving_time: if is_moving { Some(now) } else { None },
                });
            }
        }
    }

    let movements: Vec<DeviceMovement> = movement_state
        .iter()
        .filter_map(|s| {
            let last_moving = s.last_moving_time?;
            let elapsed = now.duration_since(last_moving).as_secs_f32();
            if elapsed < MOVEMENT_FADE_DURATION {
                let intensity = 1.0 - (elapsed / MOVEMENT_FADE_DURATION);
                Some(DeviceMovement {
                    device_id: s.device_id.clone(),
                    intensity,
                })
            } else {
                None
            }
        })
        .collect();

    let _ = msg_tx.send(CalibrationMessage::MovementUpdate { movements });
}

#[derive(Debug)]
enum StepError {
    RuntimeDisconnected,
    NotConnected { action: &'static str },
    Backend(String),
    DeviceNotFound { role: &'static str, serial: String },
}

impl StepError {
    fn user_message(&self) -> String {
        match self {
            Self::RuntimeDisconnected => "OpenXR runtime disconnected".into(),
            Self::NotConnected { action } => format!("Connect to Monado to enable {action}"),
            Self::Backend(detail) => format!("Calibration backend: {detail}"),
            Self::DeviceNotFound { role, serial } => {
                format!("{role} device '{serial}' not found or not tracked")
            }
        }
    }
}

impl From<libmotoc::Error> for StepError {
    fn from(error: libmotoc::Error) -> Self {
        let mut cause = &error;
        while let libmotoc::Error::Context(_, inner) = cause {
            cause = inner;
        }
        if let libmotoc::Error::Xr(code) | libmotoc::Error::HandJointLocation(code) = cause
            && matches!(Self::from(*code), Self::RuntimeDisconnected)
        {
            Self::RuntimeDisconnected
        } else {
            Self::Backend(error.to_string())
        }
    }
}

impl From<xr::sys::Result> for StepError {
    fn from(error: xr::sys::Result) -> Self {
        match error {
            xr::sys::Result::ERROR_INSTANCE_LOST | xr::sys::Result::ERROR_SESSION_LOST => {
                Self::RuntimeDisconnected
            }
            error => Self::Backend(error.to_string()),
        }
    }
}

fn check_runtime_result<T>(
    result: Result<T, StepError>,
    monado: &libmonado::Monado,
) -> Result<T, StepError> {
    result.map_err(|error| {
        if monado.clients().is_ok() {
            error
        } else {
            StepError::RuntimeDisconnected
        }
    })
}

fn report_connected<S: MessageSender>(
    msg_tx: &S,
    result: Result<(), StepError>,
    monado: &libmonado::Monado,
) -> Result<(), StepError> {
    match check_runtime_result(result, monado) {
        Err(StepError::RuntimeDisconnected) => Err(StepError::RuntimeDisconnected),
        result => {
            report(msg_tx, result);
            Ok(())
        }
    }
}

fn report<S: MessageSender>(msg_tx: &S, result: Result<(), StepError>) {
    match result {
        Err(StepError::RuntimeDisconnected) => {
            let _ = msg_tx.send(CalibrationMessage::RuntimeDisconnected);
        }
        Err(error) => {
            let _ = msg_tx.send(CalibrationMessage::Error(error.user_message()));
        }
        Ok(()) => {}
    }
}

fn sleep_until(next: &mut Instant, interval: Duration) {
    *next += interval;
    let now = Instant::now();
    if *next > now {
        std::thread::sleep(*next - now);
    }
}

fn load_calibrator_data<'a>(
    session: &xr::Session<xr::headless::Headless>,
    monado: &'a libmonado::Monado,
) -> Result<CalibratorData<'a>, StepError> {
    let list = session.get_xdev_list().map_err(StepError::from)?;
    let xdevs = list.enumerate_xdevs().map_err(StepError::from)?;
    let mut devices = Vec::new();
    for inner in monado
        .devices()
        .map_err(|e| StepError::Backend(format!("{e:?}")))?
    {
        let serial = inner
            .serial()
            .map_err(|e| StepError::Backend(format!("{e:?}")))?;
        if let Some(xdev) = xdevs
            .iter()
            .find(|d| d.serial() == serial && d.can_create_space())
        {
            devices.push(libmotoc::Device {
                tracking_origin: inner
                    .get_info_u32(libmonado::MndProperty::PropertyTrackingOriginU32)
                    .map_err(|e| StepError::Backend(format!("{e:?}")))?,
                index: inner.index,
                serial,
                space: xdev
                    .create_space(xr::Posef::IDENTITY)
                    .map_err(StepError::from)?,
                inner,
            });
        }
    }
    Ok(CalibratorData {
        monado,
        tracking_origins: monado
            .tracking_origins()
            .map_err(|e| StepError::Backend(format!("{e:?}")))?
            .into_iter()
            .collect(),
        devices,
        stage: create_reference_space(session, xr::ReferenceSpaceType::STAGE)?,
        local: create_reference_space(session, xr::ReferenceSpaceType::LOCAL)?,
        view: create_reference_space(session, xr::ReferenceSpaceType::VIEW)?,
        now: session.instance().now().map_err(StepError::from)?,
    })
}

fn create_reference_space(
    session: &xr::Session<xr::headless::Headless>,
    kind: xr::ReferenceSpaceType,
) -> Result<xr::Space, StepError> {
    session
        .create_reference_space(kind, xr::Posef::IDENTITY)
        .map_err(StepError::from)
}

fn run_calibrator<S: MessageSender>(
    session: &mut XrSession,
    data: &mut CalibratorData<'_>,
    mut method: Box<dyn Calibrator>,
    msg_tx: &S,
) -> Result<Option<Box<dyn Calibrator>>, StepError> {
    let mut next = Instant::now();
    let deadline = next + Duration::from_secs(120);
    let result = (|| {
        let mut step = method.init(data)?;
        loop {
            match step {
                StepResult::End => return Ok(None),
                StepResult::Replace(replacement) => {
                    // Hand the continuous method back to the command loop so
                    // it remains responsive to stop/reset/recenter/shutdown.
                    return Ok(Some(replacement));
                }
                StepResult::Continue => {}
            }
            if Instant::now() >= deadline {
                return Err(StepError::Backend(
                    "Timed out; check tracking and try again".into(),
                ));
            }
            session.poll_events()?;
            data.now = session.instance.now().map_err(StepError::from)?;
            let (next_step, status) = method.step(data)?;
            if let Some(CalibratorStatus::Progress { current, max, .. }) = status {
                let _ = msg_tx.send(CalibrationMessage::Progress {
                    collected: current.try_into().unwrap_or(u32::MAX),
                    total: max.try_into().unwrap_or(u32::MAX),
                });
            }
            step = next_step;
            sleep_until(&mut next, Duration::from_millis(33));
        }
    })();
    let finish = method.finish(data).map_err(StepError::from);
    result.and_then(|next| finish.map(|()| next))
}

#[allow(clippy::too_many_arguments)]
fn handle_start_sampled<S: MessageSender>(
    session: &mut XrSession,
    data: &mut CalibratorData<'_>,
    msg_tx: &S,
    source_serial: &str,
    target_serial: &str,
    sample_count: u32,
    continuous: bool,
) -> Result<Option<Box<dyn Calibrator>>, StepError> {
    if sample_count < 3 {
        return Err(StepError::Backend(
            "At least three samples are required".into(),
        ));
    }
    let find = |serial: &str, role| {
        data.devices
            .iter()
            .position(|d| d.serial == serial || d.inner.name == serial)
            .ok_or_else(|| StepError::DeviceNotFound {
                role,
                serial: serial.into(),
            })
    };
    let src = find(source_serial, "Source")?;
    let dst = find(target_serial, "Target")?;
    if data.devices[src].tracking_origin == data.devices[dst].tracking_origin {
        return Err(StepError::Backend(
            "Select devices from different tracking origins".into(),
        ));
    }
    for seconds in (1..=3).rev() {
        let _ = msg_tx.send(CalibrationMessage::Countdown { seconds });
        std::thread::sleep(Duration::from_secs(1));
    }
    let mut next = run_calibrator(
        session,
        data,
        Box::new(SampledMethod::new(
            src,
            dst,
            continuous,
            sample_count,
            DEFAULT_PROFILE.into(),
        )),
        msg_tx,
    )?;
    // SampledMethod applies the offset itself. The UI must not apply it again.
    if let Some(method) = next.as_mut() {
        method.init(data)?;
    }
    let _ = msg_tx.send(CalibrationMessage::SampledComplete {
        continuous: next.is_some(),
    });
    Ok(next)
}

fn handle_recenter<S: MessageSender>(
    xr_session: Option<&mut XrSession>,
    msg_tx: &S,
) -> Result<(), StepError> {
    let session = xr_session.ok_or(StepError::NotConnected { action: "recenter" })?;
    let monado = libmonado::Monado::auto_connect().map_err(StepError::Backend)?;
    let mut data = load_calibrator_data(&session.session, &monado)?;
    for seconds in (1..=3).rev() {
        let _ = msg_tx.send(CalibrationMessage::RecenterCountdown { seconds });
        std::thread::sleep(Duration::from_secs(1));
    }
    let method = RecenterMethod::new("stage", &Some("keep".into()))?;
    run_calibrator(session, &mut data, Box::new(method), msg_tx)?;
    let _ = msg_tx.send(CalibrationMessage::RecenterComplete);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_loss_reports_a_silent_disconnect_each_time() {
        let (tx, rx) = mpsc::channel();
        for code in [
            xr::sys::Result::ERROR_INSTANCE_LOST,
            xr::sys::Result::ERROR_SESSION_LOST,
            xr::sys::Result::ERROR_INSTANCE_LOST,
        ] {
            report(&tx, Err(code.into()));
            assert!(matches!(
                rx.try_recv(),
                Ok(CalibrationMessage::RuntimeDisconnected)
            ));
            assert!(rx.try_recv().is_err());
        }
    }

    #[test]
    fn contextual_libmotoc_runtime_loss_is_also_a_disconnect() {
        let error = libmotoc::Error::Context(
            "sampling".into(),
            Box::new(libmotoc::Error::Context(
                "locating device".into(),
                Box::new(libmotoc::Error::Xr(xr::sys::Result::ERROR_SESSION_LOST)),
            )),
        );
        assert!(matches!(
            StepError::from(error),
            StepError::RuntimeDisconnected
        ));
    }

    #[test]
    fn other_backend_errors_still_notify() {
        let (tx, rx) = mpsc::channel();
        report(&tx, Err(xr::sys::Result::ERROR_VALIDATION_FAILURE.into()));
        assert!(matches!(rx.try_recv(), Ok(CalibrationMessage::Error(_))));
        report(&tx, Ok(()));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn only_missing_profiles_are_ignored() {
        assert!(
            optional_profile(Err(libmotoc::Error::Io(std::io::Error::from(
                std::io::ErrorKind::NotFound
            ),)))
            .unwrap()
            .is_none()
        );
        assert!(
            optional_profile(Err(libmotoc::Error::Io(std::io::Error::from(
                std::io::ErrorKind::PermissionDenied
            ),)))
            .is_err()
        );
        let corrupt = serde_json::from_str::<SavedCalibration>("{broken")
            .err()
            .unwrap();
        assert!(optional_profile(Err(libmotoc::Error::Json(corrupt))).is_err());
    }

    #[test]
    fn restore_composes_saved_offset_before_current_source_like_motoc() {
        let mut pose: libmonado::Pose = TransformD::default().into();
        pose.position.z = 2.0;
        pose.orientation.v.y = std::f32::consts::FRAC_1_SQRT_2;
        pose.orientation.s = std::f32::consts::FRAC_1_SQRT_2;
        let saved = SavedCalibration {
            offset_type: OffsetType::TrackingOrigin,
            src: "source".into(),
            dst: "target".into(),
            offset: pose.into(),
        };
        let mut source: libmonado::Pose = TransformD::default().into();
        source.position.x = 1.0;
        let restored = restored_origin_offset(&saved, source.into());
        assert!(restored.origin.x.abs() < 1e-6);
        assert!((restored.origin.z - 1.0).abs() < 1e-6);
    }
}

pub mod mndx;

use openxr as xr;
use std::sync::mpsc;
use std::ptr;
use std::time::{Duration, Instant};
use crate::calibration::{CalibrationCommand, CalibrationMessage, CalibrationResult, DeviceMovement, TransformD};
use crate::calibration::sampled::{SampleCollector, PoseSample};
use crate::calibration::floor::FloorCalibrator;
use crate::error::XrError;

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

pub use mndx::Mndx;


const XR_TYPE_GRAPHICS_BINDING_HEADLESS_MND: i32 = 1_000_246_000;

#[repr(C)]
struct GraphicsBindingHeadlessMND {
    ty: xr::sys::StructureType,
    next: *const std::ffi::c_void,
}


struct Headless;

impl xr::Graphics for Headless {
    type Requirements = ();
    type SessionCreateInfo = GraphicsBindingHeadlessMND;
    type Format = i64;
    type SwapchainImage = ();

    fn raise_format(x: i64) -> Self::Format {
        x
    }

    fn lower_format(x: Self::Format) -> i64 {
        x
    }

    fn requirements(_instance: &xr::Instance, _system: xr::SystemId) -> xr::Result<Self::Requirements> {
        Ok(())
    }

    unsafe fn create_session(
        instance: &xr::Instance,
        system: xr::SystemId,
        info: &Self::SessionCreateInfo,
    ) -> xr::Result<xr::sys::Session> {
        let session_create_info = xr::sys::SessionCreateInfo {
            ty: xr::sys::SessionCreateInfo::TYPE,
            next: std::ptr::from_ref(info).cast(),
            create_flags: xr::sys::SessionCreateFlags::EMPTY,
            system_id: system,
        };

        let mut session_raw: xr::sys::Session = xr::sys::Session::NULL;
        // SAFETY: Calling OpenXR FFI function with valid parameters
        let result = unsafe {
            (instance.fp().create_session)(
                instance.as_raw(),
                &raw const session_create_info,
                &raw mut session_raw,
            )
        };

        if result != xr::sys::Result::SUCCESS {
            return Err(result);
        }

        Ok(session_raw)
    }

    fn enumerate_swapchain_images(_swapchain: &xr::Swapchain<Self>) -> xr::Result<Vec<Self::SwapchainImage>> {
        // Headless mode doesn't use swapchains
        Ok(Vec::new())
    }
}

pub struct XrSession {
    instance: xr::Instance,
    session: xr::Session<xr::AnyGraphics>,
    mndx: Option<Mndx>,
}

impl XrSession {
    pub fn new() -> Result<Self, XrError> {
        let entry = unsafe { xr::Entry::load() }
            .map_err(|e| XrError::RuntimeError(format!("Failed to load OpenXR: {e:?}")))?;

        let available = entry.enumerate_extensions()
            .map_err(|e| XrError::RuntimeError(format!("Failed to enumerate extensions: {e:?}")))?;

        if !available.mnd_headless {
            return Err(XrError::ExtensionNotAvailable("MND_headless".to_string()));
        }

        let mut exts = xr::ExtensionSet::default();
        exts.mnd_headless = true;

        let has_mndx = available.other.iter().any(|e| e == "XR_MNDX_xdev_space");
        if has_mndx {
            exts.other.push("XR_MNDX_xdev_space".to_string());
        }
        if available.khr_convert_timespec_time {
            exts.khr_convert_timespec_time = true;
        }

        let instance = entry.create_instance(
            &xr::ApplicationInfo {
                application_name: "spacecal-for-monado",
                application_version: 1,
                engine_name: "none",
                engine_version: 0,
                api_version: xr::Version::new(1, 0, 0),
            },
            &exts,
            &[],
        ).map_err(|e| XrError::InstanceCreationFailed(format!("{e:?}")))?;

        let system = instance.system(xr::FormFactor::HEAD_MOUNTED_DISPLAY)
            .map_err(|e| XrError::RuntimeError(format!("Failed to get system: {e:?}")))?;

        let graphics_binding = GraphicsBindingHeadlessMND {
            ty: xr::sys::StructureType::from_raw(XR_TYPE_GRAPHICS_BINDING_HEADLESS_MND),
            next: ptr::null(),
        };

        let (session, _frame_waiter, _frame_stream) = unsafe {
            instance.create_session::<Headless>(system, &graphics_binding)
                .map_err(|e| XrError::SessionCreationFailed(format!("{e:?}")))?
        };

        let session = session.into_any_graphics();

        let mndx = if has_mndx {
            Mndx::new(&instance).ok()
        } else {
            None
        };

        Ok(Self {
            instance,
            session,
            mndx,
        })
    }

    pub fn now(&self) -> xr::Time {
        self.instance.now().unwrap_or(xr::Time::from_nanos(1))
    }
}


const MOVEMENT_LINEAR_THRESHOLD: f32 = 0.2;
const MOVEMENT_ANGULAR_THRESHOLD: f32 = 0.5; // rad/s (about 30 deg/s)
const MOVEMENT_FADE_DURATION: f32 = 2.0;

fn mag3(v: xr::Vector3f) -> f32 {
    (v.x * v.x + v.y * v.y + v.z * v.z).sqrt()
}

struct CachedDevice {
    id: String,
    space: xr::Space,
}

struct MovementCache {
    _list: mndx::XDevList,
    reference_space: xr::Space,
    devices: Vec<CachedDevice>,
}

fn build_movement_cache(mndx: &mndx::Mndx, session: &xr::Session<xr::AnyGraphics>) -> Option<MovementCache> {
    let list = mndx.create_list(session).ok()?;
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
            if s.is_empty() { d.name().to_string() } else { s.to_string() }
        };
        if let Ok(space) = d.create_space(session.clone()) {
            devices.push(CachedDevice { id, space });
        }
    }

    if devices.is_empty() {
        return None;
    }

    Some(MovementCache { _list: list, reference_space, devices })
}

struct DeviceMovementState {
    device_id: String,
    last_moving_time: Option<Instant>,
}

#[allow(clippy::needless_pass_by_value, clippy::too_many_lines)]
pub fn xr_event_loop<S: MessageSender>(
    cmd_rx: mpsc::Receiver<CalibrationCommand>,
    msg_tx: S,
) {
    let mut xr_session = XrSession::new().ok();

    let mut movement_detection_active = false;
    let mut movement_cache: Option<MovementCache> = None;
    let mut movement_state: Vec<DeviceMovementState> = Vec::new();
    let mut last_movement_update = Instant::now();
    let mut last_reconnect_attempt = Instant::now();
    let mut consecutive_failures = 0u32;

    loop {
        let recv_result = if movement_detection_active {
            cmd_rx.recv_timeout(Duration::from_millis(100))
        } else {
            cmd_rx.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected)
        };

        if (xr_session.is_none() || consecutive_failures >= 5)
            && last_reconnect_attempt.elapsed() >= Duration::from_secs(2)
        {
            movement_cache = None;
            if movement_detection_active {
                movement_detection_active = false;
                movement_state.clear();
                let _ = msg_tx.send(CalibrationMessage::MovementUpdate { movements: vec![] });
            }
            last_reconnect_attempt = Instant::now();
            consecutive_failures = 0;
            xr_session = XrSession::new().ok();
        }

        if movement_detection_active && last_movement_update.elapsed() >= Duration::from_millis(100) {
            let mut op_ok = false;

            if let Some(ref cache) = movement_cache
                && let Some(ref xr_session) = xr_session
            {
                op_ok = true;
                process_movement_update(cache, xr_session, &mut movement_state, &msg_tx);
            } else if xr_session.is_some() {
                consecutive_failures += 1;
            }

            if op_ok {
                consecutive_failures = 0;
            } else if xr_session.is_some() && movement_cache.is_some() {
                consecutive_failures += 1;
            }

            last_movement_update = Instant::now();
        }

        match recv_result {
            Ok(CalibrationCommand::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Ok(CalibrationCommand::StartMovementDetection) => {
                movement_detection_active = true;
                movement_state.clear();
                last_movement_update = Instant::now();
                if let Some(ref xr_session) = xr_session
                    && let Some(ref mndx) = xr_session.mndx
                {
                    movement_cache = build_movement_cache(mndx, &xr_session.session);
                }
            }
            Ok(CalibrationCommand::StopMovementDetection) => {
                movement_cache = None;
                movement_detection_active = false;
                movement_state.clear();
                let _ = msg_tx.send(CalibrationMessage::MovementUpdate { movements: vec![] });
            }
            Ok(CalibrationCommand::StartSampled { source_serial, target_serial, target_origin_index, sample_count, stage_offset }) => {
                report(&msg_tx, handle_start_sampled(
                    xr_session.as_ref(),
                    &msg_tx,
                    &source_serial,
                    &target_serial,
                    target_origin_index,
                    sample_count,
                    stage_offset.as_ref(),
                ));
            }
            Ok(CalibrationCommand::CalibrateFloor { target_serial }) => {
                report(&msg_tx, handle_calibrate_floor(xr_session.as_ref(), &msg_tx, &target_serial));
            }
            Ok(CalibrationCommand::Recenter) => {
                report(&msg_tx, handle_recenter(xr_session.as_ref(), &msg_tx));
            }
        }
    }
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
            let linear_valid = velocity.velocity_flags.contains(xr::SpaceVelocityFlags::LINEAR_VALID);
            let angular_valid = velocity.velocity_flags.contains(xr::SpaceVelocityFlags::ANGULAR_VALID);

            if !linear_valid && !angular_valid {
                continue;
            }

            let linear_moving = linear_valid && mag3(velocity.linear_velocity) > MOVEMENT_LINEAR_THRESHOLD;
            let angular_moving = angular_valid && mag3(velocity.angular_velocity) > MOVEMENT_ANGULAR_THRESHOLD;
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
                Some(DeviceMovement { device_id: s.device_id.clone(), intensity })
            } else {
                None
            }
        })
        .collect();

    let _ = msg_tx.send(CalibrationMessage::MovementUpdate { movements });
}

#[derive(Debug)]
enum StepError {
    NotConnected { action: &'static str },
    MndxUnavailable,
    DeviceList(String),
    Enumerate(String),
    DeviceNotFound { role: &'static str, serial: String },
    Unsupported { role: &'static str, name: String },
    SpaceCreate { role: &'static str, detail: String },
    ReferenceSpace { kind: &'static str, detail: String },
    Locate { what: &'static str, detail: String },
    InvalidSample(String),
    Floor(String),
    HmdPoseInvalid { pos_valid: bool, ori_valid: bool },
    Compute(crate::error::CalibrationError),
}

impl StepError {
    fn user_message(&self) -> String {
        match self {
            Self::NotConnected { action } => format!("Connect to Monado to enable {action}"),
            Self::MndxUnavailable => "MNDX_xdev_space extension not available".to_string(),
            Self::DeviceList(detail) => format!("Failed to create device list: {detail}"),
            Self::Enumerate(detail) => format!("Failed to enumerate devices: {detail}"),
            Self::DeviceNotFound { role, serial } => format!("{role} device '{serial}' not found"),
            Self::Unsupported { role, name } => format!("{role} device '{name}' does not support pose tracking"),
            Self::SpaceCreate { role, detail } => format!("Failed to create space for {} device: {detail}", role.to_lowercase()),
            Self::ReferenceSpace { kind, detail } => format!("Failed to create {kind} space: {detail}"),
            Self::Locate { what, detail } => format!("Failed to locate {what}: {detail}"),
            Self::InvalidSample(detail) => format!("Invalid pose sample: {detail}"),
            Self::Floor(detail) => format!("Floor calibration failed: {detail}"),
            Self::HmdPoseInvalid { pos_valid, ori_valid } => format!("HMD pose not valid (pos={pos_valid}, ori={ori_valid})"),
            Self::Compute(e) => match e {
                crate::error::CalibrationError::InsufficientSamples { .. } =>
                    "Not enough movement \u{2014} grip both devices firmly and sweep a wide figure-eight".to_string(),
                crate::error::CalibrationError::SvdFailed =>
                    "Calibration math failed \u{2014} try again with more varied motion".to_string(),
                other => format!("Calibration failed: {other:?}"),
            },
        }
    }
}

fn report<S: MessageSender>(msg_tx: &S, result: Result<(), StepError>) {
    if let Err(e) = result {
        let _ = msg_tx.send(CalibrationMessage::Error(e.user_message()));
    }
}

fn sleep_until(next: &mut Instant, interval: Duration) {
    *next += interval;
    let now = Instant::now();
    if *next > now {
        std::thread::sleep(*next - now);
    }
}

fn enumerate_devices(xr_session: &XrSession) -> Result<Vec<mndx::XDev>, StepError> {
    let mndx = xr_session.mndx.as_ref().ok_or(StepError::MndxUnavailable)?;
    let list = mndx.create_list(&xr_session.session)
        .map_err(|e| StepError::DeviceList(format!("{e:?}")))?;
    list.enumerate_xdevs()
        .map_err(|e| StepError::Enumerate(format!("{e:?}")))
}

fn resolve_device<'a>(
    devices: &'a [mndx::XDev],
    serial: &str,
    role: &'static str,
) -> Result<&'a mndx::XDev, StepError> {
    let dev = devices.iter()
        .find(|d| d.serial() == serial || d.name() == serial)
        .ok_or_else(|| StepError::DeviceNotFound { role, serial: serial.to_string() })?;

    if !dev.can_create_space() {
        return Err(StepError::Unsupported { role, name: dev.name().to_string() });
    }

    Ok(dev)
}

fn create_device_space(
    dev: &mndx::XDev,
    session: &xr::Session<xr::AnyGraphics>,
    role: &'static str,
) -> Result<xr::Space, StepError> {
    dev.create_space(session.clone())
        .map_err(|e| StepError::SpaceCreate { role, detail: format!("{e:?}") })
}

fn create_reference_space(
    session: &xr::Session<xr::AnyGraphics>,
    space_type: xr::ReferenceSpaceType,
    kind: &'static str,
) -> Result<xr::Space, StepError> {
    session.create_reference_space(space_type, xr::Posef::IDENTITY)
        .map_err(|e| StepError::ReferenceSpace { kind, detail: format!("{e:?}") })
}

fn create_stage_or_local_space(session: &xr::Session<xr::AnyGraphics>) -> Result<xr::Space, StepError> {
    if let Ok(space) = session.create_reference_space(xr::ReferenceSpaceType::STAGE, xr::Posef::IDENTITY) {
        return Ok(space);
    }
    create_reference_space(session, xr::ReferenceSpaceType::LOCAL, "reference")
}

fn collect_pose_samples<S: MessageSender>(
    xr_session: &XrSession,
    source_space: &xr::Space,
    target_space: &xr::Space,
    reference_space: &xr::Space,
    sample_count: u32,
    stage_offset: Option<&([f64; 3], [f64; 4])>,
    msg_tx: &S,
) -> Result<SampleCollector, StepError> {
    let mut collector = SampleCollector::new(sample_count);

    let sample_interval = Duration::from_millis(33);
    let mut next_sample_time = Instant::now();

    while !collector.is_complete() {
        let time = xr_session.now();

        let source_location = source_space.locate(reference_space, time)
            .map_err(|e| StepError::Locate { what: "source space", detail: format!("{e:?}") })?;
        let target_location = target_space.locate(reference_space, time)
            .map_err(|e| StepError::Locate { what: "target space", detail: format!("{e:?}") })?;

        let source_valid = source_location.location_flags.contains(
            xr::SpaceLocationFlags::POSITION_VALID | xr::SpaceLocationFlags::ORIENTATION_VALID
        );
        let target_valid = target_location.location_flags.contains(
            xr::SpaceLocationFlags::POSITION_VALID | xr::SpaceLocationFlags::ORIENTATION_VALID
        );

        if !source_valid || !target_valid {
            sleep_until(&mut next_sample_time, sample_interval);
            continue;
        }

        let source_pose = source_location.pose;
        let target_pose = target_location.pose;

        let mut source_transform = TransformD::from_xr_pose(
            [source_pose.position.x, source_pose.position.y, source_pose.position.z],
            [source_pose.orientation.x, source_pose.orientation.y,
             source_pose.orientation.z, source_pose.orientation.w],
        );
        let mut target_transform = TransformD::from_xr_pose(
            [target_pose.position.x, target_pose.position.y, target_pose.position.z],
            [target_pose.orientation.x, target_pose.orientation.y,
             target_pose.orientation.z, target_pose.orientation.w],
        );

        if let Some((pos, ori)) = stage_offset {
            let stage = TransformD::from_position_orientation_f64(*pos, *ori);
            source_transform = stage.mul(&source_transform);
            target_transform = stage.mul(&target_transform);
        }

        let sample = PoseSample::from_transforms(&source_transform, &target_transform);
        collector.add_sample(sample)
            .map_err(|e| StepError::InvalidSample(format!("{e:?}")))?;

        let (collected, total) = collector.progress();
        let _ = msg_tx.send(CalibrationMessage::Progress { collected, total });

        if !collector.is_complete() {
            sleep_until(&mut next_sample_time, sample_interval);
        }
    }

    Ok(collector)
}

fn handle_start_sampled<S: MessageSender>(
    xr_session: Option<&XrSession>,
    msg_tx: &S,
    source_serial: &str,
    target_serial: &str,
    target_origin_index: u32,
    sample_count: u32,
    stage_offset: Option<&([f64; 3], [f64; 4])>,
) -> Result<(), StepError> {
    let xr_session = xr_session.ok_or(StepError::NotConnected { action: "calibration" })?;

    let devices = enumerate_devices(xr_session)?;
    let source_dev = resolve_device(&devices, source_serial, "Source")?;
    let target_dev = resolve_device(&devices, target_serial, "Target")?;

    let source_space = create_device_space(source_dev, &xr_session.session, "Source")?;
    let target_space = create_device_space(target_dev, &xr_session.session, "Target")?;
    let reference_space = create_stage_or_local_space(&xr_session.session)?;

    // Countdown only after setup is known-good, so a misconfigured device fails fast.
    for seconds_left in (1..=3).rev() {
        let _ = msg_tx.send(CalibrationMessage::Countdown { seconds: seconds_left });
        std::thread::sleep(Duration::from_secs(1));
    }

    let collector = collect_pose_samples(
        xr_session,
        &source_space,
        &target_space,
        &reference_space,
        sample_count,
        stage_offset,
        msg_tx,
    )?;

    let (offset, median_error_degrees, axis_diversity) =
        collector.compute_calibration().map_err(StepError::Compute)?;

    // O = S × T⁻¹ (averaged); satisfies O × T = S, applied to the target origin.
    let _ = msg_tx.send(CalibrationMessage::SampledComplete(CalibrationResult {
        transform: offset,
        target_origin_index,
        median_error_degrees,
        axis_diversity,
    }));
    Ok(())
}

fn handle_calibrate_floor<S: MessageSender>(
    xr_session: Option<&XrSession>,
    msg_tx: &S,
    target_serial: &str,
) -> Result<(), StepError> {
    let xr_session = xr_session.ok_or(StepError::NotConnected { action: "floor calibration" })?;

    let devices = enumerate_devices(xr_session)?;
    let target_dev = resolve_device(&devices, target_serial, "Target")?;
    let target_space = create_device_space(target_dev, &xr_session.session, "Target")?;
    let reference_space = create_stage_or_local_space(&xr_session.session)?;

    let mut floor_cal = FloorCalibrator::with_default_config();
    floor_cal.start();

    let sample_interval = Duration::from_millis(33);
    let mut next_sample_time = Instant::now();

    while floor_cal.is_active() {
        let time = xr_session.now();

        let Ok(location) = target_space.locate(&reference_space, time) else {
            sleep_until(&mut next_sample_time, sample_interval);
            continue;
        };

        if location.location_flags.contains(xr::SpaceLocationFlags::POSITION_VALID) {
            match floor_cal.add_sample(location.pose.position.y) {
                Ok(Some(adjustment)) => {
                    let _ = msg_tx.send(CalibrationMessage::FloorComplete {
                        height_adjustment: adjustment,
                    });
                    break;
                }
                Ok(None) => {
                    let (collected, total) = floor_cal.progress();
                    let _ = msg_tx.send(CalibrationMessage::FloorProgress { collected, total });
                }
                Err(e) => return Err(StepError::Floor(format!("{e:?}"))),
            }
        }

        sleep_until(&mut next_sample_time, sample_interval);
    }
    Ok(())
}

fn handle_recenter<S: MessageSender>(
    xr_session: Option<&XrSession>,
    msg_tx: &S,
) -> Result<(), StepError> {
    let xr_session = xr_session.ok_or(StepError::NotConnected { action: "recenter" })?;

    let view_space = create_reference_space(&xr_session.session, xr::ReferenceSpaceType::VIEW, "VIEW")?;
    let stage_space = create_reference_space(&xr_session.session, xr::ReferenceSpaceType::STAGE, "STAGE")?;

    // Countdown after spaces exist, so the HMD pose is captured once the user is in position.
    for seconds_left in (1..=3).rev() {
        let _ = msg_tx.send(CalibrationMessage::RecenterCountdown { seconds: seconds_left });
        std::thread::sleep(Duration::from_secs(1));
    }

    let time = xr_session.now();
    let location = view_space.locate(&stage_space, time)
        .map_err(|e| StepError::Locate { what: "HMD", detail: format!("{e:?}") })?;

    let pos_valid = location.location_flags.contains(xr::SpaceLocationFlags::POSITION_VALID);
    let ori_valid = location.location_flags.contains(xr::SpaceLocationFlags::ORIENTATION_VALID);
    if !pos_valid || !ori_valid {
        return Err(StepError::HmdPoseInvalid { pos_valid, ori_valid });
    }

    let position = [
        location.pose.position.x,
        location.pose.position.y,
        location.pose.position.z,
    ];
    let orientation = [
        location.pose.orientation.x,
        location.pose.orientation.y,
        location.pose.orientation.z,
        location.pose.orientation.w,
    ];

    let _ = msg_tx.send(CalibrationMessage::RecenterComplete { position, orientation });
    Ok(())
}

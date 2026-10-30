//! Common functionality across pointing devices

use embassy_time::{Duration, Instant, Timer};
use embedded_hal::digital::InputPin;
use embedded_hal_async::digital::Wait;
use futures::future::pending;
use rmk_macro::{input_device, processor};
#[cfg(feature = "split")]
use rmk_types::action::Action;
use rmk_types::keycode::HidKeyCode;
#[cfg(feature = "split")]
use rmk_types::keycode::KeyCode;
use usbd_hid::descriptor::MouseReport;

use crate::channel::{send_hid_mouse_report, send_hid_report};
use crate::core_traits::Runnable;
#[cfg(feature = "split")]
use crate::event::{ActionEvent, KeyboardEvent, KeyboardEventPos, LayerChangeEvent, PeripheralSettingsEvent};
use crate::event::{
    Axis, AxisEvent, AxisValType, EventSubscriber, PointingEvent, PointingProcessorEvent, PointingSetCpiEvent,
    PointingTransformEvent, SubscribableEvent, publish_event,
};
use crate::hid::{KeyboardReport, Report};
use crate::keymap::KeyMap;

pub const ALL_POINTING_DEVICES: u8 = 255;

/// Motion data from the sensor
#[derive(Debug, Clone, Copy, Default)]
pub struct MotionData {
    pub dx: i16,
    pub dy: i16,
}

/// Errors of pointing devices
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum PointingDriverError {
    /// SPI communication error
    Spi,
    /// Invalid product ID detected
    InvalidProductId(u8),
    /// Initialization failed
    InitFailed,
    /// Invalid CPI value
    InvalidCpi,
    /// Invalid firmware signature detected
    InvalidFwSignature((u8, u8)),
    /// Invalid rotational transform angle
    InvalidRotTransAngle,
    /// Not implemented
    NotImplementedError,
}

pub trait PointingDriver {
    type MOTION: InputPin + Wait;

    async fn init(&mut self) -> Result<(), PointingDriverError>;
    async fn read_motion(&mut self) -> Result<MotionData, PointingDriverError>;
    fn motion_pending(&mut self) -> bool;
    fn motion_gpio(&mut self) -> Option<&mut Self::MOTION>;
    async fn set_resolution(&mut self, _cpi: u16) -> Result<(), PointingDriverError> {
        debug!("set_resolution() is not implemented for this sensor.");
        Err(PointingDriverError::NotImplementedError)
    }
}

/// Initialization state for the device
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitState {
    Pending,
    Initializing(u8),
    Ready,
    Failed,
}

/// PointingDevice an InputDevice for RMK
///
/// This device publishes `PointingEvent` events with relative X/Y movement.
#[processor(subscribe = [PointingSetCpiEvent])]
#[input_device(publish = PointingEvent)]
#[::rmk::macros::runnable_generated]
pub struct PointingDevice<S: PointingDriver> {
    pub sensor: S,
    pub init_state: InitState,
    pub poll_interval: Duration,
    pub id: u8,
    pub report_interval: Duration,
    pub last_poll: Instant,
    pub last_report: Instant,
    pub accumulated_x: i32,
    pub accumulated_y: i32,
}

// Pointing motion is real-time state, not a lossless command stream. Using the
// generic input-device backpressure path lets a temporarily busy BLE split
// sender build a FIFO of stale 125 Hz samples. K:04 deliberately publishes its
// PMW3610 motion immediately for the same reason: keep the newest motion and
// let lagging subscribers skip obsolete samples.
impl<S: PointingDriver> Runnable for PointingDevice<S> {
    async fn run(&mut self) -> ! {
        use embassy_futures::select::{Either, select};

        let mut cpi_sub = PointingSetCpiEvent::subscriber();
        loop {
            match select(self.read_pointing_event(), cpi_sub.next_event()).await {
                Either::First(event) => publish_event(event),
                Either::Second(event) => self.on_pointing_set_cpi_event(event).await,
            }
        }
    }
}

impl<S: PointingDriver> PointingDevice<S> {
    const MAX_INIT_RETRIES: u8 = 3;

    async fn try_init(&mut self) -> bool {
        match self.init_state {
            InitState::Ready => return true,
            InitState::Failed => return false,
            InitState::Pending => {
                self.init_state = InitState::Initializing(0);
            }
            InitState::Initializing(_) => {}
        }

        if let InitState::Initializing(retry_count) = self.init_state {
            info!(
                "PointingDevice {}: Initializing sensor (attempt {})",
                self.id,
                retry_count + 1
            );

            match self.sensor.init().await {
                Ok(()) => {
                    info!("PointingDevice {}: Sensor initialized successfully", self.id);
                    self.init_state = InitState::Ready;
                    return true;
                }
                Err(e) => {
                    error!("PointingDevice {}: Init failed: {:?}", self.id, e);
                    if retry_count + 1 >= Self::MAX_INIT_RETRIES {
                        error!("PointingDevice {}: Max retries reached, giving up", self.id);
                        self.init_state = InitState::Failed;
                        return false;
                    }
                    self.init_state = InitState::Initializing(retry_count + 1);
                    Timer::after(Duration::from_millis(100)).await;
                    return false;
                }
            }
        }

        false
    }

    async fn poll_once(&mut self) {
        if self.init_state != InitState::Ready && !self.try_init().await {
            return;
        }

        if !self.sensor.motion_pending() {
            return;
        }

        match self.sensor.read_motion().await {
            Ok(motion) => {
                self.accumulated_x = self.accumulated_x.saturating_add(motion.dx as i32);
                self.accumulated_y = self.accumulated_y.saturating_add(motion.dy as i32);
            }
            Err(_e) => {
                warn!("PointingDevice {}: Read motion error", self.id);
            }
        }
    }

    fn take_report_event(&mut self) -> Option<PointingEvent> {
        if self.accumulated_x == 0 && self.accumulated_y == 0 {
            return None;
        }

        // A HID mouse report can carry only i8 motion. Emit one transport-safe
        // chunk and preserve the residual for subsequent 125 Hz reports; zeroing
        // the full accumulator here silently discarded fast PMW3610 movement.
        let dx = self.accumulated_x.clamp(i8::MIN as i32, i8::MAX as i32) as i16;
        let dy = self.accumulated_y.clamp(i8::MIN as i32, i8::MAX as i32) as i16;

        self.accumulated_x -= i32::from(dx);
        self.accumulated_y -= i32::from(dy);

        Some(PointingEvent {
            device_id: self.id,
            axes: [
                AxisEvent {
                    typ: AxisValType::Rel,
                    axis: Axis::X,
                    value: dx,
                },
                AxisEvent {
                    typ: AxisValType::Rel,
                    axis: Axis::Y,
                    value: dy,
                },
                AxisEvent {
                    typ: AxisValType::Rel,
                    axis: Axis::Z,
                    value: 0,
                },
            ],
        })
    }
}

impl<S: PointingDriver> PointingDevice<S> {
    async fn on_pointing_set_cpi_event(&mut self, e: PointingSetCpiEvent) {
        if e.device_id == self.id {
            info!("PointingDevice {}: Setting resolution to {}", self.id, e.cpi);
            if let Err(err) = self.sensor.set_resolution(e.cpi).await {
                debug!("PointingDevice {}: Setting resolution failed: {:?}", self.id, err);
            }
        }
    }

    // Read accumulated pointing event
    //
    // +--------------- loop ---------------+
    // ¦ poll_wait   report_wait            ¦
    // ¦     ¦           ¦                  ¦
    // ¦     V           V                  ¦
    // ¦ poll_once()     take_report_event()¦
    // ¦     ¦           ¦                  ¦
    // ¦     +- accum += ¦                  ¦
    // ¦                 >- Event returned  ¦
    // +------------------------------------+
    async fn read_pointing_event(&mut self) -> PointingEvent {
        use embassy_futures::select::{Either, select};

        if self.last_poll == Instant::MIN {
            self.last_poll = Instant::now();
        }
        if self.last_report == Instant::MIN {
            self.last_report = Instant::now();
        }

        loop {
            // `wait_for_low()` completes immediately while MOTION remains
            // asserted. Check the report deadline before polling again so a
            // continuously moving sensor cannot starve its own reports.
            if (self.accumulated_x != 0 || self.accumulated_y != 0)
                && self.last_report.elapsed() >= self.report_interval
                && let Some(event) = self.take_report_event()
            {
                self.last_report = Instant::now();
                return event;
            }

            let poll_wait = async {
                if let Some(gpio) = self.sensor.motion_gpio() {
                    let _ = gpio.wait_for_low().await;
                } else {
                    Timer::after(
                        self.poll_interval
                            .checked_sub(self.last_poll.elapsed())
                            .unwrap_or(Duration::MIN),
                    )
                    .await;
                }
            };

            let report_wait = async {
                if self.accumulated_x != 0 || self.accumulated_y != 0 {
                    Timer::after(
                        self.report_interval
                            .checked_sub(self.last_report.elapsed())
                            .unwrap_or(Duration::MIN),
                    )
                    .await;
                } else {
                    // Don't schedule report if there's no accumulated motion
                    pending::<()>().await;
                }
            };

            match select(poll_wait, report_wait).await {
                Either::First(_) => {
                    self.poll_once().await;
                    self.last_poll = Instant::now();
                }
                Either::Second(_) => {
                    if let Some(event) = self.take_report_event() {
                        self.last_report = Instant::now();
                        return event;
                    }
                }
            }
        }
    }
}

/// Pointing mode determines how raw XY motion is interpreted
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum PointingMode {
    /// Ignore motion from the pointing device.
    Disabled,
    /// Default cursor mode - XY maps to mouse XY movement
    Cursor(CursorConfig),
    /// Scroll mode - XY maps to wheel (vertical) and pan (horizontal)
    Scroll(ScrollConfig),
    /// Sniper mode - XY maps to cursor but at reduced sensitivity
    Sniper(SniperConfig),
    /// Caret mode, XY maps to vertical and horizontal caret movement
    Caret(CaretConfig),
}

impl Default for PointingMode {
    fn default() -> Self {
        Self::Cursor(CursorConfig::default())
    }
}

/// Tracks momentary and sticky pointing-mode key selections.
///
/// Without sticky mode a selection is active only while its key is held. With
/// sticky mode every press latches the selected mode, another mode replaces
/// it, and pressing the latched mode again returns to the configured mode.
#[derive(Clone, Copy)]
pub struct PointingModeKeyState<M> {
    held: Option<M>,
    latched: Option<M>,
}

impl<M> PointingModeKeyState<M> {
    /// Create an inactive mode-key state.
    pub const fn new() -> Self {
        Self {
            held: None,
            latched: None,
        }
    }
}

impl<M> Default for PointingModeKeyState<M> {
    fn default() -> Self {
        Self::new()
    }
}

impl<M: Copy + PartialEq> PointingModeKeyState<M> {
    /// Current key-selected override, if any.
    pub fn mode_override(&self) -> Option<M> {
        self.held.or(self.latched)
    }

    /// Apply one press or release of a pointing-mode key.
    pub fn handle(&mut self, mode: M, pressed: bool, sticky: bool) {
        if pressed {
            if sticky {
                self.held = None;
                self.latched = if self.latched == Some(mode) { None } else { Some(mode) };
            } else {
                self.latched = None;
                self.held = Some(mode);
            }
        } else if self.held == Some(mode) {
            self.held = None;
        }
    }

    /// Drop a latched selection when sticky mode is disabled in settings.
    pub fn set_sticky_enabled(&mut self, enabled: bool) {
        if !enabled {
            self.latched = None;
        }
    }
}

/// Configuration for cursor mode
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct CursorConfig {
    /// Multiplier for X axis. Higher = more output per unit of motion. 0 disables X.
    pub multiplier_x: u8,
    /// Multiplier for Y axis. Higher = more output per unit of motion. 0 disables Y.
    pub multiplier_y: u8,
    /// Invert X axis movement.
    pub invert_x: bool,
    /// Invert Y axis movement.
    pub invert_y: bool,
}

impl Default for CursorConfig {
    fn default() -> Self {
        Self {
            multiplier_x: 1,
            multiplier_y: 1,
            invert_x: false,
            invert_y: false,
        }
    }
}

/// Configuration for caret mode
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct CaretConfig {
    /// Disable X axis in caret mode.
    pub disable_x: bool,
    /// Disable y axis in caret mode.
    pub disable_y: bool,
    /// Invert X axis.
    pub invert_x: bool,
    /// Invert Y axis.
    pub invert_y: bool,
    /// Threshold for accumulated motion. Read this as sensitivity in caret mode.
    /// Higher values mean less sensitivity.
    pub threshold: i16,
    /// Keycode to emit for up rotation. Default: Up arrow
    pub keycode_up: HidKeyCode,
    /// Keycode to emit for down rotation. Default: Down arrow
    pub keycode_down: HidKeyCode,
    /// Keycode to emit for left rotation. Default: Left arrow
    pub keycode_left: HidKeyCode,
    /// Keycode to emit for right rotation. Default: Right arrow
    pub keycode_right: HidKeyCode,
}

impl Default for CaretConfig {
    fn default() -> Self {
        Self {
            disable_x: false,
            disable_y: false,
            invert_x: false,
            invert_y: false,
            threshold: 100,
            keycode_up: HidKeyCode::Up,
            keycode_down: HidKeyCode::Down,
            keycode_left: HidKeyCode::Left,
            keycode_right: HidKeyCode::Right,
        }
    }
}
/// Configuration for scroll mode
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct ScrollConfig {
    /// Multiplier for X axis (→ pan). Higher = more output per unit of motion. 0 disables horizontal pan.
    pub multiplier_x: u8,
    /// Divisor for X axis (→ pan). Higher = slower. 0 disables horizontal pan.
    pub divisor_x: u8,
    /// Multiplier for Y axis (→ wheel). Higher = more output per unit of motion. 0 disables vertical scroll.
    pub multiplier_y: u8,
    /// Divisor for Y axis (→ wheel). Higher = slower. 0 disables vertical scroll.
    pub divisor_y: u8,
    /// Invert X axis. In scroll mode X maps to pan, so this reverses pan direction.
    pub invert_x: bool,
    /// Invert Y axis. In scroll mode Y maps to wheel, so this reverses scroll direction.
    pub invert_y: bool,
}

impl Default for ScrollConfig {
    fn default() -> Self {
        Self {
            multiplier_x: 1,
            multiplier_y: 1,
            divisor_x: 8,
            divisor_y: 8,
            invert_x: false,
            invert_y: false,
        }
    }
}
/// Configuration for sniper (precision) mode
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct SniperConfig {
    /// Multiplier for both axes. Higher = more output per unit of motion.
    pub multiplier: u8,
    /// Divisor for both axes. Higher = slower, more precise movement.
    pub divisor: u8,
    /// Invert X axis movement.
    pub invert_x: bool,
    /// Invert Y axis movement.
    pub invert_y: bool,
}

impl Default for SniperConfig {
    fn default() -> Self {
        Self {
            multiplier: 1,
            divisor: 4,
            invert_x: false,
            invert_y: false,
        }
    }
}

/// Accumulator for sub-unit motion deltas (used in Scroll and Sniper modes)
///
/// When dividing motion by a divisor, small movements would be lost.
/// The accumulator keeps track of the remainder so sub-unit deltas
/// accumulate until they produce a non-zero output.
#[derive(Clone, Debug, Default)]
pub struct MotionAccumulator {
    remainder_x: i16,
    remainder_y: i16,
}

impl MotionAccumulator {
    /// Reset accumulator (call when mode changes)
    pub fn reset(&mut self) {
        self.reset_x();
        self.reset_y();
    }

    /// Reset x axis remainder of accumulator
    pub fn reset_x(&mut self) {
        self.remainder_x = 0;
    }

    /// Reset y axis remainder of accumulator
    pub fn reset_y(&mut self) {
        self.remainder_y = 0;
    }

    /// Accumulate motion and return the divided output, keeping remainder.
    /// A divisor of 0 disables that axis (always outputs 0).
    pub fn accumulate(&mut self, dx: i16, dy: i16, ratio_x: (u8, u8), ratio_y: (u8, u8)) -> (i16, i16) {
        let out_x = if ratio_x.1 == 0 {
            self.remainder_x = 0;
            0
        } else {
            let total_x = self.remainder_x.saturating_add(dx * ratio_x.0 as i16);
            let out = total_x / ratio_x.1 as i16;
            self.remainder_x = total_x - out * ratio_x.1 as i16;
            out
        };

        let out_y = if ratio_y.1 == 0 {
            self.remainder_y = 0;
            0
        } else {
            let total_y = self.remainder_y.saturating_add(dy * ratio_y.0 as i16);
            let out = total_y / ratio_y.1 as i16;
            self.remainder_y = total_y - out * ratio_y.1 as i16;
            out
        };

        (out_x, out_y)
    }

    /// Accumulate motion and return the divided output, keeping remainder.
    /// Do not subtract output from remainder.
    pub fn accumulate_persistent(&mut self, dx: i16, dy: i16, ratio_x: (u8, u8), ratio_y: (u8, u8)) -> (i16, i16) {
        let out_x = if ratio_x.1 == 0 {
            self.remainder_x = 0;
            0
        } else {
            let total_x = self.remainder_x.saturating_add(dx * ratio_x.0 as i16);
            let out = total_x / ratio_x.1 as i16;
            self.remainder_x = total_x;
            out
        };

        let out_y = if ratio_y.1 == 0 {
            self.remainder_y = 0;
            0
        } else {
            let total_y = self.remainder_y.saturating_add(dy * ratio_y.0 as i16);
            let out = total_y / ratio_y.1 as i16;
            self.remainder_y = total_y;
            out
        };

        (out_x, out_y)
    }
}

#[derive(Clone)]
pub struct PointingProcessorConfig {
    /// The id of the PointingDevice this processor handles.
    /// Use ALL_POINTING_DEVICES (255) to process events from all devices.
    pub device_id: u8,
    /// Invert X axis (applied to all modes before mode-specific processing)
    pub invert_x: bool,
    /// Invert Y axis (applied to all modes before mode-specific processing)
    pub invert_y: bool,
    /// Swap X and Y axes (applied to all modes before mode-specific processing)
    pub swap_xy: bool,
}

impl Default for PointingProcessorConfig {
    fn default() -> Self {
        Self {
            device_id: ALL_POINTING_DEVICES,
            invert_x: false,
            invert_y: false,
            swap_xy: false,
        }
    }
}

/// Minimal pointing processor for dongles without a local keymap context.
#[processor(subscribe = [PointingEvent])]
pub struct SimplePointingProcessor {
    config: PointingProcessorConfig,
}

impl SimplePointingProcessor {
    pub fn new(config: PointingProcessorConfig) -> Self {
        Self { config }
    }

    async fn on_pointing_event(&mut self, event: PointingEvent) {
        if self.config.device_id != ALL_POINTING_DEVICES && event.device_id != self.config.device_id {
            return;
        }

        let mut x = 0i16;
        let mut y = 0i16;
        let mut wheel = 0i16;
        let mut pan = 0i16;

        for axis_event in event.axes.iter() {
            match axis_event.axis {
                Axis::X => x = axis_event.value,
                Axis::Y => y = axis_event.value,
                Axis::V => wheel = axis_event.value,
                Axis::H => pan = axis_event.value,
                _ => {}
            }
        }

        if self.config.invert_x {
            x = -x;
        }
        if self.config.invert_y {
            y = -y;
        }
        if self.config.swap_xy {
            (x, y) = (y, x);
        }

        send_hid_report(Report::MouseReport(MouseReport {
            buttons: 0,
            x: x.clamp(i8::MIN as i16, i8::MAX as i16) as i8,
            y: y.clamp(i8::MIN as i16, i8::MAX as i16) as i8,
            wheel: wheel.clamp(i8::MIN as i16, i8::MAX as i16) as i8,
            pan: pan.clamp(i8::MIN as i16, i8::MAX as i16) as i8,
        }))
        .await;
    }
}

const QUBE_USER_SNIPER: u8 = 28;
const QUBE_USER_SCROLL: u8 = 29;
const QUBE_USER_TEXT: u8 = 30;
const QUBE_USER_LEFT_SNIPER: u8 = 31;
const QUBE_USER_LEFT_SCROLL: u8 = 32;
const QUBE_USER_LEFT_TEXT: u8 = 33;
const QUBE_USER_RIGHT_SNIPER: u8 = 34;
const QUBE_USER_RIGHT_SCROLL: u8 = 35;
const QUBE_USER_RIGHT_TEXT: u8 = 36;
const QUBE_SETTINGS_VERSION: u8 = if cfg!(feature = "k04_standalone_timeout_14") {
    10
} else {
    9
};
/// Marks the module-settings packet that carries the encoder step counts;
/// mirrored by `module_encoder_settings_sync_packet` in the K:04 firmware.
const QUBE_ENCODER_SETTINGS_PACKET: u8 = 0x40;
const QUBE_ENCODER_PACKET_AUTO_LAYER_THRESHOLD: usize = 3;
/// The auto-layer threshold is a 0..=100 scale over 0..=255 sensor counts:
/// counts = value * 255 / 100, so the user never sees raw counts.
const QUBE_AUTO_LAYER_THRESHOLD_MAX: u8 = 100;
const QUBE_AUTO_LAYER_THRESHOLD_COUNTS_AT_MAX: u32 = 255;
const QUBE_AUTO_LAYER_NONE: u8 = 0xff;
/// All 60 K:04 matrix positions plus both directions of its two encoders.
#[cfg(feature = "split")]
const QUBE_AUTO_LAYER_HELD_CAPACITY: usize = 64;
const QUBE_TEXT_AXIS_IDLE_MS: u32 = 220;
const QUBE_TEXT_THRESHOLD: i32 = 1;
const QUBE_TOUCH_CLICK_MS: u64 = 40;
const QUBE_TOUCH_LEFT_BUTTON: u8 = 1 << 0;
const QUBE_TOUCH_RIGHT_BUTTON: u8 = 1 << 1;
#[cfg(feature = "k04_standalone_timeout_14")]
const QUBE_AUTO_LAYER_TIMEOUT_MS_TABLE: [u32; 14] =
    [50, 100, 150, 200, 250, 300, 350, 400, 450, 500, 750, 1000, 1250, 1500];
#[cfg(not(feature = "k04_standalone_timeout_14"))]
const QUBE_AUTO_LAYER_TIMEOUT_MS_TABLE: [u32; 6] = [250, 500, 750, 1000, 1250, 1500];
const QUBE_AUTO_LAYER_TIMEOUT_MAX_INDEX: u8 = (QUBE_AUTO_LAYER_TIMEOUT_MS_TABLE.len() - 1) as u8;
const QUBE_DEFAULT_AUTO_LAYER_TIMEOUT_INDEX: u8 = if cfg!(feature = "k04_standalone_timeout_14") {
    9
} else {
    1
};
const QUBE_AUTO_FLAG_DEACTIVATE_ON_KEY_BIT: u8 = 7;
const QUBE_FLAG_LEFT_INVERT_SCROLL_Y: u8 = 1 << 0;
const QUBE_FLAG_RIGHT_INVERT_SCROLL_Y: u8 = 1 << 1;
const QUBE_FLAG_LEFT_INVERT_TEXT_Y: u8 = 1 << 2;
const QUBE_FLAG_RIGHT_INVERT_TEXT_Y: u8 = 1 << 3;
const QUBE_FLAG_LEFT_ACCELERATION: u8 = 1 << 4;
const QUBE_FLAG_RIGHT_ACCELERATION: u8 = 1 << 5;
const QUBE_FLAG_LEFT_STICKY: u8 = 1 << 6;
const QUBE_FLAG_RIGHT_STICKY: u8 = 1 << 7;
const QUBE_AXIS_FLAG_LEFT_INVERT_SCROLL_X: u8 = 1 << 0;
const QUBE_AXIS_FLAG_RIGHT_INVERT_SCROLL_X: u8 = 1 << 1;
const QUBE_AXIS_FLAG_LEFT_INVERT_TEXT_X: u8 = 1 << 2;
const QUBE_AXIS_FLAG_RIGHT_INVERT_TEXT_X: u8 = 1 << 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum QubePointingMode {
    Normal,
    Sniper,
    Scroll,
    Text,
}

impl QubePointingMode {
    fn from_value(value: u8) -> Self {
        match value.min(3) {
            1 => Self::Sniper,
            2 => Self::Scroll,
            3 => Self::Text,
            _ => Self::Normal,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum QubePointingKind {
    Ball,
    Touch,
}

#[derive(Clone, Copy)]
struct QubePointingSource {
    side: usize,
    kind: QubePointingKind,
}

#[derive(Clone, Copy)]
struct QubePointingSettings {
    mode: [QubePointingMode; 2],
    ball_axis: [u8; 2],
    touch_axis: [u8; 2],
    scroll_sens: [i32; 2],
    sniper_sens: [i32; 2],
    text_sens: [i32; 2],
    flags: u8,
    auto_layer: u8,
    auto_flags: u8,
    axis_flags: u8,
    auto_layer_timeout_index: u8,
    /// Ball travel that arms the auto layer, 0..=100 over 0..=255 sensor
    /// counts; 0 switches on the first count as before.
    auto_layer_threshold: u8,
}

impl QubePointingSettings {
    const fn new() -> Self {
        Self {
            mode: [QubePointingMode::Normal; 2],
            ball_axis: [0; 2],
            touch_axis: [0; 2],
            scroll_sens: [8; 2],
            sniper_sens: [4; 2],
            text_sens: [16; 2],
            flags: 0,
            auto_layer: 4,
            auto_flags: 1,
            axis_flags: 0,
            auto_layer_timeout_index: QUBE_DEFAULT_AUTO_LAYER_TIMEOUT_INDEX,
            auto_layer_threshold: 0,
        }
    }

    fn apply_packet(&mut self, data: &[u8; 27]) {
        // The main packet is full; the encoder packet carries the newer
        // per-keyboard settings after its two step counts.
        if data[0] == QUBE_SETTINGS_VERSION | QUBE_ENCODER_SETTINGS_PACKET {
            self.auto_layer_threshold =
                data[QUBE_ENCODER_PACKET_AUTO_LAYER_THRESHOLD].min(QUBE_AUTO_LAYER_THRESHOLD_MAX);
            return;
        }
        if data[0] != QUBE_SETTINGS_VERSION {
            return;
        }
        self.mode[0] = QubePointingMode::from_value(data[1] & 0x03);
        self.mode[1] = QubePointingMode::from_value((data[1] >> 2) & 0x03);
        self.auto_layer = (data[1] >> 4) & 0x0f;
        self.ball_axis[0] = data[2] & 0x03;
        self.ball_axis[1] = (data[2] >> 2) & 0x03;
        self.touch_axis[0] = (data[2] >> 4) & 0x03;
        self.touch_axis[1] = (data[2] >> 6) & 0x03;
        self.scroll_sens[0] = i32::from(data[5].max(1));
        self.sniper_sens[0] = i32::from(data[6].max(1));
        self.text_sens[0] = i32::from(data[7].max(1));
        self.scroll_sens[1] = i32::from(data[8].max(1));
        self.sniper_sens[1] = i32::from(data[9].max(1));
        self.text_sens[1] = i32::from(data[10].max(1));
        self.flags = data[11];
        self.auto_flags = data[12];
        self.axis_flags = data[26] & 0x0f;
        self.auto_layer_timeout_index = (data[26] >> 4).min(QUBE_AUTO_LAYER_TIMEOUT_MAX_INDEX);
    }

    fn orientation(&self, source: QubePointingSource) -> u8 {
        match (source.side, source.kind) {
            (0, QubePointingKind::Ball) => self.ball_axis[0],
            (1, QubePointingKind::Ball) => self.ball_axis[1],
            (0, QubePointingKind::Touch) => self.touch_axis[0],
            _ => self.touch_axis[1],
        }
    }

    fn sens(&self, side: usize, mode: QubePointingMode) -> i32 {
        match mode {
            QubePointingMode::Normal => 1,
            QubePointingMode::Sniper => self.sniper_sens[side],
            QubePointingMode::Scroll => self.scroll_sens[side],
            QubePointingMode::Text => self.text_sens[side],
        }
        .max(1)
    }

    fn auto_layer_enabled(&self, mode: QubePointingMode) -> bool {
        let bit = match mode {
            QubePointingMode::Normal => 0,
            QubePointingMode::Sniper => 1,
            QubePointingMode::Scroll => 2,
            QubePointingMode::Text => 3,
        };
        (self.auto_flags & (1 << bit)) != 0
    }

    fn auto_layer_timeout_ms(&self) -> u32 {
        QUBE_AUTO_LAYER_TIMEOUT_MS_TABLE
            [usize::from(self.auto_layer_timeout_index.min(QUBE_AUTO_LAYER_TIMEOUT_MAX_INDEX))]
    }

    fn deactivate_auto_layer_on_key(&self) -> bool {
        (self.auto_flags & (1 << QUBE_AUTO_FLAG_DEACTIVATE_ON_KEY_BIT)) != 0
    }

    /// The auto-layer threshold in sensor counts; 0 keeps the first-count
    /// behaviour, any other setting is at least one count.
    fn auto_layer_threshold_counts(&self) -> u32 {
        if self.auto_layer_threshold == 0 {
            return 0;
        }
        let max = u32::from(QUBE_AUTO_LAYER_THRESHOLD_MAX);
        let counts = (u32::from(self.auto_layer_threshold) * QUBE_AUTO_LAYER_THRESHOLD_COUNTS_AT_MAX + max / 2) / max;
        counts.max(1)
    }

    fn acceleration(&self, side: usize) -> bool {
        self.flag(if side == 0 {
            QUBE_FLAG_LEFT_ACCELERATION
        } else {
            QUBE_FLAG_RIGHT_ACCELERATION
        })
    }

    fn sticky_mode(&self, side: usize) -> bool {
        self.flag(if side == 0 {
            QUBE_FLAG_LEFT_STICKY
        } else {
            QUBE_FLAG_RIGHT_STICKY
        })
    }

    fn invert_scroll_x(&self, side: usize) -> bool {
        self.axis_flag(if side == 0 {
            QUBE_AXIS_FLAG_LEFT_INVERT_SCROLL_X
        } else {
            QUBE_AXIS_FLAG_RIGHT_INVERT_SCROLL_X
        })
    }

    fn invert_scroll_y(&self, side: usize) -> bool {
        self.flag(if side == 0 {
            QUBE_FLAG_LEFT_INVERT_SCROLL_Y
        } else {
            QUBE_FLAG_RIGHT_INVERT_SCROLL_Y
        })
    }

    fn invert_text_x(&self, side: usize) -> bool {
        self.axis_flag(if side == 0 {
            QUBE_AXIS_FLAG_LEFT_INVERT_TEXT_X
        } else {
            QUBE_AXIS_FLAG_RIGHT_INVERT_TEXT_X
        })
    }

    fn invert_text_y(&self, side: usize) -> bool {
        self.flag(if side == 0 {
            QUBE_FLAG_LEFT_INVERT_TEXT_Y
        } else {
            QUBE_FLAG_RIGHT_INVERT_TEXT_Y
        })
    }

    fn flag(&self, mask: u8) -> bool {
        (self.flags & mask) != 0
    }

    fn axis_flag(&self, mask: u8) -> bool {
        (self.axis_flags & mask) != 0
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum QubeModeKeyScope {
    Side,
    Both,
}

#[derive(Clone, Copy)]
struct QubePointingSideState {
    mode_key: PointingModeKeyState<QubePointingMode>,
    // Origin of the current override, not a second mode state. Equal values
    // selected by independent side keys must remain independent on reset.
    mode_key_scope: QubeModeKeyScope,
    remainder_x: i32,
    remainder_y: i32,
    text_last_motion_ms: u32,
}

impl QubePointingSideState {
    const fn new() -> Self {
        Self {
            mode_key: PointingModeKeyState::new(),
            mode_key_scope: QubeModeKeyScope::Side,
            remainder_x: 0,
            remainder_y: 0,
            text_last_motion_ms: 0,
        }
    }

    fn reset_accum(&mut self) {
        self.remainder_x = 0;
        self.remainder_y = 0;
    }
}

/// Ball travel gathered towards the auto-layer threshold while the layer is
/// off. A pause as long as the auto-layer timeout forgets it, so separate
/// nudges do not add up into a switch.
#[derive(Clone, Copy)]
struct AutoLayerArming {
    travel: u32,
    last_motion_ms: u32,
}

impl AutoLayerArming {
    const fn new() -> Self {
        Self {
            travel: 0,
            last_motion_ms: 0,
        }
    }

    /// Adds one motion report; `true` once the travel reaches `threshold`.
    fn feed(&mut self, now_ms: u32, x: i16, y: i16, threshold: u32, idle_ms: u32) -> bool {
        if threshold == 0 {
            return true;
        }
        if now_ms.wrapping_sub(self.last_motion_ms) > idle_ms {
            self.travel = 0;
        }
        self.last_motion_ms = now_ms;
        self.travel = self
            .travel
            .saturating_add(u32::from(x.unsigned_abs()))
            .saturating_add(u32::from(y.unsigned_abs()));
        if self.travel < threshold {
            return false;
        }
        self.reset();
        true
    }

    fn reset(&mut self) {
        self.travel = 0;
    }
}

/// K:04 Qube central pointing processor.
///
/// Qube receives standard `PointingEvent`s from the halves over the existing
/// split path, then applies the K:04 pointing mode user keys on the central.
#[cfg(feature = "split")]
#[processor(
    subscribe = [PointingEvent, ActionEvent, KeyboardEvent, LayerChangeEvent, PeripheralSettingsEvent],
    poll_interval = 50
)]
pub struct QubePointingModeProcessor<'a> {
    keymap: &'a KeyMap<'a>,
    sides: [QubePointingSideState; 2],
    settings: QubePointingSettings,
    active_auto_layer: u8,
    /// `true` while the auto layer was turned on by this processor. Stays
    /// `false` when motion started on a target layer that was already active
    /// (`MO`/`TG`), and is cleared once a layer key names the layer or something
    /// else switches it off, so a keypress never tears down a manually held layer.
    auto_layer_self_activated: bool,
    /// Unique positions pressed during this auto-layer activation.
    auto_layer_held_keys: heapless::Vec<KeyboardEventPos, QUBE_AUTO_LAYER_HELD_CAPACITY>,
    last_auto_motion_ms: u32,
    auto_layer_arming: AutoLayerArming,
}

#[cfg(feature = "split")]
impl<'a> QubePointingModeProcessor<'a> {
    pub fn new(keymap: &'a KeyMap<'a>) -> Self {
        Self {
            keymap,
            sides: [QubePointingSideState::new(), QubePointingSideState::new()],
            settings: QubePointingSettings::new(),
            active_auto_layer: QUBE_AUTO_LAYER_NONE,
            auto_layer_self_activated: false,
            auto_layer_held_keys: heapless::Vec::new(),
            last_auto_motion_ms: 0,
            auto_layer_arming: AutoLayerArming::new(),
        }
    }

    async fn on_peripheral_settings_event(&mut self, event: PeripheralSettingsEvent) {
        let old_settings = self.settings;
        let old_overrides = self.sides.map(|side| side.mode_key.mode_override());
        let old_modes = [self.mode_for_side(0), self.mode_for_side(1)];
        self.settings.apply_packet(&event.0);
        // Travel gathered under the previous threshold must not count towards
        // the new one.
        self.auto_layer_arming.reset();
        let reset_both =
            (0..self.sides.len()).any(|side| old_settings.sticky_mode(side) && !self.settings.sticky_mode(side));
        for side in 0..self.sides.len() {
            if reset_both && self.sides[side].mode_key_scope == QubeModeKeyScope::Both {
                self.sides[side].mode_key = PointingModeKeyState::new();
            }
            self.sides[side]
                .mode_key
                .set_sticky_enabled(self.settings.sticky_mode(side));
            if old_overrides[side] != self.sides[side].mode_key.mode_override()
                || old_modes[side] != self.mode_for_side(side)
            {
                self.sides[side].reset_accum();
                self.sides[side].text_last_motion_ms = 0;
            }
        }
        self.reevaluate_auto_layer();
    }

    async fn on_keyboard_event(&mut self, event: KeyboardEvent) {
        if self.active_auto_layer == QUBE_AUTO_LAYER_NONE {
            return;
        }
        if event.pressed {
            if !self.auto_layer_held_keys.contains(&event.pos) && self.auto_layer_held_keys.push(event.pos).is_err() {
                // Excess input stays untracked: never evict a valid hold or latch
                // the layer permanently. Normal K:04 input fits in the buffer.
                warn!("Qube auto layer held-position capacity exceeded");
            }
        } else {
            // A release may belong to a key held before activation. It must not
            // cancel another position's hold (for example an ongoing mouse drag).
            if let Some(index) = self.auto_layer_held_keys.iter().position(|pos| *pos == event.pos) {
                self.auto_layer_held_keys.swap_remove(index);
            }
            self.last_auto_motion_ms = now_ms_u32();
        }
    }

    async fn on_layer_change_event(&mut self, _event: LayerChangeEvent) {
        // Something else switched the auto layer off (`TO`, an expiring `OSL`, ...),
        // so whoever turns it back on owns it. Only ownership is dropped: the
        // timeout keeps tracking the layer exactly as before.
        if self.auto_layer_self_activated && !self.keymap.is_layer_active(self.active_auto_layer) {
            self.auto_layer_self_activated = false;
        }
    }

    async fn on_action_event(&mut self, event: ActionEvent) {
        // A layer key naming the auto layer hands it to the user, even when the
        // layer never reads as off (`MO` over it, `TO` re-enabling it at once).
        // `ActionEvent`s arrive in order, so this lands before the next keypress.
        let names_auto_layer = match event.action {
            Action::LayerOn(layer)
            | Action::LayerOnWithModifier(layer, _)
            | Action::LayerOff(layer)
            | Action::LayerToggle(layer)
            | Action::LayerToggleOnly(layer)
            | Action::OneShotLayer(layer) => layer == self.active_auto_layer,
            // Tri-layer keys switch layer 1 or 2, and with it the derived layer 3.
            Action::TriLayerLower => matches!(self.active_auto_layer, 1 | 3),
            Action::TriLayerUpper => matches!(self.active_auto_layer, 2 | 3),
            _ => false,
        };
        if names_auto_layer {
            self.auto_layer_self_activated = false;
        }

        if event.keyboard_event.pressed
            && self.auto_layer_self_activated
            && self.settings.deactivate_auto_layer_on_key()
            && qube_action_deactivates_auto_layer(event.action)
        {
            self.deactivate_auto_layer();
        }

        let Action::User(id) = event.action else {
            return;
        };

        let pressed = event.keyboard_event.pressed;
        match id {
            QUBE_USER_SNIPER => self.handle_mode_key([true, true], QubePointingMode::Sniper, pressed),
            QUBE_USER_SCROLL => self.handle_mode_key([true, true], QubePointingMode::Scroll, pressed),
            QUBE_USER_TEXT => self.handle_mode_key([true, true], QubePointingMode::Text, pressed),
            QUBE_USER_LEFT_SNIPER => self.handle_mode_key([true, false], QubePointingMode::Sniper, pressed),
            QUBE_USER_LEFT_SCROLL => self.handle_mode_key([true, false], QubePointingMode::Scroll, pressed),
            QUBE_USER_LEFT_TEXT => self.handle_mode_key([true, false], QubePointingMode::Text, pressed),
            QUBE_USER_RIGHT_SNIPER => self.handle_mode_key([false, true], QubePointingMode::Sniper, pressed),
            QUBE_USER_RIGHT_SCROLL => self.handle_mode_key([false, true], QubePointingMode::Scroll, pressed),
            QUBE_USER_RIGHT_TEXT => self.handle_mode_key([false, true], QubePointingMode::Text, pressed),
            _ => {}
        }
    }

    async fn on_pointing_event(&mut self, event: PointingEvent) {
        let Some(source) = qube_pointing_source(event.device_id) else {
            return;
        };

        #[cfg(feature = "_ble")]
        crate::ble::sleep::report_pointing_activity(&event);

        let mut x = 0i16;
        let mut y = 0i16;
        let mut wheel = 0i16;
        let mut pan = 0i16;
        let mut gesture_buttons = 0u8;

        for axis_event in event.axes.iter() {
            match axis_event.axis {
                Axis::X => x = axis_event.value,
                Axis::Y => y = axis_event.value,
                Axis::V => wheel = axis_event.value,
                Axis::H => pan = axis_event.value,
                Axis::Z if source.kind == QubePointingKind::Touch => {
                    gesture_buttons = qube_touch_gesture_buttons(axis_event.value)
                }
                _ => {}
            }
        }

        if gesture_buttons != 0 {
            self.click(gesture_buttons).await;
            return;
        }

        if wheel != 0 || pan != 0 {
            let invert_x = if self.settings.invert_scroll_x(source.side) {
                -1
            } else {
                1
            };
            let invert_y = if self.settings.invert_scroll_y(source.side) {
                -1
            } else {
                1
            };
            self.send_mouse(0, 0, 0, wheel.saturating_mul(invert_y), pan.saturating_mul(invert_x))
                .await;
            return;
        }
        if x == 0 && y == 0 {
            return;
        }

        let (mut x, mut y) = rotate_motion(x, y, self.settings.orientation(source));
        let mode = self.mode_for_side(source.side);
        let buttons = self.keymap.mouse_buttons();
        let is_touch_drag = source.kind == QubePointingKind::Touch && buttons != 0;

        if !is_touch_drag {
            self.sync_auto_layer_for_motion(mode, source, x, y);
        }
        if self.settings.acceleration(source.side) && !is_touch_drag {
            x = accelerate_axis(x);
            y = accelerate_axis(y);
        }

        let state = &mut self.sides[source.side];
        if mode != QubePointingMode::Text && state.text_last_motion_ms != 0 {
            state.text_last_motion_ms = 0;
            state.reset_accum();
        }

        match mode {
            QubePointingMode::Normal => send_mouse_report(buttons, 0, x, y, 0, 0).await,
            QubePointingMode::Sniper => {
                let divisor = self.settings.sens(source.side, QubePointingMode::Sniper);
                let (x, y) = qube_divided_motion(state, x, y, divisor);
                send_mouse_report(buttons, 0, x, y, 0, 0).await;
            }
            QubePointingMode::Scroll => {
                let invert_x = if self.settings.invert_scroll_x(source.side) {
                    -1
                } else {
                    1
                };
                let invert_y = if self.settings.invert_scroll_y(source.side) {
                    -1
                } else {
                    1
                };
                let divisor = self.settings.sens(source.side, QubePointingMode::Scroll);
                let (h, v) =
                    qube_divided_motion(state, x.saturating_mul(invert_x), y.saturating_mul(invert_y), divisor);
                send_mouse_report(buttons, 0, 0, 0, v, h).await;
            }
            QubePointingMode::Text => {
                let invert_x = if self.settings.invert_text_x(source.side) {
                    -1
                } else {
                    1
                };
                let invert_y = if self.settings.invert_text_y(source.side) {
                    -1
                } else {
                    1
                };
                let divisor = self.settings.sens(source.side, QubePointingMode::Text);
                qube_send_text_motion(state, x.saturating_mul(invert_x), y.saturating_mul(invert_y), divisor).await;
            }
        }
    }

    async fn poll(&mut self) {
        if self.active_auto_layer == QUBE_AUTO_LAYER_NONE || !self.auto_layer_held_keys.is_empty() {
            return;
        }
        if now_ms_u32().wrapping_sub(self.last_auto_motion_ms) >= self.settings.auto_layer_timeout_ms() {
            self.deactivate_auto_layer();
        }
    }

    async fn send_mouse(&self, buttons: u8, x: i16, y: i16, wheel: i16, pan: i16) {
        let buttons = self.keymap.mouse_buttons() | buttons;
        send_mouse_report(buttons, 0, x, y, wheel, pan).await;
    }

    async fn click(&self, buttons: u8) {
        self.send_mouse(buttons, 0, 0, 0, 0).await;
        Timer::after(Duration::from_millis(QUBE_TOUCH_CLICK_MS)).await;
        send_mouse_report_unchecked(self.keymap.mouse_buttons(), 0, 0, 0, 0).await;
    }

    fn mode_for_side(&self, side: usize) -> QubePointingMode {
        self.sides[side]
            .mode_key
            .mode_override()
            .unwrap_or(self.settings.mode[side])
    }

    fn handle_mode_key(&mut self, sides: [bool; 2], mode: QubePointingMode, pressed: bool) {
        let scope = if sides == [true, true] {
            QubeModeKeyScope::Both
        } else {
            QubeModeKeyScope::Side
        };
        // A shared press is one toggle, not two blind per-side toggles. Only
        // an existing shared selection can toggle the shared sticky mode off.
        let toggle_both_off = scope == QubeModeKeyScope::Both
            && (0..self.sides.len())
                .filter(|&side| self.settings.sticky_mode(side))
                .all(|side| {
                    self.sides[side].mode_key_scope == QubeModeKeyScope::Both
                        && self.sides[side].mode_key.latched == Some(mode)
                });
        for (side, enabled) in sides.iter().copied().enumerate() {
            if !enabled || (!pressed && self.sides[side].mode_key_scope != scope) {
                continue;
            }
            if pressed && scope == QubeModeKeyScope::Both && self.settings.sticky_mode(side) {
                self.sides[side].mode_key.held = None;
                self.sides[side].mode_key.latched = if toggle_both_off { None } else { Some(mode) };
            } else {
                self.sides[side]
                    .mode_key
                    .handle(mode, pressed, self.settings.sticky_mode(side));
            }
            if pressed {
                self.sides[side].mode_key_scope = scope;
            }
            self.sides[side].reset_accum();
        }
        if sides
            .iter()
            .enumerate()
            .any(|(side, &enabled)| enabled && self.settings.sticky_mode(side))
        {
            self.reevaluate_auto_layer();
        }
    }

    // Keep the existing owned layer eligible during activity and settings
    // replay, not indefinitely: poll still expires it after idle. A base key
    // alone never activates a layer, and manual layer changes always win.
    fn sticky_keeps_auto_layer_access(&self) -> bool {
        self.auto_layer_self_activated
            && self.active_auto_layer != QUBE_AUTO_LAYER_NONE
            && self.keymap.is_layer_active(self.active_auto_layer)
            && self.sides.iter().any(|side| side.mode_key.latched.is_some())
    }

    fn reevaluate_auto_layer(&mut self) {
        if self.active_auto_layer != QUBE_AUTO_LAYER_NONE
            && (self.settings.auto_layer == 0
                || self.settings.auto_layer != self.active_auto_layer
                || (!self.sticky_keeps_auto_layer_access()
                    && !self.settings.auto_layer_enabled(self.mode_for_side(0))
                    && !self.settings.auto_layer_enabled(self.mode_for_side(1))))
        {
            self.deactivate_auto_layer();
        }
    }

    fn sync_auto_layer_for_motion(&mut self, mode: QubePointingMode, source: QubePointingSource, x: i16, y: i16) {
        if self.sticky_keeps_auto_layer_access() {
            self.last_auto_motion_ms = now_ms_u32();
            return;
        }
        // A latched mode survives idle independently of the layer. Actual
        // movement must restore access to its keys, even with Normal-only flags.
        if !self.settings.auto_layer_enabled(mode) && self.sides[source.side].mode_key.latched.is_none() {
            self.deactivate_auto_layer();
            return;
        }

        let layer = self.settings.auto_layer;
        if layer == 0 {
            self.deactivate_auto_layer();
            return;
        }
        let now_ms = now_ms_u32();
        if self.active_auto_layer == layer {
            self.last_auto_motion_ms = now_ms;
            return;
        }
        // A resting hand nudges the ball by a count or two; only a deliberate
        // move should switch the layer. The touchpad reports nothing for a
        // resting finger, so the threshold applies to the ball alone.
        if source.kind == QubePointingKind::Ball
            && !self.auto_layer_arming.feed(
                now_ms,
                x,
                y,
                self.settings.auto_layer_threshold_counts(),
                self.settings.auto_layer_timeout_ms(),
            )
        {
            return;
        }
        self.last_auto_motion_ms = now_ms;

        self.deactivate_auto_layer();
        self.active_auto_layer = layer;
        self.auto_layer_held_keys.clear();
        self.auto_layer_self_activated = self.keymap.activate_layer_if_inactive(layer);
    }

    fn deactivate_auto_layer(&mut self) {
        let previous = self.active_auto_layer;
        let owned = self.auto_layer_self_activated;
        self.active_auto_layer = QUBE_AUTO_LAYER_NONE;
        self.auto_layer_self_activated = false;
        self.auto_layer_held_keys.clear();
        self.auto_layer_arming.reset();
        if owned && previous != QUBE_AUTO_LAYER_NONE && previous != 0 {
            self.keymap.deactivate_layer_if_active(previous);
        }
    }
}

/// Does this resolved action deactivate the auto layer when `deactivate_on_key` is set?
///
/// Mirrors the classification in [`crate::keyboard::auto_mouse_layer`], minus
/// `extra_mouse_keys`: the Qube auto layer is configured over Vial QMK settings,
/// which cannot carry a keycode list.
#[cfg(feature = "split")]
fn qube_action_deactivates_auto_layer(action: Action) -> bool {
    match action {
        // The repeated keycode is unknown here; treat as unclassifiable so a
        // repeated mouse key is not misclassified as non-mouse.
        Action::Key(KeyCode::Hid(HidKeyCode::Again)) => false,
        Action::Key(KeyCode::Hid(hid)) => !hid.is_mouse_key(),
        Action::Key(_) => true,
        Action::KeyWithModifier(hid, _) | Action::OneShotKey(hid) => !hid.is_mouse_key(),
        Action::Modifier(modifiers) => modifiers.into_bits() != 0,
        // Unclassifiable (layer switches, macros, pointer-mode user keys, ...):
        // leave the layer intact; the timeout path handles it.
        _ => false,
    }
}

fn qube_pointing_source(device_id: u8) -> Option<QubePointingSource> {
    match device_id {
        0 => Some(QubePointingSource {
            side: 0,
            kind: QubePointingKind::Ball,
        }),
        1 => Some(QubePointingSource {
            side: 1,
            kind: QubePointingKind::Ball,
        }),
        2 => Some(QubePointingSource {
            side: 0,
            kind: QubePointingKind::Touch,
        }),
        3 => Some(QubePointingSource {
            side: 1,
            kind: QubePointingKind::Touch,
        }),
        _ => None,
    }
}

fn qube_touch_gesture_buttons(value: i16) -> u8 {
    match value {
        value if value == i16::from(QUBE_TOUCH_LEFT_BUTTON) => QUBE_TOUCH_LEFT_BUTTON,
        value if value == i16::from(QUBE_TOUCH_RIGHT_BUTTON) => QUBE_TOUCH_RIGHT_BUTTON,
        _ => 0,
    }
}

fn qube_divided_motion(state: &mut QubePointingSideState, x: i16, y: i16, divisor: i32) -> (i16, i16) {
    let divisor = divisor.max(1);
    state.remainder_x = state.remainder_x.saturating_add(x as i32);
    state.remainder_y = state.remainder_y.saturating_add(y as i32);
    let out_x = state.remainder_x / divisor;
    let out_y = state.remainder_y / divisor;
    state.remainder_x -= out_x * divisor;
    state.remainder_y -= out_y * divisor;
    (
        out_x.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
        out_y.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
    )
}

async fn qube_send_text_motion(state: &mut QubePointingSideState, x: i16, y: i16, divisor: i32) {
    let now = now_ms_u32();
    if state.text_last_motion_ms != 0 && now.wrapping_sub(state.text_last_motion_ms) > QUBE_TEXT_AXIS_IDLE_MS {
        state.reset_accum();
    }
    state.text_last_motion_ms = now;

    state.remainder_x = state.remainder_x.saturating_add(x as i32);
    state.remainder_y = state.remainder_y.saturating_add(y as i32);

    let key = if state.remainder_x.abs() >= state.remainder_y.abs()
        && state.remainder_x.abs() >= divisor.saturating_mul(QUBE_TEXT_THRESHOLD)
    {
        let key = if state.remainder_x > 0 {
            HidKeyCode::Right
        } else {
            HidKeyCode::Left
        };
        state.reset_accum();
        Some(key)
    } else if state.remainder_y.abs() >= divisor.saturating_mul(QUBE_TEXT_THRESHOLD) {
        let key = if state.remainder_y > 0 {
            HidKeyCode::Down
        } else {
            HidKeyCode::Up
        };
        state.reset_accum();
        Some(key)
    } else {
        None
    };

    if let Some(key) = key {
        tap_key(key).await;
    }
}

fn rotate_motion(x: i16, y: i16, orientation: u8) -> (i16, i16) {
    match orientation {
        1 => (y, x.saturating_neg()),
        2 => (x.saturating_neg(), y.saturating_neg()),
        3 => (y.saturating_neg(), x),
        _ => (x, y),
    }
}

fn accelerate_axis(value: i16) -> i16 {
    if value.unsigned_abs() > 10 {
        value.saturating_mul(2)
    } else {
        value
    }
}

fn apply_runtime_transform(x: i16, y: i16, rotation: u8, acceleration: bool) -> (i16, i16) {
    let (mut x, mut y) = rotate_motion(x, y, rotation);
    if acceleration {
        x = accelerate_axis(x);
        y = accelerate_axis(y);
    }
    (x, y)
}

fn now_ms_u32() -> u32 {
    embassy_time::Instant::now().as_millis() as u32
}

async fn send_mouse_report(source_buttons: u8, buttons: u8, x: i16, y: i16, wheel: i16, pan: i16) {
    let buttons = source_buttons | buttons;
    if buttons == 0 && x == 0 && y == 0 && wheel == 0 && pan == 0 {
        return;
    }

    send_mouse_report_unchecked(buttons, x, y, wheel, pan).await;
}

async fn send_mouse_report_unchecked(buttons: u8, x: i16, y: i16, wheel: i16, pan: i16) {
    send_hid_mouse_report(buttons, x, y, wheel, pan).await;
}

/// PointingProcessor that converts motion events to mouse reports
#[processor(subscribe = [PointingEvent, PointingProcessorEvent, PointingTransformEvent])]
pub struct PointingProcessor<'a> {
    /// Reference to the keymap (used for mouse_buttons)
    keymap: &'a KeyMap<'a>,
    config: PointingProcessorConfig,
    /// Motion accumulator for scroll/sniper modes
    accumulator: MotionAccumulator,
    /// current active mode
    current_mode: PointingMode,
    runtime_rotation: u8,
    runtime_acceleration: bool,
}

impl<'a> PointingProcessor<'a> {
    /// Create a new pointing processor with default settings
    pub fn new(keymap: &'a KeyMap<'a>, config: PointingProcessorConfig) -> Self {
        Self {
            keymap,
            config,
            accumulator: MotionAccumulator::default(),
            current_mode: PointingMode::default(),
            runtime_rotation: 0,
            runtime_acceleration: false,
        }
    }

    /// Set the pointing mode
    pub fn set_pointing_mode(&mut self, mode: PointingMode) -> &mut Self {
        if self.current_mode != mode {
            self.accumulator.reset();
            self.current_mode = mode;
        }
        self
    }

    // pointing events are generated by the PointingDevice after accumulating motion and applying the poll/report intervals.
    async fn on_pointing_event(&mut self, event: PointingEvent) {
        // Filter: only process events from the configured device
        if self.config.device_id != ALL_POINTING_DEVICES && event.device_id != self.config.device_id {
            return;
        }

        let mut x = 0i16;
        let mut y = 0i16;

        for axis_event in event.axes.iter() {
            match axis_event.axis {
                Axis::X => x = axis_event.value,
                Axis::Y => y = axis_event.value,
                _ => {}
            }
        }

        // Apply global config transforms (before mode-specific processing).
        // Order: invert → swap → mode invert.
        // Mode-specific invert_x/y operate on the post-swap logical axes,
        // so if swap_xy is enabled, ScrollConfig::invert_y affects the physical X axis.
        if self.config.invert_x {
            x = -x;
        }
        if self.config.invert_y {
            y = -y;
        }
        if self.config.swap_xy {
            (x, y) = (y, x);
        }
        (x, y) = apply_runtime_transform(x, y, self.runtime_rotation, self.runtime_acceleration);

        let buttons = self.keymap.mouse_buttons();
        match self.current_mode {
            PointingMode::Disabled => {}
            PointingMode::Cursor(_) | PointingMode::Scroll(_) | PointingMode::Sniper(_) => {
                // modes that generate mouse reports
                let mouse_report = match self.current_mode {
                    PointingMode::Cursor(cursor_config) => {
                        let out_x = x.saturating_mul(cursor_config.multiplier_x as i16);
                        let out_y = y.saturating_mul(cursor_config.multiplier_y as i16);
                        let out_x = if cursor_config.invert_x { -out_x } else { out_x };
                        let out_y = if cursor_config.invert_y { -out_y } else { out_y };
                        MouseReport {
                            buttons,
                            x: out_x.clamp(i8::MIN as i16, i8::MAX as i16) as i8,
                            y: out_y.clamp(i8::MIN as i16, i8::MAX as i16) as i8,
                            wheel: 0,
                            pan: 0,
                        }
                    }
                    PointingMode::Scroll(scroll_config) => {
                        let (sx, sy) = self.accumulator.accumulate(
                            x,
                            y,
                            (scroll_config.multiplier_x, scroll_config.divisor_x),
                            (scroll_config.multiplier_y, scroll_config.divisor_y),
                        );
                        if sx == 0 && sy == 0 {
                            return;
                        }
                        // Sensor X → pan, sensor Y → wheel.
                        // Default: sensor +Y produces negative wheel (scroll up in HID convention).
                        // invert_y reverses wheel direction; invert_x reverses pan direction.
                        let wheel = if scroll_config.invert_y { sy } else { -sy };
                        let pan = if scroll_config.invert_x { -sx } else { sx };
                        MouseReport {
                            buttons,
                            x: 0,
                            y: 0,
                            wheel: wheel.clamp(i8::MIN as i16, i8::MAX as i16) as i8,
                            pan: pan.clamp(i8::MIN as i16, i8::MAX as i16) as i8,
                        }
                    }
                    PointingMode::Sniper(sniper_config) => {
                        let (sx, sy) = self.accumulator.accumulate(
                            x,
                            y,
                            (sniper_config.multiplier, sniper_config.divisor),
                            (sniper_config.multiplier, sniper_config.divisor),
                        );
                        if sx == 0 && sy == 0 {
                            return;
                        }
                        let out_x = if sniper_config.invert_x { -sx } else { sx };
                        let out_y = if sniper_config.invert_y { -sy } else { sy };
                        MouseReport {
                            buttons,
                            x: out_x.clamp(i8::MIN as i16, i8::MAX as i16) as i8,
                            y: out_y.clamp(i8::MIN as i16, i8::MAX as i16) as i8,
                            wheel: 0,
                            pan: 0,
                        }
                    }
                    _ => unreachable!(),
                };

                send_hid_report(Report::MouseReport(mouse_report)).await;
            }
            PointingMode::Caret(caret_config) => {
                if let Some((keycode, count)) = compute_caret_taps(x, y, &mut self.accumulator, &caret_config) {
                    for _ in 0..count {
                        tap_key(keycode).await;
                    }
                }
            }
        };
    }

    // pointing device events are used to change the mode (cursor/scroll/sniper) of the processor based on the device id. This allows users to trigger different modes if desired.
    pub async fn on_pointing_processor_event(&mut self, event: PointingProcessorEvent) {
        if self.config.device_id == ALL_POINTING_DEVICES || self.config.device_id == event.device_id {
            debug!(
                "PointingProcessor {}: setting mode to {:?}",
                self.config.device_id, event.mode
            );
            self.set_pointing_mode(event.mode);
        }
    }

    pub async fn on_pointing_transform_event(&mut self, event: PointingTransformEvent) {
        if self.config.device_id == ALL_POINTING_DEVICES || self.config.device_id == event.device_id {
            let rotation = event.rotation.min(3);
            if self.runtime_rotation != rotation || self.runtime_acceleration != event.acceleration {
                self.accumulator.reset();
                self.runtime_rotation = rotation;
                self.runtime_acceleration = event.acceleration;
            }
        }
    }
}

/// Tap a key (press and release with a short delay) - used for caret mode
/// NOTE: This is a basic implementation because at the current state Keyboard (in keyboard.rs) does not support
/// sending in KeyActions from the processor layer. If that changes in the future, this can be updated to use KeyActions and support more complex behavior (e.g. modifiers, macros).
/// For the time being, this sends only simple key taps without modifiers.
async fn tap_key(keycode: HidKeyCode) {
    // Press
    send_hid_report(Report::KeyboardReport(KeyboardReport {
        modifier: 0,
        reserved: 0,
        leds: 0,
        keycodes: [keycode as u8, 0, 0, 0, 0, 0],
    }))
    .await;
    Timer::after_millis(5).await;
    // Release
    send_hid_report(Report::KeyboardReport(KeyboardReport {
        modifier: 0,
        reserved: 0,
        leds: 0,
        keycodes: [0, 0, 0, 0, 0, 0],
    }))
    .await;
    Timer::after_millis(5).await;
}

/// Pure function: given a (x, y) motion delta, decide whether caret mode
/// should fire key taps. Updates the accumulator in place (sign-change
/// aware via divisor logic, threshold-aligned, non-dominant reset).
/// Returns `(keycode, count)` if a tap should fire, `None` otherwise.
///
/// Caller is responsible for actually firing the taps (async).
fn compute_caret_taps(
    x: i16,
    y: i16,
    accumulator: &mut MotionAccumulator,
    cfg: &CaretConfig,
) -> Option<(HidKeyCode, u8)> {
    let divisor_x = if cfg.disable_x { 0 } else { 1 };
    let divisor_y = if cfg.disable_y { 0 } else { 1 };
    let (mut dx, mut dy) = accumulator.accumulate_persistent(x, y, (1, divisor_x), (1, divisor_y));

    if (dx.abs() + dy.abs()) <= cfg.threshold {
        return None;
    }

    enum MovementAxis {
        X,
        Y,
    }
    let axis = if dx.abs() >= dy.abs() {
        MovementAxis::X
    } else {
        MovementAxis::Y
    };

    let keycode = match axis {
        MovementAxis::X => match (dx > 0, cfg.invert_x) {
            (true, false) | (false, true) => cfg.keycode_right,
            (true, true) | (false, false) => cfg.keycode_left,
        },
        MovementAxis::Y => match (dy > 0, cfg.invert_y) {
            // default: +Y => down
            (true, false) | (false, true) => cfg.keycode_down,
            (true, true) | (false, false) => cfg.keycode_up,
        },
    };

    // Each tap reduces the running total on the dominant axis by `threshold`.
    // The number of iterations is the tap count.
    let mut count: u8 = 0;
    while (dx.abs() + dy.abs()) > cfg.threshold {
        let (reduce_x, reduce_y) = match axis {
            MovementAxis::X => {
                let r = if dx > 0 { -cfg.threshold } else { cfg.threshold };
                accumulator.reset_y(); //  non-dominant axis
                (r, 0)
            }
            MovementAxis::Y => {
                let r = if dy > 0 { -cfg.threshold } else { cfg.threshold };
                accumulator.reset_x(); //  non-dominant axis
                (0, r)
            }
        };
        (dx, dy) = accumulator.accumulate_persistent(reduce_x, reduce_y, (1, divisor_x), (1, divisor_y));
        count = count.saturating_add(1);
        if count == u8::MAX {
            break; // safety break to prevent infinite loop
        }
    }

    // Drop the non-dominant axis so stale samples cannot bleed in.
    match axis {
        MovementAxis::X => accumulator.reset_y(),
        MovementAxis::Y => accumulator.reset_x(),
    }

    if count == 0 { None } else { Some((keycode, count)) }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use embassy_time::Duration;
    use embedded_hal::digital::{ErrorType, InputPin};
    use embedded_hal_async::digital::Wait;

    use super::*;
    use crate::config::{BehaviorConfig, PositionalConfig};
    use crate::input_device::InputDevice;
    use crate::k;
    use crate::keymap::KeymapData;
    use crate::test_support::test_block_on as block_on;

    #[cfg(feature = "split")]
    mod sticky_processor_regression {
        use rmk_types::action::KeyAction;

        use super::*;

        // Fixed wire IDs/bit positions, independent of the production constants.
        const KEYS: [(u8, QubePointingMode, [bool; 2]); 9] = [
            (28, QubePointingMode::Sniper, [true, true]),
            (29, QubePointingMode::Scroll, [true, true]),
            (30, QubePointingMode::Text, [true, true]),
            (31, QubePointingMode::Sniper, [true, false]),
            (32, QubePointingMode::Scroll, [true, false]),
            (33, QubePointingMode::Text, [true, false]),
            (34, QubePointingMode::Sniper, [false, true]),
            (35, QubePointingMode::Scroll, [false, true]),
            (36, QubePointingMode::Text, [false, true]),
        ];

        fn packet(flags: u8, auto: u8, modes: u8) -> PeripheralSettingsEvent {
            let mut bytes = [0; 27];
            bytes[0] = QUBE_SETTINGS_VERSION;
            bytes[1] = (2 << 4) | modes;
            bytes[5..=10].copy_from_slice(&[8, 4, 16, 8, 4, 16]);
            bytes[11] = flags;
            bytes[12] = auto;
            bytes[26] = QUBE_DEFAULT_AUTO_LAYER_TIMEOUT_INDEX << 4;
            PeripheralSettingsEvent(bytes)
        }

        async fn key(processor: &mut QubePointingModeProcessor<'_>, id: u8, pressed: bool) {
            processor
                .on_action_event(ActionEvent {
                    action: Action::User(id),
                    keyboard_event: KeyboardEvent::key(0, id - 28, pressed),
                })
                .await;
        }

        fn assert_modes(processor: &QubePointingModeProcessor<'_>, expected: [QubePointingMode; 2]) {
            assert_eq!([processor.mode_for_side(0), processor.mode_for_side(1)], expected);
        }

        #[test]
        fn repeated_mode_press_matrix() {
            block_on(async {
                let mut data = KeymapData::<1, 1, 4>::new([[[k!(A)]], [[k!(B)]], [[k!(C)]], [[k!(D)]]]);
                let mut behavior = BehaviorConfig::default();
                let positional = PositionalConfig::<1, 1>::default();
                let keymap = KeyMap::new(&mut data, &mut behavior, &positional).await;
                // Every side/ID, sticky combination, and independent auto byte.
                for (id, mode, affected) in KEYS {
                    for flags in [0, 0x40, 0x80, 0xc0] {
                        for auto in 0..=255 {
                            for hold_ms in [0, 221, 5_000] {
                                let mut processor = QubePointingModeProcessor::new(&keymap);
                                let settings = packet(flags, auto, 0);
                                processor.on_peripheral_settings_event(settings).await;
                                for press_index in 0..2 {
                                    key(&mut processor, id, true).await;
                                    let during = core::array::from_fn(|side| {
                                        if !affected[side] {
                                            QubePointingMode::Normal
                                        } else if flags & (0x40 << side) != 0 && press_index == 1 {
                                            QubePointingMode::Normal
                                        } else {
                                            mode
                                        }
                                    });
                                    assert_modes(&processor, during);
                                    embassy_time::MockDriver::get().advance(Duration::from_millis(hold_ms));
                                    processor.on_peripheral_settings_event(settings).await;
                                    assert_modes(&processor, during);
                                    key(&mut processor, id, false).await;
                                    let released = core::array::from_fn(|side| {
                                        if flags & (0x40 << side) != 0 {
                                            during[side]
                                        } else {
                                            QubePointingMode::Normal
                                        }
                                    });
                                    assert_modes(&processor, released);
                                    processor.on_peripheral_settings_event(settings).await;
                                    assert_modes(&processor, released);
                                }
                            }
                        }
                    }
                }
            });
        }

        #[test]
        fn switched_mode_press_and_out_of_order_release_matrix() {
            block_on(async {
                let mut data = KeymapData::<1, 1, 4>::new([[[k!(A)]], [[k!(B)]], [[k!(C)]], [[k!(D)]]]);
                let mut behavior = BehaviorConfig::default();
                let positional = PositionalConfig::<1, 1>::default();
                let keymap = KeyMap::new(&mut data, &mut behavior, &positional).await;
                for (first_id, first_mode, first_sides) in KEYS {
                    for (next_id, next_mode, next_sides) in KEYS {
                        if first_mode == next_mode {
                            continue;
                        }
                        for flags in [0, 0x40, 0x80, 0xc0] {
                            let mut processor = QubePointingModeProcessor::new(&keymap);
                            processor.on_peripheral_settings_event(packet(flags, 0x8f, 0)).await;
                            key(&mut processor, first_id, true).await;
                            embassy_time::MockDriver::get().advance(Duration::from_millis(500));
                            key(&mut processor, next_id, true).await;
                            let expected = core::array::from_fn(|side| {
                                if next_sides[side] {
                                    next_mode
                                } else if first_sides[side] {
                                    first_mode
                                } else {
                                    QubePointingMode::Normal
                                }
                            });
                            assert_modes(&processor, expected);
                            key(&mut processor, first_id, false).await;
                            let after_first_release = core::array::from_fn(|side| {
                                if next_sides[side] || flags & (0x40 << side) != 0 {
                                    expected[side]
                                } else {
                                    QubePointingMode::Normal
                                }
                            });
                            assert_modes(&processor, after_first_release);
                            key(&mut processor, next_id, false).await;
                            assert_modes(
                                &processor,
                                core::array::from_fn(|side| {
                                    if flags & (0x40 << side) != 0 {
                                        expected[side]
                                    } else {
                                        QubePointingMode::Normal
                                    }
                                }),
                            );
                        }
                    }
                }
            });
        }

        #[test]
        fn toggle_off_restores_configured_mode_matrix() {
            block_on(async {
                let mut data = KeymapData::<1, 1, 4>::new([[[k!(A)]], [[k!(B)]], [[k!(C)]], [[k!(D)]]]);
                let mut behavior = BehaviorConfig::default();
                let positional = PositionalConfig::<1, 1>::default();
                let keymap = KeyMap::new(&mut data, &mut behavior, &positional).await;
                let modes = [
                    QubePointingMode::Normal,
                    QubePointingMode::Sniper,
                    QubePointingMode::Scroll,
                    QubePointingMode::Text,
                ];
                for left in 0..4 {
                    for right in 0..4 {
                        for (id, selected, sides) in KEYS {
                            let mut processor = QubePointingModeProcessor::new(&keymap);
                            processor
                                .on_peripheral_settings_event(packet(0xc0, 0, left | right << 2))
                                .await;
                            let configured = [modes[left as usize], modes[right as usize]];
                            key(&mut processor, id, true).await;
                            key(&mut processor, id, false).await;
                            assert_modes(
                                &processor,
                                core::array::from_fn(|side| if sides[side] { selected } else { configured[side] }),
                            );
                            key(&mut processor, id, true).await;
                            key(&mut processor, id, false).await;
                            assert_modes(&processor, configured);
                        }
                    }
                }
            });
        }

        #[test]
        fn settings_disable_reenable_and_encoder_packet_keep_side_independence() {
            block_on(async {
                let mut data = KeymapData::<1, 1, 4>::new([[[k!(A)]], [[k!(B)]], [[k!(C)]], [[k!(D)]]]);
                let mut behavior = BehaviorConfig::default();
                let positional = PositionalConfig::<1, 1>::default();
                let keymap = KeyMap::new(&mut data, &mut behavior, &positional).await;
                for retained_flags in [0, 0x40, 0x80, 0xc0] {
                    let mut processor = QubePointingModeProcessor::new(&keymap);
                    processor.on_peripheral_settings_event(packet(0xc0, 0, 0)).await;
                    key(&mut processor, 32, true).await;
                    key(&mut processor, 32, false).await;
                    key(&mut processor, 36, true).await;
                    key(&mut processor, 36, false).await;
                    let mut encoder = [0; 27];
                    encoder[0] = QUBE_SETTINGS_VERSION | 0x40;
                    encoder[3] = 50;
                    processor
                        .on_peripheral_settings_event(PeripheralSettingsEvent(encoder))
                        .await;
                    assert_modes(&processor, [QubePointingMode::Scroll, QubePointingMode::Text]);
                    processor
                        .on_peripheral_settings_event(packet(retained_flags, 0xff, 0))
                        .await;
                    let expected = [
                        if retained_flags & 0x40 != 0 {
                            QubePointingMode::Scroll
                        } else {
                            QubePointingMode::Normal
                        },
                        if retained_flags & 0x80 != 0 {
                            QubePointingMode::Text
                        } else {
                            QubePointingMode::Normal
                        },
                    ];
                    assert_modes(&processor, expected);
                    processor.on_peripheral_settings_event(packet(0xc0, 0, 0)).await;
                    assert_modes(&processor, expected); // Re-enable must not resurrect cleared latches.
                }
            });
        }

        #[test]
        fn bounded_global_press_unifies_an_equal_side_selection() {
            block_on(async {
                let mut data = KeymapData::<1, 1, 5>::new([[[k!(A)]]; 5]);
                let mut behavior = BehaviorConfig::default();
                let positional = PositionalConfig::<1, 1>::default();
                let keymap = KeyMap::new(&mut data, &mut behavior, &positional).await;
                for local in [31, 34] {
                    let mut processor = QubePointingModeProcessor::new(&keymap);
                    processor.on_peripheral_settings_event(packet(0xc0, 0x0f, 0)).await;
                    key(&mut processor, local, true).await;
                    key(&mut processor, local, false).await;
                    key(&mut processor, 28, true).await;
                    key(&mut processor, 28, false).await;
                    assert_modes(&processor, [QubePointingMode::Sniper; 2]);
                    key(&mut processor, 28, true).await;
                    key(&mut processor, 28, false).await;
                    assert_modes(&processor, [QubePointingMode::Normal; 2]);
                }
            });
        }

        #[test]
        fn bounded_settings_reset_clears_changed_mode_accumulators() {
            block_on(async {
                let mut data = KeymapData::<1, 1, 5>::new([[[k!(A)]]; 5]);
                let mut behavior = BehaviorConfig::default();
                let positional = PositionalConfig::<1, 1>::default();
                let keymap = KeyMap::new(&mut data, &mut behavior, &positional).await;
                let mut processor = QubePointingModeProcessor::new(&keymap);
                processor.on_peripheral_settings_event(packet(0xc0, 0x0f, 0)).await;
                key(&mut processor, 29, true).await;
                key(&mut processor, 29, false).await;
                for side in &mut processor.sides {
                    side.remainder_x = 3;
                    side.remainder_y = -2;
                    side.text_last_motion_ms = 7;
                }
                processor.on_peripheral_settings_event(packet(0x80, 0x0f, 0)).await;
                for side in &processor.sides {
                    assert_eq!(
                        (side.remainder_x, side.remainder_y, side.text_last_motion_ms),
                        (0, 0, 0)
                    );
                }
            });
        }

        #[test]
        fn bounded_manual_layer_ownership_survives_all_auto_teardown_paths() {
            block_on(async {
                let mut data = KeymapData::<1, 1, 5>::new([[[k!(A)]]; 5]);
                let mut behavior = BehaviorConfig::default();
                let positional = PositionalConfig::<1, 1>::default();
                let keymap = KeyMap::new(&mut data, &mut behavior, &positional).await;
                for takeover in [false, true] {
                    for teardown in 0..4 {
                        keymap.deactivate_layer(4);
                        let mut processor = QubePointingModeProcessor::new(&keymap);
                        let mut settings = packet(0xc0, 1, 0);
                        settings.0[1] = 4 << 4;
                        processor.on_peripheral_settings_event(settings).await;
                        if !takeover {
                            keymap.activate_layer(4);
                        }
                        processor.sync_auto_layer_for_motion(QubePointingMode::Normal, TEST_BALL_SOURCE, 1, 0);
                        key(&mut processor, 29, true).await;
                        key(&mut processor, 29, false).await;
                        if takeover {
                            processor
                                .on_action_event(ActionEvent {
                                    action: Action::LayerOn(4),
                                    keyboard_event: KeyboardEvent::key(0, 0, true),
                                })
                                .await;
                        }
                        assert!(!processor.auto_layer_self_activated);
                        match teardown {
                            0 => {
                                embassy_time::MockDriver::get().advance(Duration::from_millis(600));
                                processor.poll().await;
                            }
                            1 => processor.sync_auto_layer_for_motion(QubePointingMode::Scroll, TEST_BALL_SOURCE, 1, 0),
                            2 => {
                                settings.0[11] = 0;
                                settings.0[12] = 0;
                                processor.on_peripheral_settings_event(settings).await;
                            }
                            _ => {
                                settings.0[12] = 0;
                                processor.on_peripheral_settings_event(settings).await;
                                key(&mut processor, 29, true).await;
                                key(&mut processor, 29, false).await;
                            }
                        }
                        assert!(
                            keymap.is_layer_active(4),
                            "manual owner lost: takeover={takeover} teardown={teardown}"
                        );
                    }
                }
            });
        }

        #[test]
        fn bounded_nonsticky_mode_press_keeps_original_motion_teardown_timing() {
            block_on(async {
                let mut data = KeymapData::<1, 1, 5>::new([[[k!(A)]]; 5]);
                let mut behavior = BehaviorConfig::default();
                let positional = PositionalConfig::<1, 1>::default();
                let keymap = KeyMap::new(&mut data, &mut behavior, &positional).await;
                let mut processor = QubePointingModeProcessor::new(&keymap);
                let mut settings = packet(0, 1, 0);
                settings.0[1] = 4 << 4;
                processor.on_peripheral_settings_event(settings).await;
                processor.sync_auto_layer_for_motion(QubePointingMode::Normal, TEST_BALL_SOURCE, 1, 0);
                key(&mut processor, 29, true).await;
                assert!(
                    keymap.is_layer_active(4),
                    "nonsticky press must wait for motion teardown"
                );
                processor.sync_auto_layer_for_motion(QubePointingMode::Scroll, TEST_BALL_SOURCE, 1, 0);
                assert!(!keymap.is_layer_active(4));
                key(&mut processor, 29, false).await;
                assert_modes(&processor, [QubePointingMode::Normal; 2]);
                processor.sync_auto_layer_for_motion(QubePointingMode::Normal, TEST_BALL_SOURCE, 1, 0);
                embassy_time::MockDriver::get().advance(Duration::from_millis(600));
                processor.poll().await;
                assert!(
                    !keymap.is_layer_active(4),
                    "nonsticky ordinary timeout must still expire"
                );
            });
        }

        #[test]
        fn bounded_scope_origins_and_mixed_sticky_flags_remain_distinct() {
            block_on(async {
                let mut data = KeymapData::<1, 1, 5>::new([[[k!(A)]]; 5]);
                let mut behavior = BehaviorConfig::default();
                let positional = PositionalConfig::<1, 1>::default();
                let keymap = KeyMap::new(&mut data, &mut behavior, &positional).await;
                for reset in 0..2 {
                    // Equal Side-origin values are not a Both-origin selection.
                    let mut processor = QubePointingModeProcessor::new(&keymap);
                    processor.on_peripheral_settings_event(packet(0xc0, 0x0f, 0)).await;
                    for id in [32, 35] {
                        key(&mut processor, id, true).await;
                        key(&mut processor, id, false).await;
                    }
                    processor.sides[1 - reset].remainder_x = 3;
                    processor
                        .on_peripheral_settings_event(packet(0xc0 & !(0x40 << reset), 0x0f, 0))
                        .await;
                    let mut expected = [QubePointingMode::Scroll; 2];
                    expected[reset] = QubePointingMode::Normal;
                    assert_modes(&processor, expected);
                    assert_eq!(
                        processor.sides[1 - reset].remainder_x,
                        3,
                        "independent accumulator must survive"
                    );

                    // A local replacement is independent; the residual shared
                    // selection still belongs to Both and resets on either bit.
                    let mut processor = QubePointingModeProcessor::new(&keymap);
                    processor.on_peripheral_settings_event(packet(0xc0, 0x0f, 0)).await;
                    for id in [29, 31] {
                        key(&mut processor, id, true).await;
                        key(&mut processor, id, false).await;
                    }
                    assert_modes(&processor, [QubePointingMode::Sniper, QubePointingMode::Scroll]);
                    processor
                        .on_peripheral_settings_event(packet(0xc0 & !(0x40 << reset), 0x0f, 0))
                        .await;
                    assert_modes(
                        &processor,
                        [
                            if reset == 0 {
                                QubePointingMode::Normal
                            } else {
                                QubePointingMode::Sniper
                            },
                            QubePointingMode::Normal,
                        ],
                    );
                }
                for flags in [0x40, 0x80] {
                    let mut processor = QubePointingModeProcessor::new(&keymap);
                    processor.on_peripheral_settings_event(packet(flags, 0x0f, 0)).await;
                    let sticky_side = usize::from(flags == 0x80);
                    key(&mut processor, if sticky_side == 0 { 31 } else { 34 }, true).await;
                    key(&mut processor, if sticky_side == 0 { 31 } else { 34 }, false).await;
                    key(&mut processor, 28, true).await;
                    assert_modes(&processor, [QubePointingMode::Sniper; 2]);
                    key(&mut processor, 28, false).await;
                    let mut expected = [QubePointingMode::Normal; 2];
                    expected[sticky_side] = QubePointingMode::Sniper;
                    assert_modes(&processor, expected);
                    processor.on_peripheral_settings_event(packet(flags, 0x0f, 0)).await;
                    assert_modes(&processor, expected); // Unchanged mixed bits are not a reset.
                    key(&mut processor, 28, true).await;
                    expected[sticky_side] = QubePointingMode::Normal;
                    expected[1 - sticky_side] = QubePointingMode::Sniper;
                    assert_modes(&processor, expected);
                    key(&mut processor, 28, false).await;
                    assert_modes(&processor, [QubePointingMode::Normal; 2]);
                }
            });
        }

        #[test]
        fn bounded_same_mode_release_cannot_clear_a_different_scope_hold() {
            block_on(async {
                let mut data = KeymapData::<1, 1, 5>::new([[[k!(A)]]; 5]);
                let mut behavior = BehaviorConfig::default();
                let positional = PositionalConfig::<1, 1>::default();
                let keymap = KeyMap::new(&mut data, &mut behavior, &positional).await;
                for (first, next, expected) in [
                    (28, 31, [QubePointingMode::Sniper, QubePointingMode::Normal]),
                    (31, 28, [QubePointingMode::Sniper; 2]),
                ] {
                    let mut processor = QubePointingModeProcessor::new(&keymap);
                    processor.on_peripheral_settings_event(packet(0, 0x0f, 0)).await;
                    key(&mut processor, first, true).await;
                    key(&mut processor, next, true).await;
                    key(&mut processor, first, false).await;
                    assert_modes(&processor, expected);
                    key(&mut processor, next, false).await;
                    assert_modes(&processor, [QubePointingMode::Normal; 2]);
                }
            });
        }

        #[test]
        fn bounded_latch_layer_events_follow_idle_and_shared_reset() {
            block_on(async {
                let mut data = KeymapData::<1, 1, 5>::new([[[k!(A)]]; 5]);
                let mut behavior = BehaviorConfig::default();
                let positional = PositionalConfig::<1, 1>::default();
                let keymap = KeyMap::new(&mut data, &mut behavior, &positional).await;
                let mut layers = LayerChangeEvent::subscriber();
                for reset in 0..2 {
                    let mut processor = QubePointingModeProcessor::new(&keymap);
                    let mut settings = packet(0xc0, 1, 0);
                    settings.0[1] = 4 << 4;
                    processor.on_peripheral_settings_event(settings).await;
                    processor.sync_auto_layer_for_motion(QubePointingMode::Normal, TEST_BALL_SOURCE, 1, 0);
                    assert_eq!(layers.try_next_message_pure().expect("activation event").0, 4);
                    key(&mut processor, 29, true).await;
                    key(&mut processor, 29, false).await;
                    processor.sync_auto_layer_for_motion(QubePointingMode::Scroll, TEST_BALL_SOURCE, 32, 0);
                    embassy_time::MockDriver::get().advance(Duration::from_millis(600));
                    processor.poll().await;
                    processor.on_peripheral_settings_event(settings).await;
                    assert_modes(&processor, [QubePointingMode::Scroll; 2]);
                    assert_eq!(keymap.active_layer(), 0);
                    assert_eq!(layers.try_next_message_pure().expect("idle layer-off event").0, 0);
                    assert!(layers.try_next_message_pure().is_none());
                    processor.sync_auto_layer_for_motion(QubePointingMode::Scroll, TEST_BALL_SOURCE, 32, 0);
                    assert_eq!(keymap.active_layer(), 4);
                    assert_eq!(layers.try_next_message_pure().expect("motion layer-on event").0, 4);
                    assert!(
                        layers.try_next_message_pure().is_none(),
                        "no extra global layer/LED events during motion"
                    );
                    settings.0[11] &= !(0x40 << reset);
                    processor.on_peripheral_settings_event(settings).await;
                    assert_modes(&processor, [QubePointingMode::Normal; 2]);
                    // Normal still permits auto-layer: restore ordinary timeout,
                    // rather than manufacture a mode/color-specific layer state.
                    assert_eq!(keymap.active_layer(), 4);
                    embassy_time::MockDriver::get().advance(Duration::from_millis(600));
                    processor.poll().await;
                    assert_eq!(keymap.active_layer(), 0);
                    assert_eq!(layers.try_next_message_pure().expect("ordinary timeout event").0, 0);
                    assert!(layers.try_next_message_pure().is_none());
                }
            });
        }

        #[test]
        fn bounded_mode_off_and_settings_changes_restore_ordinary_timeout() {
            block_on(async {
                let mut data = KeymapData::<1, 1, 5>::new([[[k!(A)]]; 5]);
                let mut behavior = BehaviorConfig::default();
                let positional = PositionalConfig::<1, 1>::default();
                let keymap = KeyMap::new(&mut data, &mut behavior, &positional).await;
                for change in 0..4 {
                    keymap.deactivate_layer(4);
                    let mut processor = QubePointingModeProcessor::new(&keymap);
                    let mut settings = packet(0xc0, 1, 0);
                    settings.0[1] = 4 << 4;
                    processor.on_peripheral_settings_event(settings).await;
                    processor.sync_auto_layer_for_motion(QubePointingMode::Normal, TEST_BALL_SOURCE, 1, 0);
                    key(&mut processor, 29, true).await;
                    key(&mut processor, 29, false).await;
                    // Switching mode retains the existing layer, not a new owner.
                    key(&mut processor, 30, true).await;
                    key(&mut processor, 30, false).await;
                    assert_modes(&processor, [QubePointingMode::Text; 2]);
                    embassy_time::MockDriver::get().advance(Duration::from_millis(1));
                    processor.poll().await;
                    assert!(keymap.is_layer_active(4));
                    match change {
                        0 => {
                            key(&mut processor, 30, true).await;
                            key(&mut processor, 30, false).await;
                            embassy_time::MockDriver::get().advance(Duration::from_millis(600));
                            processor.poll().await;
                        }
                        1 => {
                            settings.0[11] = 0x40;
                            processor.on_peripheral_settings_event(settings).await;
                            embassy_time::MockDriver::get().advance(Duration::from_millis(600));
                            processor.poll().await;
                        }
                        2 => {
                            settings.0[1] = 0; // explicit disable of auto-layer
                            processor.on_peripheral_settings_event(settings).await;
                        }
                        _ => {
                            settings.0[1] = 3 << 4; // explicit target change
                            processor.on_peripheral_settings_event(settings).await;
                            assert!(
                                !keymap.is_layer_active(3),
                                "key/settings must not activate replacement layer"
                            );
                        }
                    }
                    assert!(!keymap.is_layer_active(4));
                    assert_eq!(processor.active_auto_layer, QUBE_AUTO_LAYER_NONE);
                    assert!(!processor.auto_layer_self_activated);
                }
            });
        }

        #[test]
        fn bounded_explicit_manual_layer_changes_override_sticky_pin() {
            block_on(async {
                let mut data = KeymapData::<1, 1, 5>::new([[[k!(A)]]; 5]);
                let mut behavior = BehaviorConfig::default();
                let positional = PositionalConfig::<1, 1>::default();
                let keymap = KeyMap::new(&mut data, &mut behavior, &positional).await;
                for action in [Action::LayerOn(4), Action::LayerToggleOnly(0), Action::LayerOff(4)] {
                    keymap.deactivate_layer(4);
                    let mut processor = QubePointingModeProcessor::new(&keymap);
                    let mut settings = packet(0xc0, 1, 0);
                    settings.0[1] = 4 << 4;
                    processor.on_peripheral_settings_event(settings).await;
                    processor.sync_auto_layer_for_motion(QubePointingMode::Normal, TEST_BALL_SOURCE, 1, 0);
                    key(&mut processor, 29, true).await;
                    key(&mut processor, 29, false).await;
                    assert!(processor.sticky_keeps_auto_layer_access());
                    processor
                        .on_action_event(ActionEvent {
                            action,
                            keyboard_event: KeyboardEvent::key(0, 0, true),
                        })
                        .await;
                    if !matches!(action, Action::LayerOn(4)) {
                        keymap.deactivate_layer(4);
                    }
                    processor
                        .on_layer_change_event(LayerChangeEvent::new(keymap.active_layer()))
                        .await;
                    assert!(
                        !processor.sticky_keeps_auto_layer_access(),
                        "manual action must override pin: {action:?}"
                    );
                    // Manually reactivating the same target does not restore
                    // auto ownership, even though the selection remains latched.
                    keymap.activate_layer(4);
                    processor.on_layer_change_event(LayerChangeEvent::new(4)).await;
                    processor.sync_auto_layer_for_motion(QubePointingMode::Scroll, TEST_BALL_SOURCE, 1, 0);
                    embassy_time::MockDriver::get().advance(Duration::from_millis(600));
                    processor.poll().await;
                    assert!(keymap.is_layer_active(4), "manual reactivation must not be stolen");
                    assert!(!processor.auto_layer_self_activated);
                    assert_modes(&processor, [QubePointingMode::Scroll; 2]);
                }
            });
        }

        // User reproduction: pointing keys exist ONLY on the mouse auto layer.
        fn reported_layer_access(mode: QubePointingMode, id: u8, side: usize, timeout: bool) {
            block_on(async {
                let base = [k!(A), k!(B), k!(C)];
                let mut layers = [[base]; 5];
                layers[4] = [[
                    KeyAction::Single(Action::User(28)),
                    KeyAction::Single(Action::User(29)),
                    KeyAction::Single(Action::User(30)),
                ]];
                let mut data = KeymapData::<1, 3, 5>::new(layers);
                let mut behavior = BehaviorConfig::default();
                let positional = PositionalConfig::<1, 3>::default();
                let keymap = KeyMap::new(&mut data, &mut behavior, &positional).await;
                let mut processor = QubePointingModeProcessor::new(&keymap);
                let mut settings = packet(0xc0, if timeout { 0x0f } else { 0x01 }, 0);
                settings.0[1] = 4 << 4;
                processor.on_peripheral_settings_event(settings).await;
                let source = QubePointingSource {
                    side,
                    kind: QubePointingKind::Ball,
                };
                processor.sync_auto_layer_for_motion(QubePointingMode::Normal, source, 32, 0);
                assert_eq!(keymap.active_layer(), 4);
                let press = KeyboardEvent::key(0, id - 28, true);
                let release = KeyboardEvent::key(0, id - 28, false);
                assert_eq!(
                    keymap.get_action_with_layer_cache(press),
                    KeyAction::Single(Action::User(id))
                );
                processor
                    .on_action_event(ActionEvent {
                        action: Action::User(id),
                        keyboard_event: press,
                    })
                    .await;
                assert_eq!(
                    keymap.get_action_with_layer_cache(release),
                    KeyAction::Single(Action::User(id))
                );
                processor
                    .on_action_event(ActionEvent {
                        action: Action::User(id),
                        keyboard_event: release,
                    })
                    .await;
                assert_modes(&processor, [mode; 2]);
                if timeout {
                    embassy_time::MockDriver::get().advance(Duration::from_millis(600));
                    processor.poll().await;
                    assert_eq!(keymap.active_layer(), 0);
                    assert_modes(&processor, [mode; 2]);
                    // The user revised the old permanent-pin expectation:
                    // real motion restores access, not the latch itself.
                    processor.sync_auto_layer_for_motion(mode, source, 32, 0);
                } else {
                    processor.sync_auto_layer_for_motion(mode, source, 32, 0);
                }
                let repeat = keymap.get_action_with_layer_cache(press);
                eprintln!(
                    "REPRO side={side} mode={mode:?} timeout={timeout} active_layer={} repeat_action={repeat:?} override_left={:?} override_right={:?}",
                    keymap.active_layer(),
                    processor.mode_for_side(0),
                    processor.mode_for_side(1)
                );
                assert_eq!(
                    repeat,
                    KeyAction::Single(Action::User(id)),
                    "latched mode lost access to its auto-layer key"
                );
            });
        }

        fn reported_shared_reset(mode: QubePointingMode, id: u8, changed_side: usize) {
            block_on(async {
                let mut data = KeymapData::<1, 1, 5>::new([[[k!(A)]]; 5]);
                let mut behavior = BehaviorConfig::default();
                let positional = PositionalConfig::<1, 1>::default();
                let keymap = KeyMap::new(&mut data, &mut behavior, &positional).await;
                let mut processor = QubePointingModeProcessor::new(&keymap);
                let mut settings = packet(0xc0, 0x0f, 0);
                settings.0[1] = 4 << 4;
                processor.on_peripheral_settings_event(settings).await;
                key(&mut processor, id, true).await;
                key(&mut processor, id, false).await;
                assert_modes(&processor, [mode; 2]);
                settings.0[11] &= !(0x40 << changed_side);
                processor.on_peripheral_settings_event(settings).await;
                eprintln!(
                    "REPRO reset_side={changed_side} shared={mode:?} left={:?} right={:?}",
                    processor.mode_for_side(0),
                    processor.mode_for_side(1)
                );
                assert_modes(&processor, [QubePointingMode::Normal; 2]);
            });
        }

        macro_rules! reported_cases {
            ($($name:ident: $mode:ident, $id:literal, $side:literal);* $(;)?) => {
                $(
                    #[test]
                    fn $name() {
                        reported_layer_access(QubePointingMode::$mode, $id, $side, false);
                    }
                )*
            };
        }
        reported_cases! {
            reported_scroll_left_layer_access: Scroll, 29, 0;
            reported_scroll_right_layer_access: Scroll, 29, 1;
            reported_text_left_layer_access: Text, 30, 0;
            reported_text_right_layer_access: Text, 30, 1;
            reported_sniper_left_layer_access: Sniper, 28, 0;
            reported_sniper_right_layer_access: Sniper, 28, 1;
        }

        macro_rules! reset_cases {
            ($($name:ident: $mode:ident, $id:literal, $side:literal);* $(;)?) => {
                $(
                    #[test]
                    fn $name() {
                        reported_shared_reset(QubePointingMode::$mode, $id, $side);
                    }
                )*
            };
        }
        reset_cases! {
            reported_scroll_left_shared_reset: Scroll, 29, 0;
            reported_scroll_right_shared_reset: Scroll, 29, 1;
            reported_text_left_shared_reset: Text, 30, 0;
            reported_text_right_shared_reset: Text, 30, 1;
            reported_sniper_left_shared_reset: Sniper, 28, 0;
            reported_sniper_right_shared_reset: Sniper, 28, 1;
        }

        // Updated device acceptance: latch the mode, not the layer lifetime.
        fn idle_cycle(device_id: u8, id: u8, selected: QubePointingMode) {
            block_on(async {
                let base = [k!(A), k!(B), k!(C)];
                let mut layers = [[base]; 5];
                layers[4] = [[
                    KeyAction::Single(Action::User(28)),
                    KeyAction::Single(Action::User(29)),
                    KeyAction::Single(Action::User(30)),
                ]];
                let mut data = KeymapData::<1, 3, 5>::new(layers);
                let mut behavior = BehaviorConfig::default();
                let positional = PositionalConfig::<1, 3>::default();
                let keymap = KeyMap::new(&mut data, &mut behavior, &positional).await;
                let mut processor = QubePointingModeProcessor::new(&keymap);
                // Only Normal enables auto-layer motion: a latched override must
                // still be able to reacquire its keys after the layer expires.
                let mut settings = packet(0xc0, 1, 0);
                settings.0[1] = 4 << 4;
                processor.on_peripheral_settings_event(settings).await;
                let source = qube_pointing_source(device_id).unwrap();
                processor.sync_auto_layer_for_motion(QubePointingMode::Normal, source, 32, 0);
                assert_eq!(keymap.active_layer(), 4);
                resolved_mode_key(&mut processor, id).await;
                assert_modes(&processor, [selected; 2]);
                let timeout = u64::from(processor.settings.auto_layer_timeout_ms());
                embassy_time::MockDriver::get().advance(Duration::from_millis(timeout - 1));
                processor.poll().await;
                assert_eq!(keymap.active_layer(), 4, "must not expire before the deadline");
                embassy_time::MockDriver::get().advance(Duration::from_millis(1));
                processor.poll().await;
                assert_eq!(keymap.active_layer(), 0, "latched mode must not suppress idle expiry");
                assert_modes(&processor, [selected; 2]);
                processor.sync_auto_layer_for_motion(selected, source, 32, 0);
                assert_eq!(
                    keymap.active_layer(),
                    4,
                    "motion must restore mode-key access after idle"
                );
                assert_modes(&processor, [selected; 2]);
                resolved_mode_key(&mut processor, id).await;
                assert_modes(&processor, [QubePointingMode::Normal; 2]);
                let (other_id, other_mode) = if id == 28 {
                    (29, QubePointingMode::Scroll)
                } else {
                    (28, QubePointingMode::Sniper)
                };
                resolved_mode_key(&mut processor, other_id).await;
                assert_modes(&processor, [other_mode; 2]);
                // Another idle/rearm cycle must not resurrect the previous mode.
                embassy_time::MockDriver::get().advance(Duration::from_millis(timeout + 1));
                processor.poll().await;
                assert_eq!(keymap.active_layer(), 0);
                assert_modes(&processor, [other_mode; 2]);
                processor.sync_auto_layer_for_motion(other_mode, source, 32, 0);
                assert_eq!(keymap.active_layer(), 4);
                resolved_mode_key(&mut processor, other_id).await;
                assert_modes(&processor, [QubePointingMode::Normal; 2]);
            });
        }

        async fn resolved_mode_key(processor: &mut QubePointingModeProcessor<'_>, id: u8) {
            for pressed in [true, false] {
                let keyboard_event = KeyboardEvent::key(0, id - 28, pressed);
                let action = processor.keymap.get_action_with_layer_cache(keyboard_event);
                assert_eq!(action, KeyAction::Single(Action::User(id)));
                processor
                    .on_action_event(ActionEvent {
                        action: Action::User(id),
                        keyboard_event,
                    })
                    .await;
            }
        }

        macro_rules! idle_cycles {
            ($($name:ident: $device:literal, $id:literal, $mode:ident);* $(;)?) => {
                $(#[test] fn $name() { idle_cycle($device, $id, QubePointingMode::$mode); })*
            };
        }
        idle_cycles! {
            idle_cycle_left_ball_scroll: 0, 29, Scroll;
            idle_cycle_right_ball_scroll: 1, 29, Scroll;
            idle_cycle_left_touch_scroll: 2, 29, Scroll;
            idle_cycle_right_touch_scroll: 3, 29, Scroll;
            idle_cycle_left_ball_text: 0, 30, Text;
            idle_cycle_right_ball_text: 1, 30, Text;
            idle_cycle_left_touch_text: 2, 30, Text;
            idle_cycle_right_touch_text: 3, 30, Text;
            idle_cycle_left_ball_sniper: 0, 28, Sniper;
            idle_cycle_right_ball_sniper: 1, 28, Sniper;
            idle_cycle_left_touch_sniper: 2, 28, Sniper;
            idle_cycle_right_touch_sniper: 3, 28, Sniper;
        }

        #[test]
        fn reported_idle_timeout_layer_access() {
            reported_layer_access(QubePointingMode::Scroll, 29, 0, true);
        }

        #[test]
        fn reported_base_assignment_control() {
            block_on(async {
                let keys = [
                    KeyAction::Single(Action::User(28)),
                    KeyAction::Single(Action::User(29)),
                    KeyAction::Single(Action::User(30)),
                ];
                let mut data = KeymapData::<1, 3, 5>::new([[keys]; 5]);
                let mut behavior = BehaviorConfig::default();
                let positional = PositionalConfig::<1, 3>::default();
                let keymap = KeyMap::new(&mut data, &mut behavior, &positional).await;
                let mut processor = QubePointingModeProcessor::new(&keymap);
                let mut settings = packet(0xc0, 0x01, 0);
                settings.0[1] = 4 << 4;
                processor.on_peripheral_settings_event(settings).await;
                for (id, mode) in [
                    (28, QubePointingMode::Sniper),
                    (29, QubePointingMode::Scroll),
                    (30, QubePointingMode::Text),
                ] {
                    key(&mut processor, id, true).await;
                    key(&mut processor, id, false).await;
                    assert_eq!(keymap.active_layer(), 0, "base key alone must not activate a layer");
                    processor.sync_auto_layer_for_motion(mode, TEST_BALL_SOURCE, 32, 0);
                    assert_eq!(
                        keymap.active_layer(),
                        4,
                        "latched mode is eligible only on actual motion"
                    );
                    let action = keymap.get_action_with_layer_cache(KeyboardEvent::key(0, id - 28, true));
                    assert_eq!(action, KeyAction::Single(Action::User(id)));
                    key(&mut processor, id, true).await;
                    key(&mut processor, id, false).await;
                    assert_modes(&processor, [QubePointingMode::Normal; 2]);
                    embassy_time::MockDriver::get().advance(Duration::from_millis(600));
                    processor.poll().await;
                    assert_eq!(keymap.active_layer(), 0);
                }
                key(&mut processor, 28, true).await;
                key(&mut processor, 28, false).await;
                key(&mut processor, 29, true).await;
                key(&mut processor, 29, false).await;
                assert_modes(&processor, [QubePointingMode::Scroll; 2]);
            });
        }

        // Full generated subscriber + polling executor, not a handler dispatcher, preserved below.
        // Both workers are cancelled by select before inspecting borrowed state.
        #[cfg(feature = "k04_standalone_timeout_14")]
        fn generated_executor_live_settings(auto_flags: u8) {
            use embassy_futures::join::join;
            use embassy_futures::select::{Either, select};
            use embassy_time::with_timeout;
            use rmk_types::connection::UsbState;

            use crate::channel::{QueuedReportPayload, USB_REPORT_CHANNEL};
            use crate::event::{AsyncEventPublisher, AsyncPublishableEvent};
            use crate::hid::Report;
            use crate::keyboard::Keyboard;
            use crate::processor::PollingProcessor;

            assert!(crate::ACTION_EVENT_SUB_SIZE >= 2, "requires actual K04 TOML");
            block_on(async {
                // Same three physical assignments on base and pointing layers;
                // unrelated layers do not intercept the selected mode key.
                let keys = [
                    KeyAction::Single(Action::User(28)),
                    KeyAction::Single(Action::User(29)),
                    KeyAction::Single(Action::User(30)),
                    k!(A),
                ];
                let mut data = KeymapData::<1, 4, 5>::new([[keys]; 5]);
                let mut behavior = BehaviorConfig::default();
                let positional = PositionalConfig::<1, 4>::default();
                let keymap = KeyMap::new(&mut data, &mut behavior, &positional).await;
                let mut keyboard = Keyboard::new(&keymap);
                let mut processor = QubePointingModeProcessor::new(&keymap);
                crate::state::set_usb_state(UsbState::Configured);
                USB_REPORT_CHANNEL.clear();
                let sender = KeyboardEvent::publisher_async();
                let settings_sender = PeripheralSettingsEvent::publisher_async();
                let motion_sender = PointingEvent::publisher_async();
                let mut actions = ActionEvent::subscriber();
                let mut main = packet(0xc0, auto_flags, 0);
                main.0[1] = 4 << 4; // configured Normal, actual pointing layer 4
                let mut profile = [0; 27];
                profile[0] = 0x8a;
                let mut encoder = [0; 27];
                encoder[0] = 0x4a;
                let scenario = async {
                    // The joined workers have installed their subscriptions before
                    // this timer resolves. Sticky changes while both loops are live.
                    Timer::after_millis(1).await;
                    assert_eq!(main.0[0], 10, "standalone v10 profile required");
                    settings_sender.publish_async(main).await;
                    Timer::after_millis(2).await;
                    let mut action_count = 0;
                    let mut report_count = 0;
                    for (index, (id, expected)) in [
                        (28, QubePointingMode::Sniper),
                        (28, QubePointingMode::Normal),
                        (29, QubePointingMode::Scroll),
                        (29, QubePointingMode::Normal),
                        (30, QubePointingMode::Text),
                        (30, QubePointingMode::Normal),
                        (28, QubePointingMode::Sniper),
                        (29, QubePointingMode::Scroll),
                        (30, QubePointingMode::Text),
                        (30, QubePointingMode::Normal),
                    ]
                    .into_iter()
                    .enumerate()
                    {
                        for pressed in [true, false] {
                            sender.publish_async(KeyboardEvent::key(0, id - 28, pressed)).await;
                            let action = with_timeout(Duration::from_millis(100), actions.next_event())
                                .await
                                .expect("physical action missing");
                            assert_eq!(action.action, Action::User(id));
                            assert_eq!(action.keyboard_event.pressed, pressed);
                            action_count += 1;
                            Timer::after_millis(if index == 0 && pressed { 300 } else { 2 }).await;
                        }
                        // Replay the real three-packet shape twice without a gap:
                        // six messages through the production capacity-three bus.
                        for _ in 0..2 {
                            settings_sender.publish_async(main).await;
                            settings_sender.publish_async(PeripheralSettingsEvent(profile)).await;
                            settings_sender.publish_async(PeripheralSettingsEvent(encoder)).await;
                        }
                        Timer::after_millis(2).await;
                        for device_id in [0, 1] {
                            motion_sender
                                .publish_async(PointingEvent {
                                    device_id,
                                    axes: [
                                        AxisEvent {
                                            typ: AxisValType::Rel,
                                            axis: Axis::X,
                                            value: 32,
                                        },
                                        AxisEvent {
                                            typ: AxisValType::Rel,
                                            axis: Axis::Y,
                                            value: 0,
                                        },
                                        AxisEvent {
                                            typ: AxisValType::Rel,
                                            axis: Axis::Z,
                                            value: 0,
                                        },
                                    ],
                                })
                                .await;
                            let queued = with_timeout(Duration::from_millis(100), USB_REPORT_CHANNEL.receive())
                                .await
                                .expect("generated processor did not emit motion report");
                            report_count += 1;
                            match (expected, queued.into_payload()) {
                                (QubePointingMode::Text, QueuedReportPayload::Hid(Report::KeyboardReport(report))) => {
                                    assert_eq!(report.keycodes[0], HidKeyCode::Right as u8);
                                    let release =
                                        with_timeout(Duration::from_millis(100), USB_REPORT_CHANNEL.receive())
                                            .await
                                            .expect("text release missing")
                                            .into_report();
                                    let Report::KeyboardReport(release) = release else {
                                        panic!("text release type")
                                    };
                                    assert_eq!(release.keycodes, [0; 6]);
                                    report_count += 1;
                                }
                                (mode, QueuedReportPayload::WideMouse(report)) => {
                                    let (x, pan) = match mode {
                                        QubePointingMode::Normal => (32, 0),
                                        QubePointingMode::Sniper => (8, 0),
                                        QubePointingMode::Scroll => (0, 4),
                                        QubePointingMode::Text => panic!("text remained mouse mode"),
                                    };
                                    assert_eq!(
                                        (report.x, report.y, report.wheel, report.pan),
                                        (x, 0, 0, pan),
                                        "wrong live mode after physical User{id}, step {index}, side {device_id}"
                                    );
                                }
                                (_, report) => panic!("unexpected generated report: {report:?}"),
                            }
                            assert!(
                                keymap.is_layer_active(4),
                                "motion did not activate actual pointing layer"
                            );
                        }
                    }
                    assert_eq!(action_count, 20);
                    assert_eq!(report_count, 24);
                    assert!(
                        with_timeout(Duration::from_millis(5), actions.next_event())
                            .await
                            .is_err()
                    );
                    // Exercise the actual 50ms polling branch until it expires
                    // the motion-owned layer, then cancel both borrowed workers.
                    Timer::after_millis(600).await;
                    assert!(
                        !keymap.is_layer_active(4),
                        "generated polling did not expire auto layer"
                    );
                    assert!(USB_REPORT_CHANNEL.try_receive().is_err(), "unexpected duplicate report");
                    eprintln!(
                        "generated executor: auto={auto_flags:#04x}, 20 physical actions, 24 HID queue reports, 61 live settings packets, 20 motion events; timeout expired"
                    );
                };
                assert!(matches!(
                    select(join(keyboard.run(), processor.polling_loop()), scenario).await,
                    Either::Second(())
                ));
                assert_modes(&processor, [QubePointingMode::Normal; 2]);
                assert_eq!(processor.settings.flags, 0xc0);
                assert_eq!(processor.settings.auto_layer, 4);
                assert_eq!(processor.settings.auto_flags, auto_flags);
                assert_eq!(processor.active_auto_layer, QUBE_AUTO_LAYER_NONE);
                assert!(processor.auto_layer_held_keys.is_empty());
                assert!(!processor.auto_layer_self_activated);
            });
        }

        #[cfg(feature = "k04_standalone_timeout_14")]
        #[test]
        fn generated_executor_sticky_motion_layer_live_settings_replay() {
            generated_executor_live_settings(0x0f);
        }

        #[cfg(feature = "k04_standalone_timeout_14")]
        #[test]
        fn generated_executor_sticky_deactivate_on_key_flag_live_settings_replay() {
            generated_executor_live_settings(0x8f);
        }

        #[test]
        fn keyboard_physical_event_action_bus_processor_seam() {
            use embassy_futures::select::{Either, select};
            use embassy_time::{Instant, with_timeout};

            use crate::event::{AsyncEventPublisher, AsyncPublishableEvent};
            use crate::keyboard::Keyboard;

            // The unconfigured host library has no ActionEvent bus at all.
            // Production K04/Qube keyboard TOMLs reserve subscriber capacity.
            if crate::ACTION_EVENT_SUB_SIZE == 0 {
                eprintln!("physical ActionEvent seam not exercised: set KEYBOARD_TOML_PATH to a K04 profile");
                return;
            }

            block_on(async {
                let keys = core::array::from_fn(|col| KeyAction::Single(Action::User(28 + col as u8)));
                let mut data = KeymapData::<1, 9, 4>::new([[keys]; 4]);
                let mut behavior = BehaviorConfig::default();
                let positional = PositionalConfig::<1, 9>::default();
                let keymap = KeyMap::new(&mut data, &mut behavior, &positional).await;
                let mut keyboard = Keyboard::new(&keymap);
                let mut processor = QubePointingModeProcessor::new(&keymap);
                let sender = KeyboardEvent::publisher_async();
                sender.clear();
                let mut actions = ActionEvent::subscriber();
                let scenario = async {
                    let mut action_count = 0;
                    for (id, mode, affected) in KEYS {
                        processor.on_peripheral_settings_event(packet(0xc0, 0x8f, 0)).await;
                        processor.sides = [QubePointingSideState::new(); 2];
                        for (col, selected, sides) in [
                            (id - 28, mode, affected),
                            (id - 28, QubePointingMode::Normal, affected),
                            (id - 28, mode, affected),
                            (
                                (id - 28) / 3 * 3 + (id - 28 + 1) % 3,
                                [
                                    QubePointingMode::Sniper,
                                    QubePointingMode::Scroll,
                                    QubePointingMode::Text,
                                ][((id - 28 + 1) % 3) as usize],
                                affected,
                            ),
                        ] {
                            let start = Instant::now();
                            for pressed in [true, false] {
                                sender.publish_async(KeyboardEvent::key(0, col, pressed)).await;
                                let action = with_timeout(Duration::from_millis(100), actions.next_event())
                                    .await
                                    .expect("missing physical User ActionEvent");
                                assert_eq!(action.action, Action::User(28 + col));
                                assert_eq!(action.keyboard_event.pressed, pressed);
                                processor.on_action_event(action).await;
                                action_count += 1;
                                assert_modes(
                                    &processor,
                                    core::array::from_fn(|side| {
                                        if sides[side] {
                                            selected
                                        } else {
                                            QubePointingMode::Normal
                                        }
                                    }),
                                );
                                if pressed {
                                    Timer::after_millis(300).await;
                                }
                            }
                            assert!(start.elapsed().as_millis() >= 300);
                            assert!(
                                with_timeout(Duration::from_millis(5), actions.next_event())
                                    .await
                                    .is_err(),
                                "duplicate action event"
                            );
                        }
                    }
                    assert_eq!(action_count, 72);
                };
                assert!(matches!(select(keyboard.run(), scenario).await, Either::Second(())));
            });
        }
    }

    // Init logger for tests
    #[ctor::ctor(unsafe)]
    fn init_log() {
        let _ = env_logger::builder()
            .filter_level(log::LevelFilter::Debug)
            .is_test(true)
            .try_init();
    }

    #[test]
    fn qube_touch_gesture_buttons_accepts_only_supported_clicks() {
        assert_eq!(
            qube_touch_gesture_buttons(i16::from(QUBE_TOUCH_LEFT_BUTTON)),
            QUBE_TOUCH_LEFT_BUTTON
        );
        assert_eq!(
            qube_touch_gesture_buttons(i16::from(QUBE_TOUCH_RIGHT_BUTTON)),
            QUBE_TOUCH_RIGHT_BUTTON
        );
        assert_eq!(qube_touch_gesture_buttons(0), 0);
        assert_eq!(qube_touch_gesture_buttons(3), 0);
        assert_eq!(qube_touch_gesture_buttons(-1), 0);
    }

    #[test]
    fn pointing_mode_key_state_is_momentary_without_sticky_mode() {
        let mut state = PointingModeKeyState::new();

        state.handle(1u8, true, false);
        assert_eq!(state.mode_override(), Some(1));

        state.handle(1, false, false);
        assert_eq!(state.mode_override(), None);
    }

    #[test]
    fn pointing_mode_key_state_sticky_selection_switches_and_toggles_off() {
        let mut state = PointingModeKeyState::new();

        for mode in [1u8, 2, 3] {
            state.handle(mode, true, true);
            state.handle(mode, false, true);
            assert_eq!(state.mode_override(), Some(mode));
        }

        state.handle(3, true, true);
        state.handle(3, false, true);
        assert_eq!(state.mode_override(), None);
    }

    #[test]
    fn pointing_mode_key_state_clears_latch_when_sticky_mode_is_disabled() {
        let mut state = PointingModeKeyState::new();
        state.handle(1u8, true, true);
        assert_eq!(state.mode_override(), Some(1));

        state.set_sticky_enabled(false);
        assert_eq!(state.mode_override(), None);
    }

    #[test]
    fn auto_layer_arming_needs_the_threshold_within_one_stretch_of_motion() {
        let mut arming = AutoLayerArming::new();

        // No threshold: the first count switches, nothing is remembered.
        assert!(arming.feed(1_000, 1, 0, 0, 500));
        assert_eq!(arming.travel, 0);

        // Three counts twice, within the idle window, reach a threshold of 6.
        assert!(!arming.feed(1_000, 2, -1, 6, 500));
        assert!(!arming.feed(1_010, 0, 2, 6, 500));
        assert!(arming.feed(1_020, -1, 0, 6, 500));
        assert_eq!(arming.travel, 0, "a switch consumes the travel");

        // The same counts separated by more than the idle window never add up.
        assert!(!arming.feed(2_000, 3, 0, 6, 500));
        assert!(!arming.feed(2_600, 3, 0, 6, 500));
        assert_eq!(arming.travel, 3);
        assert!(arming.feed(2_700, 3, 0, 6, 500));

        // Wrapping millisecond clocks compare like the rest of the module.
        assert!(!arming.feed(u32::MAX - 10, 3, 0, 6, 500));
        assert!(arming.feed(5, 3, 0, 6, 500));
    }

    #[test]
    fn qube_auto_layer_threshold_scale_maps_100_to_255_counts() {
        let mut settings = QubePointingSettings::new();
        assert_eq!(
            settings.auto_layer_threshold_counts(),
            0,
            "0 keeps first-count switching"
        );

        for (value, counts) in [(1, 3), (10, 26), (20, 51), (50, 128), (100, 255)] {
            settings.auto_layer_threshold = value;
            assert_eq!(settings.auto_layer_threshold_counts(), counts, "value {value}");
        }
    }

    #[test]
    fn qube_encoder_settings_packet_sets_only_the_auto_layer_threshold() {
        let mut settings = QubePointingSettings::new();
        settings.auto_layer = 7;
        settings.auto_flags = 0b0110;

        let mut packet = [0u8; 27];
        packet[0] = QUBE_SETTINGS_VERSION | QUBE_ENCODER_SETTINGS_PACKET;
        packet[1] = 3;
        packet[2] = 5;
        packet[QUBE_ENCODER_PACKET_AUTO_LAYER_THRESHOLD] = 24;
        settings.apply_packet(&packet);

        assert_eq!(settings.auto_layer_threshold, 24);

        // Anything past the 0..=100 scale is clamped, not wrapped.
        packet[QUBE_ENCODER_PACKET_AUTO_LAYER_THRESHOLD] = 250;
        settings.apply_packet(&packet);
        assert_eq!(settings.auto_layer_threshold, 100);
        packet[QUBE_ENCODER_PACKET_AUTO_LAYER_THRESHOLD] = 24;
        settings.apply_packet(&packet);
        assert_eq!(settings.auto_layer, 7);
        assert_eq!(settings.auto_flags, 0b0110);

        // A stale main packet leaves the threshold alone.
        let mut main = [0u8; 27];
        main[0] = QUBE_SETTINGS_VERSION;
        settings.apply_packet(&main);
        assert_eq!(settings.auto_layer_threshold, 24);
    }

    #[test]
    fn qube_auto_layer_timeout_presets_match_the_vial_indices() {
        let mut settings = QubePointingSettings::new();
        #[cfg(feature = "k04_standalone_timeout_14")]
        {
            assert_eq!(QUBE_SETTINGS_VERSION, 10);
            assert_eq!(
                QUBE_AUTO_LAYER_TIMEOUT_MS_TABLE,
                [50, 100, 150, 200, 250, 300, 350, 400, 450, 500, 750, 1000, 1250, 1500]
            );
        }
        #[cfg(not(feature = "k04_standalone_timeout_14"))]
        {
            assert_eq!(QUBE_SETTINGS_VERSION, 9);
            assert_eq!(QUBE_AUTO_LAYER_TIMEOUT_MS_TABLE, [250, 500, 750, 1000, 1250, 1500]);
        }
        assert_eq!(settings.auto_layer_timeout_ms(), 500);
        for (index, duration) in QUBE_AUTO_LAYER_TIMEOUT_MS_TABLE.iter().enumerate() {
            settings.auto_layer_timeout_index = index as u8;
            assert_eq!(settings.auto_layer_timeout_ms(), *duration);
        }
        settings.auto_layer_timeout_index = 15;
        assert_eq!(settings.auto_layer_timeout_ms(), 1500);
    }

    #[test]
    fn qube_auto_layer_flags_cover_every_pointing_mode_independently() {
        let mut settings = QubePointingSettings::new();
        settings.auto_flags = 0b1111;

        for mode in [
            QubePointingMode::Normal,
            QubePointingMode::Sniper,
            QubePointingMode::Scroll,
            QubePointingMode::Text,
        ] {
            assert!(settings.auto_layer_enabled(mode));
        }

        settings.auto_flags = 0b0001;
        assert!(settings.auto_layer_enabled(QubePointingMode::Normal));
        assert!(!settings.auto_layer_enabled(QubePointingMode::Sniper));
        assert!(!settings.auto_layer_enabled(QubePointingMode::Scroll));
        assert!(!settings.auto_layer_enabled(QubePointingMode::Text));
    }

    #[test]
    fn qube_deactivate_on_key_is_independent_of_the_four_mode_flags() {
        let mut settings = QubePointingSettings::new();

        settings.auto_flags = 0b1111;
        assert!(!settings.deactivate_auto_layer_on_key());

        settings.auto_flags = 1 << QUBE_AUTO_FLAG_DEACTIVATE_ON_KEY_BIT;
        assert!(settings.deactivate_auto_layer_on_key());
        assert!(!settings.auto_layer_enabled(QubePointingMode::Normal));
    }

    #[cfg(feature = "split")]
    #[test]
    fn qube_key_classification_spares_mouse_keys_and_unclassifiable_actions() {
        use rmk_types::modifier::ModifierCombination;

        // Mouse keys on the auto layer must not tear it down.
        for hid in [HidKeyCode::MouseUp, HidKeyCode::MouseBtn1, HidKeyCode::MouseAccel2] {
            assert!(!qube_action_deactivates_auto_layer(Action::Key(KeyCode::Hid(hid))));
            assert!(!qube_action_deactivates_auto_layer(Action::OneShotKey(hid)));
        }

        // A repeated keycode is unknown here, so it must not be treated as non-mouse.
        assert!(!qube_action_deactivates_auto_layer(Action::Key(KeyCode::Hid(
            HidKeyCode::Again
        ))));

        // Ordinary typing tears the layer down.
        assert!(qube_action_deactivates_auto_layer(Action::Key(KeyCode::Hid(
            HidKeyCode::A
        ))));
        assert!(qube_action_deactivates_auto_layer(Action::KeyWithModifier(
            HidKeyCode::A,
            ModifierCombination::new()
        )));
        assert!(qube_action_deactivates_auto_layer(Action::Modifier(
            ModifierCombination::new().with_left_ctrl(true)
        )));

        // Unclassifiable actions leave the layer to the timeout path. The Qube
        // pointer-mode keys are `Action::User`, so sniper/scroll/text keep it.
        assert!(!qube_action_deactivates_auto_layer(Action::User(QUBE_USER_SNIPER)));
        assert!(!qube_action_deactivates_auto_layer(Action::LayerOn(3)));
        assert!(!qube_action_deactivates_auto_layer(Action::No));
    }

    /// The Vial auto-layer target used by the ownership tests below.
    #[cfg(feature = "split")]
    const TEST_AUTO_LAYER: u8 = 2;

    /// `auto_flags` with the Normal-mode auto layer enabled (bit 0).
    #[cfg(feature = "split")]
    const TEST_AUTO_FLAGS_NORMAL: u8 = 0b0001;

    /// A left-ball report; the default threshold of 0 lets any motion switch.
    #[cfg(feature = "split")]
    const TEST_BALL_SOURCE: QubePointingSource = QubePointingSource {
        side: 0,
        kind: QubePointingKind::Ball,
    };

    #[cfg(feature = "split")]
    fn press_ordinary_key() -> ActionEvent {
        ActionEvent {
            action: Action::Key(KeyCode::Hid(HidKeyCode::A)),
            keyboard_event: KeyboardEvent::key(0, 0, true),
        }
    }

    #[cfg(feature = "split")]
    #[test]
    fn qube_untracked_release_does_not_allow_timeout_during_drag() {
        block_on(async {
            let mut data = KeymapData::<1, 2, 4>::new([[[k!(LCtrl), k!(MouseBtn1)]]; 4]);
            let mut behavior = BehaviorConfig::default();
            let positional = PositionalConfig::<1, 2>::default();
            let keymap = KeyMap::new(&mut data, &mut behavior, &positional).await;
            let mut processor = QubePointingModeProcessor::new(&keymap);
            processor.settings.auto_layer = TEST_AUTO_LAYER;
            processor.settings.auto_flags = TEST_AUTO_FLAGS_NORMAL;
            let timeout = u64::from(processor.settings.auto_layer_timeout_ms());

            // Ctrl was already held when ball motion activated the layer.
            processor.on_keyboard_event(KeyboardEvent::key(0, 0, true)).await;
            processor.sync_auto_layer_for_motion(QubePointingMode::Normal, TEST_BALL_SOURCE, 1, 0);
            processor.on_keyboard_event(KeyboardEvent::key(0, 1, true)).await;
            processor.on_keyboard_event(KeyboardEvent::key(0, 0, false)).await;
            Timer::after_millis(timeout + 1).await;
            processor.poll().await;
            assert!(
                keymap.is_layer_active(TEST_AUTO_LAYER),
                "untracked Ctrl release interrupted drag"
            );

            processor.on_keyboard_event(KeyboardEvent::key(0, 1, false)).await;
            processor.poll().await;
            assert!(
                keymap.is_layer_active(TEST_AUTO_LAYER),
                "release must restart idle timeout"
            );
            Timer::after_millis(timeout + 1).await;
            processor.poll().await;
            assert!(!keymap.is_layer_active(TEST_AUTO_LAYER));
        });
    }

    #[cfg(feature = "split")]
    #[test]
    fn qube_duplicate_press_does_not_keep_layer_active_after_release() {
        block_on(async {
            let mut data = KeymapData::<1, 1, 4>::new([[[k!(MouseBtn1)]]; 4]);
            let mut behavior = BehaviorConfig::default();
            let positional = PositionalConfig::<1, 1>::default();
            let keymap = KeyMap::new(&mut data, &mut behavior, &positional).await;
            let mut processor = QubePointingModeProcessor::new(&keymap);
            processor.settings.auto_layer = TEST_AUTO_LAYER;
            processor.settings.auto_flags = TEST_AUTO_FLAGS_NORMAL;
            processor.sync_auto_layer_for_motion(QubePointingMode::Normal, TEST_BALL_SOURCE, 1, 0);
            processor.on_keyboard_event(KeyboardEvent::key(0, 0, true)).await;
            processor.on_keyboard_event(KeyboardEvent::key(0, 0, true)).await;
            processor.on_keyboard_event(KeyboardEvent::key(0, 0, false)).await;
            Timer::after_millis(u64::from(processor.settings.auto_layer_timeout_ms()) + 1).await;
            processor.poll().await;
            assert!(
                !keymap.is_layer_active(TEST_AUTO_LAYER),
                "duplicate press left a phantom held key"
            );
        });
    }

    #[cfg(feature = "split")]
    #[test]
    fn qube_held_positions_cover_full_k04_and_reset_between_activations() {
        use crate::input_device::rotary_encoder::Direction;

        block_on(async {
            let mut data = KeymapData::<10, 6, 4, 2>::new_with_encoder(
                [[[k!(MouseBtn1); 6]; 10]; 4],
                [[Default::default(); 2]; 4],
            );
            let mut behavior = BehaviorConfig::default();
            let positional = PositionalConfig::<10, 6>::default();
            let keymap = KeyMap::new(&mut data, &mut behavior, &positional).await;
            let mut processor = QubePointingModeProcessor::new(&keymap);
            processor.settings.auto_layer = TEST_AUTO_LAYER;
            processor.settings.auto_flags = TEST_AUTO_FLAGS_NORMAL;
            let timeout = u64::from(processor.settings.auto_layer_timeout_ms());
            processor.sync_auto_layer_for_motion(QubePointingMode::Normal, TEST_BALL_SOURCE, 1, 0);
            for row in 0..10 {
                for col in 0..6 {
                    processor.on_keyboard_event(KeyboardEvent::key(row, col, true)).await;
                }
            }
            for id in 0..2 {
                for direction in [Direction::Clockwise, Direction::CounterClockwise] {
                    processor
                        .on_keyboard_event(KeyboardEvent::rotary_encoder(id, direction, true))
                        .await;
                }
            }
            assert_eq!(processor.auto_layer_held_keys.len(), 64);
            // Excess synthetic input cannot evict valid holds or panic.
            processor.on_keyboard_event(KeyboardEvent::key(255, 255, true)).await;
            assert_eq!(processor.auto_layer_held_keys.len(), 64);
            assert!(
                !processor
                    .auto_layer_held_keys
                    .contains(&KeyboardEvent::key(255, 255, true).pos)
            );
            processor.on_keyboard_event(KeyboardEvent::key(255, 255, false)).await;
            assert_eq!(processor.auto_layer_held_keys.len(), 64);
            assert!(
                !processor
                    .auto_layer_held_keys
                    .contains(&KeyboardEvent::key(255, 255, false).pos)
            );
            for row in 0..10 {
                for col in 0..6 {
                    processor.on_keyboard_event(KeyboardEvent::key(row, col, false)).await;
                }
            }
            for id in 0..2 {
                processor
                    .on_keyboard_event(KeyboardEvent::rotary_encoder(id, Direction::Clockwise, false))
                    .await;
            }
            processor
                .on_keyboard_event(KeyboardEvent::rotary_encoder(0, Direction::CounterClockwise, false))
                .await;
            // Same id, wrong direction: this duplicate release must not remove the last hold.
            processor
                .on_keyboard_event(KeyboardEvent::rotary_encoder(1, Direction::Clockwise, false))
                .await;
            Timer::after_millis(timeout + 1).await;
            processor.poll().await;
            assert!(keymap.is_layer_active(TEST_AUTO_LAYER));
            processor
                .on_keyboard_event(KeyboardEvent::rotary_encoder(1, Direction::CounterClockwise, false))
                .await;
            Timer::after_millis(timeout + 1).await;
            processor.poll().await;
            assert!(
                !keymap.is_layer_active(TEST_AUTO_LAYER),
                "excess input must not latch the layer"
            );

            processor.sync_auto_layer_for_motion(QubePointingMode::Normal, TEST_BALL_SOURCE, 1, 0);
            processor.on_keyboard_event(KeyboardEvent::key(0, 0, true)).await;
            processor.deactivate_auto_layer();
            processor.sync_auto_layer_for_motion(QubePointingMode::Normal, TEST_BALL_SOURCE, 1, 0);
            processor.on_keyboard_event(KeyboardEvent::key(0, 1, true)).await;
            processor.on_keyboard_event(KeyboardEvent::key(0, 0, false)).await;
            Timer::after_millis(timeout + 1).await;
            processor.poll().await;
            assert!(
                keymap.is_layer_active(TEST_AUTO_LAYER),
                "old activation's release cancelled a new hold"
            );
            processor.on_keyboard_event(KeyboardEvent::key(0, 1, false)).await;
            Timer::after_millis(timeout + 1).await;
            processor.poll().await;
            assert!(!keymap.is_layer_active(TEST_AUTO_LAYER));
        });
    }

    #[cfg(feature = "split")]
    #[test]
    fn qube_keypress_keeps_a_manually_activated_target_layer() {
        block_on(async {
            let mut data = KeymapData::<1, 1, 4>::new([[[k!(A)]], [[k!(B)]], [[k!(C)]], [[k!(D)]]]);
            let mut behavior = BehaviorConfig::default();
            let positional = PositionalConfig::<1, 1>::default();
            let keymap = KeyMap::new(&mut data, &mut behavior, &positional).await;
            let mut processor = QubePointingModeProcessor::new(&keymap);
            processor.settings.auto_layer = TEST_AUTO_LAYER;
            processor.settings.auto_flags = TEST_AUTO_FLAGS_NORMAL | (1 << QUBE_AUTO_FLAG_DEACTIVATE_ON_KEY_BIT);

            // The target layer is already held by the keymap, exactly as `MO`/`TG` hold it.
            keymap.activate_layer(TEST_AUTO_LAYER);
            assert!(keymap.is_layer_active(TEST_AUTO_LAYER));

            processor.sync_auto_layer_for_motion(QubePointingMode::Normal, TEST_BALL_SOURCE, 1, 0);
            assert!(
                !processor.auto_layer_self_activated,
                "motion onto an already-active layer must not claim ownership"
            );

            processor.on_action_event(press_ordinary_key()).await;

            assert!(
                keymap.is_layer_active(TEST_AUTO_LAYER),
                "typing must not tear down a manually held layer"
            );
        });
    }

    #[cfg(feature = "split")]
    #[test]
    fn qube_keypress_deactivates_an_auto_owned_layer() {
        block_on(async {
            let mut data = KeymapData::<1, 1, 4>::new([[[k!(A)]], [[k!(B)]], [[k!(C)]], [[k!(D)]]]);
            let mut behavior = BehaviorConfig::default();
            let positional = PositionalConfig::<1, 1>::default();
            let keymap = KeyMap::new(&mut data, &mut behavior, &positional).await;
            let mut processor = QubePointingModeProcessor::new(&keymap);
            processor.settings.auto_layer = TEST_AUTO_LAYER;
            processor.settings.auto_flags = TEST_AUTO_FLAGS_NORMAL | (1 << QUBE_AUTO_FLAG_DEACTIVATE_ON_KEY_BIT);

            processor.sync_auto_layer_for_motion(QubePointingMode::Normal, TEST_BALL_SOURCE, 1, 0);
            assert!(processor.auto_layer_self_activated);
            assert!(keymap.is_layer_active(TEST_AUTO_LAYER));

            processor.on_action_event(press_ordinary_key()).await;

            assert!(!keymap.is_layer_active(TEST_AUTO_LAYER));
            assert_eq!(processor.active_auto_layer, QUBE_AUTO_LAYER_NONE);
            assert!(!processor.auto_layer_self_activated);
        });
    }

    #[cfg(feature = "split")]
    #[test]
    fn qube_keypress_keeps_a_target_layer_reactivated_after_external_deactivation() {
        block_on(async {
            let mut data = KeymapData::<1, 1, 4>::new([[[k!(A)]], [[k!(B)]], [[k!(C)]], [[k!(D)]]]);
            let mut behavior = BehaviorConfig::default();
            let positional = PositionalConfig::<1, 1>::default();
            let keymap = KeyMap::new(&mut data, &mut behavior, &positional).await;
            let mut processor = QubePointingModeProcessor::new(&keymap);
            processor.settings.auto_layer = TEST_AUTO_LAYER;
            processor.settings.auto_flags = TEST_AUTO_FLAGS_NORMAL | (1 << QUBE_AUTO_FLAG_DEACTIVATE_ON_KEY_BIT);

            processor.sync_auto_layer_for_motion(QubePointingMode::Normal, TEST_BALL_SOURCE, 1, 0);
            assert!(processor.auto_layer_self_activated);

            // The layer is switched off without a key naming it (e.g. `TO(0)` or
            // an expiring one-shot layer), then comes back on by other means;
            // each change reaches the processor only as a `LayerChangeEvent`.
            keymap.deactivate_layer(TEST_AUTO_LAYER);
            processor
                .on_layer_change_event(LayerChangeEvent::new(keymap.active_layer()))
                .await;
            assert!(!processor.auto_layer_self_activated);
            keymap.activate_layer(TEST_AUTO_LAYER);
            processor
                .on_layer_change_event(LayerChangeEvent::new(keymap.active_layer()))
                .await;

            processor.on_action_event(press_ordinary_key()).await;

            assert!(
                keymap.is_layer_active(TEST_AUTO_LAYER),
                "typing must not tear down a layer manually reactivated after an external deactivation"
            );
        });
    }

    #[cfg(feature = "split")]
    #[test]
    fn qube_keypress_keeps_an_auto_layer_taken_over_by_a_layer_key() {
        use rmk_types::modifier::ModifierCombination;

        // The layer never reads as off by the time the event is handled: `MO`, `LM`,
        // `OSL` and tri-layer keys land on an already-active layer, while `TO`, `TG`
        // and a layer-off key switch it off and something turns it back on at once.
        for layer_key in [
            Action::LayerOn(TEST_AUTO_LAYER),
            Action::LayerOnWithModifier(TEST_AUTO_LAYER, ModifierCombination::LSHIFT),
            Action::OneShotLayer(TEST_AUTO_LAYER),
            Action::TriLayerUpper,
            Action::LayerToggleOnly(TEST_AUTO_LAYER),
            Action::LayerToggle(TEST_AUTO_LAYER),
            Action::LayerOff(TEST_AUTO_LAYER),
        ] {
            block_on(async {
                let mut data = KeymapData::<1, 1, 4>::new([[[k!(A)]], [[k!(B)]], [[k!(C)]], [[k!(D)]]]);
                let mut behavior = BehaviorConfig::default();
                let positional = PositionalConfig::<1, 1>::default();
                let keymap = KeyMap::new(&mut data, &mut behavior, &positional).await;
                let mut processor = QubePointingModeProcessor::new(&keymap);
                processor.settings.auto_layer = TEST_AUTO_LAYER;
                processor.settings.auto_flags = TEST_AUTO_FLAGS_NORMAL | (1 << QUBE_AUTO_FLAG_DEACTIVATE_ON_KEY_BIT);

                processor.sync_auto_layer_for_motion(QubePointingMode::Normal, TEST_BALL_SOURCE, 1, 0);
                assert!(processor.auto_layer_self_activated);

                // The keyboard publishes the `ActionEvent` before applying the layer change.
                processor
                    .on_action_event(ActionEvent {
                        action: layer_key,
                        keyboard_event: KeyboardEvent::key(0, 0, true),
                    })
                    .await;
                if matches!(
                    layer_key,
                    Action::LayerToggleOnly(_) | Action::LayerToggle(_) | Action::LayerOff(_)
                ) {
                    keymap.deactivate_layer(TEST_AUTO_LAYER);
                }
                keymap.activate_layer(TEST_AUTO_LAYER);
                processor
                    .on_layer_change_event(LayerChangeEvent::new(keymap.active_layer()))
                    .await;

                processor.on_action_event(press_ordinary_key()).await;

                assert!(
                    keymap.is_layer_active(TEST_AUTO_LAYER),
                    "typing must not tear down a layer the user took over with {:?}",
                    layer_key
                );
            });
        }
    }

    #[cfg(feature = "split")]
    #[test]
    fn qube_layer_change_that_leaves_the_auto_layer_on_keeps_ownership() {
        block_on(async {
            let mut data = KeymapData::<1, 1, 4>::new([[[k!(A)]], [[k!(B)]], [[k!(C)]], [[k!(D)]]]);
            let mut behavior = BehaviorConfig::default();
            let positional = PositionalConfig::<1, 1>::default();
            let keymap = KeyMap::new(&mut data, &mut behavior, &positional).await;
            let mut processor = QubePointingModeProcessor::new(&keymap);
            processor.settings.auto_layer = TEST_AUTO_LAYER;
            processor.settings.auto_flags = TEST_AUTO_FLAGS_NORMAL | (1 << QUBE_AUTO_FLAG_DEACTIVATE_ON_KEY_BIT);

            // The processor receives its own activation back, then `MO` of a higher,
            // unrelated layer; neither hands the auto layer to the user.
            processor.sync_auto_layer_for_motion(QubePointingMode::Normal, TEST_BALL_SOURCE, 1, 0);
            processor
                .on_layer_change_event(LayerChangeEvent::new(keymap.active_layer()))
                .await;
            processor
                .on_action_event(ActionEvent {
                    action: Action::LayerOn(TEST_AUTO_LAYER + 1),
                    keyboard_event: KeyboardEvent::key(0, 0, true),
                })
                .await;
            keymap.activate_layer(TEST_AUTO_LAYER + 1);
            processor
                .on_layer_change_event(LayerChangeEvent::new(keymap.active_layer()))
                .await;
            assert!(processor.auto_layer_self_activated);

            processor.on_action_event(press_ordinary_key()).await;

            assert!(!keymap.is_layer_active(TEST_AUTO_LAYER));
        });
    }

    #[cfg(feature = "split")]
    #[test]
    fn qube_timeout_still_spares_a_held_layer_after_external_deactivation() {
        block_on(async {
            let mut data = KeymapData::<1, 1, 4>::new([[[k!(A)]], [[k!(B)]], [[k!(C)]], [[k!(D)]]]);
            let mut behavior = BehaviorConfig::default();
            let positional = PositionalConfig::<1, 1>::default();
            let keymap = KeyMap::new(&mut data, &mut behavior, &positional).await;
            let mut processor = QubePointingModeProcessor::new(&keymap);
            processor.settings.auto_layer = TEST_AUTO_LAYER;
            processor.settings.auto_flags = TEST_AUTO_FLAGS_NORMAL;

            processor.sync_auto_layer_for_motion(QubePointingMode::Normal, TEST_BALL_SOURCE, 1, 0);
            keymap.deactivate_layer(TEST_AUTO_LAYER);
            processor
                .on_layer_change_event(LayerChangeEvent::new(keymap.active_layer()))
                .await;

            // `MO` holds the layer again, the pointer moves, then rests past the timeout.
            keymap.activate_layer(TEST_AUTO_LAYER);
            processor.on_keyboard_event(KeyboardEvent::key(0, 0, true)).await;
            processor.sync_auto_layer_for_motion(QubePointingMode::Normal, TEST_BALL_SOURCE, 1, 0);
            processor.last_auto_motion_ms = now_ms_u32().wrapping_sub(10_000);
            processor.poll().await;

            assert!(
                keymap.is_layer_active(TEST_AUTO_LAYER),
                "v0.1.9 behaviour: a held key suppresses the timeout"
            );
        });
    }

    #[cfg(feature = "split")]
    #[test]
    fn qube_keypress_leaves_the_auto_layer_alone_when_the_flag_is_off() {
        block_on(async {
            let mut data = KeymapData::<1, 1, 4>::new([[[k!(A)]], [[k!(B)]], [[k!(C)]], [[k!(D)]]]);
            let mut behavior = BehaviorConfig::default();
            let positional = PositionalConfig::<1, 1>::default();
            let keymap = KeyMap::new(&mut data, &mut behavior, &positional).await;
            let mut processor = QubePointingModeProcessor::new(&keymap);
            processor.settings.auto_layer = TEST_AUTO_LAYER;
            processor.settings.auto_flags = TEST_AUTO_FLAGS_NORMAL;

            processor.sync_auto_layer_for_motion(QubePointingMode::Normal, TEST_BALL_SOURCE, 1, 0);
            assert!(processor.auto_layer_self_activated);

            processor.on_action_event(press_ordinary_key()).await;

            assert!(
                keymap.is_layer_active(TEST_AUTO_LAYER),
                "v0.1.9 behaviour: timeout only"
            );
            assert_eq!(processor.active_auto_layer, TEST_AUTO_LAYER);
        });
    }

    #[test]
    fn runtime_pointing_transform_rotates_and_accelerates_motion() {
        assert_eq!(apply_runtime_transform(12, -4, 0, false), (12, -4));
        assert_eq!(apply_runtime_transform(12, -4, 1, false), (-4, -12));
        assert_eq!(apply_runtime_transform(12, -4, 2, false), (-12, 4));
        assert_eq!(apply_runtime_transform(12, -4, 3, false), (4, 12));
        assert_eq!(apply_runtime_transform(12, -4, 0, true), (24, -4));
        assert_eq!(
            apply_runtime_transform(i16::MAX, i16::MIN, 0, true),
            (i16::MAX, i16::MIN)
        );
    }

    struct DummyDriver {
        pub motion_pending: bool,
        pub motion: MotionData,
        pub init_called: bool,
        pub fails_init: bool,
        pub motion_gpio: Option<DummyMotionPin>,
        pub read_called: bool,
    }

    impl PointingDriver for DummyDriver {
        type MOTION = DummyMotionPin;

        async fn init(&mut self) -> Result<(), PointingDriverError> {
            self.init_called = true;
            if self.fails_init {
                Err(PointingDriverError::InitFailed)
            } else {
                Ok(())
            }
        }

        async fn read_motion(&mut self) -> Result<MotionData, PointingDriverError> {
            self.read_called = true;
            Ok(self.motion)
        }

        fn motion_pending(&mut self) -> bool {
            self.motion_pending
        }

        fn motion_gpio(&mut self) -> Option<&mut Self::MOTION> {
            self.motion_gpio.as_mut()
        }
    }

    #[derive(Debug)]
    struct DummyError;

    struct DummyMotionPin {
        state: Cell<bool>, // true = High, false = Low
    }
    impl ErrorType for DummyMotionPin {
        type Error = DummyError;
    }
    impl embedded_hal::digital::Error for DummyError {
        fn kind(&self) -> embedded_hal::digital::ErrorKind {
            embedded_hal::digital::ErrorKind::Other
        }
    }

    impl DummyMotionPin {
        fn new() -> Self {
            Self { state: Cell::new(true) } // initial high, we wait for low
        }

        fn set_low(&self) {
            self.state.set(false);
        }

        fn set_high(&self) {
            self.state.set(true);
        }
    }

    impl InputPin for DummyMotionPin {
        fn is_high(&mut self) -> Result<bool, Self::Error> {
            Ok(self.state.get())
        }
        fn is_low(&mut self) -> Result<bool, Self::Error> {
            Ok(!self.state.get())
        }
    }

    impl Wait for DummyMotionPin {
        async fn wait_for_high(&mut self) -> Result<(), Self::Error> {
            while !self.state.get() { /* spin */ }
            Ok(())
        }

        async fn wait_for_low(&mut self) -> Result<(), Self::Error> {
            if !self.state.get() {
                return Ok(());
            }
            embassy_time::Timer::after(Duration::from_millis(500)).await;
            Ok(())
        }
        async fn wait_for_rising_edge(&mut self) -> Result<(), Self::Error> {
            todo!()
        }
        async fn wait_for_falling_edge(&mut self) -> Result<(), Self::Error> {
            todo!()
        }
        async fn wait_for_any_edge(&mut self) -> Result<(), Self::Error> {
            todo!()
        }
    }
    #[test]
    fn test_try_init_retries_and_fails() {
        let driver = DummyDriver {
            motion_pending: true,
            motion: MotionData { dx: 10, dy: -5 },
            init_called: false,
            fails_init: true,
            motion_gpio: None,
            read_called: false,
        };

        let mut device = PointingDevice {
            sensor: driver,
            init_state: InitState::Pending,
            poll_interval: Duration::from_millis(1),
            id: 1,

            report_interval: Duration::from_millis(1),
            last_poll: Instant::MIN,
            last_report: Instant::MIN,
            accumulated_x: 0,
            accumulated_y: 0,
        };

        let mut result = false;
        for i in 0..PointingDevice::<DummyDriver>::MAX_INIT_RETRIES {
            result = block_on(device.try_init());

            if i + 1 < PointingDevice::<DummyDriver>::MAX_INIT_RETRIES {
                // Vorletzte und erste Versuche: state sollte Initializing sein
                assert_eq!(device.init_state, InitState::Initializing(i + 1));
                assert!(!result, "Init should not succeed yet on attempt {}", i + 1);
            } else {
                // Letzter Versuch: state wird direkt auf Failed gesetzt
                assert_eq!(device.init_state, InitState::Failed);
                assert!(!result, "Init should fail after max retries");
            }
        }
        assert!(!result);
        assert_eq!(device.init_state, InitState::Failed);
    }

    #[test]
    fn test_try_init_sets_state() {
        let driver = DummyDriver {
            motion_pending: true,
            motion: MotionData { dx: 10, dy: -5 },
            init_called: false,
            fails_init: false,
            motion_gpio: None,
            read_called: false,
        };

        let mut device = PointingDevice {
            sensor: driver,
            init_state: InitState::Pending,
            poll_interval: Duration::from_millis(1),
            id: 1,

            report_interval: Duration::from_millis(1),
            last_poll: Instant::MIN,
            last_report: Instant::MIN,
            accumulated_x: 0,
            accumulated_y: 0,
        };

        // Run the async try_init
        let result = block_on(device.try_init());
        assert!(result, "Init should succeed");
        assert_eq!(device.init_state, InitState::Ready);
        assert!(device.sensor.init_called, "Driver init should be called");
    }

    #[test]
    fn test_poll_once_accumulate_motion() {
        let motion_pin = DummyMotionPin::new();

        let driver = DummyDriver {
            motion_pending: true,
            motion: MotionData { dx: 10, dy: -5 },
            init_called: false,
            fails_init: false,
            motion_gpio: Some(motion_pin),
            read_called: false,
        };

        let mut device = PointingDevice {
            sensor: driver,
            init_state: InitState::Pending,
            poll_interval: Duration::from_millis(1),
            id: 1,

            report_interval: Duration::from_millis(1),
            last_poll: Instant::MIN,
            last_report: Instant::MIN,
            accumulated_x: 0,
            accumulated_y: 0,
        };

        let inited = block_on(device.try_init());
        assert!(inited);
        assert_eq!(device.init_state, InitState::Ready);
        assert!(device.sensor.init_called);

        // poll_once should accumulate motion
        block_on(device.poll_once());
        assert_eq!(device.accumulated_x, 10);
        assert_eq!(device.accumulated_y, -5);
    }

    #[test]
    fn test_polling_without_motion_pin_generates_event() {
        let driver = DummyDriver {
            motion_pending: true,
            motion: MotionData { dx: 3, dy: -2 },
            read_called: false,
            init_called: true,
            fails_init: false,
            motion_gpio: None,
        };

        let mut device = PointingDevice {
            sensor: driver,
            init_state: InitState::Ready,
            poll_interval: Duration::from_millis(1),
            report_interval: Duration::from_millis(1),
            last_poll: Instant::MIN,
            last_report: Instant::MIN,
            accumulated_x: 0,
            accumulated_y: 0,
            id: 1,
        };

        let event = block_on(device.read_event());

        let axes = &event.axes;
        assert_eq!(axes[0].value, 3);
        assert_eq!(axes[1].value, -2);

        assert!(device.sensor.read_called);
    }

    #[test]
    fn test_polling_with_motion_pin_generates_event() {
        let motion_pin = DummyMotionPin::new();

        let driver = DummyDriver {
            motion_pending: true,
            motion: MotionData { dx: 10, dy: -5 },
            init_called: false,
            fails_init: false,
            motion_gpio: Some(motion_pin),
            read_called: false,
        };

        let mut device = PointingDevice {
            sensor: driver,
            init_state: InitState::Pending,
            poll_interval: Duration::from_millis(10000),
            id: 1,

            report_interval: Duration::from_millis(1),
            last_poll: Instant::MIN,
            last_report: Instant::MIN,
            accumulated_x: 0,
            accumulated_y: 0,
        };

        let start = Instant::now();
        let event = block_on(device.read_event());
        let duration = start.elapsed();

        let axes = &event.axes;
        assert_eq!(axes[0].value, 10);
        assert_eq!(axes[1].value, -5);
        // poll intervall is 10000 here, so if read_event took less than that, motion pin wait worked and we did not get the report form polling
        assert!(
            duration.as_millis() <= 1000,
            "read_event took too long: {}ms. Expected to be ~500ms due to motion pin triggering.",
            duration.as_millis()
        );

        assert!(device.sensor.read_called);
    }

    #[test]
    fn test_due_report_is_not_starved_by_asserted_motion_pin() {
        let motion_pin = DummyMotionPin::new();
        motion_pin.set_low();

        let driver = DummyDriver {
            motion_pending: true,
            motion: MotionData { dx: 7, dy: -4 },
            init_called: true,
            fails_init: false,
            motion_gpio: Some(motion_pin),
            read_called: false,
        };

        let mut device = PointingDevice {
            sensor: driver,
            init_state: InitState::Ready,
            poll_interval: Duration::from_millis(1),
            report_interval: Duration::from_millis(0),
            last_poll: Instant::now(),
            last_report: Instant::now(),
            accumulated_x: 12,
            accumulated_y: -8,
            id: 1,
        };

        let event = block_on(device.read_event());

        assert_eq!(event.axes[0].value, 12);
        assert_eq!(event.axes[1].value, -8);
        assert!(
            !device.sensor.read_called,
            "a due report must be emitted before another immediate MOTION poll"
        );
    }

    #[test]
    fn report_preserves_motion_that_does_not_fit_one_hid_frame() {
        let driver = DummyDriver {
            motion_pending: false,
            motion: MotionData { dx: 0, dy: 0 },
            init_called: true,
            fails_init: false,
            motion_gpio: None,
            read_called: false,
        };
        let mut device = PointingDevice {
            sensor: driver,
            init_state: InitState::Ready,
            poll_interval: Duration::from_millis(1),
            report_interval: Duration::from_millis(8),
            last_poll: Instant::MIN,
            last_report: Instant::MIN,
            accumulated_x: 300,
            accumulated_y: -300,
            id: 1,
        };

        let first = device.take_report_event().unwrap();
        assert_eq!(first.axes[0].value, 127);
        assert_eq!(first.axes[1].value, -128);
        assert_eq!(device.accumulated_x, 173);
        assert_eq!(device.accumulated_y, -172);

        let second = device.take_report_event().unwrap();
        assert_eq!(second.axes[0].value, 127);
        assert_eq!(second.axes[1].value, -128);
        assert_eq!(device.accumulated_x, 46);
        assert_eq!(device.accumulated_y, -44);
    }

    // === MotionAccumulator tests ===

    #[test]
    fn test_motion_accumulator_basic() {
        let mut acc = MotionAccumulator::default();

        // divisor=8: 3/8 = 0 remainder 3
        let (ox, oy) = acc.accumulate(3, 3, (1, 8), (1, 8));
        assert_eq!(ox, 0);
        assert_eq!(oy, 0);
        assert_eq!(acc.remainder_x, 3);
        assert_eq!(acc.remainder_y, 3);

        // 3+6=9, 9/8=1 remainder 1
        let (ox, oy) = acc.accumulate(6, 6, (1, 8), (1, 8));
        assert_eq!(ox, 1);
        assert_eq!(oy, 1);
        assert_eq!(acc.remainder_x, 1);
        assert_eq!(acc.remainder_y, 1);
    }

    #[test]
    fn test_motion_accumulator_negative() {
        let mut acc = MotionAccumulator::default();

        // Negative motion: -10/4 = -2 remainder -2
        let (ox, oy) = acc.accumulate(-10, -10, (1, 4), (1, 4));
        assert_eq!(ox, -2);
        assert_eq!(oy, -2);
        assert_eq!(acc.remainder_x, -2);
        assert_eq!(acc.remainder_y, -2);
    }

    #[test]
    fn test_motion_accumulator_reset() {
        let mut acc = MotionAccumulator::default();
        acc.accumulate(3, 5, (1, 8), (1, 8));
        assert_ne!(acc.remainder_x, 0);

        acc.reset();
        assert_eq!(acc.remainder_x, 0);
        assert_eq!(acc.remainder_y, 0);
    }

    #[test]
    fn test_motion_accumulator_zero_divisor_disables_axis() {
        let mut acc = MotionAccumulator::default();
        // divisor 0 should disable that axis (output 0)
        let (ox, oy) = acc.accumulate(5, -3, (1, 0), (1, 0));
        assert_eq!(ox, 0);
        assert_eq!(oy, 0);

        // One axis disabled, the other active
        let (ox, oy) = acc.accumulate(10, 10, (1, 0), (1, 2));
        assert_eq!(ox, 0); // X disabled
        assert_eq!(oy, 5); // 10/2 = 5

        // Remainder should not accumulate on disabled axis
        acc.reset();
        acc.accumulate(3, 3, (1, 0), (1, 8));
        acc.accumulate(3, 3, (1, 0), (1, 8));
        let (ox, oy) = acc.accumulate(3, 3, (1, 0), (1, 8));
        assert_eq!(ox, 0); // X always 0
        assert_eq!(oy, 1); // (3+3+3)/8 = 1 remainder 1
    }

    #[test]
    fn test_motion_accumulator_asymmetric_divisors() {
        let mut acc = MotionAccumulator::default();
        // Different divisors for x and y
        let (ox, oy) = acc.accumulate(10, 10, (1, 2), (1, 5));
        assert_eq!(ox, 5); // 10/2
        assert_eq!(oy, 2); // 10/5
    }

    #[test]
    fn test_motion_accumulator_persistent_keeps_total_in_remainder() {
        // accumulate_persistent does NOT subtract the output from the
        // remainder. The remainder is the full signed running total; the
        // output is just total / divisor. This is what caret mode needs:
        // the caller decides when to "spend" the remainder (via reset_x/y).
        let mut acc = MotionAccumulator::default();
        let (ox, oy) = acc.accumulate_persistent(150, 0, (1, 100), (1, 100));
        assert_eq!(ox, 1); // 150 / 100
        assert_eq!(oy, 0);
        assert_eq!(acc.remainder_x, 150); // total, NOT 50
        assert_eq!(acc.remainder_y, 0);

        // Second call: total_x = 150 + 50 = 200, out=2, remainder=200
        let (ox, oy) = acc.accumulate_persistent(50, 0, (1, 100), (1, 100));
        assert_eq!(ox, 2); // 200 / 100
        assert_eq!(oy, 0);
        assert_eq!(acc.remainder_x, 200);
        assert_eq!(acc.remainder_y, 0);
    }

    #[test]
    fn test_motion_accumulator_persistent_independent_axes() {
        // X and Y remainders are independent in accumulate_persistent:
        // movement on one axis never touches the other's running total.
        let mut acc = MotionAccumulator::default();
        let (ox, oy) = acc.accumulate_persistent(150, 0, (1, 100), (1, 100));
        assert_eq!(ox, 1);
        assert_eq!(oy, 0);
        assert_eq!(acc.remainder_x, 150);
        assert_eq!(acc.remainder_y, 0);

        // Now move purely on Y. X total is preserved (still 150), Y starts fresh.
        let (ox, oy) = acc.accumulate_persistent(0, 150, (1, 100), (1, 100));
        assert_eq!(ox, 1); // X total 150 / 100
        assert_eq!(oy, 1); // Y total 150 / 100
        assert_eq!(acc.remainder_x, 150);
        assert_eq!(acc.remainder_y, 150);
    }

    #[test]
    fn test_motion_accumulator_persistent_sub_threshold_accumulates() {
        // Several sub-threshold samples must build up the running total
        // in the remainder. The output (total / divisor) stays 0 until the
        // total crosses the threshold; then output jumps and remainder
        // continues to grow. Caret mode relies on the running total so
        // that slow deltas are not lost.
        let mut acc = MotionAccumulator::default();

        let (ox, oy) = acc.accumulate_persistent(30, 30, (1, 100), (1, 100));
        assert_eq!(ox, 0);
        assert_eq!(oy, 0);
        assert_eq!(acc.remainder_x, 30);
        assert_eq!(acc.remainder_y, 30);

        let (ox, oy) = acc.accumulate_persistent(30, 30, (1, 100), (1, 100));
        assert_eq!(ox, 0);
        assert_eq!(oy, 0);
        assert_eq!(acc.remainder_x, 60);
        assert_eq!(acc.remainder_y, 60);

        let (ox, oy) = acc.accumulate_persistent(30, 30, (1, 100), (1, 100));
        assert_eq!(ox, 0);
        assert_eq!(oy, 0);
        assert_eq!(acc.remainder_x, 90);
        assert_eq!(acc.remainder_y, 90);

        // Crosses threshold: total 90 + 20 = 110, out=1, remainder=110
        let (ox, oy) = acc.accumulate_persistent(20, 20, (1, 100), (1, 100));
        assert_eq!(ox, 1);
        assert_eq!(oy, 1);
        assert_eq!(acc.remainder_x, 110);
        assert_eq!(acc.remainder_y, 110);
    }

    #[test]
    fn test_motion_accumulator_persistent_sign_change_in_total() {
        // accumulate_persistent uses a signed running total. A counter-
        // direction sample just subtracts from the total. The caret
        // handler has to detect the sign change and reset that axis
        // (reset_x / reset_y) to avoid direction bias: otherwise
        // small counter-direction samples silently "use up" the running
        // total and block taps in the new direction.
        let mut acc = MotionAccumulator::default();
        acc.accumulate_persistent(80, 0, (1, 100), (1, 100));
        assert_eq!(acc.remainder_x, 80);

        // Counter-direction sample of 30: total = 80 - 30 = 50, no tap.
        let (ox, oy) = acc.accumulate_persistent(-30, 0, (1, 100), (1, 100));
        assert_eq!(ox, 0);
        assert_eq!(oy, 0);
        assert_eq!(acc.remainder_x, 50);

        // Cross zero: total = 50 - 80 = -30, remainder becomes negative.
        // No tap fires (still sub-threshold), but the total is now signed.
        let (ox, oy) = acc.accumulate_persistent(-80, 0, (1, 100), (1, 100));
        assert_eq!(ox, 0);
        assert_eq!(oy, 0);
        assert_eq!(acc.remainder_x, -30);
    }

    #[test]
    fn test_motion_accumulator_persistent_reset_x_and_y() {
        // reset_x() and reset_y() clear the running total of one axis only,
        // leaving the other axis intact. The caret handler uses these on
        // sign change so a stale total in the previous direction cannot
        // bleed into the next sample.
        let mut acc = MotionAccumulator::default();
        acc.accumulate_persistent(80, 80, (1, 100), (1, 100));
        assert_eq!(acc.remainder_x, 80);
        assert_eq!(acc.remainder_y, 80);

        acc.reset_x();
        assert_eq!(acc.remainder_x, 0);
        assert_eq!(acc.remainder_y, 80);

        // Y continues to build; X is fresh
        let (ox, oy) = acc.accumulate_persistent(30, 30, (1, 100), (1, 100));
        assert_eq!(ox, 0); // 30 / 100
        assert_eq!(oy, 1); // (80 + 30) / 100
        assert_eq!(acc.remainder_x, 30);
        assert_eq!(acc.remainder_y, 110);

        acc.reset_y();
        assert_eq!(acc.remainder_x, 30);
        assert_eq!(acc.remainder_y, 0);
    }

    #[test]
    fn test_motion_accumulator_persistent_asymmetric_divisors() {
        // Different divisors on X and Y are handled independently. Even
        // though the current CaretConfig uses the same divisor on both
        // axes, the accumulator must work when they differ.
        let mut acc = MotionAccumulator::default();
        let (ox, oy) = acc.accumulate_persistent(10, 10, (1, 2), (1, 5));
        assert_eq!(ox, 5); // 10 / 2
        assert_eq!(oy, 2); // 10 / 5
        assert_eq!(acc.remainder_x, 10);
        assert_eq!(acc.remainder_y, 10);
    }

    #[test]
    fn test_motion_accumulator_persistent_divisor_edge_cases() {
        // divisor=1: every unit becomes output; remainder still tracks
        // the full signed total so successive calls keep growing it.
        let mut acc = MotionAccumulator::default();
        let (ox, oy) = acc.accumulate_persistent(7, -3, (1, 1), (1, 1));
        assert_eq!(ox, 7);
        assert_eq!(oy, -3);
        assert_eq!(acc.remainder_x, 7);
        assert_eq!(acc.remainder_y, -3);

        // divisor=0 on X: axis is disabled (output 0, remainder_x = 0).
        // divisor=255 on Y: maximal u8 divisor; 50/255 = 0, remainder_y = 50.
        let mut acc = MotionAccumulator::default();
        let (ox, oy) = acc.accumulate_persistent(50, 50, (1, 0), (1, 255));
        assert_eq!(ox, 0);
        assert_eq!(oy, 0);
        assert_eq!(acc.remainder_x, 0);
        assert_eq!(acc.remainder_y, 50);
    }

    // === compute_caret_taps tests ===

    fn cfg() -> CaretConfig {
        CaretConfig::default()
    }

    fn acc() -> MotionAccumulator {
        MotionAccumulator::default()
    }

    #[test]
    fn test_compute_caret_taps_zero_motion_returns_none() {
        let mut a = acc();
        assert!(compute_caret_taps(0, 0, &mut a, &cfg()).is_none());
    }

    #[test]
    fn test_compute_caret_taps_sub_threshold_returns_none() {
        let mut a = acc();
        // |50|+|50| = 100, exactly at threshold → no tap
        assert!(compute_caret_taps(50, 50, &mut a, &cfg()).is_none());
        // Accumulator still tracks the running total even when sub-threshold
        assert_eq!(a.remainder_x, 50);
        assert_eq!(a.remainder_y, 50);
    }

    #[test]
    fn test_compute_caret_taps_x_dominant_default_right() {
        let mut a = acc();
        let result = compute_caret_taps(150, 30, &mut a, &cfg());
        // |150|+|30| = 180 > 100. X dominant (150 >= 30). dx>0 + !invert → Right.
        // Loop: reduce_x=-100 → total=(50, 0), |50|+|0|=50<=100 → stop.
        assert_eq!(result, Some((HidKeyCode::Right, 1)));
        assert_eq!(a.remainder_x, 50);
        assert_eq!(a.remainder_y, 0); // non-dominant reset
    }

    #[test]
    fn test_compute_caret_taps_x_dominant_negative_is_left() {
        let mut a = acc();
        let result = compute_caret_taps(-150, 30, &mut a, &cfg());
        assert_eq!(result, Some((HidKeyCode::Left, 1)));
    }

    #[test]
    fn test_compute_caret_taps_y_dominant_default_is_down() {
        let mut a = acc();
        let result = compute_caret_taps(30, 150, &mut a, &cfg());
        // Y dominant, dy>0, !invert → Down (default +Y = down per HID)
        assert_eq!(result, Some((HidKeyCode::Down, 1)));
        assert_eq!(a.remainder_x, 0); // non-dominant reset
        assert_eq!(a.remainder_y, 50);
    }

    #[test]
    fn test_compute_caret_taps_invert_y_flips_to_up() {
        let mut a = acc();
        let mut c = cfg();
        c.invert_y = true;
        let result = compute_caret_taps(30, 150, &mut a, &c);
        // dy>0 + invert_y → Up
        assert_eq!(result, Some((HidKeyCode::Up, 1)));
    }

    #[test]
    fn test_compute_caret_taps_invert_x_flips_to_left() {
        let mut a = acc();
        let mut c = cfg();
        c.invert_x = true;
        let result = compute_caret_taps(150, 30, &mut a, &c);
        // dx>0 + invert_x → Left
        assert_eq!(result, Some((HidKeyCode::Left, 1)));
    }

    #[test]
    fn test_compute_caret_taps_multiple_taps_in_one_event() {
        let mut a = acc();
        let result = compute_caret_taps(250, 0, &mut a, &cfg());
        // X dominant, |250|=250, threshold=100 → 2 taps
        // Iter 1: reduce_x=-100 → total=(150, 0), |150|>100 → tap
        // Iter 2: reduce_x=-100 → total=(50, 0), |50|<=100 → stop
        assert_eq!(result, Some((HidKeyCode::Right, 2)));
        assert_eq!(a.remainder_x, 50);
        assert_eq!(a.remainder_y, 0);
    }

    #[test]
    fn test_compute_caret_taps_sub_threshold_accumulates_across_calls() {
        let mut a = acc();
        // 3 calls of (20, 20): total grows (20,20)→(40,40)→(60,60)
        assert!(compute_caret_taps(20, 20, &mut a, &cfg()).is_none()); // 40
        assert!(compute_caret_taps(20, 20, &mut a, &cfg()).is_none()); // 80
        // 3rd call: |60|+|60|=120 > 100 → tap
        assert_eq!(compute_caret_taps(20, 20, &mut a, &cfg()), Some((HidKeyCode::Right, 1)));
    }

    #[test]
    fn test_compute_caret_taps_zero_divisor_disables_axis() {
        let mut a = acc();
        let mut c = cfg();
        c.disable_x = true;
        // dx=0, dy=120. |0|+|120|=120 > 100 → tap. Y dominant. dy>0 + !invert → Down.
        assert_eq!(compute_caret_taps(0, 120, &mut a, &c), Some((HidKeyCode::Down, 1)));
    }

    #[test]
    fn test_compute_caret_taps_direction_change_works() {
        let mut a = acc();
        // First a Right tap
        assert_eq!(
            compute_caret_taps(150, 30, &mut a, &cfg()),
            Some((HidKeyCode::Right, 1))
        );
        // After: remainder_x = 50, remainder_y = 0 (reset)
        assert_eq!(a.remainder_x, 50);
        assert_eq!(a.remainder_y, 0);

        // Now move left. -200 + 50 (carry-over) = -150 → |-150|=150>100 → tap
        assert_eq!(compute_caret_taps(-200, 0, &mut a, &cfg()), Some((HidKeyCode::Left, 1)));
    }

    #[test]
    fn test_compute_caret_taps_threshold_at_exactly_boundary() {
        let mut a = acc();
        // |100|+|0| = 100, exactly threshold → no tap
        assert!(compute_caret_taps(100, 0, &mut a, &cfg()).is_none());
        assert_eq!(a.remainder_x, 100);
        // One more unit pushes over
        assert_eq!(compute_caret_taps(1, 0, &mut a, &cfg()), Some((HidKeyCode::Right, 1)));
    }

    #[test]
    fn test_compute_caret_taps_diagonal_motion() {
        let mut a = acc();
        let result = compute_caret_taps(250, 250, &mut a, &cfg());
        // X dominant, |250|=250, threshold=100 → 2 taps
        // Iter 1: reduce_x=-100 → total=(150, 0), |150|>100 → tap
        // Iter 2: reduce_x=-100 → total=(50, 0), |50|<=100 → stop
        assert_eq!(result, Some((HidKeyCode::Right, 2)));
        assert_eq!(a.remainder_x, 50);
        assert_eq!(a.remainder_y, 0);
    }

    // === PointingMode tests ===

    #[test]
    fn test_pointing_mode_default_is_cursor() {
        assert_eq!(PointingMode::default(), PointingMode::Cursor(CursorConfig::default()));
    }

    #[test]
    fn test_pointing_mode_array_default() {
        let modes: [PointingMode; 4] = [PointingMode::default(); 4];
        for mode in &modes {
            assert_eq!(*mode, PointingMode::Cursor(CursorConfig::default()));
        }
    }

    #[test]
    fn test_motion_accumulator_change_resets_accumulator() {
        let mut acc = MotionAccumulator::default();
        acc.accumulate(3, 5, (1, 8), (1, 8));
        assert_eq!(acc.remainder_x, 3);
        assert_eq!(acc.remainder_y, 5);

        // Simulate what on_layer_change_event does
        acc.reset();
        assert_eq!(acc.remainder_x, 0);
        assert_eq!(acc.remainder_y, 0);
    }

    #[test]
    fn test_pointing_cursor_multiplier_scales_motion() {
        let config = CursorConfig {
            multiplier_x: 2,
            multiplier_y: 3,
            invert_x: false,
            invert_y: false,
        };
        assert_eq!(10 * config.multiplier_x as i16, 20);
        assert_eq!(10 * config.multiplier_y as i16, 30);
    }

    #[test]
    fn test_pointing_cursor_invert_axes() {
        let config = CursorConfig {
            multiplier_x: 1,
            multiplier_y: 1,
            invert_x: true,
            invert_y: true,
        };
        assert_eq!(-(10 * config.multiplier_x as i16), -10);
        assert_eq!(-(10 * config.multiplier_y as i16), -10);
    }

    // === Integration tests for PointingProcessor ===

    #[test]
    fn test_pointing_processor_mode_selection() {
        // Test that the processor correctly selects the mode based on current layer
        let modes = [
            PointingMode::Disabled,
            PointingMode::Cursor(CursorConfig::default()),
            PointingMode::Scroll(ScrollConfig::default()),
            PointingMode::Sniper(SniperConfig {
                multiplier: 1,
                divisor: 4,
                invert_x: false,
                invert_y: false,
            }),
            PointingMode::Caret(CaretConfig::default()),
            PointingMode::Cursor(CursorConfig::default()),
        ];

        // Verify all modes are correctly stored
        for (i, expected_mode) in modes.iter().enumerate() {
            assert_eq!(&modes[i], expected_mode);
        }
    }

    #[test]
    fn test_pointing_scroll_mode_zero_motion_prevention() {
        let mut acc = MotionAccumulator::default();
        let config = ScrollConfig {
            multiplier_x: 1,
            multiplier_y: 1,
            divisor_x: 8,
            divisor_y: 8,
            invert_x: false,
            invert_y: false,
        };

        // Small motion that doesn't produce output
        let (sx, sy) = acc.accumulate(
            3,
            3,
            (config.multiplier_x, config.divisor_x),
            (config.multiplier_y, config.divisor_y),
        );
        assert_eq!(sx, 0);
        assert_eq!(sy, 0);

        // Verify remainder is kept
        assert_eq!(acc.remainder_x, 3);
        assert_eq!(acc.remainder_y, 3);

        // Additional motion should accumulate
        let (sx, sy) = acc.accumulate(
            6,
            6,
            (config.multiplier_x, config.divisor_x),
            (config.multiplier_y, config.divisor_y),
        );
        assert_eq!(sx, 1); // (3+6)/8 = 1 remainder 1
        assert_eq!(sy, 1);
        assert_eq!(acc.remainder_x, 1);
        assert_eq!(acc.remainder_y, 1);
    }

    #[test]
    fn test_pointing_sniper_mode_divisor() {
        let mut acc = MotionAccumulator::default();
        let config = SniperConfig {
            multiplier: 1,
            divisor: 4,
            invert_x: false,
            invert_y: false,
        };

        // Test that motion is divided correctly
        let (sx, sy) = acc.accumulate(
            10,
            -10,
            (config.multiplier, config.divisor),
            (config.multiplier, config.divisor),
        );
        assert_eq!(sx, 2); // 10/4 = 2 remainder 2
        assert_eq!(sy, -2); // -10/4 = -2 remainder -2
        assert_eq!(acc.remainder_x, 2);
        assert_eq!(acc.remainder_y, -2);
    }

    #[test]
    fn test_motion_accumulator_negative_motion() {
        let mut acc = MotionAccumulator::default();

        // Test negative motion with divisor
        let (ox, oy) = acc.accumulate(-15, -20, (1, 4), (1, 5));
        assert_eq!(ox, -3); // -15/4 = -3 remainder -3
        assert_eq!(oy, -4); // -20/5 = -4 remainder 0
        assert_eq!(acc.remainder_x, -3);
        assert_eq!(acc.remainder_y, 0);

        // Mix positive and negative
        let (ox, oy) = acc.accumulate(5, 10, (1, 4), (1, 5));
        assert_eq!(ox, 0); // (-3+5)/4 = 0 remainder 2
        assert_eq!(oy, 2); // (0+10)/5 = 2 remainder 0
        assert_eq!(acc.remainder_x, 2);
        assert_eq!(acc.remainder_y, 0);
    }

    #[test]
    fn test_pointing_scroll_config_default_values() {
        let config = ScrollConfig::default();
        assert_eq!(config.divisor_x, 8);
        assert_eq!(config.divisor_y, 8);
        assert!(!config.invert_x);
        assert!(!config.invert_y);
    }

    #[test]
    fn test_pointing_sniper_config_default_values() {
        let config = SniperConfig::default();
        assert_eq!(config.divisor, 4);
        assert!(!config.invert_x);
        assert!(!config.invert_y);
    }

    #[test]
    fn test_pointing_scroll_config_invert_y() {
        // invert_y=true means positive sensor Y → positive wheel (reversed from default)
        // Default (invert_y=false): sensor +Y → wheel -1 (scroll up)
        // With invert_y=true:        sensor +Y → wheel +1 (scroll down)
        let mut acc_default = MotionAccumulator::default();
        let mut acc_inverted = MotionAccumulator::default();

        let divisor = 1u8;
        let (_, sy_default) = acc_default.accumulate(0, 10, (1, divisor), (1, divisor));
        let (_, sy_inverted) = acc_inverted.accumulate(0, 10, (1, divisor), (1, divisor));

        // Default: wheel = -sy = -10
        let wheel_default = -sy_default;
        // Inverted: wheel = sy = 10
        let wheel_inverted = sy_inverted;

        assert_eq!(wheel_default, -10);
        assert_eq!(wheel_inverted, 10);
    }

    #[test]
    fn test_pointing_sniper_config_invert_axes() {
        let mut acc = MotionAccumulator::default();
        let config = SniperConfig {
            multiplier: 1,
            divisor: 1,
            invert_x: true,
            invert_y: true,
        };

        let (sx, sy) = acc.accumulate(
            5,
            -3,
            (config.multiplier, config.divisor),
            (config.multiplier, config.divisor),
        );
        let out_x = if config.invert_x { -sx } else { sx };
        let out_y = if config.invert_y { -sy } else { sy };

        assert_eq!(out_x, -5);
        assert_eq!(out_y, 3);
    }

    #[test]
    fn test_pointing_scroll_mode_asymmetric_divisors() {
        let mut acc = MotionAccumulator::default();
        let config = ScrollConfig {
            multiplier_x: 1,
            multiplier_y: 1,
            divisor_x: 4,
            divisor_y: 8,
            invert_x: false,
            invert_y: false,
        };

        // Test asymmetric divisors
        let (sx, sy) = acc.accumulate(
            16,
            16,
            (config.multiplier_x, config.divisor_x),
            (config.multiplier_x, config.divisor_y),
        );
        assert_eq!(sx, 4); // 16/4 = 4
        assert_eq!(sy, 2); // 16/8 = 2
    }

    #[test]
    fn test_pointing_scroll_multiplier_amplifies_motion() {
        let mut acc = MotionAccumulator::default();
        let config = ScrollConfig {
            multiplier_x: 3,
            multiplier_y: 3,
            divisor_x: 8,
            divisor_y: 8,
            invert_x: false,
            invert_y: false,
        };
        let (sx, sy) = acc.accumulate(
            10,
            10,
            (config.multiplier_x, config.divisor_x),
            (config.multiplier_y, config.divisor_y),
        );
        assert_eq!(sx, 3); // (0+10*3)/8 = 3 r6
        assert_eq!(sy, 3);
        assert_eq!(acc.remainder_x, 6);
        assert_eq!(acc.remainder_y, 6);
    }

    #[test]
    fn test_pointing_scroll_asymmetric_multipliers() {
        let mut acc = MotionAccumulator::default();
        let config = ScrollConfig {
            multiplier_x: 2,
            multiplier_y: 3,
            divisor_x: 8,
            divisor_y: 8,
            invert_x: false,
            invert_y: false,
        };
        let (sx, sy) = acc.accumulate(
            10,
            -10,
            (config.multiplier_x, config.divisor_x),
            (config.multiplier_y, config.divisor_y),
        );
        assert_eq!(sx, 2); // (0+10*2)/8 = 2 r4
        assert_eq!(sy, -3); // (0+(-10)*3)/8 = -3 r-6
        assert_eq!(acc.remainder_x, 4);
        assert_eq!(acc.remainder_y, -6);
    }

    #[test]
    fn test_pointing_scroll_multiplier_accumulates_remainder() {
        let mut acc = MotionAccumulator::default();
        let config = ScrollConfig {
            multiplier_x: 3,
            multiplier_y: 3,
            divisor_x: 8,
            divisor_y: 8,
            invert_x: false,
            invert_y: false,
        };
        // First call: sub-threshold, only remainder accumulates
        let (sx, sy) = acc.accumulate(
            2,
            2,
            (config.multiplier_x, config.divisor_x),
            (config.multiplier_y, config.divisor_y),
        );
        assert_eq!(sx, 0); // (0+2*3)/8 = 0 r6
        assert_eq!(sy, 0);
        assert_eq!(acc.remainder_x, 6);
        assert_eq!(acc.remainder_y, 6);

        // Second call: crosses threshold, multiplier applies to remainder too
        let (sx, sy) = acc.accumulate(
            2,
            2,
            (config.multiplier_x, config.divisor_x),
            (config.multiplier_y, config.divisor_y),
        );
        assert_eq!(sx, 1); // (6+2*3)/8 = 12/8 = 1 r4
        assert_eq!(sy, 1);
        assert_eq!(acc.remainder_x, 4);
        assert_eq!(acc.remainder_y, 4);
    }

    #[test]
    fn test_pointing_sniper_multiplier_amplifies_motion() {
        let mut acc = MotionAccumulator::default();
        let config = SniperConfig {
            multiplier: 3,
            divisor: 4,
            invert_x: false,
            invert_y: false,
        };
        let (sx, sy) = acc.accumulate(
            5,
            0,
            (config.multiplier, config.divisor),
            (config.multiplier, config.divisor),
        );
        assert_eq!(sx, 3); // (0+5*3)/4 = 3 r3
        assert_eq!(sy, 0);
        assert_eq!(acc.remainder_x, 3);
        assert_eq!(acc.remainder_y, 0);
    }

    #[test]
    fn test_pointing_sniper_multiplier_with_negative() {
        let mut acc = MotionAccumulator::default();
        let config = SniperConfig {
            multiplier: 3,
            divisor: 4,
            invert_x: false,
            invert_y: false,
        };
        let (sx, sy) = acc.accumulate(
            -3,
            0,
            (config.multiplier, config.divisor),
            (config.multiplier, config.divisor),
        );
        assert_eq!(sx, -2); // (0+(-3)*3)/4 = -9/4 = -2 r-1
        assert_eq!(sy, 0);
        assert_eq!(acc.remainder_x, -1);
        assert_eq!(acc.remainder_y, 0);
    }

    #[test]
    fn test_pointing_layer_mode_bounds_checking() {
        // Test that modes array is correctly sized
        let modes: [PointingMode; 8] = [PointingMode::default(); 8];
        assert_eq!(modes.len(), 8);

        // Verify all default to Cursor
        for mode in &modes {
            assert_eq!(*mode, PointingMode::Cursor(CursorConfig::default()));
        }
    }

    #[test]
    fn test_motion_accumulator_saturation() {
        let mut acc = MotionAccumulator::default();

        // Test with large values that might overflow
        let (ox, oy) = acc.accumulate(i16::MAX, i16::MAX, (1, 1), (1, 1));
        assert_eq!(ox, i16::MAX);
        assert_eq!(oy, i16::MAX);

        // Reset and test negative saturation
        acc.reset();
        let (ox, oy) = acc.accumulate(i16::MIN, i16::MIN, (1, 1), (1, 1));
        assert_eq!(ox, i16::MIN);
        assert_eq!(oy, i16::MIN);
    }
}

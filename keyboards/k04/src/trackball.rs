use embassy_futures::select::{select, select3, Either, Either3};
use embassy_nrf::gpio::{Flex, Input, Level, Output, OutputDrive, Pull};
use embassy_nrf::Peri;
use embassy_time::{Duration, Instant, Timer};
use rmk::core_traits::Runnable;
use rmk::driver::bitbang_spi::BitBangSpiBus;
use rmk::event::{publish_event, Axis, AxisEvent, AxisValType, EventSubscriber, PointingEvent};
use rmk::input_device::pmw3610::{Pmw3610, Pmw3610Config, Pmw3610Health};
use rmk::input_device::pointing::PointingDriver;
use rmk::processor::Processor;

use crate::module_settings;

mod motion_pacing;
use motion_pacing::{
    take_report_axis, LEGACY_TRANSPORT_REPORT_INTERVAL_US, LOCAL_REPORT_INTERVAL_US, SPLIT_REPORT_INTERVAL_US,
};

#[cfg(all(
    feature = "production_v22",
    any(
        feature = "rtt_diag",
        feature = "usb_debug",
        feature = "usb_debug_force_ble",
        feature = "host_fixed_15ms",
        feature = "pmw_force_awake_diag",
        feature = "pmw_smart_off_diag",
        feature = "pmw_raw_600_diag",
        feature = "pmw_raw_1000_diag",
        feature = "pmw_axes_600_diag"
    )
))]
compile_error!("production_v22 cannot be combined with diagnostic or USB debug features");

#[cfg(all(feature = "pmw_force_awake_diag", feature = "pmw_smart_off_diag"))]
compile_error!("pmw_force_awake_diag and pmw_smart_off_diag are mutually exclusive diagnostics");

#[cfg(all(feature = "pmw_raw_600_diag", feature = "pmw_raw_1000_diag"))]
compile_error!("pmw_raw_600_diag and pmw_raw_1000_diag are mutually exclusive diagnostics");

#[cfg(all(
    feature = "pmw_axes_600",
    any(feature = "pmw_raw_600_diag", feature = "pmw_raw_1000_diag")
))]
compile_error!("pmw_axes_600 cannot be combined with stage-10 raw diagnostics");

const FAST_PROBE_INTERVAL: Duration = Duration::from_millis(250);
const SLOW_PROBE_INTERVAL: Duration = Duration::from_secs(2);
const FAST_PROBE_WINDOW: Duration = Duration::from_secs(10);
// Preserve the validated v22 recovery behavior in production: check every
// motion-affecting register once a second so a briefly disconnected module
// cannot return with reset orientation/CPI state.
#[cfg(feature = "pmw_axes_600")]
const HEALTH_CHECK_INTERVAL: Duration = Duration::from_secs(1);
#[cfg(not(feature = "pmw_axes_600"))]
const HEALTH_CHECK_INTERVAL: Duration = Duration::from_secs(60);
// v22 keeps local motion at 250 Hz, but paces the split peripheral at about
// 67 Hz. At weak RSSI, retransmissions can make notifications arrive at the
// central in bursts. Fifteen milliseconds gives every motion report two full
// 7.5 ms split connection windows, while full i16 deltas preserve the exact
// accumulated movement for the central's vector-preserving HID writer.
const LOCAL_REPORT_INTERVAL: Duration = Duration::from_micros(LOCAL_REPORT_INTERVAL_US);
const SPLIT_REPORT_INTERVAL: Duration = Duration::from_micros(SPLIT_REPORT_INTERVAL_US);
const LEGACY_TRANSPORT_REPORT_INTERVAL: Duration = Duration::from_micros(LEGACY_TRANSPORT_REPORT_INTERVAL_US);
// If MOTION stops toggling during an active gesture, briefly poll at the
// report cadence. This bridges transient GPIO/power-mode gaps without keeping
// the sensor awake indefinitely after the user stops the ball.
const MOTION_FALLBACK_INTERVAL: Duration = Duration::from_millis(8);
const MOTION_FALLBACK_WINDOW: Duration = Duration::from_millis(750);
const MOTION_ACCUM_MIN: i32 = i16::MIN as i32;
const MOTION_ACCUM_MAX: i32 = i16::MAX as i32;
const MAX_MOTION_READS_PER_WAKE: usize = 16;
#[cfg(feature = "rtt_diag")]
const SCHEDULER_PROBE_INTERVAL: Duration = Duration::from_millis(10);
const SLEEP_MOTION_THRESHOLD: u32 = 2;
const SLEEP_MOTION_WINDOW: Duration = Duration::from_millis(20);
// Even in Rest3 the module drains the half about 1%/h. Short pauses keep the
// ball able to wake the board; a long sleep shuts the sensor down until a key
// wakes it.
const SLEEP_SHUTDOWN_DELAY: Duration = Duration::from_secs(60 * 60);
#[cfg(any(feature = "pmw_raw_600_diag", feature = "pmw_axes_600"))]
const DEFAULT_CPI: u16 = 600;
#[cfg(not(any(feature = "pmw_raw_600_diag", feature = "pmw_axes_600")))]
const DEFAULT_CPI: u16 = 1000;

pub type K04Trackball = Pmw3610<BitBangSpiBus<Output<'static>, Flex<'static>>, Output<'static>, Input<'static>>;

pub fn new_trackball(
    id: u8,
    sck: Output<'static>,
    sdio: Flex<'static>,
    cs: Output<'static>,
    motion: Input<'static>,
) -> K04Trackball {
    let spi = BitBangSpiBus::new(sck, sdio);
    let config = Pmw3610Config {
        res_cpi: DEFAULT_CPI as i16,
        swap_xy: !cfg!(feature = "pmw_axes_600"),
        invert_x: false,
        invert_y: false,
        force_awake: cfg!(feature = "pmw_force_awake_diag"),
        smart_mode: !cfg!(feature = "pmw_smart_off_diag"),
    };
    Pmw3610::new(id, spi, cs, Some(motion), config)
}

#[cfg(any(feature = "pmw_raw_600_diag", feature = "pmw_axes_600_diag"))]
fn configured_ball_cpi(_device_id: u8) -> u16 {
    600
}

#[cfg(feature = "pmw_raw_1000_diag")]
fn configured_ball_cpi(_device_id: u8) -> u16 {
    1000
}

#[cfg(not(any(
    feature = "pmw_raw_600_diag",
    feature = "pmw_raw_1000_diag",
    feature = "pmw_axes_600_diag"
)))]
fn configured_ball_cpi(device_id: u8) -> u16 {
    module_settings::ball_cpi(device_id)
}

pub fn new_trackball_from_pins(
    id: u8,
    sck: Peri<'static, embassy_nrf::peripherals::P0_01>,
    sdio: Peri<'static, embassy_nrf::peripherals::P0_00>,
    cs: Peri<'static, embassy_nrf::peripherals::P0_05>,
    motion: Peri<'static, embassy_nrf::peripherals::P1_09>,
) -> K04Trackball {
    new_trackball(
        id,
        Output::new(sck, Level::High, OutputDrive::Standard),
        Flex::new(sdio),
        Output::new(cs, Level::High, OutputDrive::Standard),
        Input::new(motion, Pull::Up),
    )
}

pub struct Trackball {
    trackball: K04Trackball,
    device_id: u8,
    is_central: bool,
    ready: bool,
    acc_x: i32,
    acc_y: i32,
    last_report: Instant,
    last_motion_activity: Option<Instant>,
    sleep_motion_deadline: Option<Instant>,
    sleep_started: Option<Instant>,
    next_probe: Instant,
    next_health_check: Instant,
    unavailable_since: Option<Instant>,
    current_cpi: u16,
    last_vbus_detect: Option<bool>,
    recovery_pending: bool,
    recovery_count: u32,
}

impl Trackball {
    // This source is shared by separate central and peripheral binaries, so
    // each binary intentionally leaves one constructor unused.
    #[allow(dead_code)]
    pub fn new_central(trackball: K04Trackball, device_id: u8) -> Self {
        Self::new(trackball, device_id, true)
    }

    #[allow(dead_code)]
    pub fn new_peripheral(trackball: K04Trackball, device_id: u8) -> Self {
        Self::new(trackball, device_id, false)
    }

    fn new(trackball: K04Trackball, device_id: u8, is_central: bool) -> Self {
        Self {
            trackball,
            device_id,
            is_central,
            ready: false,
            acc_x: 0,
            acc_y: 0,
            last_report: Instant::MIN,
            last_motion_activity: None,
            sleep_motion_deadline: None,
            sleep_started: None,
            next_probe: Instant::MIN,
            next_health_check: Instant::MIN,
            unavailable_since: None,
            current_cpi: DEFAULT_CPI,
            last_vbus_detect: None,
            recovery_pending: false,
            recovery_count: 0,
        }
    }

    async fn run_loop(&mut self) -> ! {
        loop {
            let selection = module_settings::module_selection(self.device_id);
            if selection != module_settings::ModuleSelection::Trackball {
                self.deactivate();
                let _ = module_settings::wait_for_module_selection_change(self.device_id, selection).await;
                continue;
            }

            let sleeping = module_settings::module_sleeping();
            if !self.ready {
                // Do not probe an unavailable sensor while the keyboard is
                // asleep. A configured PMW3610 follows MOTION below instead.
                if sleeping {
                    match select(
                        module_settings::wait_for_module_selection_change(
                            self.device_id,
                            module_settings::ModuleSelection::Trackball,
                        ),
                        module_settings::wait_for_module_sleep_change(sleeping),
                    )
                    .await
                    {
                        Either::First(_) => self.deactivate(),
                        Either::Second(_) => self.resume_from_sleep(),
                    }
                    continue;
                }

                let now = Instant::now();
                if now < self.next_probe {
                    match select3(
                        Timer::at(self.next_probe),
                        module_settings::wait_for_module_selection_change(
                            self.device_id,
                            module_settings::ModuleSelection::Trackball,
                        ),
                        module_settings::wait_for_module_sleep_change(sleeping),
                    )
                    .await
                    {
                        Either3::First(_) => {}
                        Either3::Second(_) => {
                            self.deactivate();
                            continue;
                        }
                        Either3::Third(next_sleeping) => {
                            if next_sleeping {
                                self.park_for_sleep();
                            } else {
                                self.resume_from_sleep();
                            }
                            continue;
                        }
                    }
                }

                // Avoid initializing in the small race between the retry
                // deadline and a sleep-state update.
                if module_settings::module_sleeping() {
                    self.park_for_sleep();
                    continue;
                }

                if self.trackball.init().await.is_ok() {
                    let now = Instant::now();
                    self.current_cpi = configured_ball_cpi(self.device_id);
                    if self.trackball.set_resolution(self.current_cpi).await.is_err() {
                        self.reject_sensor_configuration(now, 2, None, false);
                        continue;
                    }
                    let health = match self.trackball.configuration_health(self.current_cpi).await {
                        Ok(health) if health.is_valid() => health,
                        Ok(health) => {
                            self.reject_sensor_configuration(now, 2, Some(health), false);
                            continue;
                        }
                        Err(_) => {
                            self.reject_sensor_configuration(now, 2, None, false);
                            continue;
                        }
                    };
                    #[cfg(any(feature = "pmw_raw_600_diag", feature = "pmw_raw_1000_diag"))]
                    defmt::info!("[PMW_RAW_CFG_V13] cpi={} smart=on force_awake=off", self.current_cpi);
                    #[cfg(feature = "pmw_axes_600_diag")]
                    defmt::info!(
                        "[PMW_AXIS_CFG_V18] cpi={} hardware_swap=false software_swap=true smart=on force_awake=off res_step={:#04x}",
                        self.current_cpi,
                        health.res_step
                    );
                    self.ready = true;
                    self.acc_x = 0;
                    self.acc_y = 0;
                    self.last_report = now;
                    self.last_motion_activity = None;
                    self.next_health_check = now + HEALTH_CHECK_INTERVAL;
                    self.unavailable_since = None;
                    self.last_vbus_detect = Some(crate::battery_nrf::usb_power_status().0);
                    if self.recovery_pending {
                        #[cfg(feature = "pmw_axes_600_diag")]
                        defmt::info!(
                            "[PMW_RECOVERED_V18] side={} count={} cpi={} res_step={:#04x}",
                            self.device_id,
                            self.recovery_count,
                            self.current_cpi,
                            health.res_step
                        );
                        self.recovery_pending = false;
                    }
                    let _ = health;
                } else {
                    self.mark_unavailable(Instant::now());
                    continue;
                }
            }

            let sleeping = module_settings::module_sleeping();
            let shutdown_deadline =
                sleeping.then(|| *self.sleep_started.get_or_insert_with(Instant::now) + SLEEP_SHUTDOWN_DELAY);
            if shutdown_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                // init() on wake brings the sensor back from shutdown.
                let _ = self.trackball.shutdown().await;
                self.park_for_sleep();
                self.ready = false;
                self.next_probe = Instant::MIN;
                continue;
            }
            if !sleeping {
                self.apply_configured_cpi().await;
            }
            let report_interval = self.report_interval();
            let report_deadline = self
                .has_reportable_motion(sleeping)
                .then_some(self.last_report + report_interval);
            let base_deadline = if sleeping {
                [report_deadline, self.sleep_motion_deadline, shutdown_deadline]
                    .into_iter()
                    .flatten()
                    .min()
            } else {
                Some(
                    report_deadline
                        .map(|report| report.min(self.next_health_check))
                        .unwrap_or(self.next_health_check),
                )
            };
            let now = Instant::now();
            let fallback_deadline = if sleeping {
                None
            } else {
                self.last_motion_activity.and_then(|last_motion| {
                    let fallback_until = last_motion + MOTION_FALLBACK_WINDOW;
                    (now < fallback_until).then_some((now + MOTION_FALLBACK_INTERVAL).min(fallback_until))
                })
            };
            let deadline = match (base_deadline, fallback_deadline) {
                (Some(base), Some(fallback)) => Some(base.min(fallback)),
                (base, fallback) => base.or(fallback),
            };

            #[cfg(feature = "rtt_diag")]
            let motion_pending_at_arm = self.trackball.motion_pending();
            #[cfg(feature = "rtt_diag")]
            let motion_wait_started = Instant::now();
            let motion_or_deadline = async {
                match (self.trackball.motion_gpio(), deadline) {
                    (Some(gpio), Some(deadline)) => {
                        matches!(select(gpio.wait_for_low(), Timer::at(deadline)).await, Either::First(_))
                    }
                    (Some(gpio), None) => {
                        gpio.wait_for_low().await;
                        true
                    }
                    (None, Some(deadline)) => {
                        Timer::at(deadline).await;
                        false
                    }
                    (None, None) => core::future::pending::<bool>().await,
                }
            };
            let motion_woke = match select3(
                motion_or_deadline,
                module_settings::wait_for_module_selection_change(
                    self.device_id,
                    module_settings::ModuleSelection::Trackball,
                ),
                module_settings::wait_for_module_sleep_change(sleeping),
            )
            .await
            {
                Either3::First(motion_woke) => motion_woke,
                Either3::Second(_) => {
                    self.deactivate();
                    continue;
                }
                Either3::Third(next_sleeping) => {
                    if next_sleeping {
                        self.park_for_sleep();
                    } else {
                        self.resume_from_sleep();
                    }
                    continue;
                }
            };

            // Validate before consuming a motion burst. A VBUS transition can
            // happen while this task is waiting on MOTION; checking here keeps
            // deltas from a reset or partially reconnected sensor out of HID.
            if !sleeping && !self.verify_sensor_configuration_if_needed(Instant::now()).await {
                continue;
            }

            let fallback_poll =
                !motion_woke && fallback_deadline.is_some_and(|fallback_deadline| Instant::now() >= fallback_deadline);

            if motion_woke || fallback_poll {
                #[cfg(feature = "rtt_diag")]
                if motion_woke {
                    rmk::rtt_diag::record_motion_wake(
                        Instant::now().duration_since(motion_wait_started).as_micros() as u32,
                        motion_pending_at_arm,
                    );
                }
                let mut reads = 0usize;
                while reads < MAX_MOTION_READS_PER_WAKE
                    && (self.trackball.motion_pending() || (fallback_poll && reads == 0))
                {
                    reads += 1;
                    #[cfg(feature = "rtt_diag")]
                    let motion_gpio_low = self.trackball.motion_pending();
                    #[cfg(feature = "rtt_diag")]
                    let read_started = Instant::now();
                    match self.trackball.read_motion().await {
                        Ok(motion) => {
                            let now = Instant::now();
                            let native_dx = motion.dx;
                            let native_dy = motion.dy;
                            #[cfg(feature = "pmw_axes_600")]
                            let (output_dx, output_dy) = (native_dy, native_dx);
                            #[cfg(not(feature = "pmw_axes_600"))]
                            let (output_dx, output_dy) = (native_dx, native_dy);
                            #[cfg(all(feature = "rtt_diag", feature = "pmw_axes_600"))]
                            rmk::rtt_diag::record_pmw_axis_transform(native_dx, native_dy, output_dx, output_dy);
                            #[cfg(feature = "rtt_diag")]
                            rmk::rtt_diag::record_pmw_read(
                                output_dx,
                                output_dy,
                                now.duration_since(read_started).as_micros() as u32,
                                motion_gpio_low,
                            );
                            #[cfg(feature = "rtt_diag")]
                            if fallback_poll && reads == 1 {
                                rmk::rtt_diag::record_pmw_fallback(output_dx, output_dy);
                            }
                            if output_dx != 0 || output_dy != 0 {
                                self.last_motion_activity = Some(now);
                            }
                            if sleeping && self.acc_x == 0 && self.acc_y == 0 && (output_dx != 0 || output_dy != 0) {
                                self.sleep_motion_deadline = Some(now + SLEEP_MOTION_WINDOW);
                            }
                            let (next_x, dropped_x) = accumulate_motion(self.acc_x, output_dx);
                            let (next_y, dropped_y) = accumulate_motion(self.acc_y, output_dy);
                            self.acc_x = next_x;
                            self.acc_y = next_y;
                            #[cfg(feature = "rtt_diag")]
                            rmk::rtt_diag::record_pmw_accum_drop(dropped_x, dropped_y);
                            #[cfg(not(feature = "rtt_diag"))]
                            let _ = (dropped_x, dropped_y);
                            if self.has_reportable_motion(sleeping)
                                && now.duration_since(self.last_report) >= report_interval
                            {
                                self.send_accumulated_motion();
                                self.last_report = now;
                            }
                        }
                        Err(_) => {
                            #[cfg(feature = "rtt_diag")]
                            rmk::rtt_diag::record_pmw_error();
                            self.reject_sensor_configuration(Instant::now(), 3, None, true);
                            break;
                        }
                    }
                }
                if reads == MAX_MOTION_READS_PER_WAKE && self.trackball.motion_pending() {
                    // A stuck-low/noisy MOTION line must not monopolize the
                    // executor. The next pass resumes immediately if data is
                    // still pending.
                    Timer::after(Duration::from_micros(50)).await;
                }
            }

            if !self.ready {
                continue;
            }

            let now = Instant::now();
            if sleeping && self.sleep_motion_deadline.is_some_and(|deadline| now >= deadline) {
                if self.has_reportable_motion(true) {
                    self.send_accumulated_motion();
                    self.last_report = now;
                } else {
                    // A single +/-1 sample that was not followed by motion in
                    // the short confirmation window is settling noise.
                    self.acc_x = 0;
                    self.acc_y = 0;
                    self.sleep_motion_deadline = None;
                }
            } else if self.has_reportable_motion(sleeping) && now.duration_since(self.last_report) >= report_interval {
                self.send_accumulated_motion();
                self.last_report = now;
            }
        }
    }

    async fn verify_sensor_configuration_if_needed(&mut self, now: Instant) -> bool {
        let vbus_detect = crate::battery_nrf::usb_power_status().0;
        let vbus_changed = self.last_vbus_detect.is_some_and(|previous| previous != vbus_detect);
        if !vbus_changed && now < self.next_health_check {
            return true;
        }

        self.last_vbus_detect = Some(vbus_detect);
        self.next_health_check = now + HEALTH_CHECK_INTERVAL;
        let trigger = if vbus_changed { 1 } else { 0 };

        match self.trackball.configuration_health(self.current_cpi).await {
            Ok(health) if health.is_valid() => {
                #[cfg(feature = "pmw_axes_600_diag")]
                if vbus_changed {
                    defmt::info!(
                        "[PMW_HEALTH_V18] side={} trigger={} vbus={} valid=true pid={:#04x} obs={:#04x} perf={:#04x} res_step={:#04x} smart={:#04x}",
                        self.device_id,
                        trigger,
                        vbus_detect,
                        health.product_id,
                        health.observation1,
                        health.performance,
                        health.res_step,
                        health.smart_mode
                    );
                }
                let _ = health;
                true
            }
            Ok(health) => {
                self.reject_sensor_configuration(now, trigger, Some(health), true);
                false
            }
            Err(_) => {
                self.reject_sensor_configuration(now, trigger, None, true);
                false
            }
        }
    }

    fn reject_sensor_configuration(
        &mut self,
        now: Instant,
        trigger: u8,
        health: Option<Pmw3610Health>,
        immediate_retry: bool,
    ) {
        match health {
            Some(health) => {
                #[cfg(feature = "pmw_axes_600_diag")]
                defmt::warn!(
                    "[PMW_HEALTH_FAIL_V18] side={} trigger={} pid={:#04x} obs={:#04x} perf={:#04x}/{:#04x} res_step={:#04x}/{:#04x} smart={:#04x}/{:#04x}",
                    self.device_id,
                    trigger,
                    health.product_id,
                    health.observation1,
                    health.performance,
                    health.expected_performance,
                    health.res_step,
                    health.expected_res_step,
                    health.smart_mode,
                    health.expected_smart_mode
                );
                let _ = health;
            }
            None => {
                #[cfg(feature = "pmw_axes_600_diag")]
                defmt::warn!(
                    "[PMW_HEALTH_IO_FAIL_V18] side={} trigger={} cpi={}",
                    self.device_id,
                    trigger,
                    self.current_cpi
                );
            }
        }
        let _ = trigger;
        if !self.recovery_pending {
            self.recovery_count = self.recovery_count.saturating_add(1);
        }
        self.recovery_pending = true;
        self.mark_unavailable(now);
        if immediate_retry {
            self.next_probe = now;
        }
    }

    fn has_reportable_motion(&self, sleeping: bool) -> bool {
        if sleeping {
            self.acc_x.unsigned_abs() >= SLEEP_MOTION_THRESHOLD || self.acc_y.unsigned_abs() >= SLEEP_MOTION_THRESHOLD
        } else {
            self.acc_x != 0 || self.acc_y != 0
        }
    }

    fn park_for_sleep(&mut self) {
        // PMW3610 enters Rest3 on its own when force-awake is disabled. Stop
        // timer-driven SPI traffic and leave only the MOTION line armed.
        self.acc_x = 0;
        self.acc_y = 0;
        self.last_report = Instant::now();
        self.last_motion_activity = None;
        self.sleep_motion_deadline = None;
        self.next_health_check = Instant::MIN;
    }

    fn resume_from_sleep(&mut self) {
        // Discard a lone +/-1 sample retained while sleeping. Deliberate
        // accumulated motion has already been published to trigger the wake.
        if !self.has_reportable_motion(true) {
            self.acc_x = 0;
            self.acc_y = 0;
        }
        self.sleep_motion_deadline = None;
        self.sleep_started = None;
        if self.ready {
            self.next_health_check = Instant::now() + HEALTH_CHECK_INTERVAL;
        }
    }

    fn report_interval(&self) -> Duration {
        if cfg!(any(feature = "production_v22", feature = "pmw_axes_600_diag")) {
            if self.is_central {
                LOCAL_REPORT_INTERVAL
            } else {
                SPLIT_REPORT_INTERVAL
            }
        } else if self.is_central
            && matches!(
                rmk::state::current_connection_status().decide_active(),
                Some(rmk::types::connection::ConnectionType::Usb)
            )
        {
            LOCAL_REPORT_INTERVAL
        } else {
            LEGACY_TRANSPORT_REPORT_INTERVAL
        }
    }

    async fn apply_configured_cpi(&mut self) {
        let configured_cpi = configured_ball_cpi(self.device_id);
        if configured_cpi != self.current_cpi && self.trackball.set_resolution(configured_cpi).await.is_ok() {
            self.current_cpi = configured_cpi;
        }
    }

    fn mark_unavailable(&mut self, now: Instant) {
        self.ready = false;
        let unavailable_since = *self.unavailable_since.get_or_insert(now);
        let retry_interval = if now.duration_since(unavailable_since) < FAST_PROBE_WINDOW {
            FAST_PROBE_INTERVAL
        } else {
            SLOW_PROBE_INTERVAL
        };
        self.next_probe = now + retry_interval;
        self.next_health_check = Instant::MIN;
        self.acc_x = 0;
        self.acc_y = 0;
        self.last_motion_activity = None;
        self.sleep_motion_deadline = None;
    }

    fn deactivate(&mut self) {
        self.ready = false;
        self.acc_x = 0;
        self.acc_y = 0;
        self.last_report = Instant::MIN;
        self.last_motion_activity = None;
        self.sleep_motion_deadline = None;
        self.sleep_started = None;
        self.next_probe = Instant::MIN;
        self.next_health_check = Instant::MIN;
        self.unavailable_since = None;
        self.last_vbus_detect = None;
        self.recovery_pending = false;
    }

    fn send_accumulated_motion(&mut self) {
        if self.acc_x == 0 && self.acc_y == 0 {
            return;
        }

        let report_x = take_report_axis(self.is_central, self.acc_x);
        let report_y = take_report_axis(self.is_central, self.acc_y);
        self.acc_x -= report_x as i32;
        self.acc_y -= report_y as i32;
        if self.acc_x == 0 && self.acc_y == 0 {
            self.sleep_motion_deadline = None;
        }

        publish_event(PointingEvent {
            device_id: self.device_id,
            axes: [
                AxisEvent {
                    typ: AxisValType::Rel,
                    axis: Axis::X,
                    value: report_x,
                },
                AxisEvent {
                    typ: AxisValType::Rel,
                    axis: Axis::Y,
                    value: report_y,
                },
                AxisEvent {
                    typ: AxisValType::Rel,
                    axis: Axis::Z,
                    value: 0,
                },
            ],
        });
        #[cfg(feature = "rtt_diag")]
        rmk::rtt_diag::record_pmw_publish();
    }
}

struct NeverSub;
pub struct NeverEvent;

impl EventSubscriber for NeverSub {
    type Event = NeverEvent;

    async fn next_event(&mut self) -> NeverEvent {
        core::future::pending().await
    }
}

impl Runnable for Trackball {
    async fn run(&mut self) -> ! {
        self.run_loop().await
    }
}

impl Processor for Trackball {
    type Event = NeverEvent;

    fn subscriber() -> impl EventSubscriber<Event = NeverEvent> {
        NeverSub
    }

    async fn process(&mut self, _: NeverEvent) {}

    async fn process_loop(&mut self) -> ! {
        self.run().await
    }
}

/// Independent timer heartbeat for diagnostic builds. If both PMW reads and
/// this task become late, the executor was delayed; if only PMW/MOTION pauses,
/// the stall is inside the sensor path.
#[cfg(feature = "rtt_diag")]
pub struct RttSchedulerProbe;

#[cfg(feature = "rtt_diag")]
impl RttSchedulerProbe {
    pub const fn new() -> Self {
        Self
    }

    async fn run_loop(&mut self) -> ! {
        #[cfg(feature = "pmw_axes_600_diag")]
        defmt::info!(
            "[RIGHT_DIAG_V22] mode=split_radio_aligned_pacing cpi=600 hardware_swap=false software_swap=true report_us=15000 split_link_us=7500 windows_per_report=2 delta=i16_full accum_min=-32768 accum_max=32767 hid_us=7500 vector=preserve health_ms=1000 smart=on force_awake=off"
        );
        #[cfg(feature = "pmw_raw_600_diag")]
        defmt::info!("[RIGHT_DIAG_V13] mode=pmw_raw_sign cpi=600 smart=on force_awake=off");
        #[cfg(feature = "pmw_raw_1000_diag")]
        defmt::info!("[RIGHT_DIAG_V13] mode=pmw_raw_sign cpi=1000 smart=on force_awake=off");
        #[cfg(feature = "pmw_smart_off_diag")]
        defmt::info!("[RIGHT_DIAG_V12] mode=pmw_optics smart=off force_awake=off");
        #[cfg(all(
            not(any(
                feature = "pmw_raw_600_diag",
                feature = "pmw_raw_1000_diag",
                feature = "pmw_axes_600_diag"
            )),
            not(feature = "pmw_smart_off_diag"),
            feature = "pmw_force_awake_diag"
        ))]
        defmt::info!("[RIGHT_DIAG_V11] mode=pmw_optics smart=on force_awake=on");
        #[cfg(all(
            not(any(
                feature = "pmw_raw_600_diag",
                feature = "pmw_raw_1000_diag",
                feature = "pmw_axes_600_diag"
            )),
            not(feature = "pmw_smart_off_diag"),
            not(feature = "pmw_force_awake_diag")
        ))]
        defmt::info!("[RIGHT_DIAG_V10] mode=pmw_optics smart=on force_awake=off");
        let mut deadline = Instant::now() + SCHEDULER_PROBE_INTERVAL;
        loop {
            Timer::at(deadline).await;
            let now = Instant::now();
            rmk::rtt_diag::record_scheduler_tick(now.duration_since(deadline).as_micros() as u32);
            deadline = now + SCHEDULER_PROBE_INTERVAL;
        }
    }
}

#[cfg(feature = "rtt_diag")]
impl Runnable for RttSchedulerProbe {
    async fn run(&mut self) -> ! {
        self.run_loop().await
    }
}

#[cfg(feature = "rtt_diag")]
impl Processor for RttSchedulerProbe {
    type Event = NeverEvent;

    fn subscriber() -> impl EventSubscriber<Event = NeverEvent> {
        NeverSub
    }

    async fn process(&mut self, _: NeverEvent) {}

    async fn process_loop(&mut self) -> ! {
        self.run().await
    }
}

fn accumulate_motion(current: i32, delta: i16) -> (i32, u32) {
    let unbounded = current.saturating_add(i32::from(delta));
    let bounded = unbounded.clamp(MOTION_ACCUM_MIN, MOTION_ACCUM_MAX);
    (bounded, unbounded.abs_diff(bounded))
}

use embassy_nrf::twim::Twim;
use embassy_time::{Duration, Instant, Timer};
use rmk::core_traits::Runnable;
use rmk::event::{publish_event, Axis, AxisEvent, AxisValType, EventSubscriber, PointingEvent};
use rmk::processor::Processor;

use crate::module_settings;

const IQS5XX_ADDR: u8 = 0x74;
const REG_PRODUCT_NUMBER: u16 = 0x0000;
const REG_PREVIOUS_CYCLE_TIME: u16 = 0x000c;
const REG_BOTTOM_BETA: u16 = 0x0637;
const REG_FILTER_SETTINGS: u16 = 0x0632;
const REG_STATIONARY_THRESH: u16 = 0x0672;
const REG_SYSTEM_CONTROL_0: u16 = 0x0431;
const REG_SYSTEM_CONTROL_1: u16 = 0x0432;
const REG_REPORT_RATE_ACTIVE: u16 = 0x057a;
const REG_REPORT_RATE_IDLE_TOUCH: u16 = 0x057c;
const REG_REPORT_RATE_IDLE: u16 = 0x057e;
const REG_REPORT_RATE_LP1: u16 = 0x0580;
const REG_REPORT_RATE_LP2: u16 = 0x0582;
const REG_ACTIVE_MODE_TIMEOUT: u16 = 0x0584;
const REG_IDLE_TOUCH_MODE_TIMEOUT: u16 = 0x0585;
const REG_IDLE_MODE_TIMEOUT: u16 = 0x0586;
const REG_LP1_MODE_TIMEOUT: u16 = 0x0587;
const REG_SYSTEM_CONFIG_0: u16 = 0x058e;
const REG_SYSTEM_CONFIG_1: u16 = 0x058f;
const REG_XY_CONFIG_0: u16 = 0x0669;
const REG_SINGLE_FINGER_GESTURES: u16 = 0x06b7;
const REG_MULTI_FINGER_GESTURES: u16 = 0x06b8;
const REG_HOLD_TIME: u16 = 0x06bd;
const REG_SCROLL_INITIAL_DISTANCE: u16 = 0x06c8;
const REG_END_COMMS: u16 = 0xeeee;

const REPORT_RATE_ACTIVE_MS: u16 = 8;
const REPORT_RATE_IDLE_TOUCH_MS: u16 = 8;
const REPORT_RATE_IDLE_MS: u16 = 40;
const REPORT_RATE_LP1_MS: u16 = 160;
// LP2 keeps the LP1 cycle time: the extra current of scanning twice as often in
// the deepest mode is a few microamps, far below the radio's share of the power
// budget, while a 320 ms cycle costs up to 320 ms before a touch is noticed.
const REPORT_RATE_LP2_MS: u16 = 160;
const ACTIVE_MODE_TIMEOUT_SECS: u8 = 1;
const IDLE_TOUCH_MODE_TIMEOUT_SECS: u8 = 255;
const IDLE_MODE_TIMEOUT_SECS: u8 = 5;
const LP1_MODE_TIMEOUT_20_SECS: u8 = 1;
// K:04 does not route IQS5xx RDY. Forced I2C reads can report the cadence of
// the wake cycle, so the host must downshift from observed inactivity instead
// of feeding PREVIOUS_CYCLE_TIME back into its own polling schedule.
const TOUCH_ACTIVE_POLL_WINDOW: Duration = Duration::from_secs(1);
const TOUCH_IDLE_POLL_WINDOW: Duration = Duration::from_secs(6);
const TOUCH_LP1_POLL_WINDOW: Duration = Duration::from_secs(26);
const TOUCH_EVENT_BLOCK_LEN: usize = 10;
const TOUCH_FAST_PROBE_INTERVAL: Duration = Duration::from_millis(500);
const TOUCH_SLOW_PROBE_INTERVAL: Duration = Duration::from_secs(2);
const TOUCH_FAST_PROBE_WINDOW: Duration = Duration::from_secs(10);
const TOUCH_READ_FAILURE_REINIT_THRESHOLD: u8 = 4;
// Consecutive two-finger samples required before scroll is believed. Holding a
// finger still for 10-15 s lets the IQS5xx reference track towards it, and the
// contact patch can momentarily read as a second finger; a single such sample
// used to turn ordinary cursor motion into a scroll jump.
const TOUCH_SCROLL_CONFIRM_SAMPLES: u8 = 3;
const TOUCH_MOTION_ACCUM_LIMIT: i32 = (i8::MAX as i32) * 2;
// Keep sampling and report pacing aligned so continuous motion is emitted on
// every active sample instead of falling into an alternating 15/30 ms cadence.
const TOUCH_REPORT_INTERVAL: Duration = Duration::from_millis(8);
const SCROLL_DIVISOR: i16 = 8;
const BUTTON_LEFT: u8 = 1 << 0;
const BUTTON_RIGHT: u8 = 1 << 1;
const GESTURE_0_SINGLE_TAP: u8 = 1 << 0;
const GESTURE_0_PRESS_AND_HOLD: u8 = 1 << 1;
const GESTURE_1_TWO_FINGER_TAP: u8 = 1 << 0;
const GESTURE_1_SCROLL: u8 = 1 << 1;
const SYSTEM_INFO_0_SHOW_RESET: u8 = 1 << 7;
// System Info 0 also carries the charging (power) mode in its low three bits,
// plus the ATI status flags; see the datasheet section quoted in
// rmk/src/input_device/iqs5xx.rs.
const SYSTEM_INFO_0_CHARGING_MODE: u8 = 0b111;
const CHARGING_MODE_ACTIVE: u8 = 0b000;
const CHARGING_MODE_IDLE_TOUCH: u8 = 0b001;
const SYSTEM_INFO_0_ATI_ERROR: u8 = 1 << 3;
const SYSTEM_INFO_0_REATI_OCCURRED: u8 = 1 << 4;
const SYSTEM_INFO_1_TP_MOVEMENT: u8 = 1 << 0;
const SYSTEM_CONTROL_0_ACK_RESET: u8 = 1 << 7;
const FILTER_IIR: u8 = 1 << 0;
const FILTER_MAV: u8 = 1 << 1;
const FILTER_ALP_COUNT: u8 = 1 << 3;
const SYSTEM_CONTROL_1_WAKE: u8 = 0;

pub struct Touchpad {
    i2c: Twim<'static>,
    device_id: u8,
    side: u8,
    ready: bool,
    read_failures: u8,
    multi_finger_samples: u8,
    acc_x: i32,
    acc_y: i32,
    last_report: Instant,
    last_activity: Instant,
    next_probe: Instant,
    next_poll: Instant,
    poll_interval: Duration,
    unavailable_since: Option<Instant>,
}

impl Touchpad {
    pub fn new(device_id: u8, i2c: Twim<'static>) -> Self {
        Self {
            i2c,
            device_id,
            side: side_for_device_id(device_id),
            ready: false,
            read_failures: 0,
            multi_finger_samples: 0,
            acc_x: 0,
            acc_y: 0,
            last_report: Instant::MIN,
            last_activity: Instant::MIN,
            next_probe: Instant::MIN,
            next_poll: Instant::MIN,
            poll_interval: Duration::from_millis(REPORT_RATE_ACTIVE_MS as u64),
            unavailable_since: None,
        }
    }

    async fn run_loop(&mut self) -> ! {
        loop {
            let deadline = if self.ready { self.next_poll } else { self.next_probe };
            Timer::at(deadline).await;
            self.poll_once().await;
        }
    }

    async fn poll_once(&mut self) {
        if !self.ready {
            if !self.init().await {
                self.schedule_next_probe(Instant::now());
                return;
            }
            self.last_report = Instant::now();
        }

        let active_sample = match self.read_motion().await {
            TouchReadResult::Motion { x, y } => {
                self.read_failures = 0;
                let x = module_settings::scale_touch_delta(x, self.side);
                let y = module_settings::scale_touch_delta(y, self.side);
                self.acc_x = clamp_motion_accum(self.acc_x.saturating_add(x as i32));
                self.acc_y = clamp_motion_accum(self.acc_y.saturating_add(y as i32));
                true
            }
            TouchReadResult::Gesture { buttons } => {
                self.read_failures = 0;
                self.send_gesture(buttons);
                true
            }
            TouchReadResult::Scroll { h, v } => {
                self.read_failures = 0;
                self.send_scroll(h, v);
                true
            }
            TouchReadResult::Idle { touching } => {
                self.read_failures = 0;
                // A finger already on the pad counts as activity even before it
                // produces a delta, so the first stroke is sampled at the
                // active rate instead of waiting out the deep-sleep interval.
                touching
            }
            TouchReadResult::ReadFailed => {
                self.read_failures = self.read_failures.saturating_add(1);
                if self.read_failures >= TOUCH_READ_FAILURE_REINIT_THRESHOLD {
                    self.reset();
                    Timer::after(Duration::from_millis(50)).await;
                } else {
                    self.next_poll = Instant::now() + self.poll_interval;
                }
                return;
            }
        };

        let now = Instant::now();
        if active_sample {
            self.last_activity = now;
        }
        self.poll_interval = touch_poll_interval(now.duration_since(self.last_activity));
        self.next_poll = now + self.poll_interval;

        if now.duration_since(self.last_report) >= TOUCH_REPORT_INTERVAL {
            self.send_accumulated_motion();
            self.last_report = now;
        }
    }

    async fn init(&mut self) -> bool {
        let product = self.read_u16(REG_PRODUCT_NUMBER).await.unwrap_or(0);
        if product == 0 {
            return false;
        }

        let _ = self.write_u8(REG_SYSTEM_CONTROL_1, SYSTEM_CONTROL_1_WAKE).await;
        let _ = self.end_session().await;
        Timer::after(Duration::from_millis(100)).await;

        if self.read_u16(REG_PRODUCT_NUMBER).await.unwrap_or(0) == 0 {
            return false;
        }

        let mut ok = true;
        ok &= self.write_u16(REG_REPORT_RATE_ACTIVE, REPORT_RATE_ACTIVE_MS).await;
        ok &= self
            .write_u16(REG_REPORT_RATE_IDLE_TOUCH, REPORT_RATE_IDLE_TOUCH_MS)
            .await;
        ok &= self.write_u16(REG_REPORT_RATE_IDLE, REPORT_RATE_IDLE_MS).await;
        ok &= self.write_u16(REG_REPORT_RATE_LP1, REPORT_RATE_LP1_MS).await;
        ok &= self.write_u16(REG_REPORT_RATE_LP2, REPORT_RATE_LP2_MS).await;
        ok &= self.write_u8(REG_ACTIVE_MODE_TIMEOUT, ACTIVE_MODE_TIMEOUT_SECS).await;
        ok &= self
            .write_u8(REG_IDLE_TOUCH_MODE_TIMEOUT, IDLE_TOUCH_MODE_TIMEOUT_SECS)
            .await;
        ok &= self.write_u8(REG_IDLE_MODE_TIMEOUT, IDLE_MODE_TIMEOUT_SECS).await;
        ok &= self.write_u8(REG_LP1_MODE_TIMEOUT, LP1_MODE_TIMEOUT_20_SECS).await;
        ok &= self.write_u8(REG_SYSTEM_CONFIG_1, 0x46).await;
        ok &= self.write_u8(REG_BOTTOM_BETA, 5).await;
        ok &= self.write_u8(REG_STATIONARY_THRESH, 5).await;
        ok &= self
            .write_u8(REG_FILTER_SETTINGS, FILTER_IIR | FILTER_MAV | FILTER_ALP_COUNT)
            .await;
        ok &= self.write_u8(REG_XY_CONFIG_0, 0x05).await;
        ok &= self
            .write_u8(
                REG_SINGLE_FINGER_GESTURES,
                GESTURE_0_SINGLE_TAP | GESTURE_0_PRESS_AND_HOLD,
            )
            .await;
        ok &= self
            .write_u8(REG_MULTI_FINGER_GESTURES, GESTURE_1_TWO_FINGER_TAP | GESTURE_1_SCROLL)
            .await;
        ok &= self.write_u16(REG_HOLD_TIME, 0x012c).await;
        ok &= self.write_u16(REG_SCROLL_INITIAL_DISTANCE, 0x0001).await;
        ok &= self.write_u8(REG_SYSTEM_CONFIG_0, 0x60).await;
        ok &= self.end_session().await;

        if ok {
            self.ready = true;
            self.read_failures = 0;
            let now = Instant::now();
            self.next_probe = Instant::MIN;
            self.next_poll = now + Duration::from_millis(REPORT_RATE_ACTIVE_MS as u64);
            self.poll_interval = Duration::from_millis(REPORT_RATE_ACTIVE_MS as u64);
            self.last_activity = now;
            self.unavailable_since = None;
            self.acc_x = 0;
            self.acc_y = 0;
        }
        ok
    }

    async fn read_motion(&mut self) -> TouchReadResult {
        let mut data = [0u8; TOUCH_EVENT_BLOCK_LEN];
        if !self.read(REG_PREVIOUS_CYCLE_TIME, &mut data).await {
            return TouchReadResult::ReadFailed;
        }

        let previous_cycle_time = data[0];
        let gesture_0 = data[1];
        let gesture_1 = data[2];
        let system_info_0 = data[3];
        let system_info_1 = data[4];
        let number_of_fingers = data[5];
        let movement_or_scroll =
            (system_info_1 & SYSTEM_INFO_1_TP_MOVEMENT) != 0 || (gesture_1 & GESTURE_1_SCROLL) != 0;
        let x = if movement_or_scroll {
            i16::from_be_bytes([data[6], data[7]])
        } else {
            0
        };
        let y = if movement_or_scroll {
            i16::from_be_bytes([data[8], data[9]])
        } else {
            0
        };

        if (system_info_0 & SYSTEM_INFO_0_SHOW_RESET) != 0 {
            let _ = self.write_u8(REG_SYSTEM_CONTROL_0, SYSTEM_CONTROL_0_ACK_RESET).await;
            let _ = self.end_session().await;
            self.reset();
            return TouchReadResult::Idle { touching: false };
        }

        if !self.end_session().await {
            return TouchReadResult::ReadFailed;
        }

        // A real touch pulls the chip into active mode in the same cycle, so
        // finger counts and relative deltas reported from a low-power mode are
        // never a user's finger — they are whatever the transition left in the
        // registers. Same for the cycle in which the chip re-runs ATI: after a
        // long contact the reference drifts, and the recalibration lands as a
        // burst of bogus fingers and deltas on an empty pad, which used to
        // surface as a phantom scroll ten to twenty seconds after lifting off.
        let charging_mode = system_info_0 & SYSTEM_INFO_0_CHARGING_MODE;
        let scanning_for_touch = charging_mode == CHARGING_MODE_ACTIVE || charging_mode == CHARGING_MODE_IDLE_TOUCH;
        if !scanning_for_touch || (system_info_0 & (SYSTEM_INFO_0_ATI_ERROR | SYSTEM_INFO_0_REATI_OCCURRED)) != 0 {
            self.multi_finger_samples = 0;
            return TouchReadResult::Idle { touching: false };
        }

        self.multi_finger_samples = if number_of_fingers >= 2 {
            self.multi_finger_samples.saturating_add(1)
        } else {
            0
        };
        let two_fingers_settled = self.multi_finger_samples >= TOUCH_SCROLL_CONFIRM_SAMPLES;

        let gestures_enabled = module_settings::touch_gestures_enabled(self.side);

        if gestures_enabled && (gesture_0 & (GESTURE_0_SINGLE_TAP | GESTURE_0_PRESS_AND_HOLD)) != 0 {
            return TouchReadResult::Gesture { buttons: BUTTON_LEFT };
        }

        // Keep the IQS5xx/QMK guard that suppresses duplicate two-finger taps.
        if gestures_enabled && (gesture_1 & GESTURE_1_TWO_FINGER_TAP) != 0 && previous_cycle_time != 0 {
            return TouchReadResult::Gesture { buttons: BUTTON_RIGHT };
        }

        // Both the chip's scroll gesture and the bare two-finger fallback now
        // require the second finger to persist: a one-sample blip no longer
        // reroutes cursor motion into the wheel.
        if gestures_enabled && two_fingers_settled && ((gesture_1 & GESTURE_1_SCROLL) != 0 || x != 0 || y != 0) {
            return match scroll_delta(x, y) {
                Some((h, v)) => TouchReadResult::Scroll { h, v },
                None => TouchReadResult::Idle {
                    touching: number_of_fingers > 0,
                },
            };
        }

        if number_of_fingers != 1 || (x == 0 && y == 0) {
            return TouchReadResult::Idle {
                touching: number_of_fingers > 0,
            };
        }

        TouchReadResult::Motion {
            x,
            y: y.saturating_neg(),
        }
    }

    fn reset(&mut self) {
        let now = Instant::now();
        self.ready = false;
        self.read_failures = 0;
        self.acc_x = 0;
        self.acc_y = 0;
        self.schedule_next_probe(now);
    }

    fn schedule_next_probe(&mut self, now: Instant) {
        let unavailable_since = *self.unavailable_since.get_or_insert(now);
        let interval = if now.duration_since(unavailable_since) < TOUCH_FAST_PROBE_WINDOW {
            TOUCH_FAST_PROBE_INTERVAL
        } else {
            TOUCH_SLOW_PROBE_INTERVAL
        };
        self.next_probe = now + interval;
        self.next_poll = Instant::MIN;
    }

    fn send_gesture(&self, buttons: u8) {
        // Qube reserves relative Z on touch sources for a button pulse. Keeping
        // the signal inside PointingEvent preserves the existing split wire
        // format, so a new central remains compatible with an RC48 peripheral.
        publish_event(PointingEvent {
            device_id: self.device_id,
            axes: [
                relative_axis(Axis::X, 0),
                relative_axis(Axis::Y, 0),
                relative_axis(Axis::Z, i16::from(buttons)),
            ],
        });
    }

    fn send_scroll(&self, h: i16, v: i16) {
        publish_event(PointingEvent {
            device_id: self.device_id,
            axes: [
                relative_axis(Axis::H, h),
                relative_axis(Axis::V, v),
                relative_axis(Axis::Z, 0),
            ],
        });
    }

    fn send_accumulated_motion(&mut self) {
        if self.acc_x == 0 && self.acc_y == 0 {
            return;
        }

        let report_x = self.acc_x.clamp(i8::MIN as i32, i8::MAX as i32) as i16;
        let report_y = self.acc_y.clamp(i8::MIN as i32, i8::MAX as i32) as i16;
        self.acc_x -= report_x as i32;
        self.acc_y -= report_y as i32;

        publish_event(PointingEvent {
            device_id: self.device_id,
            axes: [
                relative_axis(Axis::X, report_x),
                relative_axis(Axis::Y, report_y),
                relative_axis(Axis::Z, 0),
            ],
        });
    }

    async fn read_u16(&mut self, reg: u16) -> Option<u16> {
        let mut buf = [0u8; 2];
        if self.read(reg, &mut buf).await {
            Some(u16::from_be_bytes(buf))
        } else {
            None
        }
    }

    async fn read(&mut self, reg: u16, out: &mut [u8]) -> bool {
        let address = reg.to_be_bytes();
        if self.i2c.write_read(IQS5XX_ADDR, &address, out).await.is_ok() {
            return true;
        }

        // IQS5xx deliberately NACKs the first address while waking from LP or
        // suspend. The datasheet requires a retry after at least 150 us.
        Timer::after(Duration::from_micros(200)).await;
        self.i2c.write_read(IQS5XX_ADDR, &address, out).await.is_ok()
    }

    async fn write_u8(&mut self, reg: u16, val: u8) -> bool {
        let r = reg.to_be_bytes();
        self.i2c.write(IQS5XX_ADDR, &[r[0], r[1], val]).await.is_ok()
    }

    async fn write_u16(&mut self, reg: u16, val: u16) -> bool {
        let r = reg.to_be_bytes();
        let v = val.to_be_bytes();
        self.i2c.write(IQS5XX_ADDR, &[r[0], r[1], v[0], v[1]]).await.is_ok()
    }

    async fn end_session(&mut self) -> bool {
        let r = REG_END_COMMS.to_be_bytes();
        self.i2c.write(IQS5XX_ADDR, &[r[0], r[1], 0]).await.is_ok()
    }
}

fn side_for_device_id(device_id: u8) -> u8 {
    match device_id {
        2 => 0,
        3 => 1,
        _ => device_id.min(1),
    }
}

// `Idle::touching` reports a finger resting on the pad: no delta this cycle,
// but the host must not treat it as inactivity.
enum TouchReadResult {
    Motion { x: i16, y: i16 },
    Gesture { buttons: u8 },
    Scroll { h: i16, v: i16 },
    Idle { touching: bool },
    ReadFailed,
}

fn relative_axis(axis: Axis, value: i16) -> AxisEvent {
    AxisEvent {
        typ: AxisValType::Rel,
        axis,
        value,
    }
}

fn scroll_delta(x: i16, y: i16) -> Option<(i16, i16)> {
    // IQS5xx and the original K:04 backend report only one scroll direction
    // per sample, with horizontal motion taking priority.
    if x != 0 {
        let h = x / SCROLL_DIVISOR;
        return (h != 0).then_some((h, 0));
    }
    if y != 0 {
        let v = y / SCROLL_DIVISOR;
        return (v != 0).then_some((0, v));
    }
    None
}

fn touch_poll_interval(idle_for: Duration) -> Duration {
    let interval_ms = if idle_for < TOUCH_ACTIVE_POLL_WINDOW {
        REPORT_RATE_ACTIVE_MS
    } else if idle_for < TOUCH_IDLE_POLL_WINDOW {
        REPORT_RATE_IDLE_MS
    } else if idle_for < TOUCH_LP1_POLL_WINDOW {
        REPORT_RATE_LP1_MS
    } else {
        REPORT_RATE_LP2_MS
    };
    Duration::from_millis(interval_ms as u64)
}

struct NeverSub;
pub struct NeverEvent;

impl EventSubscriber for NeverSub {
    type Event = NeverEvent;

    async fn next_event(&mut self) -> NeverEvent {
        core::future::pending().await
    }
}

impl Runnable for Touchpad {
    async fn run(&mut self) -> ! {
        self.run_loop().await
    }
}

impl Processor for Touchpad {
    type Event = NeverEvent;

    fn subscriber() -> impl EventSubscriber<Event = NeverEvent> {
        NeverSub
    }

    async fn process(&mut self, _: NeverEvent) {}

    async fn process_loop(&mut self) -> ! {
        self.run().await
    }
}

fn clamp_motion_accum(value: i32) -> i32 {
    value.clamp(-TOUCH_MOTION_ACCUM_LIMIT, TOUCH_MOTION_ACCUM_LIMIT)
}

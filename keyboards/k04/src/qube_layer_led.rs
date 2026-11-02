use embassy_nrf::pwm::{SequenceConfig, SequencePwm, SingleSequenceMode, SingleSequencer};
use embassy_time::{Duration, Instant, Timer};
use rmk::event::{
    BatteryStatusEvent, LayerChangeEvent, SleepStateEvent, SplitConnectionState, SplitConnectionStateEvent,
};
use rmk::macros::processor;
use rmk::processor::DeadlineProcessor;
use rmk::types::battery::BatteryStatus;

use crate::module_settings::{self, Rgb};

const LED_COUNT: usize = 1;
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const SPLIT_BLINK_PERIOD_MS: u64 = 360;
const SPLIT_BLINK_ON_MS: u64 = 180;
const CONNECTED_INDICATOR_MS: u64 = 1_000;
const CHARGED_BATTERY_MIN: u8 = 95;
const PWM_POLARITY_INVERTED: u16 = 0x8000;
const PWM_T0H: u16 = PWM_POLARITY_INVERTED | 6;
const PWM_T1H: u16 = PWM_POLARITY_INVERTED | 13;
const RESET_SLOTS: usize = 80;
const FRAME_WORDS: usize = LED_COUNT * 24 + RESET_SLOTS;

#[processor(
    subscribe = [
        LayerChangeEvent,
        SplitConnectionStateEvent,
        SleepStateEvent,
        BatteryStatusEvent
    ],
    deadline
)]
pub struct LayerLed {
    led: SequencePwm<'static>,
    side: u8,
    current_layer: Option<u8>,
    current_color: Option<Rgb>,
    split_state: SplitConnectionState,
    sleeping: bool,
    next_poll: Instant,
    phase_started: Instant,
    connected_until: Option<Instant>,
    latest_battery: Option<u8>,
    last_vbus_status: Option<(bool, bool)>,
}

impl LayerLed {
    pub fn new(led: SequencePwm<'static>, side: u8) -> Self {
        let now = Instant::now();
        Self {
            led,
            side,
            current_layer: Some(0),
            current_color: None,
            split_state: rmk::event::current_split_connection_state(),
            sleeping: false,
            next_poll: now,
            phase_started: now,
            connected_until: None,
            latest_battery: None,
            last_vbus_status: None,
        }
    }

    async fn on_layer_change_event(&mut self, event: LayerChangeEvent) {
        self.current_layer = Some(event.0);
        self.render(Instant::now()).await;
    }

    async fn on_split_connection_state_event(&mut self, event: SplitConnectionStateEvent) {
        self.apply_split_state(event.0, Instant::now(), 0);
        self.render(Instant::now()).await;
    }

    async fn on_sleep_state_event(&mut self, event: SleepStateEvent) {
        self.sleeping = event.0;
        self.next_poll = Instant::now();
        self.render(Instant::now()).await;
    }

    async fn on_battery_status_event(&mut self, event: BatteryStatusEvent) {
        match event.0 {
            BatteryStatus::Available { level, .. } => {
                self.latest_battery = level;
            }
            BatteryStatus::Unavailable => {
                self.latest_battery = None;
            }
        }
        self.render(Instant::now()).await;
    }

    async fn render(&mut self, now: Instant) {
        // Reconcile from the sticky snapshot in case Connected was published
        // before this processor installed its event subscription.
        self.apply_split_state(rmk::event::current_split_connection_state(), now, 1);

        let vbus_status = crate::battery_nrf::usb_power_status();
        if self.last_vbus_status != Some(vbus_status) {
            self.last_vbus_status = Some(vbus_status);
            defmt::info!(
                "[VBUS_Q_V17] side={} vbus={} outputrdy={} effective={}",
                self.side,
                vbus_status.0,
                vbus_status.1,
                vbus_status.0
            );
        }

        let (color, reason) = self.display_color(now, vbus_status);
        if self.current_color == Some(color) {
            return;
        }
        self.current_color = Some(color);
        defmt::info!(
            "[LED_Q_V15] side={} reason={} split={:?} rgb=({},{},{})",
            self.side,
            reason,
            self.split_state,
            color.r,
            color.g,
            color.b
        );
        send_color(&mut self.led, color).await;
    }

    fn apply_split_state(&mut self, state: SplitConnectionState, now: Instant, source: u8) {
        if self.split_state == state {
            return;
        }
        self.split_state = state;
        self.phase_started = now;
        self.connected_until =
            (state == SplitConnectionState::Connected).then(|| now + Duration::from_millis(CONNECTED_INDICATOR_MS));
        defmt::info!(
            "[SPLIT_LED_Q_V15] side={} source={} state={:?}",
            self.side,
            source,
            state
        );
    }

    fn display_color(&self, now: Instant, vbus_status: (bool, bool)) -> (Rgb, u8) {
        if self.sleeping || self.split_state == SplitConnectionState::Idle {
            return (color_off(), 0);
        }

        // Split acquisition is more important than charging status. USB power
        // must not turn the post-UF2 searching blink into a misleading solid
        // yellow indication.
        if self.split_state == SplitConnectionState::Searching {
            let elapsed_ms = now.duration_since(self.phase_started).as_millis();
            return if elapsed_ms % SPLIT_BLINK_PERIOD_MS < SPLIT_BLINK_ON_MS {
                (color_yellow(), 1)
            } else {
                (color_off(), 1)
            };
        }

        if self.connected_until.is_some_and(|until| now < until) {
            return (color_green(), 2);
        }

        if self.current_layer == Some(0) && vbus_status.0 && module_settings::charge_indicator_enabled() {
            return if self.latest_battery.is_some_and(|level| level >= CHARGED_BATTERY_MIN) {
                (color_green(), 3)
            } else {
                (color_yellow(), 3)
            };
        }

        (self.current_layer.map(color_for_layer).unwrap_or_else(color_off), 4)
    }
}

// The LED is dark while the half sleeps, so the poll that drives blinking and
// VBUS detection stops: a 10 ms tick would keep the MCU waking all night.
impl DeadlineProcessor for LayerLed {
    fn deadline(&self) -> Option<Instant> {
        (!self.sleeping).then_some(self.next_poll)
    }

    async fn on_deadline(&mut self) {
        let now = Instant::now();
        self.next_poll = now + POLL_INTERVAL;
        self.render(now).await;
    }
}

fn color_for_layer(layer: u8) -> Rgb {
    scale_color(module_settings::layer_color(layer))
}

fn color_yellow() -> Rgb {
    scale_color(Rgb { r: 255, g: 180, b: 0 })
}

fn color_green() -> Rgb {
    scale_color(Rgb { r: 0, g: 255, b: 0 })
}

fn color_off() -> Rgb {
    Rgb { r: 0, g: 0, b: 0 }
}

fn scale_color(color: Rgb) -> Rgb {
    Rgb {
        r: scale(color.r),
        g: scale(color.g),
        b: scale(color.b),
    }
}

fn scale(value: u8) -> u8 {
    ((u16::from(value) * u16::from(module_settings::led_brightness())) / 255).min(255) as u8
}

async fn send_color(led: &mut SequencePwm<'static>, color: Rgb) {
    let mut words = [0u16; FRAME_WORDS];
    let mut i = 0usize;

    for byte in [color.g, color.r, color.b] {
        for bit in (0..8).rev() {
            words[i] = if (byte & (1 << bit)) != 0 { PWM_T1H } else { PWM_T0H };
            i += 1;
        }
    }

    let sequencer = SingleSequencer::new(led, &words, SequenceConfig::default());
    let _ = sequencer.start(SingleSequenceMode::Times(1));
    Timer::after(Duration::from_micros(200)).await;
    sequencer.stop();
}

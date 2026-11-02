//! The layer LED of a half: one WS2812 on GPIO25, lit in the Entropy color of
//! the active layer.
//!
//! The K:04 `layer_led.rs` drives its LED with nRF PWM and mostly shows BLE,
//! battery and split pairing state, none of which a wired Phenom has.

use embassy_rp::interrupt::typelevel::{Binding, DMA_IRQ_0, PIO0_IRQ_0};
use embassy_rp::peripherals::{DMA_CH0, PIN_25, PIO0};
use embassy_rp::pio::{self, Pio};
use embassy_rp::pio_programs::ws2812::{Grb, PioWs2812, PioWs2812Program};
use embassy_rp::{dma, Peri};
use embassy_time::{Duration, Instant};
use rmk::event::{ConnectionStatusChangeEvent, LayerChangeEvent, PeripheralSettingsEvent};
use rmk::macros::processor;
use rmk::processor::DeadlineProcessor;
use rmk::types::connection::UsbState;
use smart_leds::RGB8;

use crate::module_settings;

#[processor(
    subscribe = [LayerChangeEvent, PeripheralSettingsEvent, ConnectionStatusChangeEvent],
    deadline
)]
pub struct LayerLed {
    led: PioWs2812<'static, PIO0, 0, 1, Grb>,
    layer: u8,
    /// When the base-layer color goes dark.
    dark_at: Option<Instant>,
    dark: bool,
    /// The host suspended USB, so the LED stays off.
    suspended: bool,
    shown: Option<RGB8>,
}

impl LayerLed {
    pub fn new<I>(pio: Peri<'static, PIO0>, dma: Peri<'static, DMA_CH0>, pin: Peri<'static, PIN_25>, irqs: I) -> Self
    where
        I: Binding<PIO0_IRQ_0, pio::InterruptHandler<PIO0>>
            + Binding<DMA_IRQ_0, dma::InterruptHandler<DMA_CH0>>
            + Copy
            + 'static,
    {
        let Pio { mut common, sm0, .. } = Pio::new(pio, irqs);
        let program = PioWs2812Program::new(&mut common);
        Self {
            led: PioWs2812::new(&mut common, sm0, dma, irqs, pin, &program),
            layer: 0,
            // The first deadline turns the LED off at once, so it never keeps
            // what it showed before the reset. Settings and layer events light it.
            dark_at: Some(Instant::now()),
            dark: false,
            suspended: false,
            shown: None,
        }
    }

    async fn on_layer_change_event(&mut self, event: LayerChangeEvent) {
        self.layer = event.0;
        self.arm_timeout();
        self.render().await;
    }

    async fn on_peripheral_settings_event(&mut self, event: PeripheralSettingsEvent) {
        // ModuleSettingsSync applies the same packet, but may run after us.
        module_settings::apply_settings_packet(&event.0);
        self.arm_timeout();
        self.render().await;
    }

    async fn on_connection_status_change_event(&mut self, event: ConnectionStatusChangeEvent) {
        self.suspended = event.0.usb == UsbState::Suspended;
        self.render().await;
    }

    /// A non-base layer stays lit while it is active. The base-layer color
    /// goes dark after the Entropy LED timeout, unless that is 0.
    fn arm_timeout(&mut self) {
        let timeout = module_settings::led_timeout_sec();
        self.dark = false;
        self.dark_at =
            (self.layer == 0 && timeout != 0).then(|| Instant::now() + Duration::from_secs(u64::from(timeout)));
    }

    async fn render(&mut self) {
        let color = if self.dark || self.suspended {
            RGB8::default()
        } else {
            let color = module_settings::layer_color(self.layer);
            let brightness = u16::from(module_settings::led_brightness());
            let scale = |value: u8| (u16::from(value) * brightness / 255) as u8;
            RGB8::new(scale(color.r), scale(color.g), scale(color.b))
        };
        if self.shown != Some(color) {
            self.shown = Some(color);
            self.led.write(&[color]).await;
        }
    }
}

impl DeadlineProcessor for LayerLed {
    fn deadline(&self) -> Option<Instant> {
        self.dark_at
    }

    async fn on_deadline(&mut self) {
        self.dark_at = None;
        self.dark = true;
        self.render().await;
    }
}

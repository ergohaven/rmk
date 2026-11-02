//! Ergohaven Qube dongle display (ST7789V over SPI).
//!
//! Full landscape **280×240** UI without a full-frame RGB565 buffer
//! (~134 KiB would OOM / kill HID on nRF52840).
//!
//! Strategy: **stripe multipass**
//! - Logical size: 280×240 (full panel after Deg90)
//! - Physical FB: 280×48×2 ≈ 27 KiB (< EasyDMA MAXCNT 65535, RAM-safe)
//! - Each redraw: for each stripe → clip-draw full UI → SPI that stripe
//!
//! Pinout (`qube.overlay`):
//! SPI2 SCK=P1.11 MOSI=P1.10 · CS=P1.13 · DC=P0.28 · RST=P0.03 · BL=P0.02
//!
//! The overlay uses SPI3, but with the nrf-sdc/mpsl 0.4 core SPIM3's ENABLE reads back 0
//! about 145 ms after boot and the panel stays dark. SPIM2 tops out at 8 MHz, the rate the
//! panel runs at anyway.

use core::fmt::Write as _;

use defmt::{info, warn};
use embassy_futures::select::{select, Either};
use embassy_nrf::gpio::{Level, Output, OutputDrive};
use embassy_nrf::peripherals::{P0_02, P0_03, P0_28, P1_10, P1_11, P1_13, SPI2};
use embassy_nrf::spim::{self, Spim};
use embassy_nrf::{interrupt, Peri};
use embassy_time::{Delay, Duration, Instant, Timer};
use embedded_graphics::mono_font::ascii::{FONT_10X20, FONT_6X10, FONT_8X13, FONT_9X15};
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{
    PrimitiveStyle, PrimitiveStyleBuilder, Rectangle, RoundedRectangle,
};
use embedded_graphics::text::{Alignment, Baseline, Text, TextStyleBuilder};
use embedded_hal_bus::spi::{ExclusiveDevice, NoDelay};
use lcd_async::interface::SpiInterface;
use lcd_async::models::ST7789;
use lcd_async::options::{ColorInversion, Orientation, Rotation};
use lcd_async::{Builder, Display as LcdDisplay};
use rmk::core_traits::Runnable;
use rmk::display::{DisplayRenderer, RenderContext};
use rmk::event::{
    BatteryStatusEvent, CentralConnectedEvent, ConnectionStatusChangeEvent, EventSubscriber,
    KeyboardEvent, LayerChangeEvent, LedIndicatorEvent, ModifierEvent, PeripheralBatteryEvent,
    PeripheralConnectedEvent, SleepStateEvent, SubscribableEvent, WpmUpdateEvent,
};
use rmk_types::battery::BatteryStatus;
use static_cell::StaticCell;

// --- Panel geometry ---------------------------------------------------------

pub const PANEL_NATIVE_W: usize = 240;
pub const PANEL_NATIVE_H: usize = 280;
const PANEL_ROTATION: Rotation = Rotation::Deg90;

/// Full landscape frame (after Deg90).
pub const SCREEN_W: usize = 280;
pub const SCREEN_H: usize = 240;

/// Stripe height: 280×48×2 = 26_880 B < 64 KiB EasyDMA, comfortable RAM.
const STRIPE_H: usize = 48;
const STRIPE_BYTES: usize = SCREEN_W * STRIPE_H * 2;

const BACKLIGHT_ACTIVE_HIGH: bool = true;
const SAFE_X: i32 = 18;
const SAFE_W: u32 = SCREEN_W as u32 - (SAFE_X as u32 * 2);
const MEDIA_VISIBLE_CHARS: usize = 26;
const MEDIA_GAP_CHARS: usize = 3;
// The longest artist and title with the separator between them.
const MEDIA_LABEL_LEN: usize = 2 * rmk::host_data::HOST_TEXT_LEN + 3;
const PANEL_RADIUS: u32 = 14;
const CHIP_RADIUS: u32 = 7;
const BAR_RADIUS: u32 = 5;

// Keep status-only redraws away from the split/USB critical path. A complete
// 280x240 RGB565 transfer at 8 MHz takes over 130 ms before render overhead;
// these bands match the fixed vertical zones rendered below.
const HEADER_DIRTY: DirtyRegion = DirtyRegion::range(12, 44);
const LAYER_DIRTY: DirtyRegion = DirtyRegion::range(50, 138);
const MODIFIER_DIRTY: DirtyRegion = DirtyRegion::range(144, 166);
const BATTERY_DIRTY: DirtyRegion = DirtyRegion::range(174, 224);
// Display state may be a few frames late, but cursor motion must never wait
// behind framebuffer rendering or SPI. Apply pending UI changes once the
// pointing stream has been quiet for this window.
const POINTING_REDRAW_QUIET_PERIOD: Duration = Duration::from_millis(100);
// Modifier chips are interactive feedback. Render the complete band once,
// then commit it in one asynchronous EasyDMA transfer so the screen never
// exposes intermediate stripe renders and the executor remains available.
const MODIFIER_REDRAW_MIN_INTERVAL: Duration = Duration::from_millis(16);

const COL_BG: Rgb565 = Rgb565::new(0, 2, 4);
const COL_FG: Rgb565 = Rgb565::new(29, 61, 30);
const COL_MUTED: Rgb565 = Rgb565::new(11, 24, 20);
const COL_LABEL: Rgb565 = Rgb565::new(16, 36, 28);
const COL_DIM: Rgb565 = Rgb565::new(5, 12, 14);
const COL_ACCENT: Rgb565 = Rgb565::new(3, 38, 31);
const COL_ACCENT_DIM: Rgb565 = Rgb565::new(1, 16, 18);
const COL_YELLOW: Rgb565 = Rgb565::new(31, 50, 0);
const COL_RED: Rgb565 = Rgb565::new(31, 5, 5);
const COL_BAR_BG: Rgb565 = Rgb565::new(2, 7, 9);
const COL_BAR_FG: Rgb565 = Rgb565::new(3, 42, 30);
const COL_PANEL: Rgb565 = Rgb565::new(2, 6, 9);
const COL_PANEL_HI: Rgb565 = Rgb565::new(3, 9, 13);
const COL_BORDER: Rgb565 = Rgb565::new(5, 13, 16);
const COL_BORDER_DIM: Rgb565 = Rgb565::new(3, 8, 11);

type SpiDev = ExclusiveDevice<Spim<'static>, Output<'static>, NoDelay>;
type Di = SpiInterface<SpiDev, Output<'static>>;
type Panel = LcdDisplay<Di, ST7789, Output<'static>>;

#[derive(Clone, Copy)]
enum DirtyRegion {
    Full,
    Range { y0: u16, y1: u16 },
}

impl DirtyRegion {
    const fn range(y0: u16, y1: u16) -> Self {
        Self::Range { y0, y1 }
    }

    fn union(self, other: Self) -> Self {
        match (self, other) {
            (Self::Full, _) | (_, Self::Full) => Self::Full,
            (Self::Range { y0: a0, y1: a1 }, Self::Range { y0: b0, y1: b1 }) => Self::Range {
                y0: a0.min(b0),
                y1: a1.max(b1),
            },
        }
    }
}

// --- Stripe framebuffer (clip window into full screen) ----------------------

struct StripeLcd {
    display: Panel,
    buffer: &'static mut [u8; STRIPE_BYTES],
    /// Top of the active stripe in full-screen coordinates.
    band_y: u16,
    /// Height of the active stripe (≤ STRIPE_H), last stripe may be shorter.
    band_h: u16,
}

impl StripeLcd {
    fn set_band(&mut self, y: u16, h: u16) {
        self.band_y = y;
        self.band_h = h.min(STRIPE_H as u16);
    }

    fn clear_stripe(&mut self, color: Rgb565) {
        let c = color.into_storage().to_be_bytes();
        let bytes = SCREEN_W * self.band_h as usize * 2;
        for pix in self.buffer[..bytes].chunks_exact_mut(2) {
            pix[0] = c[0];
            pix[1] = c[1];
        }
    }

    fn put_pixel(&mut self, x: i32, y: i32, color: Rgb565) {
        if x < 0 || y < 0 {
            return;
        }
        let x = x as u32;
        let y = y as u32;
        if x >= SCREEN_W as u32 {
            return;
        }
        let by = self.band_y as u32;
        let bh = self.band_h as u32;
        if y < by || y >= by + bh {
            return;
        }
        let ly = (y - by) as usize;
        let lx = x as usize;
        let off = (ly * SCREEN_W + lx) * 2;
        if off + 1 >= self.buffer.len() {
            return;
        }
        let c = color.into_storage().to_be_bytes();
        self.buffer[off] = c[0];
        self.buffer[off + 1] = c[1];
    }

    async fn flush_band(&mut self) {
        let w = SCREEN_W as u16;
        let h = self.band_h;
        let y = self.band_y;
        // Only send used rows (last stripe may be shorter).
        let bytes = (SCREEN_W * h as usize) * 2;
        let slice = &self.buffer[..bytes];
        let _ = self.display.show_raw_data(0, y, w, h, slice).await;
    }
}

impl OriginDimensions for StripeLcd {
    fn size(&self) -> Size {
        Size::new(SCREEN_W as u32, SCREEN_H as u32)
    }
}

impl DrawTarget for StripeLcd {
    type Color = Rgb565;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Rgb565>>,
    {
        for Pixel(p, col) in pixels {
            self.put_pixel(p.x, p.y, col);
        }
        Ok(())
    }

    fn fill_solid(&mut self, area: &Rectangle, color: Rgb565) -> Result<(), Self::Error> {
        let by = self.band_y as i32;
        let bh = self.band_h as i32;
        let band = Rectangle::new(Point::new(0, by), Size::new(SCREEN_W as u32, bh as u32));
        let isect = area.intersection(&band);
        if isect.is_zero_sized() {
            return Ok(());
        }
        // Fast path: full-width clear of the stripe
        if isect.top_left.x == 0
            && isect.size.width == SCREEN_W as u32
            && isect.top_left.y == by
            && isect.size.height == bh as u32
        {
            self.clear_stripe(color);
            return Ok(());
        }
        let x0 = isect.top_left.x;
        let y0 = isect.top_left.y;
        let x1 = x0 + isect.size.width as i32;
        let y1 = y0 + isect.size.height as i32;
        for y in y0..y1 {
            for x in x0..x1 {
                self.put_pixel(x, y, color);
            }
        }
        Ok(())
    }

    fn clear(&mut self, color: Rgb565) -> Result<(), Self::Error> {
        // Only clear the active stripe (multipass re-renders full UI per band).
        self.clear_stripe(color);
        Ok(())
    }
}

// --- Lazy init --------------------------------------------------------------

struct PendingPins {
    spi: Peri<'static, SPI2>,
    sck: Peri<'static, P1_11>,
    mosi: Peri<'static, P1_10>,
    cs: Peri<'static, P1_13>,
    dc: Peri<'static, P0_28>,
    rst: Peri<'static, P0_03>,
}

enum LcdState {
    Pending(PendingPins),
    Active(StripeLcd),
    Failed,
}

pub struct LazyQubeLcd<I> {
    state: LcdState,
    irq: I,
}

impl<I> LazyQubeLcd<I>
where
    I: interrupt::typelevel::Binding<
            <SPI2 as spim::Instance>::Interrupt,
            spim::InterruptHandler<SPI2>,
        > + Copy
        + 'static,
{
    async fn ensure_init(&mut self) {
        let pins = match core::mem::replace(&mut self.state, LcdState::Failed) {
            LcdState::Pending(p) => p,
            LcdState::Active(d) => {
                self.state = LcdState::Active(d);
                return;
            }
            LcdState::Failed => return,
        };
        match try_init_lcd(pins, self.irq).await {
            Some(lcd) => {
                info!("ST7789 ready (full-screen stripe mode)");
                self.state = LcdState::Active(lcd);
            }
            None => {
                warn!("ST7789 init failed — HID keeps running");
                self.state = LcdState::Failed;
            }
        }
    }

    /// Redraw the requested vertical region via stripe multipass.
    async fn present_dirty(&mut self, renderer: &mut QubeStatusRenderer, ctx: &RenderContext, dirty: DirtyRegion) {
        self.ensure_init().await;
        let LcdState::Active(lcd) = &mut self.state else {
            return;
        };

        let (y0, y1) = match dirty {
            DirtyRegion::Full => (0, SCREEN_H as u16),
            DirtyRegion::Range { y0, y1 } => (y0.min(SCREEN_H as u16), y1.min(SCREEN_H as u16)),
        };

        let mut y = y0;
        while y < y1 {
            let remaining = y1.saturating_sub(y);
            let h = remaining.min(STRIPE_H as u16);
            lcd.set_band(y, h);
            lcd.clear_stripe(COL_BG);
            // Re-run full UI; DrawTarget keeps only this stripe's pixels.
            renderer.render(ctx, lcd);
            lcd.flush_band().await;
            y = y.saturating_add(h);
        }
    }

    /// Render interactive modifier feedback once before touching the panel.
    async fn present_modifiers(&mut self, renderer: &QubeStatusRenderer, ctx: &RenderContext) {
        self.ensure_init().await;
        let LcdState::Active(lcd) = &mut self.state else {
            return;
        };
        let DirtyRegion::Range { y0, y1 } = MODIFIER_DIRTY else {
            return;
        };

        lcd.set_band(y0, y1.saturating_sub(y0));
        lcd.clear_stripe(COL_BG);
        renderer.render_modifiers(ctx, lcd);
        lcd.flush_band().await;
    }
}

impl<I> OriginDimensions for LazyQubeLcd<I> {
    fn size(&self) -> Size {
        Size::new(SCREEN_W as u32, SCREEN_H as u32)
    }
}

// DrawTarget on LazyQubeLcd only needed if something draws before present;
// multipass uses StripeLcd directly via present().

async fn try_init_lcd<I>(pins: PendingPins, irq: I) -> Option<StripeLcd>
where
    I: interrupt::typelevel::Binding<
            <SPI2 as spim::Instance>::Interrupt,
            spim::InterruptHandler<SPI2>,
        > + Copy
        + 'static,
{
    let mut spi_cfg = spim::Config::default();
    spi_cfg.frequency = spim::Frequency::M8;
    let spim = Spim::new_txonly(pins.spi, irq, pins.sck, pins.mosi, spi_cfg);

    let cs = Output::new(pins.cs, Level::High, OutputDrive::Standard);
    let dc = Output::new(pins.dc, Level::Low, OutputDrive::Standard);
    let rst = Output::new(pins.rst, Level::High, OutputDrive::Standard);

    let spi_dev = ExclusiveDevice::new_no_delay(spim, cs).ok()?;
    let di = SpiInterface::new(spi_dev, dc);

    let mut delay = Delay;
    let display = Builder::new(ST7789, di)
        .reset_pin(rst)
        .display_size(PANEL_NATIVE_W as u16, PANEL_NATIVE_H as u16)
        .display_offset(0, 20)
        .invert_colors(ColorInversion::Inverted)
        .orientation(Orientation::new().rotate(PANEL_ROTATION))
        .init(&mut delay)
        .await
        .ok()?;

    static FB: StaticCell<[u8; STRIPE_BYTES]> = StaticCell::new();
    let buffer = FB.init([0; STRIPE_BYTES]);
    Some(StripeLcd {
        display,
        buffer,
        band_y: 0,
        band_h: STRIPE_H as u16,
    })
}

// --- Dongle screen processor (own event loop + multipass present) -----------

pub struct DongleScreen<I>
where
    I: interrupt::typelevel::Binding<
            <SPI2 as spim::Instance>::Interrupt,
            spim::InterruptHandler<SPI2>,
        > + Copy
        + 'static,
{
    lcd: LazyQubeLcd<I>,
    renderer: QubeStatusRenderer,
    ctx: RenderContext,
    last_host_data: rmk::host_data::HostData,
    last_layer_names_version: u8,
    last_render: Instant,
    last_modifier_render: Instant,
    pending: bool,
    modifier_pending: bool,
    dirty: DirtyRegion,
    min_interval: Duration,
}

pub fn create_processor<I>(
    spi: Peri<'static, SPI2>,
    sck: Peri<'static, P1_11>,
    mosi: Peri<'static, P1_10>,
    cs: Peri<'static, P1_13>,
    dc: Peri<'static, P0_28>,
    rst: Peri<'static, P0_03>,
    bl: Peri<'static, P0_02>,
    irq: I,
) -> DongleScreen<I>
where
    I: interrupt::typelevel::Binding<
            <SPI2 as spim::Instance>::Interrupt,
            spim::InterruptHandler<SPI2>,
        > + Copy
        + 'static,
{
    let level = if BACKLIGHT_ACTIVE_HIGH {
        Level::High
    } else {
        Level::Low
    };
    static BL: StaticCell<Output<'static>> = StaticCell::new();
    let _ = BL.init(Output::new(bl, level, OutputDrive::Standard));
    let host_data = rmk::host_data::snapshot();

    DongleScreen {
        lcd: LazyQubeLcd {
            state: LcdState::Pending(PendingPins {
                spi,
                sck,
                mosi,
                cs,
                dc,
                rst,
            }),
            irq,
        },
        renderer: QubeStatusRenderer {
            host_data: host_data.clone(),
        },
        ctx: RenderContext::default(),
        last_host_data: host_data,
        last_layer_names_version: crate::layer_names::version(),
        last_render: Instant::from_ticks(0),
        last_modifier_render: Instant::from_ticks(0),
        pending: true,
        modifier_pending: false,
        dirty: DirtyRegion::Full,
        min_interval: Duration::from_millis(80),
    }
}

impl<I> DongleScreen<I>
where
    I: interrupt::typelevel::Binding<
            <SPI2 as spim::Instance>::Interrupt,
            spim::InterruptHandler<SPI2>,
        > + Copy
        + 'static,
{
    fn redraw_wait(&self) -> Duration {
        let rate_limit_wait = self
            .min_interval
            .checked_sub(self.last_render.elapsed())
            .unwrap_or(Duration::MIN);
        let pointing_wait = rmk::split::ble::central::pointing_quiet_period_remaining(
            POINTING_REDRAW_QUIET_PERIOD,
        );
        if pointing_wait > rate_limit_wait {
            pointing_wait
        } else {
            rate_limit_wait
        }
    }

    fn modifier_redraw_wait(&self) -> Duration {
        MODIFIER_REDRAW_MIN_INTERVAL
            .checked_sub(self.last_modifier_render.elapsed())
            .unwrap_or(Duration::MIN)
    }

    fn next_redraw_wait(&self) -> Option<Duration> {
        match (self.pending, self.modifier_pending) {
            (true, true) => Some(self.redraw_wait().min(self.modifier_redraw_wait())),
            (true, false) => Some(self.redraw_wait()),
            (false, true) => Some(self.modifier_redraw_wait()),
            (false, false) => None,
        }
    }

    async fn redraw(&mut self) {
        self.sync_host_data();
        self.sync_layer_names();
        if self.redraw_wait() != Duration::MIN {
            self.pending = true;
            return;
        }
        self.lcd.present_dirty(&mut self.renderer, &self.ctx, self.dirty).await;
        self.ctx.key_press_latch = false;
        self.pending = false;
        self.dirty = DirtyRegion::Full;
        self.last_render = Instant::now();
    }

    async fn redraw_modifiers(&mut self) {
        if self.modifier_redraw_wait() != Duration::MIN {
            return;
        }
        self.lcd
            .present_modifiers(&self.renderer, &self.ctx)
            .await;
        self.modifier_pending = false;
        self.last_modifier_render = Instant::now();
    }

    fn request_redraw(&mut self) {
        self.pending = true;
        self.dirty = DirtyRegion::Full;
    }

    fn request_redraw_region(&mut self, dirty: DirtyRegion) {
        self.dirty = if self.pending { self.dirty.union(dirty) } else { dirty };
        self.pending = true;
    }

    fn request_modifier_redraw(&mut self) {
        self.modifier_pending = true;
    }

    fn sync_host_data(&mut self) {
        let host_data = rmk::host_data::snapshot();
        if host_data != self.last_host_data {
            self.last_host_data = host_data.clone();
            self.renderer.host_data = host_data;
            self.request_redraw_region(HEADER_DIRTY);
        }
    }

    fn sync_layer_names(&mut self) {
        let version = crate::layer_names::version();
        if version != self.last_layer_names_version {
            self.last_layer_names_version = version;
            self.request_redraw_region(LAYER_DIRTY);
        }
    }
}


impl<I> Runnable for DongleScreen<I>
where
    I: interrupt::typelevel::Binding<
            <SPI2 as spim::Instance>::Interrupt,
            spim::InterruptHandler<SPI2>,
        > + Copy
        + 'static,
{
    async fn run(&mut self) -> ! {
        self.pending = true;
        self.sync_host_data();
        self.redraw().await;

        let mut layer_sub = LayerChangeEvent::subscriber();
        let mut wpm_sub = WpmUpdateEvent::subscriber();
        let mut led_sub = LedIndicatorEvent::subscriber();
        let mut mod_sub = ModifierEvent::subscriber();
        let mut key_sub = KeyboardEvent::subscriber();
        let mut sleep_sub = SleepStateEvent::subscriber();
        let mut bat_sub = BatteryStatusEvent::subscriber();
        let mut conn_sub = ConnectionStatusChangeEvent::subscriber();
        let mut peri_conn_sub = PeripheralConnectedEvent::subscriber();
        let mut peri_bat_sub = PeripheralBatteryEvent::subscriber();
        let mut central_sub = CentralConnectedEvent::subscriber();

        loop {
            // Wait for at least one event (or deferred redraw timer).
            if let Some(wait) = self.next_redraw_wait() {
                match select(
                    Timer::after(wait),
                    Self::next_any_or_host_tick(
                        &mut layer_sub,
                        &mut wpm_sub,
                        &mut led_sub,
                        &mut mod_sub,
                        &mut key_sub,
                        &mut sleep_sub,
                        &mut bat_sub,
                        &mut conn_sub,
                        &mut peri_conn_sub,
                        &mut peri_bat_sub,
                        &mut central_sub,
                    ),
                )
                .await
                {
                    Either::First(_) => {}
                    Either::Second(ev) => {
                        self.apply(ev);
                    }
                }
            } else {
                let ev = Self::next_any_or_host_tick(
                    &mut layer_sub,
                    &mut wpm_sub,
                    &mut led_sub,
                    &mut mod_sub,
                    &mut key_sub,
                    &mut sleep_sub,
                    &mut bat_sub,
                    &mut conn_sub,
                    &mut peri_conn_sub,
                    &mut peri_bat_sub,
                    &mut central_sub,
                )
                .await;
                self.apply(ev);
            }

            // Coalesce a burst of events that arrived during the previous
            // multipass present (layer MO + OSM mods, etc.) before redrawing.
            for _ in 0..16 {
                match select(
                    Timer::after(Duration::from_millis(0)),
                    Self::next_any(
                        &mut layer_sub,
                        &mut wpm_sub,
                        &mut led_sub,
                        &mut mod_sub,
                        &mut key_sub,
                        &mut sleep_sub,
                        &mut bat_sub,
                        &mut conn_sub,
                        &mut peri_conn_sub,
                        &mut peri_bat_sub,
                        &mut central_sub,
                    ),
                )
                .await
                {
                    Either::First(_) => break,
                    Either::Second(ev) => self.apply(ev),
                }
            }

            if self.modifier_pending {
                self.redraw_modifiers().await;
            }
            if self.pending {
                self.redraw().await;
            }
        }
    }
}

/// Unified UI event for the dongle screen loop.
enum UiEv {
    Layer(LayerChangeEvent),
    Wpm(WpmUpdateEvent),
    Led(LedIndicatorEvent),
    Mod(ModifierEvent),
    Key(KeyboardEvent),
    Sleep(SleepStateEvent),
    Bat(BatteryStatusEvent),
    Conn(ConnectionStatusChangeEvent),
    PeriConn(PeripheralConnectedEvent),
    PeriBat(PeripheralBatteryEvent),
    Central(CentralConnectedEvent),
    HostDataTick,
}

impl<I> DongleScreen<I>
where
    I: interrupt::typelevel::Binding<
            <SPI2 as spim::Instance>::Interrupt,
            spim::InterruptHandler<SPI2>,
        > + Copy
        + 'static,
{
    async fn next_any_or_host_tick(
        layer: &mut impl EventSubscriber<Event = LayerChangeEvent>,
        wpm: &mut impl EventSubscriber<Event = WpmUpdateEvent>,
        led: &mut impl EventSubscriber<Event = LedIndicatorEvent>,
        mods: &mut impl EventSubscriber<Event = ModifierEvent>,
        key: &mut impl EventSubscriber<Event = KeyboardEvent>,
        sleep: &mut impl EventSubscriber<Event = SleepStateEvent>,
        bat: &mut impl EventSubscriber<Event = BatteryStatusEvent>,
        conn: &mut impl EventSubscriber<Event = ConnectionStatusChangeEvent>,
        peri_conn: &mut impl EventSubscriber<Event = PeripheralConnectedEvent>,
        peri_bat: &mut impl EventSubscriber<Event = PeripheralBatteryEvent>,
        central: &mut impl EventSubscriber<Event = CentralConnectedEvent>,
    ) -> UiEv {
        match select(
            Timer::after(Duration::from_millis(250)),
            Self::next_any(
                layer, wpm, led, mods, key, sleep, bat, conn, peri_conn, peri_bat, central,
            ),
        )
        .await
        {
            Either::First(_) => UiEv::HostDataTick,
            Either::Second(ev) => ev,
        }
    }

    async fn next_any(
        layer: &mut impl EventSubscriber<Event = LayerChangeEvent>,
        wpm: &mut impl EventSubscriber<Event = WpmUpdateEvent>,
        led: &mut impl EventSubscriber<Event = LedIndicatorEvent>,
        mods: &mut impl EventSubscriber<Event = ModifierEvent>,
        key: &mut impl EventSubscriber<Event = KeyboardEvent>,
        sleep: &mut impl EventSubscriber<Event = SleepStateEvent>,
        bat: &mut impl EventSubscriber<Event = BatteryStatusEvent>,
        conn: &mut impl EventSubscriber<Event = ConnectionStatusChangeEvent>,
        peri_conn: &mut impl EventSubscriber<Event = PeripheralConnectedEvent>,
        peri_bat: &mut impl EventSubscriber<Event = PeripheralBatteryEvent>,
        central: &mut impl EventSubscriber<Event = CentralConnectedEvent>,
    ) -> UiEv {
        // Nested select — a bit verbose but no heap / macro dependency.
        // Prefer input events; depth is fine for status UI.
        use embassy_futures::select::{select, select3, Either, Either3};

        match select3(
            select3(layer.next_event(), wpm.next_event(), led.next_event()),
            select3(mods.next_event(), key.next_event(), sleep.next_event()),
            select3(
                select(bat.next_event(), conn.next_event()),
                select(peri_conn.next_event(), peri_bat.next_event()),
                central.next_event(),
            ),
        )
        .await
        {
            Either3::First(Either3::First(e)) => UiEv::Layer(e),
            Either3::First(Either3::Second(e)) => UiEv::Wpm(e),
            Either3::First(Either3::Third(e)) => UiEv::Led(e),
            Either3::Second(Either3::First(e)) => UiEv::Mod(e),
            Either3::Second(Either3::Second(e)) => UiEv::Key(e),
            Either3::Second(Either3::Third(e)) => UiEv::Sleep(e),
            Either3::Third(Either3::First(Either::First(e))) => UiEv::Bat(e),
            Either3::Third(Either3::First(Either::Second(e))) => UiEv::Conn(e),
            Either3::Third(Either3::Second(Either::First(e))) => UiEv::PeriConn(e),
            Either3::Third(Either3::Second(Either::Second(e))) => UiEv::PeriBat(e),
            Either3::Third(Either3::Third(e)) => UiEv::Central(e),
        }
    }

    fn apply(&mut self, ev: UiEv) {
        // Keyboard matrix floods KeyboardEvent; UI doesn't show individual
        // keys — skip redraw for those so multipass can keep up with layer/mod.
        let mut need_redraw = true;
        match ev {
            UiEv::Layer(e) => {
                self.ctx.layer = e.0;
                self.request_redraw_region(LAYER_DIRTY);
                need_redraw = false;
            }
            UiEv::Wpm(e) => {
                self.ctx.wpm = e.0;
                need_redraw = false;
            }
            UiEv::Led(e) => {
                self.ctx.caps_lock = e.0.caps_lock();
                self.ctx.num_lock = e.0.num_lock();
                self.request_modifier_redraw();
                need_redraw = false;
            }
            UiEv::Mod(e) => {
                self.ctx.modifiers = e.modifier;
                self.request_modifier_redraw();
                need_redraw = false;
            }
            UiEv::Key(e) => {
                self.ctx.key_pressed = e.pressed;
                if e.pressed {
                    self.ctx.key_press_latch = true;
                }
                need_redraw = false;
            }
            UiEv::Sleep(e) => self.ctx.sleeping = e.0,
            UiEv::Bat(e) => {
                self.ctx.battery = e;
                self.request_redraw_region(BATTERY_DIRTY);
                need_redraw = false;
            }
            UiEv::Conn(e) => self.ctx.ble_status = e.0.ble,
            UiEv::PeriConn(e) => {
                if let Some(slot) = self.ctx.peripherals_connected.get_mut(e.id) {
                    *slot = e.connected;
                }
            }
            UiEv::PeriBat(e) => {
                if let Some(slot) = self.ctx.peripheral_batteries.get_mut(e.id) {
                    *slot = e.state;
                }
                self.request_redraw_region(BATTERY_DIRTY);
                need_redraw = false;
            }
            UiEv::Central(e) => self.ctx.central_connected = e.connected,
            UiEv::HostDataTick => {
                self.sync_host_data();
                self.sync_layer_names();
                if self.renderer.media_needs_marquee() {
                    self.request_redraw_region(HEADER_DIRTY);
                }
                need_redraw = false;
            }
        }
        if need_redraw {
            self.request_redraw();
        }
    }
}

// Silence unused DisplayDriver import path if needed — keep for future.

// --- Full-screen UI ---------------------------------------------------------
//
// Fixed vertical zones (280x240) so nothing overlaps:
//   14..42   compact header
//   52..136  layer panel
//   146..164 modifier state
//   176..222 battery cards

pub struct QubeStatusRenderer {
    host_data: rmk::host_data::HostData,
}

impl QubeStatusRenderer {
    fn media_needs_marquee(&self) -> bool {
        let mut media: heapless::String<MEDIA_LABEL_LEN> = heapless::String::new();
        push_media_label(&mut media, &self.host_data);
        media.len() > MEDIA_VISIBLE_CHARS
    }

    fn render_modifiers<D: DrawTarget<Color = Rgb565>>(&self, ctx: &RenderContext, display: &mut D) {
        draw_chip(display, 30, 146, 38, "CAPS", ctx.caps_lock);
        draw_chip(
            display,
            76,
            146,
            38,
            "CTRL",
            ctx.modifiers.left_ctrl() || ctx.modifiers.right_ctrl(),
        );
        draw_chip(
            display,
            122,
            146,
            46,
            "SHIFT",
            ctx.modifiers.left_shift() || ctx.modifiers.right_shift(),
        );
        draw_chip(
            display,
            176,
            146,
            34,
            "ALT",
            ctx.modifiers.left_alt() || ctx.modifiers.right_alt(),
        );
        draw_chip(
            display,
            218,
            146,
            34,
            "GUI",
            ctx.modifiers.left_gui() || ctx.modifiers.right_gui(),
        );
    }
}

impl DisplayRenderer<Rgb565> for QubeStatusRenderer {
    fn render<D: DrawTarget<Color = Rgb565>>(&mut self, ctx: &RenderContext, display: &mut D) {
        let _ = display.clear(COL_BG);

        let layer_meta = MonoTextStyle::new(&FONT_6X10, COL_LABEL);
        let header_media = MonoTextStyle::new(&FONT_6X10, COL_FG);
        let header_fallback = MonoTextStyle::new(&FONT_8X13, COL_ACCENT);
        let body = MonoTextStyle::new(&FONT_8X13, COL_FG);
        let title_shadow = MonoTextStyle::new(&FONT_10X20, COL_ACCENT_DIM);
        let title = MonoTextStyle::new(&FONT_10X20, COL_FG);
        let tc = TextStyleBuilder::new()
            .alignment(Alignment::Center)
            .baseline(Baseline::Top)
            .build();
        let tr = TextStyleBuilder::new()
            .alignment(Alignment::Right)
            .baseline(Baseline::Top)
            .build();
        let mc = TextStyleBuilder::new()
            .alignment(Alignment::Center)
            .baseline(Baseline::Middle)
            .build();
        let left = ctx.peripherals_connected.first().copied().unwrap_or(false);
        let right = ctx.peripherals_connected.get(1).copied().unwrap_or(false);
        let lp = battery_reading(ctx.peripheral_batteries.first().map(|b| b.0));
        let rp = battery_reading(ctx.peripheral_batteries.get(1).map(|b| b.0));
        let mut custom_name = [0u8; crate::layer_names::LAYER_NAME_MAX];
        let name = crate::layer_names::copy_layer_name(ctx.layer, &mut custom_name)
            .and_then(|len| core::str::from_utf8(&custom_name[..len]).ok())
            .unwrap_or_else(|| layer_name(ctx.layer));

        // Header.
        draw_panel(display, SAFE_X, 14, SAFE_W, 28, COL_PANEL, COL_BORDER_DIM);
        draw_round_fill(display, SAFE_X + 11, 23, 3, 10, 2, COL_ACCENT);
        let mut s: heapless::String<16> = heapless::String::new();
        draw_media_or_fallback(display, &self.host_data, header_media, header_fallback);
        if host_time_available(&self.host_data) {
            push_host_time(&mut s, self.host_data.hour, self.host_data.minute);
            let _ =
                Text::with_text_style(&s, Point::new(SAFE_X + SAFE_W as i32 - 14, 21), body, tr)
                    .draw(display);
        }

        // Layer panel.
        draw_panel(
            display,
            SAFE_X,
            52,
            SAFE_W,
            84,
            COL_PANEL_HI,
            COL_BORDER_DIM,
        );
        s.clear();
        let _ = write!(&mut s, "LAYER {}", ctx.layer);
        let _ = Text::with_text_style(&s, Point::new(SCREEN_W as i32 / 2, 66), layer_meta, tc)
            .draw(display);
        let _ = Text::with_text_style(
            name,
            Point::new(SCREEN_W as i32 / 2 + 1, 101),
            title_shadow,
            mc,
        )
        .draw(display);
        let _ = Text::with_text_style(name, Point::new(SCREEN_W as i32 / 2, 100), title, mc)
            .draw(display);
        draw_round_fill(display, 104, 125, 72, 2, 1, COL_ACCENT_DIM);

        // Modifier chips.
        self.render_modifiers(ctx, display);

        // Battery cards.
        draw_bat(display, SAFE_X, 176, 116, lp, left, "LEFT");
        draw_bat(display, 146, 176, 116, rp, right, "RIGHT");
    }
}

fn draw_panel<D: DrawTarget<Color = Rgb565>>(
    display: &mut D,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    fill: Rgb565,
    stroke: Rgb565,
) {
    let rect = Rectangle::new(Point::new(x, y), Size::new(w, h));
    let style = PrimitiveStyleBuilder::new()
        .fill_color(fill)
        .stroke_color(stroke)
        .stroke_width(1)
        .build();
    let _ = RoundedRectangle::with_equal_corners(rect, Size::new(PANEL_RADIUS, PANEL_RADIUS))
        .into_styled(style)
        .draw(display);
}

fn draw_round_fill<D: DrawTarget<Color = Rgb565>>(
    display: &mut D,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    radius: u32,
    fill: Rgb565,
) {
    let rect = Rectangle::new(Point::new(x, y), Size::new(w, h));
    let _ = RoundedRectangle::with_equal_corners(rect, Size::new(radius, radius))
        .into_styled(PrimitiveStyle::with_fill(fill))
        .draw(display);
}

fn draw_chip<D: DrawTarget<Color = Rgb565>>(
    display: &mut D,
    x: i32,
    y: i32,
    w: u32,
    label: &str,
    active: bool,
) {
    let text = if active {
        MonoTextStyle::new(&FONT_6X10, COL_FG)
    } else {
        MonoTextStyle::new(&FONT_6X10, COL_DIM)
    };
    if active {
        let rect = Rectangle::new(Point::new(x, y), Size::new(w, 16));
        let style = PrimitiveStyleBuilder::new()
            .fill_color(COL_ACCENT_DIM)
            .stroke_color(COL_ACCENT)
            .stroke_width(1)
            .build();
        let _ = RoundedRectangle::with_equal_corners(rect, Size::new(CHIP_RADIUS, CHIP_RADIUS))
            .into_styled(style)
            .draw(display);
    }
    let tc = TextStyleBuilder::new()
        .alignment(Alignment::Center)
        .baseline(Baseline::Top)
        .build();
    let _ =
        Text::with_text_style(label, Point::new(x + w as i32 / 2, y + 3), text, tc).draw(display);
}
#[derive(Clone, Copy)]
enum BatReading {
    Unknown,
    Pending,
    Pct(u8),
}

fn battery_reading(status: Option<BatteryStatus>) -> BatReading {
    match status {
        Some(BatteryStatus::Available {
            level: Some(level), ..
        }) => BatReading::Pct(level),
        Some(BatteryStatus::Available { level: None, .. }) => BatReading::Pending,
        Some(BatteryStatus::Unavailable) | None => BatReading::Unknown,
    }
}

fn draw_bat<D: DrawTarget<Color = Rgb565>>(
    display: &mut D,
    x: i32,
    y: i32,
    w: i32,
    reading: BatReading,
    connected: bool,
    side: &str,
) {
    let (label, col, fill_pct): (heapless::String<8>, Rgb565, Option<u8>) =
        match (connected, reading) {
            (false, _) => {
                let mut s = heapless::String::new();
                let _ = s.push_str("--");
                (s, COL_DIM, None)
            }
            (true, BatReading::Unknown) | (true, BatReading::Pending) => {
                let mut s = heapless::String::new();
                let _ = s.push_str("??");
                (s, COL_DIM, None)
            }
            (true, BatReading::Pct(p)) => {
                let mut s = heapless::String::new();
                let _ = write!(&mut s, "{}%", p);
                let c = if p < 10 {
                    COL_RED
                } else if p < 25 {
                    COL_YELLOW
                } else {
                    COL_FG
                };
                (s, c, Some(p))
            }
        };

    draw_panel(display, x, y, w as u32, 46, COL_PANEL, COL_BORDER_DIM);

    let title = MonoTextStyle::new(&FONT_6X10, COL_MUTED);
    let percent = MonoTextStyle::new(&FONT_9X15, col);
    let top = TextStyleBuilder::new().baseline(Baseline::Top).build();
    let tr = TextStyleBuilder::new()
        .alignment(Alignment::Right)
        .baseline(Baseline::Top)
        .build();
    let _ = Text::with_text_style(side, Point::new(x + 10, y + 8), title, top).draw(display);
    let _ = Text::with_text_style(&label, Point::new(x + w - 12, y + 6), percent, tr).draw(display);

    let bx = x + 10;
    let by = y + 30;
    let bw = w - 28;
    let bh = 8u32;
    let bar = RoundedRectangle::with_equal_corners(
        Rectangle::new(Point::new(bx, by), Size::new(bw as u32, bh)),
        Size::new(BAR_RADIUS, BAR_RADIUS),
    );
    let bar_style = PrimitiveStyleBuilder::new()
        .fill_color(COL_BAR_BG)
        .stroke_color(COL_BORDER)
        .stroke_width(1)
        .build();
    let _ = bar.into_styled(bar_style).draw(display);
    draw_round_fill(display, bx + bw + 2, by + 2, 4, 3, 2, COL_BORDER_DIM);
    if let Some(pct) = fill_pct {
        if pct > 0 {
            let inner = (bw - 4).max(1) as u32;
            let fw = (inner * pct as u32 / 100).max(2);
            let fc = if pct < 10 {
                COL_RED
            } else if pct < 25 {
                COL_YELLOW
            } else {
                COL_BAR_FG
            };
            draw_round_fill(display, bx + 2, by + 2, fw, bh - 4, 3, fc);
        }
    }
}

fn layer_name(layer: u8) -> &'static str {
    crate::DEFAULT_LAYER_NAMES
        .get(layer as usize)
        .copied()
        .unwrap_or("?")
}

fn push_host_time(buffer: &mut heapless::String<16>, hour: Option<u8>, minute: Option<u8>) {
    match (hour, minute) {
        (Some(hour), Some(minute)) => {
            let _ = write!(buffer, "{:02}:{:02}", hour, minute);
        }
        _ => {
            let _ = buffer.push_str("--:--");
        }
    }
}

fn draw_media_or_fallback<D: DrawTarget<Color = Rgb565>>(
    display: &mut D,
    host_data: &rmk::host_data::HostData,
    media_style: MonoTextStyle<'_, Rgb565>,
    fallback_style: MonoTextStyle<'_, Rgb565>,
) {
    let top = TextStyleBuilder::new().baseline(Baseline::Top).build();
    let mut media: heapless::String<MEDIA_LABEL_LEN> = heapless::String::new();
    push_media_label(&mut media, host_data);

    if media.is_empty() {
        if !host_time_available(host_data) {
            let _ = Text::with_text_style("QUBE", Point::new(SAFE_X + 22, 21), fallback_style, top)
                .draw(display);
        }
        return;
    }

    let mut visible: heapless::String<32> = heapless::String::new();
    if media.len() <= MEDIA_VISIBLE_CHARS {
        let _ = visible.push_str(&media);
    } else {
        let elapsed = Instant::now()
            .duration_since(Instant::from_ticks(0))
            .as_millis() as usize;
        let offset = (elapsed / 300) % (media.len() + MEDIA_GAP_CHARS);
        push_marquee_slice(&mut visible, &media, offset);
    }

    let _ = Text::with_text_style(&visible, Point::new(SAFE_X + 22, 22), media_style, top)
        .draw(display);
}

fn push_media_label(buffer: &mut heapless::String<MEDIA_LABEL_LEN>, host_data: &rmk::host_data::HostData) {
    if !host_data.media_artist.is_empty() {
        push_ascii_text(buffer, &host_data.media_artist);
    }
    if !host_data.media_title.is_empty() {
        if !buffer.is_empty() {
            let _ = buffer.push_str(" - ");
        }
        push_ascii_text(buffer, &host_data.media_title);
    }
}

fn push_ascii_text<const N: usize>(buffer: &mut heapless::String<N>, value: &str) {
    for ch in value.chars() {
        let ch = if ch.is_ascii_graphic() || ch == ' ' {
            ch
        } else {
            '?'
        };
        if buffer.push(ch).is_err() {
            break;
        }
    }
}

fn push_marquee_slice(buffer: &mut heapless::String<32>, text: &str, offset: usize) {
    let bytes = text.as_bytes();
    let cycle_len = bytes.len() + MEDIA_GAP_CHARS;
    for i in 0..MEDIA_VISIBLE_CHARS {
        let idx = (offset + i) % cycle_len;
        let ch = if idx < bytes.len() {
            bytes[idx] as char
        } else {
            ' '
        };
        let _ = buffer.push(ch);
    }
}

fn host_time_available(host_data: &rmk::host_data::HostData) -> bool {
    host_data.hour.is_some() && host_data.minute.is_some()
}

//! General rotary encoder
//!
//! The rotary encoder implementation is adapted from: <https://github.com/leshow/rotary-encoder-hal/blob/master/src/lib.rs>
use core::cell::Cell;
use core::sync::atomic::{AtomicU8, Ordering};

use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_sync::signal::Signal;
use embedded_hal::digital::InputPin;
#[cfg(feature = "async_matrix")]
use embedded_hal_async::digital::Wait;
use postcard::experimental::max_size::MaxSize;
use rmk_macro::input_device;
use serde::{Deserialize, Serialize};

use crate::event::KeyboardEvent;

const MAX_DYNAMIC_ENCODERS: usize = 32;
const MAX_ENCODER_STEPS: u8 = 8;
const ENCODER_INTERVAL_UNSET: u8 = u8::MAX;
static ENCODER_STEPS: [AtomicU8; MAX_DYNAMIC_ENCODERS] = [const { AtomicU8::new(1) }; MAX_DYNAMIC_ENCODERS];
static ENCODER_INTERVAL_MS: [AtomicU8; MAX_DYNAMIC_ENCODERS] =
    [const { AtomicU8::new(ENCODER_INTERVAL_UNSET) }; MAX_DYNAMIC_ENCODERS];
// ARMv6-M does not provide atomic read-modify-write operations for `u32`.
// Keep the shared bitset behind RMK's blocking mutex so every supported
// target can update it without polling disabled encoder tasks.
static ENABLED_ENCODERS: BlockingMutex<crate::RawMutex, Cell<u32>> = BlockingMutex::new(Cell::new(u32::MAX));
static ENCODER_ENABLED_CHANGED: [Signal<crate::RawMutex, ()>; MAX_DYNAMIC_ENCODERS] =
    [const { Signal::new() }; MAX_DYNAMIC_ENCODERS];

/// Set how many encoder actions are emitted for one physical step.
///
/// Returns `false` when `id` exceeds the supported dynamic encoder range.
pub fn set_encoder_steps(id: u8, steps: u8) -> bool {
    let Some(value) = ENCODER_STEPS.get(usize::from(id)) else {
        return false;
    };
    value.store(steps.clamp(1, MAX_ENCODER_STEPS), Ordering::Relaxed);
    true
}

/// Get how many encoder actions are emitted for one physical step.
pub fn encoder_steps(id: u8) -> u8 {
    ENCODER_STEPS
        .get(usize::from(id))
        .map(|value| value.load(Ordering::Relaxed))
        .unwrap_or(1)
}

/// Override the minimum interval between events for one encoder at runtime.
///
/// Values up to 254 ms are accepted. Until an override is installed, the
/// encoder keeps using its statically generated TOML `debounce_ms` value.
pub fn set_encoder_interval_ms(id: u8, interval_ms: u16) -> bool {
    let Some(value) = ENCODER_INTERVAL_MS.get(usize::from(id)) else {
        return false;
    };
    let Ok(interval_ms) = u8::try_from(interval_ms) else {
        return false;
    };
    if interval_ms == ENCODER_INTERVAL_UNSET {
        return false;
    }
    value.store(interval_ms, Ordering::Relaxed);
    true
}

/// Read a runtime interval override. `None` means either an invalid encoder ID
/// or that the encoder still uses its static TOML debounce value.
pub fn encoder_interval_ms(id: u8) -> Option<u16> {
    let value = ENCODER_INTERVAL_MS.get(usize::from(id))?.load(Ordering::Relaxed);
    (value != ENCODER_INTERVAL_UNSET).then_some(u16::from(value))
}

/// Restore the statically configured TOML debounce interval for one encoder.
pub fn clear_encoder_interval_override(id: u8) -> bool {
    let Some(value) = ENCODER_INTERVAL_MS.get(usize::from(id)) else {
        return false;
    };
    value.store(ENCODER_INTERVAL_UNSET, Ordering::Relaxed);
    true
}

/// Compatibility spelling for callers that describe the interval as debounce.
pub fn set_encoder_debounce_ms(id: u8, debounce_ms: u16) -> bool {
    set_encoder_interval_ms(id, debounce_ms)
}

/// Compatibility spelling for [`encoder_interval_ms`].
pub fn encoder_debounce_ms(id: u8) -> Option<u16> {
    encoder_interval_ms(id)
}

/// Enable or park one encoder task at runtime.
///
/// Encoders are enabled by default so existing generated keyboards retain
/// their behavior. A disabled encoder waits for this setting to change and
/// does not poll or wake on its GPIO pins.
pub fn set_encoder_enabled(id: u8, enabled: bool) -> bool {
    let Some(changed) = ENCODER_ENABLED_CHANGED.get(usize::from(id)) else {
        return false;
    };
    let bit = 1u32 << u32::from(id);
    let previous = ENABLED_ENCODERS.lock(|state| {
        let current = state.get();
        let previous = current & bit != 0;
        state.set(if enabled { current | bit } else { current & !bit });
        previous
    });
    if previous != enabled {
        changed.signal(());
    }
    true
}

/// Return whether an encoder task is currently active.
pub fn encoder_enabled(id: u8) -> bool {
    if usize::from(id) >= MAX_DYNAMIC_ENCODERS {
        return true;
    }
    let bit = 1u32 << u32::from(id);
    ENABLED_ENCODERS.lock(|state| state.get() & bit != 0)
}

/// Holds current/old state and both [`InputPin`](https://docs.rs/embedded-hal/latest/embedded_hal/digital/trait.InputPin.html)
#[derive(Clone, Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[input_device(publish = KeyboardEvent)]
pub struct RotaryEncoder<
    #[cfg(feature = "async_matrix")] A: InputPin + Wait,
    #[cfg(not(feature = "async_matrix"))] A: InputPin,
    #[cfg(feature = "async_matrix")] B: InputPin + Wait,
    #[cfg(not(feature = "async_matrix"))] B: InputPin,
    P: Phase,
> {
    pin_a: A,
    pin_b: B,
    state: u8,
    phase: P,
    /// The index of the rotary encoder
    id: u8,
    step_repeater: EncoderStepRepeater,
    /// Timestamp of the last emitted direction event, used for debounce.
    last_event_time: Option<embassy_time::Instant>,
    /// Minimum interval in milliseconds between emitted direction events.
    /// Contact bounce on mechanical encoders produces spurious quadrature edges;
    /// this suppresses them without altering the Phase/Resolution logic.
    debounce_ms: u16,
}

/// The encoder direction is either `Clockwise`, `CounterClockwise`, or `None`
#[derive(Serialize, Deserialize, Clone, Copy, Debug, MaxSize, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Direction {
    /// A clockwise turn
    Clockwise,
    /// A counterclockwise turn
    CounterClockwise,
    /// No change
    None,
}

#[derive(Clone, Copy, Debug, Default)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
struct EncoderStepRepeater {
    pressed_direction: Option<Direction>,
    pending_press: Option<Direction>,
    remaining_repeats: u8,
}

impl EncoderStepRepeater {
    fn begin(&mut self, direction: Direction, steps: u8) {
        self.pressed_direction = Some(direction);
        self.pending_press = None;
        self.remaining_repeats = steps.saturating_sub(1);
    }

    fn next(&mut self) -> Option<(Direction, bool)> {
        if let Some(direction) = self.pending_press.take() {
            self.pressed_direction = Some(direction);
            return Some((direction, true));
        }

        let direction = self.pressed_direction.take()?;
        if self.remaining_repeats > 0 {
            self.remaining_repeats -= 1;
            self.pending_press = Some(direction);
        }
        Some((direction, false))
    }

    fn cancel(&mut self) -> Option<Direction> {
        let pressed = self.pressed_direction.take();
        *self = Self::default();
        pressed
    }
}

/// Allows customizing which Quadrature Phases should be considered movements
/// and in which direction or ignored.
pub trait Phase {
    /// Given the current state `s`, return the direction.
    fn direction(&mut self, s: u8) -> Direction;
}

/// Default implementation of `Phase`.
pub struct DefaultPhase;

/// The useful values of `s` are:
/// - 0b0001 | 0b0111 | 0b1000 | 0b1110
/// - 0b0010 | 0b0100 | 0b1011 | 0b1101
impl Phase for DefaultPhase {
    fn direction(&mut self, s: u8) -> Direction {
        match s {
            0b0001 | 0b0111 | 0b1000 | 0b1110 => Direction::Clockwise,
            0b0010 | 0b0100 | 0b1011 | 0b1101 => Direction::CounterClockwise,
            _ => Direction::None,
        }
    }
}

/// Phase implementation for E8H7 encoder
pub struct E8H7Phase;
impl Phase for E8H7Phase {
    fn direction(&mut self, s: u8) -> Direction {
        match s {
            0b0010 | 0b1101 => Direction::Clockwise,
            0b0001 | 0b1110 => Direction::CounterClockwise,
            _ => Direction::None,
        }
    }
}

/// Phase implementation based on configurable resolution
pub struct ResolutionPhase {
    resolution: u8,
    lut: [i8; 16],
    current_pulses: i8,
}

impl ResolutionPhase {
    pub fn new(resolution: u8, reverse: bool) -> Self {
        // Each entry corresponds to a state transition and provides +1, -1, or 0 pulse
        let mut lut = [0, -1, 1, 0, 1, 0, 0, -1, -1, 0, 0, 1, 0, 1, -1, 0];
        if reverse {
            lut = lut.map(|x| -x);
        }
        Self {
            resolution,
            lut,
            current_pulses: 0,
        }
    }

    pub fn new_with_detent_and_pulse(detent: u8, pulse: u8, reverse: bool) -> Self {
        // Each entry corresponds to a state transition and provides +1, -1, or 0 pulse
        let mut lut = [0, -1, 1, 0, 1, 0, 0, -1, -1, 0, 0, 1, 0, 1, -1, 0];
        if reverse {
            lut = lut.map(|x| -x);
        }
        Self {
            resolution: pulse * 4 / detent,
            lut,
            current_pulses: 0,
        }
    }
}

impl Phase for ResolutionPhase {
    fn direction(&mut self, s: u8) -> Direction {
        // Only proceed if there was a state change
        if (s & 0xC) != (s & 0x3) {
            // Add pulse value from the lookup table
            self.current_pulses += self.lut[s as usize & 0xF];
            // Check if we've reached the resolution threshold
            if self.current_pulses >= self.resolution as i8 {
                self.current_pulses %= self.resolution as i8;
                return Direction::CounterClockwise;
            } else if self.current_pulses <= -(self.resolution as i8) {
                self.current_pulses %= self.resolution as i8;
                return Direction::Clockwise;
            }
        }

        Direction::None
    }
}

impl<
    #[cfg(feature = "async_matrix")] A: InputPin + Wait,
    #[cfg(not(feature = "async_matrix"))] A: InputPin,
    #[cfg(feature = "async_matrix")] B: InputPin + Wait,
    #[cfg(not(feature = "async_matrix"))] B: InputPin,
> RotaryEncoder<A, B, DefaultPhase>
{
    /// Accepts two [`InputPin`](https://docs.rs/embedded-hal/latest/embedded_hal/digital/trait.InputPin.html)s, these will be read on every `update()`.
    pub fn new(pin_a: A, pin_b: B, id: u8) -> Self {
        Self {
            pin_a,
            pin_b,
            state: 0u8,
            phase: DefaultPhase,
            id,
            step_repeater: EncoderStepRepeater::default(),
            last_event_time: None,
            debounce_ms: 0,
        }
    }
}

/// Create a resolution-based rotary encoder
impl<
    #[cfg(feature = "async_matrix")] A: InputPin + Wait,
    #[cfg(not(feature = "async_matrix"))] A: InputPin,
    #[cfg(feature = "async_matrix")] B: InputPin + Wait,
    #[cfg(not(feature = "async_matrix"))] B: InputPin,
> RotaryEncoder<A, B, ResolutionPhase>
{
    /// Creates a new encoder with the specified resolution
    pub fn with_resolution(pin_a: A, pin_b: B, resolution: u8, reverse: bool, id: u8) -> Self {
        Self {
            pin_a,
            pin_b,
            state: 0u8,
            phase: ResolutionPhase::new(resolution, reverse),
            id,
            step_repeater: EncoderStepRepeater::default(),
            last_event_time: None,
            debounce_ms: 0,
        }
    }
}

impl<
    #[cfg(feature = "async_matrix")] A: InputPin + Wait,
    #[cfg(not(feature = "async_matrix"))] A: InputPin,
    #[cfg(feature = "async_matrix")] B: InputPin + Wait,
    #[cfg(not(feature = "async_matrix"))] B: InputPin,
    P: Phase,
> RotaryEncoder<A, B, P>
{
    /// Accepts two [`InputPin`](https://docs.rs/embedded-hal/latest/embedded_hal/digital/trait.InputPin.html)s, these will be read on every `update()`, while using `phase` to determine the direction.
    pub fn with_phase(pin_a: A, pin_b: B, phase: P, id: u8) -> Self {
        Self {
            pin_a,
            pin_b,
            state: 0u8,
            phase,
            id,
            step_repeater: EncoderStepRepeater::default(),
            last_event_time: None,
            debounce_ms: 0,
        }
    }

    /// Set the debounce interval in milliseconds. Events arriving faster than
    /// this interval after the last emitted event are suppressed.
    pub fn with_debounce(mut self, debounce_ms: u16) -> Self {
        self.debounce_ms = debounce_ms;
        self
    }

    /// Call `update` to evaluate the next state of the encoder, propagates errors from `InputPin` read
    pub fn update(&mut self) -> Direction {
        // use mask to get previous state value
        let mut s = self.state & 0b11;

        let (a_is_low, b_is_low) = (self.pin_a.is_low(), self.pin_b.is_low());

        // move in the new state
        match a_is_low {
            Ok(true) => s |= 0b0100,
            Ok(false) => {}
            Err(_) => return Direction::None,
        }
        match b_is_low {
            Ok(true) => s |= 0b1000,
            Ok(false) => {}
            Err(_) => return Direction::None,
        }

        // move new state in
        self.state = s >> 2;

        // Use the phase implementation
        self.phase.direction(s)
    }

    /// Returns a reference to the first pin. Can be used to clear interrupt.
    pub fn pin_a(&mut self) -> &mut A {
        &mut self.pin_a
    }

    /// Returns a reference to the second pin. Can be used to clear interrupt.
    pub fn pin_b(&mut self) -> &mut B {
        &mut self.pin_b
    }

    /// Returns a reference to both pins. Can be used to clear interrupt.
    pub fn pins(&mut self) -> (&mut A, &mut B) {
        (&mut self.pin_a, &mut self.pin_b)
    }

    /// Consumes this `Rotary`, returning the underlying pins `A` and `B`.
    pub fn into_inner(self) -> (A, B) {
        (self.pin_a, self.pin_b)
    }

    /// Check whether enough time has elapsed since the last emitted event.
    /// Returns `true` if the event should pass through (first event, or
    /// debounce interval exceeded). Updates the internal timestamp when
    /// returning `true`.
    fn debounce_check(&mut self) -> bool {
        let now = embassy_time::Instant::now();
        let interval_ms = encoder_interval_ms(self.id).unwrap_or(self.debounce_ms);
        let ok = match self.last_event_time {
            Some(last) => now.duration_since(last).as_millis() >= u64::from(interval_ms),
            None => true,
        };
        if ok {
            self.last_event_time = Some(now);
        }
        ok
    }
}

impl<
    #[cfg(feature = "async_matrix")] A: InputPin + Wait,
    #[cfg(not(feature = "async_matrix"))] A: InputPin,
    #[cfg(feature = "async_matrix")] B: InputPin + Wait,
    #[cfg(not(feature = "async_matrix"))] B: InputPin,
    P: Phase,
> RotaryEncoder<A, B, P>
{
    /// Read a keyboard event from the rotary encoder.
    /// This method is called by the generated InputDevice implementation.
    async fn read_keyboard_event(&mut self) -> KeyboardEvent {
        loop {
            if !encoder_enabled(self.id) {
                if let Some(direction) = self.step_repeater.cancel() {
                    return KeyboardEvent::rotary_encoder(self.id, direction, false);
                }
                let Some(changed) = ENCODER_ENABLED_CHANGED.get(usize::from(self.id)) else {
                    core::future::pending::<()>().await;
                    unreachable!();
                };
                while !encoder_enabled(self.id) {
                    changed.wait().await;
                }
                // Synchronize the quadrature baseline without emitting the
                // transition that happened while this module was parked.
                let _ = self.update();
            }

            // Read until a valid rotary encoder event is detected.
            if let Some((direction, pressed)) = self.step_repeater.next() {
                if !pressed {
                    embassy_time::Timer::after_millis(5).await;
                }
                return KeyboardEvent::rotary_encoder(self.id, direction, pressed);
            }

            #[cfg(feature = "async_matrix")]
            {
                let enabled_changed = ENCODER_ENABLED_CHANGED.get(usize::from(self.id));
                let (pin_a, pin_b) = self.pins();
                let pin_edge = embassy_futures::select::select(pin_a.wait_for_any_edge(), pin_b.wait_for_any_edge());
                if let Some(changed) = enabled_changed {
                    if matches!(
                        embassy_futures::select::select(pin_edge, changed.wait()).await,
                        embassy_futures::select::Either::Second(_)
                    ) {
                        continue;
                    }
                } else {
                    pin_edge.await;
                }
            }

            let direction = self.update();

            if direction != Direction::None && self.debounce_check() {
                self.step_repeater.begin(direction, encoder_steps(self.id));
                return KeyboardEvent::rotary_encoder(self.id, direction, true);
            }

            #[cfg(not(feature = "async_matrix"))]
            {
                // Wait for 20ms to avoid busy loop
                embassy_time::Timer::after_millis(20).await;
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    // Init logger for tests

    #[ctor::ctor(unsafe)]
    fn init_log() {
        let _ = env_logger::builder()
            .filter_level(log::LevelFilter::Debug)
            .is_test(true)
            .try_init();
    }

    #[test]
    fn test_resolutin_phase() {
        // Check with E8H7 phase
        let mut default_phase = E8H7Phase {};
        let mut resolution_phase = ResolutionPhase::new(2, true);
        // Clockwise sequence
        for item in [0b100, 0b1101, 0b1011, 0b10] {
            let d = default_phase.direction(item);
            let d2 = resolution_phase.direction(item);
            info!("Item: {:b}, {:?} {:?}", item, d, d2);
            assert_eq!(d, d2);
        }
        // Counterclockwise sequence
        for item in [0b1000, 0b1110, 0b111, 0b1] {
            let d = default_phase.direction(item);
            let d2 = resolution_phase.direction(item);
            info!("Item: {:b}, {:?} {:?}", item, d, d2);
            assert_eq!(d, d2);
        }

        // Check with default phase
        let mut default_phase = DefaultPhase {};
        let mut resolution_phase = ResolutionPhase::new(1, false);
        for item in 0u8..16 {
            let d = default_phase.direction(item);
            let d2 = resolution_phase.direction(item);
            info!("Item: {:b}, {:?} {:?}", item, d, d2);
            assert_eq!(d, d2);
        }
    }

    #[test]
    fn encoder_step_count_is_clamped_and_defaults_to_one() {
        assert_eq!(encoder_steps(31), 1);
        assert!(set_encoder_steps(31, 0));
        assert_eq!(encoder_steps(31), 1);
        assert!(set_encoder_steps(31, 20));
        assert_eq!(encoder_steps(31), 8);
        assert!(!set_encoder_steps(32, 3));
        assert_eq!(encoder_steps(32), 1);
    }

    #[test]
    fn encoder_interval_runtime_api_checks_bounds_and_roundtrips() {
        assert!(clear_encoder_interval_override(29));
        assert_eq!(encoder_interval_ms(29), None);
        assert!(set_encoder_interval_ms(29, 100));
        assert_eq!(encoder_interval_ms(29), Some(100));
        assert_eq!(encoder_debounce_ms(29), Some(100));
        assert!(set_encoder_debounce_ms(29, 0));
        assert_eq!(encoder_interval_ms(29), Some(0));
        assert!(!set_encoder_interval_ms(29, 255));
        assert!(!set_encoder_interval_ms(29, 256));
        assert!(!set_encoder_interval_ms(32, 10));
        assert_eq!(encoder_interval_ms(32), None);
    }

    #[test]
    fn absent_runtime_interval_preserves_static_debounce() {
        assert!(clear_encoder_interval_override(28));
        let static_debounce_ms = 37;
        assert_eq!(encoder_interval_ms(28).unwrap_or(static_debounce_ms), 37);
        assert!(set_encoder_interval_ms(28, 5));
        assert_eq!(encoder_interval_ms(28).unwrap_or(static_debounce_ms), 5);
    }

    #[test]
    fn encoder_can_be_parked_and_reenabled() {
        assert!(encoder_enabled(30));
        assert!(set_encoder_enabled(30, false));
        assert!(!encoder_enabled(30));
        assert!(set_encoder_enabled(30, true));
        assert!(encoder_enabled(30));
        assert!(!set_encoder_enabled(32, false));
        assert!(encoder_enabled(32));
    }

    #[test]
    fn encoder_step_repeater_emits_complete_press_release_pairs() {
        let mut repeater = EncoderStepRepeater::default();
        repeater.begin(Direction::Clockwise, 3);

        assert_eq!(repeater.next(), Some((Direction::Clockwise, false)));
        assert_eq!(repeater.next(), Some((Direction::Clockwise, true)));
        assert_eq!(repeater.next(), Some((Direction::Clockwise, false)));
        assert_eq!(repeater.next(), Some((Direction::Clockwise, true)));
        assert_eq!(repeater.next(), Some((Direction::Clockwise, false)));
        assert_eq!(repeater.next(), None);
    }
}

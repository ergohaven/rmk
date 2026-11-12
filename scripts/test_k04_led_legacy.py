#!/usr/bin/env python3
"""Native source-backed K:04 LED owner regression (no firmware build).

Compiles the complete production state/event/render/poll/deadline implementations.
Substitutions: deterministic millisecond clock, snapshots, event delivery and PWM
sink. Subscriptions drive delivery: an unsubscribed event is dropped, not invented.
Real settings color/default/packet functions and connection/battery types are used.
Does not exercise the generated processor executor, BLE radio, ADC or physical PWM.
Run via agent-storage so rustc outputs and tempfile directories stay managed.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]


def block(source, marker):
    start = source.index(marker)
    opening = source.index('{', start)
    depth = 0
    for pos in range(opening, len(source)):
        if source[pos] == '{':
            depth += 1
        elif source[pos] == '}':
            depth -= 1
            if depth == 0:
                return source[start:pos + 1]
    raise AssertionError(marker)


def owner_source(name):
    source = (ROOT / f'keyboards/k04/src/{name}.rs').read_text()
    if 'include!("layer_led.rs");' in source:
        assert re.sub(r'//[^\n]*', '', source).strip() == 'include!("layer_led.rs");', 'Unexpected Qube wrapper'
        source = (ROOT / 'keyboards/k04/src/layer_led.rs').read_text()
    return source


def settings_source():
    source = (ROOT / 'keyboards/k04/src/module_settings.rs').read_text()
    functions = ['led_brightness', 'charge_indicator_enabled', 'led_timeout_sec',
                 'layer_color', 'bt_profile_color', 'byte', 'ensure_initialized',
                 'layer_color_index', 'set_layer_color_index', 'palette',
                 'apply_settings_packet', 'unpack_color']
    parts = [line for line in source.splitlines() if re.match(r'const \w+:[^\n]+;$', line)]
    parts += ['#[derive(Clone, Copy, PartialEq, Eq)]\n' + block(source, 'pub struct Rgb'),
              'static SETTINGS: [AtomicU8; SETTINGS_LEN] = [const { AtomicU8::new(0) }; SETTINGS_LEN];']
    for name in functions:
        match = re.search(r'^(?:pub(?:\(crate\))? )?fn ' + name + r'\(', source, re.M)
        assert match, name
        parts.append(block(source[match.start():], match.group()))
    return '\n'.join(parts)


PRELUDE = r'''
#![allow(dead_code, unused_imports, unused_variables)]
extern crate self as rmk;
extern crate self as defmt;
#[macro_export] macro_rules! info { ($($x:tt)*) => {}; }
#[macro_export] macro_rules! trace { ($($x:tt)*) => {}; }
use std::sync::atomic::{AtomicU8, AtomicU64, AtomicBool, Ordering};
static CLOCK: AtomicU64 = AtomicU64::new(0);
mod embassy_time {
    use super::*;
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)] pub struct Instant(pub u64);
    #[derive(Clone, Copy)] pub struct Duration(pub u64);
    impl Instant {
        pub fn now() -> Self { Self(CLOCK.load(Ordering::Relaxed)) }
        pub fn duration_since(self, earlier: Self) -> Duration { Duration(self.0.saturating_sub(earlier.0)) }
    }
    impl Duration {
        pub const fn from_millis(ms: u64) -> Self { Self(ms) }
        pub const fn from_secs(s: u64) -> Self { Self(s * 1000) }
        pub fn as_millis(self) -> u64 { self.0 }
    }
    impl std::ops::Add<Duration> for Instant { type Output = Self; fn add(self, d: Duration) -> Self { Self(self.0+d.0) } }
    pub struct Timer;
}
use embassy_time::{Instant, Duration};
#[derive(Default)] pub struct SequencePwm<'a>(std::marker::PhantomData<&'a ()>);
mod processor {
    use super::*;
    // Exact production callback signatures. The generated forever-loop/transport
    // are the declared boundary, not replaced LED decisions.
    @DEADLINE_SIGNATURES@
}
mod types {
    pub mod ble { @BLE_TYPES@ }
    pub mod connection {
        use super::ble::*;
        @CONNECTION_TYPES@
    }
    pub mod battery { @BATTERY_TYPES@ }
}
mod event {
    use super::*;
    use types::connection::ConnectionStatus;
    use types::battery::BatteryStatus;
    @EVENT_TYPES@
    @EVENT_QUEUE@
    static SPLIT: AtomicU8 = AtomicU8::new(1);
    pub fn current_split_connection_state() -> SplitConnectionState {
        match SPLIT.load(Ordering::Relaxed) { 0 => SplitConnectionState::Searching, 1 => SplitConnectionState::Connected, _ => SplitConnectionState::Idle }
    }
    pub fn set_split(s: SplitConnectionState) { SPLIT.store(match s { SplitConnectionState::Searching=>0, SplitConnectionState::Connected=>1, _=>2 }, Ordering::Relaxed); }
}
mod state {
    use super::*;
    use types::connection::*;
    use event::BleAdvertisingMode;
    thread_local! {
        pub static STATUS: std::cell::Cell<ConnectionStatus> = const { std::cell::Cell::new(ConnectionStatus::new()) };
        pub static MODE: std::cell::Cell<BleAdvertisingMode> = const { std::cell::Cell::new(BleAdvertisingMode::Pairing) };
    }
    struct StatusStore;
    static CONNECTION_STATUS: StatusStore = StatusStore;
    impl StatusStore { fn lock<R>(&self, f: impl FnOnce(&std::cell::Cell<ConnectionStatus>) -> R) -> R { STATUS.with(f) } }
    struct ModeStore;
    static BLE_ADVERTISING_MODE: ModeStore = ModeStore;
    impl ModeStore { fn lock<R>(&self, f: impl FnOnce(&std::cell::Cell<BleAdvertisingMode>) -> R) -> R { MODE.with(f) } }
    use crate::types::ble::BleState;
    use crate::event::{publish_event, ConnectionStatusChangeEvent, BleAdvertisingModeEvent};
    @STATE_FUNCTIONS@
}
mod channel {
    pub fn clear_and_release_report_channel(_: crate::types::connection::ConnectionType) {}
    pub struct Reports;
    impl Reports { pub fn clear(&self) {} }
    pub static BLE_REPORT_CHANNEL: Reports = Reports;
}
mod input_device {
    pub mod rotary_encoder { pub fn set_encoder_steps(_: u8, _: u8) {} }
    pub mod battery {
        use crate::types::battery::*;
        thread_local! { pub static STATUS: std::cell::Cell<BatteryStatus> = const { std::cell::Cell::new(BatteryStatus::Unavailable) }; }
        pub fn current_battery_status() -> BatteryStatus { STATUS.with(|s|s.get()) }
    }
}
mod battery_nrf {
    use super::*;
    pub static USB: AtomicBool = AtomicBool::new(false);
    pub fn usb_powered() -> bool { USB.load(Ordering::Relaxed) }
    pub fn usb_power_status() -> (bool, bool) { (usb_powered(), false) }
}
mod module_settings {
    use super::*;
    @SETTINGS@
    fn apply_module_selection_runtime() {}
    pub fn reset() {
        for byte in &SETTINGS { byte.store(0, Ordering::Relaxed); }
        ensure_initialized();
        SETTINGS[IDX_LED_BRIGHTNESS].store(255, Ordering::Relaxed);
    }
    pub fn charge(enabled: bool) { SETTINGS[IDX_AUTO_FLAGS].store(if enabled {0} else {AUTO_FLAG_CHARGE_INDICATOR_DISABLED}, Ordering::Relaxed); }
}
fn run<F: std::future::Future>(f: F) -> F::Output {
    let waker = std::task::Waker::noop();
    let mut cx = std::task::Context::from_waker(waker);
    let mut f = std::pin::pin!(f);
    match f.as_mut().poll(&mut cx) { std::task::Poll::Ready(x) => x, _ => panic!("unexpected pending hardware operation") }
}
'''


def type_block(path, marker):
    return '#[derive(Clone, Copy, Debug, PartialEq, Eq)]\n' + block((ROOT / path).read_text(), marker)


def harness(name):
    source = owner_source(name)
    match = re.search(r'#\[processor\(\s*subscribe = \[(.*?)\]', source, re.S)
    assert match, 'Missing owning subscriptions'
    subscriptions = match.group(1)
    delivered = re.findall(r'\b\w+Event\b', subscriptions)
    prefix = PRELUDE.replace('@SETTINGS@', settings_source())
    ble_path = 'rmk-types/src/ble.rs'
    con_path = 'rmk-types/src/connection.rs'
    bat_path = 'rmk-types/src/battery.rs'
    prefix = prefix.replace('@BLE_TYPES@', '\n'.join(type_block(ble_path, x) for x in ['pub enum BleState', 'pub struct BleStatus']))
    con = '\n'.join(type_block(con_path, x) for x in ['pub enum ConnectionType', 'pub enum UsbState', 'pub struct ConnectionStatus'])
    con = con.replace('    #[default]\n', '')
    connection_source = (ROOT / con_path).read_text()
    con += '\n' + block(connection_source, 'impl ConnectionStatus {')
    second = connection_source.index('impl ConnectionStatus {', connection_source.index('impl ConnectionStatus {') + 1)
    con += '\n' + block(connection_source[second:], 'impl ConnectionStatus {')
    prefix = prefix.replace('@CONNECTION_TYPES@', con)
    prefix = prefix.replace('@BATTERY_TYPES@', '\n'.join(type_block(bat_path, x) for x in ['pub enum ChargeState', 'pub enum BatteryStatus']))
    events = [type_block('rmk/src/event/connection.rs', 'pub enum BleAdvertisingMode'),
              type_block('rmk/src/event/split.rs', 'pub enum SplitConnectionState')]
    event_paths = {'ConnectionStatusChangeEvent': 'connection', 'BleAdvertisingModeEvent': 'connection',
                   'LayerChangeEvent': 'state', 'SleepStateEvent': 'state', 'SplitConnectionStateEvent': 'split',
                   'BatteryStatusEvent': 'battery', 'PeripheralBatteryRefreshEvent': 'split', 'PeripheralSettingsEvent': 'split'}
    for typ, path in event_paths.items():
        text = (ROOT / f'rmk/src/event/{path}.rs').read_text()
        decl = re.search(r'^pub struct ' + typ + r'[^\n]*;$', text, re.M)
        assert decl, typ
        events.append('#[derive(Clone, Copy)]\n' + decl.group())
    prefix = prefix.replace('@EVENT_TYPES@', '\n'.join(events))
    queue = ['pub enum Queued { ' + ', '.join(f'{x}({x})' for x in event_paths) + ' }',
             'thread_local! { pub static QUEUE: std::cell::RefCell<Vec<Queued>> = const { std::cell::RefCell::new(Vec::new()) }; }',
             'pub fn publish_event(e: impl Into<Queued>) { QUEUE.with(|q|q.borrow_mut().push(e.into())); }',
             'impl From<BatteryStatus> for BatteryStatusEvent { fn from(b: BatteryStatus) -> Self { Self(b) } }',
             'impl LayerChangeEvent { pub fn new(layer:u8) -> Self { Self(layer) } }',
             'impl SleepStateEvent { pub fn new(sleeping:bool) -> Self { Self(sleeping) } }']
    queue.extend(f'impl From<{x}> for Queued {{ fn from(e:{x}) -> Self {{ Self::{x}(e) }} }}' for x in event_paths)
    prefix = prefix.replace('@EVENT_QUEUE@', '\n'.join(queue))
    state = (ROOT / 'rmk/src/state.rs').read_text()
    state_functions = []
    for function in ['current_connection_status', 'current_ble_advertising_mode',
                     'set_ble_advertising_mode', 'update_status', 'notify_connection_status']:
        match = re.search(r'^(?:pub(?:\(crate\))? )?fn ' + function + r'\(', state, re.M)
        assert match, function
        state_functions.append(block(state[match.start():], match.group()))
    prefix = prefix.replace('@STATE_FUNCTIONS@', '\n'.join(state_functions))
    trait = (ROOT / 'rmk/src/processor/mod.rs').read_text()
    signatures = '\n'.join(re.findall(r'^    (?:async )?fn (?:deadline|on_deadline)\([^\n]+;', trait, re.M))
    assert signatures.count(';') == 2
    prefix = prefix.replace('@DEADLINE_SIGNATURES@', 'pub trait DeadlineProcessor {\n' + signatures + '\n}')
    source = re.sub(r'use embassy_nrf::pwm::[^;]+;', 'use crate::SequencePwm;', source)
    source = source.replace('use embassy_time::', 'use crate::embassy_time::')
    source = source.replace('use rmk::macros::processor;\n', '')
    source = re.sub(r'#\[processor\([\s\S]*?\)\]\n', '', source, count=1)
    send = block(source, 'async fn send_color(')
    source = source.replace(send, 'async fn send_color(_: &mut SequencePwm<\'_>, color: Rgb) { OUTPUT.with(|out| out.borrow_mut().push(color)); }')
    source += '\nthread_local! { static OUTPUT: std::cell::RefCell<Vec<Rgb>> = const { std::cell::RefCell::new(Vec::new()) }; }\n'
    dispatch = ['enum Event { ' + ', '.join(f'{x}({x})' for x in event_paths) + ' }',
                'impl LayerLed { async fn deliver(&mut self, e: Event) { match e {']
    for typ in event_paths:
        handler = 'on_' + re.sub(r'(?<!^)(?=[A-Z])', '_', typ).lower()
        call = f'self.{handler}(e).await' if typ in delivered else '{}'
        dispatch.append(f'Event::{typ}(e) => {call},')
    dispatch += ['} } }']
    peripheral = (ROOT / 'rmk/src/split/peripheral.rs').read_text()
    arms = [block(peripheral, f'SplitMessage::{arm} =>') for arm in [
        'ConnectionStatus(status)', 'BleAdvertisingMode(mode)', 'BatteryRefresh',
        'Layer(layer)', 'PeripheralSettings(settings)', 'SleepState(sleeping)']]
    forwarding = r'''
#[derive(Clone, Copy)] enum SplitMessage {
    ConnectionStatus(ConnectionStatus), BleAdvertisingMode(BleAdvertisingMode), BatteryRefresh,
    Layer(u8), PeripheralSettings([u8;27]), SleepState(bool), BatteryStatus(BatteryStatusEvent)
}
#[derive(Default)] struct RecordingDriver(Vec<SplitMessage>);
impl RecordingDriver { async fn write(&mut self, message:&SplitMessage) -> Result<(),()> { self.0.push(*message); Ok(()) } }
#[derive(Default)] struct Peripheral { split_driver:RecordingDriver }
impl Peripheral {
    async fn receive(&mut self, message:SplitMessage) {
        use crate::state::update_status;
        use crate::event::publish_event;
        match message { @ARMS@, _ => {} }
    }
}
fn forward(led:&mut LayerLed, message:SplitMessage) -> Peripheral {
    let mut peripheral = Peripheral::default();
    run(peripheral.receive(message));
    for event in event::QUEUE.with(|q|q.borrow_mut().drain(..).collect::<Vec<_>>()) {
        let event = match event { @DELIVERY@ };
        run(led.deliver(event));
    }
    peripheral
}
'''
    forwarding = forwarding.replace('@ARMS@', ',\n'.join(arms))
    forwarding = forwarding.replace('@DELIVERY@', ', '.join(f'event::Queued::{x}(e) => Event::{x}(e)' for x in event_paths))
    return prefix + '\nmod owner {\n' + source + '\n' + '\n'.join(dispatch) + '\n' + forwarding + '\n' + (ROOT / 'scripts/fixtures/k04_led_legacy.rs').read_text() + '\n}'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--owner', choices=['layer_led', 'qube_layer_led'])
    parser.add_argument('--filter', default='')
    parser.add_argument('--evidence-dir', type=Path)
    args = parser.parse_args()
    # Configuration/schema guards are structural checks, not firmware runtime tests.
    for suffix in ['', '_mini', '_micro', '_qube', '_qube_mini', '_qube_micro']:
        config = json.loads((ROOT / f'keyboards/k04/vial{suffix}.json').read_text())
        fields = [f for group in config['settings'] for f in group['fields'] if f['qsid'] == 331]
        assert fields == [{'type': 'boolean', 'title': 'Charge indicator', 'qsid': 331, 'bit': 0}], suffix
    if args.evidence_dir:
        args.evidence_dir.mkdir(parents=True, exist_ok=True)
        paths = [
            'keyboards/k04/src/layer_led.rs', 'keyboards/k04/src/qube_layer_led.rs',
            'keyboards/k04/src/module_settings.rs', 'keyboards/k04/src/battery_nrf.rs',
            'keyboards/k04/src/battery_level.rs', 'keyboards/k04/src/left.rs',
            'keyboards/k04/src/right.rs', 'keyboards/k04/src/central.rs',
            'keyboards/k04/src/peripheral.rs', 'rmk/src/state.rs',
            'rmk/src/split/peripheral.rs', 'rmk/src/split/driver.rs',
            'rmk/src/processor/mod.rs', 'rmk/src/input_device/battery.rs',
            'rmk-types/src/battery.rs', 'rmk-types/src/ble.rs', 'rmk-types/src/connection.rs',
            'rmk/src/event/connection.rs', 'rmk/src/event/split.rs',
            'rmk/src/event/state.rs', 'rmk/src/event/battery.rs',
            'scripts/test_k04_led_legacy.py', 'scripts/fixtures/k04_led_legacy.rs',
        ]
        manifest = {p: hashlib.sha256((ROOT / p).read_bytes()).hexdigest() for p in paths}
        (args.evidence_dir / 'source-hashes.json').write_text(json.dumps(manifest, indent=2) + '\n')
    failed = []
    owners = [args.owner] if args.owner else ['layer_led', 'qube_layer_led']
    with tempfile.TemporaryDirectory(prefix='k04-led-owner-') as directory:
        for name in owners:
            text = harness(name)
            path = Path(directory) / 'owner.rs'
            path.write_text(text)
            if args.evidence_dir:
                args.evidence_dir.mkdir(parents=True, exist_ok=True)
                (args.evidence_dir / f'{name}.rs').write_text(text)
            print(f'OWNER {name}; harness sha256={hashlib.sha256(text.encode()).hexdigest()}', flush=True)
            binary = Path(directory) / 'owner'
            feature = 'standalone' if name == 'layer_led' else 'qube-half'
            subprocess.run(['rustc', '--edition=2024', '--cfg', 'feature="_ble"', '--cfg', f'feature="{feature}"', '--test', str(path), '-o', str(binary)], check=True)
            result = subprocess.run([str(binary), args.filter, '--test-threads=1'], stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
            print(result.stdout, flush=True)
            assert re.search(r'test result: .*?\b[1-9]\d* (?:passed|failed)', result.stdout), 'No tests executed'
            if result.returncode:
                failed.append(name)
    if failed:
        raise SystemExit('LED owner assertions failed: ' + ', '.join(failed))


if __name__ == '__main__':
    main()

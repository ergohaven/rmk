#!/usr/bin/env python3
"""Run real touchpad init/cadence/suspend owners with inert I2C and a host clock.

This checks selected-board policy, not physical sensor latency or USB delivery.
Compiler outputs and temporary files must use the agent-storage wrapper.
"""
import os
import re
import subprocess
import tempfile
from pathlib import Path

from test_k04_led_legacy import block

ROOT = Path(__file__).resolve().parents[1]
source = (ROOT / 'keyboards/k04/src/touchpad.rs').read_text()
constants = '\n'.join(line for line in source.splitlines() if line.startswith('const '))
methods = '\n'.join(block(source, 'async fn ' + name + '(')
                    for name in ['init', 'enter_sleep', 'resume_from_sleep', 'deactivate'])
poll = block(source, 'fn touch_poll_interval(')
# A policy must not move selection/suspend admission out of the shared run loop.
loop = block(source, 'async fn run_loop(')
assert 'if !selection.allows(module_settings::ModuleSelection::Touchpad)' in loop
assert 'self.deactivate().await;' in loop
assert 'module_settings::module_sleeping()' in loop
assert 'self.enter_sleep().await;' in loop

harness = r'''
#![allow(dead_code)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
struct Duration(u64);
impl Duration {
    const fn from_millis(value: u64) -> Self { Self(value) }
    const fn from_micros(value: u64) -> Self { Self(value / 1000) }
    const fn from_secs(value: u64) -> Self { Self(value * 1000) }
    fn as_millis(self) -> u64 { self.0 }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Instant(u64);
impl Instant {
    const MIN: Self = Self(0);
    fn now() -> Self { Self(1000) }
}
impl std::ops::Add<Duration> for Instant {
    type Output = Self;
    fn add(self, duration: Duration) -> Self { Self(self.0 + duration.0) }
}
struct Timer;
impl Timer { async fn after(_: Duration) {} }
mod board { @BOARD@ }
@CONSTANTS@
#[derive(Default)]
struct Touchpad {
    ready: bool,
    suspended: bool,
    read_failures: u8,
    next_probe: Instant,
    next_poll: Instant,
    last_report: Instant,
    poll_interval: Duration,
    last_activity: Instant,
    unavailable_since: Option<Instant>,
    writes: Vec<(u16, u16)>,
    suspend_requests: Vec<bool>,
    motion_clears: usize,
}
impl Touchpad {
    async fn read_u16(&mut self, _: u16) -> Option<u16> { Some(42) }
    async fn write_u16(&mut self, address: u16, value: u16) -> bool {
        self.writes.push((address, value)); true
    }
    async fn write_u8(&mut self, address: u16, value: u8) -> bool {
        self.writes.push((address, u16::from(value))); true
    }
    async fn end_session(&mut self) -> bool { true }
    async fn set_suspend(&mut self, suspend: bool) -> bool {
        self.suspend_requests.push(suspend); true
    }
    fn clear_motion_state(&mut self) { self.motion_clears += 1; }
    @METHODS@
}
@POLL@
fn run<T>(future: impl std::future::Future<Output = T>) -> T {
    let mut future = std::pin::pin!(future);
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    match future.as_mut().poll(&mut context) {
        std::task::Poll::Ready(result) => result,
        std::task::Poll::Pending => panic!("inert boundary must not block"),
    }
}
#[test]
fn selected_pad_keeps_the_boards_idle_polling_policy() {
    for (idle_ms, k04_ms) in [(0, 8), (999, 8), (1000, 40), (5999, 40),
                              (6000, 160), (25999, 160), (26000, 160), (3600000, 160)] {
        let expected = if cfg!(phenom) { 8 } else { k04_ms };
        assert_eq!(touch_poll_interval(Duration::from_millis(idle_ms)).as_millis(),
                   expected, "idle {idle_ms} ms");
    }
}
#[test]
fn controller_idle_rates_and_low_power_admission_match_the_board() {
    let mut pad = Touchpad::default();
    assert!(run(pad.init()));
    assert!(pad.ready && !pad.suspended);
    let value = |address| pad.writes.iter().find(|(reg, _)| *reg == address).unwrap().1;
    assert_eq!(value(REG_REPORT_RATE_ACTIVE), 8);
    assert_eq!(value(REG_REPORT_RATE_IDLE_TOUCH), 8);
    assert_eq!(value(REG_REPORT_RATE_IDLE), if cfg!(phenom) { 8 } else { 40 });
    assert_eq!(value(REG_IDLE_MODE_TIMEOUT), if cfg!(phenom) { 255 } else { 5 });
    assert_eq!(value(REG_REPORT_RATE_LP1), 160);
    assert_eq!(value(REG_REPORT_RATE_LP2), 160);
    assert_eq!(pad.next_poll, Instant(1008));
}
#[test]
fn explicit_suspend_and_module_disablement_still_work() {
    let mut pad = Touchpad::default();
    assert!(run(pad.init()));
    run(pad.enter_sleep());
    assert!(pad.suspended);
    assert_eq!(pad.next_poll, Instant::MIN);
    assert_eq!(pad.suspend_requests, [true]);
    run(pad.resume_from_sleep());
    assert!(pad.ready && !pad.suspended);
    assert_eq!(pad.suspend_requests, [true, false]);
    assert_eq!(pad.next_poll, Instant::now() + TOUCH_WAKE_SETTLE);
    run(pad.deactivate());
    assert!(!pad.ready && pad.suspended);
    assert_eq!(pad.suspend_requests, [true, false, true]);
    assert_eq!(pad.next_poll, Instant::MIN);
    assert!(pad.motion_clears >= 4);
}
'''
channel = re.search(r'channel\s*=\s*"([^"\n]+)"', (ROOT / 'rust-toolchain.toml').read_text())
assert channel, 'canonical stable toolchain is missing'
toolchain = os.environ.get('RMK_STABLE_TOOLCHAIN') or channel.group(1)
with tempfile.TemporaryDirectory(prefix='touchpad-board-policy-') as directory:
    output = Path(directory)
    for board in ['k04', 'phenom']:
        policy = '\n'.join(line for line in (ROOT / f'keyboards/{board}/src/board.rs').read_text().splitlines()
                           if line.startswith('pub const TOUCHPAD_POWER_SAVING:'))
        fixture = harness.replace('@BOARD@', policy).replace('@CONSTANTS@', constants)
        fixture = fixture.replace('@METHODS@', methods).replace('@POLL@', poll)
        (output / 'test.rs').write_text(fixture)
        command = ['rustup', 'run', toolchain, 'rustc', '--edition=2021', '--test',
                   str(output / 'test.rs'), '-o', str(output / 'test')]
        if board == 'phenom':
            command += ['--cfg', 'phenom']
        subprocess.run(command, check=True)
        subprocess.run([str(output / 'test')], check=True)

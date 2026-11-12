// Source-backed owner tests; PWM, clock, transport/snapshots are host boundaries.
use super::*;
use crate::event::*;
use crate::types::battery::*;
use crate::types::ble::*;
use crate::types::connection::*;

#[test]
fn idle_qube_half_stays_dark_without_usb_but_powered_layer_remains_visible() {
    let mut led = fresh();
    run(led.deliver(Event::LayerChangeEvent(LayerChangeEvent(1))));
    event::set_split(SplitConnectionState::Idle);
    run(led.deliver(Event::SplitConnectionStateEvent(SplitConnectionStateEvent(
        SplitConnectionState::Idle,
    ))));
    assert_eq!(
        rgb(&led),
        if cfg!(feature = "qube-half") {
            (0, 0, 0)
        } else {
            (255, 0, 0)
        }
    );
    battery_nrf::USB.store(true, Ordering::Relaxed);
    at(10);
    run(led.on_deadline());
    assert_eq!(rgb(&led), (255, 0, 0));
}

#[test]
fn explicit_profile_and_battery_requests_restart_their_one_second_window() {
    for request in [0, 1] {
        let mut led = fresh();
        battery(&mut led, Some(40));
        at(100);
        if request == 0 {
            host(&mut led, status(0, BleState::Connected, UsbState::Disabled));
        } else {
            refresh(&mut led);
        }
        at(500);
        if request == 0 {
            host(&mut led, status(0, BleState::Connected, UsbState::Disabled));
        } else {
            refresh(&mut led);
        }
        at(1499);
        run(led.on_deadline());
        assert_eq!(
            rgb(&led),
            if request == 0 { (255, 0, 0) } else { (255, 80, 0) },
            "last explicit request owns a full second"
        );
        at(1500);
        run(led.on_deadline());
        assert_eq!(rgb(&led), (0, 0, 0));
    }
}

#[test]
fn powered_sleep_expires_overlays_and_retains_usb_background() {
    let mut led = fresh();
    battery_nrf::USB.store(true, Ordering::Relaxed);
    battery(&mut led, Some(94));
    at(100);
    host(&mut led, status(0, BleState::Connected, UsbState::Disabled));
    run(led.deliver(Event::SleepStateEvent(SleepStateEvent(true))));
    at(1099);
    run(led.on_deadline());
    assert_eq!(rgb(&led), (255, 0, 0));
    at(1100);
    run(led.on_deadline());
    assert_eq!(rgb(&led), (255, 180, 0));
    assert_eq!(led.deadline(), Some(Instant(1110)));
}

#[test]
fn sticky_split_snapshot_recovers_a_missed_connection_event_once() {
    let _ = fresh();
    event::set_split(SplitConnectionState::Searching);
    let mut led = LayerLed::new(SequencePwm::default(), 1);
    run(led.on_deadline());
    assert_eq!(rgb(&led), (255, 180, 0));
    at(100);
    event::set_split(SplitConnectionState::Connected);
    run(led.on_deadline());
    assert_eq!(rgb(&led), (0, 255, 0));
    at(1099);
    run(led.on_deadline());
    assert_eq!(rgb(&led), (0, 255, 0));
    at(1100);
    run(led.on_deadline());
    assert_eq!(rgb(&led), (0, 0, 0));
    at(2000);
    run(led.on_deadline());
    assert_eq!(rgb(&led), (0, 0, 0));
}

#[test]
fn powered_nonbase_layer_is_visible_even_without_a_host_connection() {
    for sleeping in [false, true] {
        let mut led = fresh();
        host(&mut led, status(0, BleState::Inactive, UsbState::Disabled));
        battery_nrf::USB.store(true, Ordering::Relaxed);
        run(led.deliver(Event::LayerChangeEvent(LayerChangeEvent(1))));
        run(led.deliver(Event::SleepStateEvent(SleepStateEvent(sleeping))));
        assert_eq!(
            rgb(&led),
            (255, 0, 0),
            "nonbase layer while USB powered, sleeping={sleeping}"
        );
        at(10_000);
        run(led.on_deadline());
        assert_eq!(rgb(&led), (255, 0, 0));
    }
}

#[test]
fn forwarded_repeat_profile_selection_is_not_lost_on_the_other_half() {
    let mut led = fresh();
    at(100);
    let current = state::current_connection_status();
    forward(&mut led, SplitMessage::ConnectionStatus(current));
    assert_eq!(rgb(&led), (255, 0, 0), "repeated BT0 must reach the LED subscriber");
    at(1100);
    run(led.on_deadline());
    assert_eq!(rgb(&led), (0, 0, 0));
}

#[test]
fn forwarded_host_usb_and_battery_refresh_follow_real_peripheral_receive_arms() {
    let mut led = fresh();
    let mut s = status(0, BleState::Inactive, UsbState::Enabled);
    s.preferred = ConnectionType::Usb;
    forward(&mut led, SplitMessage::ConnectionStatus(s));
    at(100);
    s.usb = UsbState::Configured;
    let response = forward(&mut led, SplitMessage::ConnectionStatus(s));
    assert!(matches!(
        response.split_driver.0.as_slice(),
        [SplitMessage::BatteryStatus(BatteryStatusEvent(
            BatteryStatus::Unavailable
        ))]
    ));
    assert_eq!(rgb(&led), (0, 255, 0));
    at(1100);
    run(led.on_deadline());
    assert_eq!(rgb(&led), (0, 0, 0));
    battery(&mut led, Some(40));
    at(1200);
    forward(&mut led, SplitMessage::BatteryRefresh);
    assert_eq!(rgb(&led), (255, 80, 0));
    at(2200);
    run(led.on_deadline());
    forward(
        &mut led,
        SplitMessage::BleAdvertisingMode(BleAdvertisingMode::Reconnecting),
    );
    // USB output suppresses unrelated host BLE advertising.
    s.ble.state = BleState::Advertising;
    forward(&mut led, SplitMessage::ConnectionStatus(s));
    forward(&mut led, SplitMessage::Layer(1));
    assert_eq!(rgb(&led), (255, 0, 0));
    forward(&mut led, SplitMessage::SleepState(true));
    assert_eq!(rgb(&led), (0, 0, 0));
    assert_eq!(led.deadline(), None);
}

#[test]
fn initial_host_advertising_snapshot_is_used_before_the_first_timer_tick() {
    let _ = fresh();
    state::STATUS.with(|s| s.set(status(3, BleState::Advertising, UsbState::Disabled)));
    state::MODE.with(|s| s.set(BleAdvertisingMode::Reconnecting));
    let mut led = LayerLed::new(SequencePwm::default(), 1);
    run(led.deliver(Event::LayerChangeEvent(LayerChangeEvent(0))));
    assert_eq!(
        rgb(&led),
        (255, 255, 255),
        "first event must use the forwarded host snapshot"
    );
    at(180);
    run(led.on_deadline());
    assert_eq!(rgb(&led), (0, 0, 0));
}

#[test]
fn initial_local_battery_cache_answers_refresh_without_waiting_for_another_adc_event() {
    let _ = fresh();
    input_device::battery::STATUS.with(|s| {
        s.set(BatteryStatus::Available {
            charge_state: ChargeState::Unknown,
            level: Some(21),
        })
    });
    let mut led = LayerLed::new(SequencePwm::default(), 1);
    at(100);
    refresh(&mut led);
    assert_eq!(rgb(&led), (255, 80, 0));
    at(1099);
    run(led.on_deadline());
    assert_eq!(rgb(&led), (255, 80, 0));
    at(1100);
    run(led.on_deadline());
    assert_eq!(rgb(&led), (0, 0, 0));
}

#[test]
fn custom_profile_packet_colors_are_preserved() {
    let mut led = fresh();
    let mut packet = [0u8; 27];
    packet[0] = if cfg!(feature = "standalone") { 10 } else { 9 } | 0x80;
    packet[1..6].copy_from_slice(&[8, 19, 16, 2, 6]);
    run(led.deliver(Event::PeripheralSettingsEvent(PeripheralSettingsEvent(packet))));
    at(100);
    host(&mut led, status(0, BleState::Connected, UsbState::Disabled));
    assert_eq!(rgb(&led), (0, 255, 0));
    at(1100);
    run(led.on_deadline());
    at(1200);
    host(&mut led, status(1, BleState::Inactive, UsbState::Disabled));
    assert_eq!(rgb(&led), (255, 0, 255));
}

#[test]
fn each_halfs_battery_event_changes_only_its_own_owner() {
    let mut left = fresh();
    let mut right = LayerLed::new(SequencePwm::default(), 1);
    run(right.on_deadline());
    battery(&mut left, Some(20));
    battery(&mut right, Some(75));
    at(100);
    refresh(&mut left);
    refresh(&mut right);
    assert_eq!(rgb(&left), (255, 0, 0));
    assert_eq!(rgb(&right), (0, 255, 0));
}

fn host(led: &mut LayerLed, s: ConnectionStatus) {
    state::STATUS.with(|snap| snap.set(s));
    run(led.deliver(Event::ConnectionStatusChangeEvent(ConnectionStatusChangeEvent(s))));
}

fn mode(led: &mut LayerLed, m: BleAdvertisingMode) {
    state::MODE.with(|snap| snap.set(m));
    run(led.deliver(Event::BleAdvertisingModeEvent(BleAdvertisingModeEvent(m))));
}

fn refresh(led: &mut LayerLed) {
    run(led.deliver(Event::PeripheralBatteryRefreshEvent(PeripheralBatteryRefreshEvent)));
}

#[test]
fn profile_selection_is_one_second_even_while_host_is_inactive_or_advertising() {
    let colors = [(255, 0, 0), (0, 0, 255), (255, 255, 0), (0, 255, 0), (255, 0, 255)];
    for (profile, color) in colors.into_iter().enumerate() {
        let mut led = fresh();
        at(100);
        // Identical status is the real repeat-selection notification for BT0.
        host(
            &mut led,
            status(
                profile as u8,
                if profile == 0 {
                    BleState::Connected
                } else {
                    BleState::Inactive
                },
                UsbState::Disabled,
            ),
        );
        assert_eq!(rgb(&led), color, "selected BT{profile}");
        at(200);
        host(
            &mut led,
            status(profile as u8, BleState::Advertising, UsbState::Disabled),
        );
        assert_eq!(rgb(&led), color, "advertising must not hide profile selection");
        tick(&mut led, 1099);
        assert_eq!(rgb(&led), color, "one second minus 1ms");
        at(1100);
        run(led.on_deadline());
        assert_eq!(
            rgb(&led),
            (0, 180, 255),
            "expired at exactly one second; pairing resumes"
        );
    }
}

#[test]
fn battery_refresh_displays_local_threshold_for_one_second_over_usb_and_host_state() {
    let cases = [
        (0, (255, 0, 0)),
        (20, (255, 0, 0)),
        (21, (255, 80, 0)),
        (40, (255, 80, 0)),
        (41, (255, 220, 0)),
        (74, (255, 220, 0)),
        (75, (0, 255, 0)),
        (100, (0, 255, 0)),
    ];
    for (level, color) in cases {
        for usb in [false, true] {
            let mut led = fresh();
            battery_nrf::USB.store(usb, Ordering::Relaxed);
            host(&mut led, status(0, BleState::Advertising, UsbState::Disabled));
            battery(&mut led, Some(level));
            at(100);
            refresh(&mut led);
            assert_eq!(rgb(&led), color, "local={level}, USB={usb}");
            tick(&mut led, 1099);
            assert_eq!(rgb(&led), color);
            at(1100);
            run(led.on_deadline());
            assert_eq!(
                rgb(&led),
                if usb {
                    if level >= 95 { (0, 255, 0) } else { (255, 180, 0) }
                } else {
                    (0, 180, 255)
                }
            );
        }
    }
}

#[test]
fn pending_battery_refresh_waits_for_a_real_local_measurement() {
    let mut led = fresh();
    at(100);
    refresh(&mut led);
    assert_eq!(rgb(&led), (0, 0, 0));
    at(500);
    battery(&mut led, Some(40));
    assert_eq!(rgb(&led), (255, 80, 0));
    tick(&mut led, 1499);
    assert_eq!(rgb(&led), (255, 80, 0));
    tick(&mut led, 1509);
    assert_eq!(rgb(&led), (0, 0, 0));
}

#[test]
fn low_battery_is_120ms_red_every_two_seconds_only_without_usb() {
    for level in [0, 20, 21] {
        for usb in [false, true] {
            let mut led = fresh();
            host(&mut led, status(0, BleState::Inactive, UsbState::Disabled));
            battery(&mut led, Some(level));
            battery_nrf::USB.store(usb, Ordering::Relaxed);
            tick(&mut led, 1999);
            let background = if usb { (255, 180, 0) } else { (0, 0, 0) };
            assert_eq!(rgb(&led), background);
            // Timer callbacks are quantized to 10ms, so drive the exact due boundary.
            at(2000);
            run(led.on_deadline());
            let pulsing = level <= 20 && !usb;
            assert_eq!(
                rgb(&led),
                if pulsing { (255, 0, 0) } else { background },
                "level={level},USB={usb}"
            );
            at(2119);
            run(led.on_deadline());
            assert_eq!(rgb(&led), if pulsing { (255, 0, 0) } else { background });
            at(2120);
            run(led.on_deadline());
            assert_eq!(rgb(&led), background);
            at(3999);
            run(led.on_deadline());
            assert_eq!(rgb(&led), background);
            at(4000);
            run(led.on_deadline());
            assert_eq!(rgb(&led), if pulsing { (255, 0, 0) } else { background });
            if pulsing {
                battery_nrf::USB.store(true, Ordering::Relaxed);
                at(4010);
                run(led.on_deadline());
                assert_eq!(rgb(&led), (255, 180, 0), "USB suppresses an already active low pulse");
            }
        }
    }
}

#[test]
fn pairing_reconnect_and_split_search_keep_legacy_blink_boundaries() {
    let mut led = fresh();
    host(&mut led, status(0, BleState::Advertising, UsbState::Disabled));
    for (m, color) in [
        (BleAdvertisingMode::Pairing, (0, 180, 255)),
        (BleAdvertisingMode::Reconnecting, (255, 255, 255)),
    ] {
        at(0);
        mode(&mut led, m);
        for (ms, expected) in [
            (0, color),
            (179, color),
            (180, (0, 0, 0)),
            (359, (0, 0, 0)),
            (360, color),
        ] {
            at(ms);
            run(led.on_deadline());
            assert_eq!(rgb(&led), expected, "mode={m:?},ms={ms}");
        }
    }
    at(1000);
    battery_nrf::USB.store(true, Ordering::Relaxed);
    event::set_split(SplitConnectionState::Searching);
    run(led.deliver(Event::SplitConnectionStateEvent(SplitConnectionStateEvent(
        SplitConnectionState::Searching,
    ))));
    for (ms, expected) in [
        (1000, (255, 180, 0)),
        (1179, (255, 180, 0)),
        (1180, (0, 0, 0)),
        (1359, (0, 0, 0)),
        (1360, (255, 180, 0)),
    ] {
        at(ms);
        run(led.on_deadline());
        assert_eq!(rgb(&led), expected, "split ms={ms}");
    }
}

#[test]
fn host_and_split_success_are_green_for_exactly_one_second() {
    for kind in [0, 1, 2] {
        let mut led = fresh();
        at(100);
        match kind {
            0 => {
                host(&mut led, status(0, BleState::Advertising, UsbState::Disabled));
                host(&mut led, status(0, BleState::Connected, UsbState::Disabled));
            }
            1 => {
                event::set_split(SplitConnectionState::Searching);
                run(led.deliver(Event::SplitConnectionStateEvent(SplitConnectionStateEvent(
                    SplitConnectionState::Searching,
                ))));
                event::set_split(SplitConnectionState::Connected);
                run(led.deliver(Event::SplitConnectionStateEvent(SplitConnectionStateEvent(
                    SplitConnectionState::Connected,
                ))));
            }
            _ => {
                host(&mut led, status(0, BleState::Inactive, UsbState::Enabled));
                host(&mut led, status(0, BleState::Inactive, UsbState::Configured));
            }
        }
        assert_eq!(rgb(&led), (0, 255, 0), "connection kind={kind}");
        at(1099);
        run(led.on_deadline());
        assert_eq!(rgb(&led), (0, 255, 0));
        at(1100);
        run(led.on_deadline());
        assert_eq!(rgb(&led), (0, 0, 0));
    }
}

fn at(ms: u64) {
    CLOCK.store(ms, Ordering::Relaxed);
}

fn status(profile: u8, ble: BleState, usb: UsbState) -> ConnectionStatus {
    ConnectionStatus {
        usb,
        ble: BleStatus {
            profile,
            state: ble,
            bonded: false,
        },
        preferred: ConnectionType::Ble,
    }
}

fn fresh() -> LayerLed {
    at(0);
    module_settings::reset();
    battery_nrf::USB.store(false, Ordering::Relaxed);
    event::set_split(SplitConnectionState::Connected);
    state::STATUS.with(|s| s.set(status(0, BleState::Connected, UsbState::Disabled)));
    state::MODE.with(|s| s.set(BleAdvertisingMode::Pairing));
    input_device::battery::STATUS.with(|s| s.set(BatteryStatus::Unavailable));
    OUTPUT.with(|s| s.borrow_mut().clear());
    let mut led = LayerLed::new(SequencePwm::default(), 0);
    run(led.on_deadline());
    led
}

fn tick(led: &mut LayerLed, ms: u64) {
    at(ms);
    if led.deadline().is_some_and(|due| due <= Instant::now()) {
        run(led.on_deadline());
    }
}

fn rgb(led: &LayerLed) -> (u8, u8, u8) {
    let c = led.current_color.expect("owner has not painted");
    (c.r, c.g, c.b)
}

fn battery(led: &mut LayerLed, level: Option<u8>) {
    let b = BatteryStatus::Available {
        charge_state: ChargeState::Unknown,
        level,
    };
    input_device::battery::STATUS.with(|s| s.set(b));
    run(led.deliver(Event::BatteryStatusEvent(BatteryStatusEvent(b))));
}

#[test]
fn usb_threshold_uses_this_halfs_available_battery_and_nonbase_layer_wins() {
    for level in [
        None,
        Some(0),
        Some(20),
        Some(21),
        Some(40),
        Some(41),
        Some(74),
        Some(75),
        Some(94),
        Some(95),
        Some(100),
    ] {
        let mut led = fresh();
        battery_nrf::USB.store(true, Ordering::Relaxed);
        battery(&mut led, level);
        tick(&mut led, 10);
        assert_eq!(
            rgb(&led),
            if level.is_some_and(|v| v >= 95) {
                (0, 255, 0)
            } else {
                (255, 180, 0)
            },
            "level={level:?}"
        );
        run(led.deliver(Event::LayerChangeEvent(LayerChangeEvent(1))));
        assert_eq!(rgb(&led), (255, 0, 0), "nonbase beats USB");
        module_settings::charge(false);
        run(led.deliver(Event::LayerChangeEvent(LayerChangeEvent(0))));
        assert_eq!(rgb(&led), (0, 0, 0), "QSID331 disables USB indicator");
    }
}

#[test]
fn sleep_without_usb_stops_led_timer_and_wake_rearms_immediately() {
    let mut led = fresh();
    run(led.deliver(Event::LayerChangeEvent(LayerChangeEvent(1))));
    at(100);
    run(led.deliver(Event::SleepStateEvent(SleepStateEvent(true))));
    assert_eq!(rgb(&led), (0, 0, 0));
    assert_eq!(led.deadline(), None);
    let writes = OUTPUT.with(|out| out.borrow().len());
    tick(&mut led, 10_000);
    assert_eq!(OUTPUT.with(|out| out.borrow().len()), writes);
    run(led.deliver(Event::SleepStateEvent(SleepStateEvent(false))));
    assert_eq!(led.deadline(), Some(Instant(10_000)));
    assert_eq!(rgb(&led), (255, 0, 0));
    tick(&mut led, 10_000);
    assert_eq!(led.deadline(), Some(Instant(10_010)));
}

#[test]
fn sleep_usb_retains_threshold_and_nonbase_color_and_tracks_removal() {
    for level in [None, Some(94), Some(95), Some(100)] {
        let mut led = fresh();
        battery_nrf::USB.store(true, Ordering::Relaxed);
        battery(&mut led, level);
        run(led.deliver(Event::SleepStateEvent(SleepStateEvent(true))));
        assert_eq!(
            rgb(&led),
            if level.is_some_and(|v| v >= 95) {
                (0, 255, 0)
            } else {
                (255, 180, 0)
            },
            "USB sleep level={level:?}"
        );
        assert!(
            led.deadline().is_some(),
            "powered sleeping owner must keep observing changes"
        );
        run(led.deliver(Event::LayerChangeEvent(LayerChangeEvent(1))));
        tick(&mut led, 1010);
        assert_eq!(rgb(&led), (255, 0, 0));
        battery_nrf::USB.store(false, Ordering::Relaxed);
        // A tick already armed before removal may run once; after it, stop.
        at(1020);
        run(led.on_deadline());
        assert_eq!(rgb(&led), (0, 0, 0));
        assert_eq!(led.deadline(), None);
        // A periodic battery event is the existing no-USB-sleep wake boundary.
        battery_nrf::USB.store(true, Ordering::Relaxed);
        at(15_000);
        battery(&mut led, level);
        assert_eq!(rgb(&led), (255, 0, 0));
        assert!(led.deadline().is_some());
    }
}

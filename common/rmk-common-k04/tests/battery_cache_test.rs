use rmk::input_device::battery::{current_local_battery_percent, set_local_battery_percent};

#[test]
fn current_local_battery_percent_uses_cached_adc_measurement() {
    assert_eq!(current_local_battery_percent(), None);

    set_local_battery_percent(87);

    assert_eq!(current_local_battery_percent(), Some(87));

    set_local_battery_percent(0);

    assert_eq!(current_local_battery_percent(), Some(0));
}

//! Display names for host Bluetooth profiles.
use core::fmt::Write;

use heapless::String;
use rmk_types::ble::BLE_ADV_NAME_MAX_LEN;

pub(super) fn profile_name(product: &str, profile: u8) -> String<BLE_ADV_NAME_MAX_LEN> {
    let mut suffix = String::<6>::new();
    write!(suffix, " BT{profile}").unwrap();
    let mut end = product.len().min(BLE_ADV_NAME_MAX_LEN - suffix.len());
    while !product.is_char_boundary(end) {
        end -= 1;
    }
    let mut name = String::new();
    name.push_str(&product[..end]).unwrap();
    name.push_str(&suffix).unwrap();
    name
}

#[cfg(test)]
mod tests {
    use super::profile_name;

    #[test]
    fn every_k04_profile_has_a_distinct_name() {
        for product in ["K:04", "K:04 Mini", "K:04 Micro"] {
            for profile in 0..5 {
                assert_eq!(
                    profile_name(product, profile).as_str(),
                    std::format!("{product} BT{profile}")
                );
            }
        }
    }

    #[test]
    fn gap_name_can_switch_repeatedly_without_rebuilding_the_server() {
        use embassy_sync::blocking_mutex::raw::NoopRawMutex;
        use trouble_host::prelude::*;
        let names: [_; 5] = core::array::from_fn(|i| profile_name("K:04", i as u8));
        let mut table = AttributeTable::<NoopRawMutex, 16>::new();
        GapConfig::default("K:04").build(&mut table).unwrap();
        // GAP starts the table: service, name declaration, name value.
        let name = table
            .find_characteristic_by_value_handle::<heapless::String<16>>(3)
            .unwrap();
        assert_eq!(name.uuid, characteristic::DEVICE_NAME.into());
        let server = AttributeServer::<_, DefaultPacketPool, 16, 1>::new(table);
        for i in [0, 4, 1, 3, 2, 0] {
            name.set_ro(&server, &names[i]).unwrap();
            assert_eq!(name.get(&server).unwrap(), names[i]);
        }
    }

    #[test]
    fn long_utf8_names_keep_the_suffix_and_fit_legacy_advertising() {
        assert_eq!(profile_name("12345678901ééé", 4).as_str(), "12345678901 BT4");
        assert_eq!(profile_name("0123456789abcdef", 255).as_str(), "0123456789 BT255");
    }
}

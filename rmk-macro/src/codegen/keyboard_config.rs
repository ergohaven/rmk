use quote::quote;
use rmk_config::KeyboardTomlConfig;
use rmk_config::resolved::hardware::ChipSeries;
use rmk_config::resolved::{Hardware, Host, Identity, Keymap};

pub(crate) fn read_keyboard_toml_config() -> KeyboardTomlConfig {
    // Get the path of the keyboard config file from the environment variable
    let config_toml_path = std::env::var("KEYBOARD_TOML_PATH")
        .expect("[ERROR]: KEYBOARD_TOML_PATH should be set in `.cargo/config.toml`");

    KeyboardTomlConfig::new_from_toml_path(&config_toml_path)
}

pub(crate) fn expand_keyboard_info(keymap: &Keymap) -> proc_macro2::TokenStream {
    let num_col = keymap.cols as usize;
    let num_row = keymap.rows as usize;
    let num_layer = keymap.layers as usize;
    let total_num_encoder = keymap.num_encoder;
    quote! {
        pub(crate) const COL: usize = #num_col;
        pub(crate) const ROW: usize = #num_row;
        pub(crate) const NUM_LAYER: usize = #num_layer;
        pub(crate) const NUM_ENCODER: usize = #total_num_encoder;
    }
}

pub(crate) fn expand_device_config(
    identity: &Identity,
    hardware: &Hardware,
) -> proc_macro2::TokenStream {
    let pid = identity.product_id;
    let vid = identity.vendor_id;
    let product_name = identity.product_name.clone();
    let manufacturer = identity.manufacturer.clone();
    let serial_number_tokens = match (&identity.serial_number, &hardware.chip.series) {
        (Some(s), _) => quote! { ::rmk::config::build_serial_number(#s) },
        // nRF and ESP32 BLE defaults read the hardware ID.
        (None, ChipSeries::Nrf52 | ChipSeries::Esp32) => {
            quote! { ::rmk::config::DeviceConfig::default().serial_number }
        }
        (None, ChipSeries::Rp2040) => {
            quote! { ::rmk::config::rp2040_serial_number(p.FLASH.reborrow()) }
        }
        (None, ChipSeries::Stm32) => {
            quote! { ::rmk::config::build_serial_number(&::embassy_stm32::uid::uid()) }
        }
    };
    let device_release = configured_usb_device_release();

    quote! {
        let keyboard_device_config = ::rmk::config::DeviceConfig {
            vid: #vid,
            pid: #pid,
            manufacturer: #manufacturer,
            product_name: #product_name,
            serial_number: #serial_number_tokens,
            device_release: #device_release,
        };
    }
}

pub(crate) fn configured_usb_device_release() -> u16 {
    std::env::var("RMK_FIRMWARE_VERSION_BCD")
        .ok()
        .map(|value| {
            parse_usb_device_release(&value).unwrap_or_else(|| {
                panic!("RMK_FIRMWARE_VERSION_BCD must be a hexadecimal u16 such as 0x0109, got {value:?}")
            })
        })
        .unwrap_or(0x0010)
}

fn parse_usb_device_release(value: &str) -> Option<u16> {
    let digits = value
        .trim()
        .strip_prefix("0x")
        .or_else(|| value.trim().strip_prefix("0X"))?;
    u16::from_str_radix(digits, 16).ok()
}

/// The `[host].unlock_keys` list as a `&'static [(u8, u8)]` literal (empty when unset).
fn unlock_keys_tokens(host: &Host) -> proc_macro2::TokenStream {
    if host.unlock_keys.is_empty() {
        return quote! { &[] };
    }
    let keys_expr = host.unlock_keys.iter().map(|key| {
        let row = key[0];
        let col = key[1];
        quote! { (#row, #col) }
    });
    quote! { &[#(#keys_expr),*] }
}

/// The board's settings hook named by `RMK_VIAL_DEVICE_SETTINGS_FN`, as
/// `Some(hook())`, or `None`.
fn device_settings_tokens() -> proc_macro2::TokenStream {
    std::env::var("RMK_VIAL_DEVICE_SETTINGS_FN")
        .ok()
        .and_then(|path| syn::parse_str::<syn::Path>(&path).ok())
        .map(|path| quote! { Some(#path()) })
        .unwrap_or_else(|| quote! { None })
}

/// `RMK_FIRMWARE_VERSION` packed as `major << 16 | minor << 8 | patch`.
fn configured_firmware_version() -> u32 {
    std::env::var("RMK_FIRMWARE_VERSION")
        .ok()
        .map(|value| {
            parse_firmware_version(&value).unwrap_or_else(|| {
                panic!(
                    "RMK_FIRMWARE_VERSION must be major.minor.patch with minor and patch in 0..=255, got {value:?}"
                )
            })
        })
        .unwrap_or(rmk_types::protocol::vial::VIA_FIRMWARE_VERSION)
}

pub(crate) fn expand_vial_config(host: &Host) -> proc_macro2::TokenStream {
    if !host.vial_enabled {
        return quote! {};
    }
    let firmware_version = configured_firmware_version();
    let unlock_keys = unlock_keys_tokens(host);
    let insecure = host.insecure;
    let device_settings = device_settings_tokens();
    quote! {
        include!(concat!(env!("OUT_DIR"), "/config_generated.rs"));
        const VIAL_CONFIG: ::rmk::config::VialConfig = ::rmk::config::VialConfig {
            vial_keyboard_id: &VIAL_KEYBOARD_ID,
            vial_keyboard_def: &VIAL_KEYBOARD_DEF,
            unlock_keys: #unlock_keys,
            device_settings: #device_settings,
            insecure: #insecure,
            firmware_version: #firmware_version,
        };
    }
}

/// The board constants Ergohaven's Rynk commands serve: the same Vial id and
/// Vial JSON the board's build script writes for Vial.
pub(crate) fn expand_ergohaven_config(ergohaven: bool) -> proc_macro2::TokenStream {
    if !ergohaven {
        return quote! {};
    }
    let firmware_version = configured_firmware_version();
    let device_settings = device_settings_tokens();
    quote! {
        include!(concat!(env!("OUT_DIR"), "/config_generated.rs"));
        const ERGOHAVEN_CONFIG: ::rmk::config::ErgohavenConfig = ::rmk::config::ErgohavenConfig {
            keyboard_uid: &VIAL_KEYBOARD_ID,
            board_definition: &VIAL_KEYBOARD_DEF,
            firmware_version: #firmware_version,
            device_settings: #device_settings,
        };
    }
}

pub(crate) fn expand_lock_config(host: &Host) -> proc_macro2::TokenStream {
    if !host.rynk_enabled {
        return quote! {};
    }
    let unlock_keys = unlock_keys_tokens(host);
    let insecure = host.insecure;
    let write_requires_unlock = host.write_requires_unlock;
    quote! {
        const LOCK_CONFIG: ::rmk::config::LockConfig = ::rmk::config::LockConfig {
            unlock_keys: #unlock_keys,
            insecure: #insecure,
            write_requires_unlock: #write_requires_unlock,
        };
    }
}

fn parse_firmware_version(value: &str) -> Option<u32> {
    let mut components = value.trim().split('.');
    let major = components.next()?.parse::<u16>().ok()?;
    let minor = components.next()?.parse::<u8>().ok()?;
    let patch = components.next()?.parse::<u8>().ok()?;
    if components.next().is_some() {
        return None;
    }
    Some((u32::from(major) << 16) | (u32::from(minor) << 8) | u32::from(patch))
}

#[cfg(test)]
mod tests {
    use super::{parse_firmware_version, parse_usb_device_release};

    #[test]
    fn parses_via_runtime_firmware_version() {
        assert_eq!(parse_firmware_version("0.1.3"), Some(0x0000_0103));
        assert_eq!(parse_firmware_version("12.34.56"), Some(0x000C_2238));
    }

    #[test]
    fn rejects_invalid_runtime_firmware_versions() {
        assert_eq!(parse_firmware_version("0.1"), None);
        assert_eq!(parse_firmware_version("0.1.256"), None);
        assert_eq!(parse_firmware_version("0.1.3.4"), None);
    }

    #[test]
    fn parses_usb_device_release_bcd() {
        assert_eq!(parse_usb_device_release("0x0109"), Some(0x0109));
        assert_eq!(parse_usb_device_release("0X1234"), Some(0x1234));
    }

    #[test]
    fn rejects_invalid_usb_device_release_bcd() {
        assert_eq!(parse_usb_device_release("0109"), None);
        assert_eq!(parse_usb_device_release("0x10000"), None);
        assert_eq!(parse_usb_device_release("release"), None);
    }
}

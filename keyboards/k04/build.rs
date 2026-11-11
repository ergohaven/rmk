use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::{env, fs};

use const_gen::*;
use xz2::read::XzEncoder;

fn main() {
    let active = std::env::vars()
        .filter_map(|(name, _)| {
            name.strip_prefix("CARGO_FEATURE_")
                .map(|feature| feature.to_lowercase())
        })
        .collect::<Vec<_>>()
        .join(",");
    println!("cargo:rustc-env=RMK_BOARD_FEATURES={active}");
    let rynk = std::env::var_os("CARGO_FEATURE_PROTOCOL_RYNK").is_some();
    let vial = std::env::var_os("CARGO_FEATURE_PROTOCOL_VIAL").is_some();
    assert!(
        rynk ^ vial,
        "select exactly one protocol: protocol_rynk or protocol_vial (use --no-default-features --features vial_standalone for Vial)"
    );
    assert!(
        !vial || std::env::var_os("CARGO_FEATURE_VIAL_STANDALONE").is_some(),
        "Vial requires vial_standalone to preserve standalone build options"
    );
    // VIA and USB encode only numeric major/minor/patch components. The full
    // release identity remains embedded in the selected Vial JSON.
    const STANDALONE_RELEASE_VERSION: &str = "0.2.0";
    const STANDALONE_FIRMWARE_VERSION: &str = "0.2.0";
    const STANDALONE_FIRMWARE_VERSION_BCD: &str = "0x0200";
    const QUBE_FIRMWARE_VERSION: &str = "0.2.0";
    const QUBE_FIRMWARE_VERSION_BCD: &str = "0x0200";

    let vial_path = configured_path("VIAL_JSON_PATH", "vial.json");
    let keyboard_path = configured_path("KEYBOARD_TOML_PATH", "keyboard.toml");

    println!("cargo:rerun-if-env-changed=VIAL_JSON_PATH");
    println!("cargo:rerun-if-env-changed=KEYBOARD_TOML_PATH");
    println!("cargo:rerun-if-changed={}", vial_path.display());
    println!("cargo:rerun-if-changed={}", keyboard_path.display());
    println!("cargo:rerun-if-changed=memory_halves.x");
    println!("cargo:rerun-if-changed=memory_qube.x");
    let product_id = generate_vial_config(&vial_path);
    validate_keyboard_product_id(&keyboard_path, product_id);
    validate_topology_feature(product_id);
    validate_vial_firmware_version(
        &vial_path,
        if is_standalone(product_id) {
            STANDALONE_RELEASE_VERSION
        } else {
            QUBE_FIRMWARE_VERSION
        },
    );

    let (firmware_version, firmware_version_bcd) = if is_standalone(product_id) {
        (STANDALONE_FIRMWARE_VERSION, STANDALONE_FIRMWARE_VERSION_BCD)
    } else {
        (QUBE_FIRMWARE_VERSION, QUBE_FIRMWARE_VERSION_BCD)
    };

    println!("cargo:rustc-env=RMK_FIRMWARE_VERSION={firmware_version}");
    println!("cargo:rustc-env=RMK_FIRMWARE_VERSION_BCD={firmware_version_bcd}");

    if is_standalone(product_id) || env::var_os("CARGO_FEATURE_QUBE").is_some() {
        println!("cargo:rustc-env=RMK_VIAL_DEVICE_SETTINGS_FN=crate::layer_names::vial_device_settings");
    }
    if is_standalone(product_id) {
        println!("cargo:rustc-env=RMK_BLE_HOST_POWER_CONFIG_FN=crate::ble_host_power::ble_host_power_config");
    }

    let out = &PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let memory = if env::var_os("CARGO_FEATURE_QUBE").is_some() {
        include_bytes!("memory_qube.x").as_slice()
    } else {
        include_bytes!("memory_halves.x").as_slice()
    };
    File::create(out.join("memory.x")).unwrap().write_all(memory).unwrap();
    println!("cargo:rustc-link-search={}", out.display());

    println!("cargo:rustc-link-arg=--nmagic");
    println!("cargo:rustc-link-arg=-Tlink.x");
    println!("cargo:rustc-link-arg=-Tdefmt.x");
    println!("cargo:rustc-linker=flip-link");
}

fn configured_path(variable: &str, default: &str) -> PathBuf {
    env::var_os(variable)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(default))
}

fn generate_vial_config(vial_path: &Path) -> u16 {
    let out_file = Path::new(&env::var_os("OUT_DIR").unwrap()).join("config_generated.rs");

    let mut content = String::new();
    match File::open(vial_path) {
        Ok(mut file) => {
            file.read_to_string(&mut content)
                .unwrap_or_else(|e| panic!("Cannot read {}: {e}", vial_path.display()));
        }
        Err(e) => panic!("Cannot find {}: {e}", vial_path.display()),
    };

    let parsed = json::parse(&content).unwrap_or_else(|e| panic!("Cannot parse {}: {e}", vial_path.display()));
    if env::var_os("CARGO_FEATURE_UNIVERSAL_MAC_ISO").is_some() {
        assert_eq!(
            parsed["layouts"]["labels"][0][0].as_str(),
            Some("Universal Symbols geometry"),
            "universal_mac_iso requires the matching *_universal.json definition; Mini/Qube are excluded"
        );
    }
    let product_id = parsed["productId"]
        .as_str()
        .and_then(parse_hex_u16)
        .unwrap_or_else(|| panic!("{} productId must be a hexadecimal string", vial_path.display()));
    let keyboard_id: Vec<u8> = match product_id {
        // Preserve every established Standalone and Qube identity during the
        // structural crate consolidation.
        0x0071 => vec![0x80, 0x04, 0x28, 0xAB, 0x69, 0x3E, 0x19, 0x60],
        0x0072 => vec![0x80, 0x04, 0x2D, 0x7A, 0x91, 0x44, 0x3B, 0x21],
        0x0073 => vec![0x80, 0x04, b'Q', b'0', b'4', b'M', b'I', b'C'],
        0x0074 => vec![0x80, 0x04, b'K', b'0', b'4', b'F', b'U', b'L'],
        0x0075 => vec![0x80, 0x04, b'K', b'0', b'4', b'M', b'I', b'N'],
        0x0076 => vec![0x80, 0x04, b'K', b'0', b'4', b'M', b'I', b'C'],
        _ => panic!("Unsupported K:04 Series productId: 0x{product_id:04X}"),
    };

    let vial_cfg = json::stringify(parsed);
    let mut keyboard_def_compressed: Vec<u8> = Vec::new();
    XzEncoder::new(vial_cfg.as_bytes(), 6)
        .read_to_end(&mut keyboard_def_compressed)
        .unwrap();

    let const_declarations = [
        const_declaration!(pub VIAL_KEYBOARD_DEF = keyboard_def_compressed),
        const_declaration!(pub VIAL_KEYBOARD_ID = keyboard_id),
    ]
    .map(|s| "#[allow(clippy::redundant_static_lifetimes)]\n".to_owned() + s.as_str())
    .join("\n");
    fs::write(out_file, const_declarations).unwrap();

    product_id
}

fn validate_keyboard_product_id(keyboard_path: &Path, expected: u16) {
    let content =
        fs::read_to_string(keyboard_path).unwrap_or_else(|e| panic!("Cannot read {}: {e}", keyboard_path.display()));
    let actual = content
        .lines()
        .filter_map(|line| line.split_once('='))
        .find_map(|(key, value)| (key.trim() == "product_id").then(|| value.trim()))
        .and_then(parse_hex_u16)
        .unwrap_or_else(|| panic!("{} product_id must be a hexadecimal integer", keyboard_path.display()));

    assert_eq!(
        actual,
        expected,
        "{} and selected Vial definition have different product IDs",
        keyboard_path.display()
    );
}

fn validate_vial_firmware_version(vial_path: &Path, expected: &str) {
    let content = fs::read_to_string(vial_path).unwrap_or_else(|e| panic!("Cannot read {}: {e}", vial_path.display()));
    let parsed = json::parse(&content).unwrap_or_else(|e| panic!("Cannot parse {}: {e}", vial_path.display()));
    assert_eq!(
        parsed["firmware"]["version"].as_str(),
        Some(expected),
        "{} firmware.version must match the selected release",
        vial_path.display()
    );
    assert_eq!(
        parsed["firmwareVersion"].as_str(),
        Some(expected),
        "{} firmwareVersion must match the selected release",
        vial_path.display()
    );
}

fn validate_topology_feature(product_id: u16) {
    if env::var_os("CARGO_FEATURE_QUBE").is_some() && !is_qube(product_id) {
        panic!("The qube feature requires a K:04 Qube profile, got productId 0x{product_id:04X}");
    }
}

fn parse_hex_u16(value: &str) -> Option<u16> {
    let value = value.trim().trim_matches('"');
    let value = value.strip_prefix("0x").or_else(|| value.strip_prefix("0X"))?;
    u16::from_str_radix(value, 16).ok()
}

fn is_qube(product_id: u16) -> bool {
    matches!(product_id, 0x0071..=0x0073)
}

fn is_standalone(product_id: u16) -> bool {
    matches!(product_id, 0x0074..=0x0076)
}

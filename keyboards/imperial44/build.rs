use const_gen::*;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::{env, fs};
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
    assert!(
        std::env::var_os("CARGO_FEATURE_UNIVERSAL_MAC_ISO").is_none() || vial,
        "universal_mac_iso is only supported by the Vial standalone profile"
    );
    // Test-only cache-busting version for the runtime encoder settings build.
    const FIRMWARE_VERSION: &str = "0.2.0";
    const FIRMWARE_VERSION_BCD: &str = "0x0200";

    println!("cargo:rerun-if-changed=vial.json");
    println!("cargo:rerun-if-changed=keyboard.toml");
    println!("cargo:rustc-env=RMK_FIRMWARE_VERSION={FIRMWARE_VERSION}");
    println!("cargo:rustc-env=RMK_FIRMWARE_VERSION_BCD={FIRMWARE_VERSION_BCD}");
    println!("cargo:rustc-env=RMK_VIAL_DEVICE_SETTINGS_FN=crate::encoder_device_settings::vial_device_settings");
    println!("cargo:rustc-env=RMK_BLE_HOST_POWER_CONFIG_FN=crate::ble_host_power::ble_host_power_config");

    generate_vial_config();

    let out = &PathBuf::from(env::var_os("OUT_DIR").unwrap());
    File::create(out.join("memory.x"))
        .unwrap()
        .write_all(include_bytes!("memory.x"))
        .unwrap();
    println!("cargo:rustc-link-search={}", out.display());

    println!("cargo:rerun-if-changed=memory.x");
    println!("cargo:rustc-link-arg=--nmagic");
    println!("cargo:rustc-link-arg=-Tlink.x");
    println!("cargo:rustc-link-arg=-Tdefmt.x");
    println!("cargo:rustc-linker=flip-link");
}

fn generate_vial_config() {
    let out_file = Path::new(&env::var_os("OUT_DIR").unwrap()).join("config_generated.rs");

    let path = env::var_os("VIAL_JSON_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("vial.json"));
    println!("cargo:rerun-if-env-changed=VIAL_JSON_PATH");
    println!("cargo:rerun-if-changed={}", path.display());
    let p = path.as_path();
    let mut content = String::new();
    match File::open(p) {
        Ok(mut file) => {
            file.read_to_string(&mut content).expect("Cannot read vial.json");
        }
        Err(e) => println!("Cannot find vial.json {:?}: {}", p, e),
    };

    let parsed = json::parse(&content).unwrap();
    if env::var_os("CARGO_FEATURE_UNIVERSAL_MAC_ISO").is_some() {
        assert_eq!(
            parsed["layouts"]["labels"][0][0].as_str(),
            Some("Universal Symbols geometry"),
            "universal_mac_iso requires the matching *_universal.json definition"
        );
    }
    let vial_cfg = json::stringify(parsed);
    let mut keyboard_def_compressed: Vec<u8> = Vec::new();
    XzEncoder::new(vial_cfg.as_bytes(), 6)
        .read_to_end(&mut keyboard_def_compressed)
        .unwrap();

    let keyboard_id: Vec<u8> = vec![0xB9, 0xBC, 0x09, 0xB2, 0x9D, 0x37, 0x4C, 0xEA];
    let const_declarations = [
        const_declaration!(pub VIAL_KEYBOARD_DEF = keyboard_def_compressed),
        const_declaration!(pub VIAL_KEYBOARD_ID = keyboard_id),
    ]
    .map(|s| "#[allow(clippy::redundant_static_lifetimes)]\n".to_owned() + s.as_str())
    .join("\n");
    fs::write(out_file, const_declarations).unwrap();
}

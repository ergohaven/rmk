//! This build script chooses the Qube or half-board memory layout and writes it
//! as `memory.x` into a directory where the linker can always find it at build time.
//! For many projects this is optional, as the linker always searches the
//! project root directory -- wherever `Cargo.toml` is. However, if you
//! are using a workspace or have a more complicated build setup, this
//! build script becomes required. Additionally, by requesting that
//! Cargo re-run the build script whenever the memory files are changed,
//! updating those files ensures a rebuild of the application with the
//! new memory settings.
//!
//! The build script also sets the linker flags to tell it which link script to use.

use const_gen::*;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::{env, fs};
use xz2::read::XzEncoder;

fn main() {
    let vial_path = configured_path("VIAL_JSON_PATH", "vial.json");
    let keyboard_path = configured_path("KEYBOARD_TOML_PATH", "keyboard.toml");

    println!("cargo:rerun-if-env-changed=VIAL_JSON_PATH");
    println!("cargo:rerun-if-env-changed=KEYBOARD_TOML_PATH");
    println!("cargo:rerun-if-changed={}", vial_path.display());
    println!("cargo:rerun-if-changed={}", keyboard_path.display());
    println!("cargo:rerun-if-changed=memory_halves.x");
    println!("cargo:rerun-if-changed=memory_qube.x");
    println!("cargo:rustc-check-cfg=cfg(velvet_pointing)");
    println!("cargo:rustc-check-cfg=cfg(classic_encoder_settings)");

    // Put `memory.x` in our output directory and ensure it's
    // on the linker search path.
    let out = &PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let product_id = generate_vial_config(&vial_path);
    let encoder_profile = matches!(product_id, 0x0044 | 0x0070);
    let (version, version_bcd) = if encoder_profile {
        println!("cargo:rustc-cfg=classic_encoder_settings");
        ("0.1.9", "0x0109")
    } else {
        ("0.1.8", "0x0108")
    };
    println!("cargo:rustc-env=RMK_FIRMWARE_VERSION={version}");
    println!("cargo:rustc-env=RMK_FIRMWARE_VERSION_BCD={version_bcd}");
    let settings_fn = if encoder_profile {
        "crate::encoder_device_settings::vial_device_settings"
    } else if product_id == 0x00BE {
        "crate::velvet_device_settings::vial_device_settings"
    } else {
        "crate::layer_names::vial_device_settings"
    };
    println!("cargo:rustc-env=RMK_VIAL_DEVICE_SETTINGS_FN={settings_fn}");
    generate_qube_profile(product_id, out);

    let memory = if env::var_os("CARGO_FEATURE_QUBE").is_some() {
        include_bytes!("memory_qube.x").as_slice()
    } else {
        include_bytes!("memory_halves.x").as_slice()
    };
    File::create(out.join("memory.x"))
        .unwrap()
        .write_all(memory)
        .unwrap();
    println!("cargo:rustc-link-search={}", out.display());

    // By default, Cargo will re-run a build script whenever
    // any file in the project changes. By specifying the memory files
    // here, we ensure the build script is only re-run when
    // the linker memory layouts are changed.
    // Specify linker arguments.

    // `--nmagic` is required if memory section addresses are not aligned to 0x10000,
    // for example the FLASH and RAM sections in your `memory.x`.
    // See https://github.com/rust-embedded/cortex-m-quickstart/pull/95
    println!("cargo:rustc-link-arg=--nmagic");

    // Set the linker script to the one provided by cortex-m-rt.
    println!("cargo:rustc-link-arg=-Tlink.x");

    // Set the extra linker script from defmt
    println!("cargo:rustc-link-arg=-Tdefmt.x");

    // Use flip-link overflow check: https://github.com/knurling-rs/flip-link
    println!("cargo:rustc-linker=flip-link");
}

fn configured_path(variable: &str, default: &str) -> PathBuf {
    env::var_os(variable)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(default))
}

fn generate_vial_config(vial_path: &Path) -> u16 {
    // Generated vial config file
    let out_file = Path::new(&env::var_os("OUT_DIR").unwrap()).join("config_generated.rs");

    let mut content = String::new();
    match File::open(vial_path) {
        Ok(mut file) => {
            file.read_to_string(&mut content)
                .unwrap_or_else(|e| panic!("Cannot read {}: {e}", vial_path.display()));
        }
        Err(e) => panic!("Cannot find {}: {e}", vial_path.display()),
    };

    let mut parsed = json::parse(&content).unwrap_or_else(|e| panic!("Cannot parse {}: {e}", vial_path.display()));
    let product_id = parsed["productId"]
        .as_str()
        .and_then(|value| value.strip_prefix("0x").or_else(|| value.strip_prefix("0X")))
        .and_then(|value| u16::from_str_radix(value, 16).ok())
        .unwrap_or_else(|| panic!("{} productId must be a hexadecimal string", vial_path.display()));
    match product_id {
        0x0036 | 0x0044 | 0x0070 | 0x00BE => {}
        _ => panic!("Unsupported Ergohaven Qube productId: 0x{product_id:04X}"),
    }

    if !parsed.has_key("entropy") {
        parsed.insert("entropy", json::object! {}).unwrap();
    }
    parsed["firmware"].insert("name", "RMK").unwrap();
    let mut firmware_update = json::object! {};
    firmware_update.insert("asset", qube_release_asset(product_id)).unwrap();
    parsed["entropy"].insert("firmwareUpdate", firmware_update).unwrap();
    parsed["entropy"]["liveFeatures"] = json::array!["time", "media"];
    parsed["entropy"]["batteryHalves"] = true.into();
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

    product_id
}

fn qube_release_asset(product_id: u16) -> &'static str {
    match product_id {
        0x0036 => "op36-qube",
        0x0044 => "imperial44-qube",
        0x0070 => "k03-qube",
        0x00BE => "velvet-qube",
        _ => unreachable!(),
    }
}

fn generate_qube_profile(product_id: u16, out: &Path) {
    let source = match product_id {
        0x00BE => {
            println!("cargo:rustc-cfg=velvet_pointing");
            r#"pub const DEFAULT_LAYER_NAMES: [&str; 16] =
    crate::default_layer_names::STANDARD_WITH_MOUSE;
"#
        }
        0x0044 => {
            r#"pub const DEFAULT_LAYER_NAMES: [&str; 16] =
    crate::default_layer_names::STANDARD_NO_MOUSE;
pub const ENCODER_COUNT: usize = 2;
pub const ENCODER_SETTING_KEYS: &[u16] = &[
    200, 201, 202, 203, 204, 205, 206, 207, 208, 209, 210, 211, 212, 213, 214, 215,
    340, 341, 342, 343,
];
"#
        }
        0x0070 => {
            r#"pub const DEFAULT_LAYER_NAMES: [&str; 16] =
    crate::default_layer_names::STANDARD_NO_MOUSE;
pub const ENCODER_COUNT: usize = 6;
pub const ENCODER_SETTING_KEYS: &[u16] = &[
    200, 201, 202, 203, 204, 205, 206, 207, 208, 209, 210, 211, 212, 213, 214, 215,
    340, 341, 342, 343, 344, 345, 346, 347, 348, 349, 350, 351,
];
"#
        }
        0x0036 => {
            r#"pub const DEFAULT_LAYER_NAMES: [&str; 16] =
    crate::default_layer_names::STANDARD_NO_MOUSE;
"#
        }
        _ => unreachable!(),
    };
    fs::write(out.join("qube_profile_generated.rs"), source).unwrap();
}

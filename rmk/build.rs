#[path = "./build_common.rs"]
mod common;

fn main() {
    let mut cfgs = common::CfgSet::new();
    common::set_target_cfgs(&mut cfgs);

    // `RMK_FEATURES` goes into the storage schema hash.
    // When it changes, the storage is erased.
    let mut features: Vec<String> = std::env::vars()
        .filter_map(|(key, _)| key.strip_prefix("CARGO_FEATURE_").map(str::to_lowercase))
        // Geometry changes report generation, not stored framing. Keep existing
        // Rynk layouts when enabling it; Vial retains its established hash.
        .filter(|feature| {
            std::env::var_os("CARGO_FEATURE_ERGOHAVEN_RYNK").is_none()
                || !matches!(
                    feature.as_str(),
                    "universal_symbols_iso" | "universal_symbols_iso_bit0" | "universal_symbols_iso_bit1"
                )
        })
        .collect();
    features.sort();
    println!("cargo:rustc-env=RMK_FEATURES={}", features.join(","));
}

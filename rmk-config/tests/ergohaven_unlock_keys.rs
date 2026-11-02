use std::path::Path;

use rmk_config::KeyboardTomlConfig;

#[test]
fn all_production_profiles_resolve_factory_df_unlock_positions() {
    // Independent factory-position golden data, not inferred from unlock_keys.
    let profiles = [
        ("keyboards/k03/keyboard.toml", [[2, 2], [2, 1]], false),
        ("keyboards/k04/keyboard.toml", [[2, 2], [2, 1]], false),
        ("keyboards/k04/keyboard_mini.toml", [[1, 2], [1, 1]], false),
        ("keyboards/k04/keyboard_micro.toml", [[1, 2], [1, 1]], true),
        ("keyboards/imperial44/keyboard.toml", [[1, 2], [1, 1]], false),
        ("keyboards/op36/keyboard.toml", [[1, 2], [1, 1]], true),
        ("keyboards/velvet/keyboard.toml", [[1, 3], [1, 4]], false),
        ("keyboards/classic_qube/keyboard.toml", [[1, 2], [1, 1]], true),
        ("keyboards/classic_qube/keyboard_k03.toml", [[2, 2], [2, 1]], false),
        ("keyboards/classic_qube/keyboard_velvet.toml", [[1, 3], [1, 4]], false),
        (
            "keyboards/classic_qube/keyboard_imperial44.toml",
            [[1, 2], [1, 1]],
            false,
        ),
        ("keyboards/k04/keyboard_qube.toml", [[2, 2], [2, 1]], false),
        ("keyboards/k04/keyboard_qube_mini.toml", [[1, 2], [1, 1]], false),
        ("keyboards/k04/keyboard_qube_micro.toml", [[1, 2], [1, 1]], true),
    ];
    assert_eq!(profiles.len(), 14);
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    for (path, expected, mod_tap) in profiles {
        // Same merged, alias/matrix-map-resolved API consumed by proc-macro codegen.
        let config = KeyboardTomlConfig::new_from_toml_path(root.join(path));
        let host = config.host();
        let keymap = config.keymap().unwrap_or_else(|e| panic!("{path}: {e}"));
        assert!(
            host.vial_enabled != host.rynk_enabled,
            "{path}: exactly one of Vial and Rynk must be enabled"
        );
        assert!(!host.insecure, "{path}: physical unlock must be required");
        assert_eq!(host.unlock_keys, expected, "{path}: unlock coordinates");
        let actions = expected.map(|[row, col]| keymap.keymap[0][row as usize][col as usize].clone());
        let expected_actions = if mod_tap {
            ["MT(D,LShift)", "MT(F,LCtrl)"]
        } else {
            ["D", "F"]
        };
        assert_eq!(actions, expected_actions, "{path}: resolved factory D/F actions");
        eprintln!("VERIFIED {path}: {:?}, {:?}", host.unlock_keys, actions);
    }
}

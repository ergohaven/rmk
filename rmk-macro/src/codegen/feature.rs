//! Utilities of check cargo feature
//!

/// Get enabled RMK features list
pub(crate) fn get_rmk_features() -> Option<Vec<String>> {
    // Use an absolute path. `cargo_toml::Manifest::from_path` resolves the
    // workspace root by walking ancestors of the given path; passing a
    // relative `"./Cargo.toml"` makes its fallback canonicalize an empty
    // path and fail with ENOENT inside workspace members.
    let manifest_path = std::path::Path::new(
        &std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is not set"),
    )
    .join("Cargo.toml");
    let manifest = cargo_toml::Manifest::from_path(&manifest_path).ok()?;
    let dep = manifest.dependencies.get("rmk")?;
    let mut feature_set = dep.req_features().to_vec();
    // Cargo exposes CARGO_FEATURE_* to build scripts, not to proc macros.
    // Selectable boards forward the active set through rustc-env.
    let selected = std::env::var("RMK_BOARD_FEATURES").unwrap_or_default();
    let active: Vec<String> = manifest
        .features
        .keys()
        .filter(|name| {
            selected
                .split(',')
                .any(|active| active == name.replace('-', "_"))
        })
        .cloned()
        .collect();
    // Only protocol selection is newly dynamic here. Preserve the existing
    // code-generation contract for nonprotocol board aliases.
    feature_set.extend(
        resolve_rmk_feature_aliases(&manifest.features, active)
            .into_iter()
            .filter(|feature| matches!(feature.as_str(), "vial" | "host_lock" | "ergohaven_rynk")),
    );

    // Add default features to the feature list.
    // Must mirror `default = [...]` in rmk/Cargo.toml.
    if dep.detail().is_none_or(|d| d.default_features) {
        feature_set.push("defmt".to_string());
        feature_set.push("storage".to_string());
        feature_set.push("vial".to_string());
        feature_set.push("host_lock".to_string());
        feature_set.push("watchdog".to_string());
    }
    // Must mirror `ergohaven_rynk = ["rynk", ...]` in rmk/Cargo.toml.
    if feature_set.iter().any(|f| f == "ergohaven_rynk") {
        feature_set.push("rynk".to_string());
    }
    Some(feature_set)
}

fn resolve_rmk_feature_aliases(
    features: &std::collections::BTreeMap<String, Vec<String>>,
    mut active: Vec<String>,
) -> Vec<String> {
    let mut visited = std::collections::BTreeSet::new();
    let mut resolved = std::collections::BTreeSet::new();
    while let Some(name) = active.pop() {
        if !visited.insert(name.clone()) {
            continue;
        }
        if let Some(feature) = name.strip_prefix("rmk/") {
            resolved.insert(feature.to_string());
        } else if let Some(children) = features.get(&name) {
            active.extend(children.iter().cloned());
        }
    }
    resolved.into_iter().collect()
}

#[cfg(test)]
mod profile_tests {
    use super::*;

    #[test]
    fn selectable_protocol_features_follow_only_active_aliases() {
        let features = std::collections::BTreeMap::from([
            (
                "default".into(),
                vec!["protocol_rynk".into(), "standalone".into()],
            ),
            ("protocol_rynk".into(), vec!["rmk/ergohaven_rynk".into()]),
            (
                "protocol_vial".into(),
                vec!["rmk/vial".into(), "rmk/host_lock".into()],
            ),
            ("standalone".into(), vec!["rmk/defmt".into()]),
            (
                "vial_standalone".into(),
                vec!["protocol_vial".into(), "standalone".into()],
            ),
        ]);
        let vial = resolve_rmk_feature_aliases(&features, vec!["vial_standalone".into()]);
        assert!(vial.contains(&"vial".into()));
        assert!(vial.contains(&"defmt".into()));
        assert!(!vial.contains(&"ergohaven_rynk".into()));
        let rynk = resolve_rmk_feature_aliases(&features, vec!["default".into()]);
        assert!(rynk.contains(&"ergohaven_rynk".into()));
        assert!(!rynk.contains(&"vial".into()));
        assert!(resolve_rmk_feature_aliases(&features, vec![]).is_empty());
    }
}

/// Check whether the given feature is enabled
pub(crate) fn is_feature_enabled(feature_list: &Option<Vec<String>>, feature: &str) -> bool {
    feature_list
        .as_ref()
        .is_some_and(|features| features.iter().any(|f| f == feature))
}

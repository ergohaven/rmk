#!/usr/bin/env bash
set -euo pipefail
# shellcheck source=_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/_lib.sh"

export CARGO_BUILD_TARGET="${CARGO_BUILD_TARGET:-x86_64-unknown-linux-gnu}"

export CARGO_NET_OFFLINE=false
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$target_root/test}"
nx=(nextest run --config-file "$repo_root/.config/nextest.toml" --profile ci)

log_section "Configuration and shared types"
log_section "Tests: rmk-config, default features"
cargo +stable "${nx[@]}" --manifest-path rmk-config/Cargo.toml
log_section "Tests: rmk-types, default features"
cargo +stable "${nx[@]}" --manifest-path rmk-types/Cargo.toml
log_section "Tests: rmk-types, default features + host"
cargo +stable "${nx[@]}" --manifest-path rmk-types/Cargo.toml --features host
# Exercise the rynk protocol module (gated behind `rynk`) and the Ergohaven commands.
log_section "Tests: rmk-types, default features + host,ergohaven"
cargo +stable "${nx[@]}" --manifest-path rmk-types/Cargo.toml --features host,ergohaven
log_section "Tests: rmk-types, default features + steno"
cargo +stable "${nx[@]}" --manifest-path rmk-types/Cargo.toml --features steno

# nextest does not run doctests. rmk and rmk-macro disable them.
log_section "Doctests"
log_section "Doctests: rmk-types, default features"
cargo +stable test --manifest-path rmk-types/Cargo.toml --doc
log_section "Doctests: rmk-types, default features + host"
cargo +stable test --manifest-path rmk-types/Cargo.toml --features host --doc
# The Ergohaven rynk protocol module and its commands.
log_section "Doctests: rmk-types, default features + host,ergohaven"
cargo +stable test --manifest-path rmk-types/Cargo.toml --features host,ergohaven --doc
log_section "Doctests: rmk-config, default features"
cargo +stable test --manifest-path rmk-config/Cargo.toml --doc

log_section "Macro tests and expansion snapshots"
log_section "Tests: rmk-macro, default features + _simulator,steno"
cargo +stable "${nx[@]}" --manifest-path rmk-macro/Cargo.toml --features _simulator,steno

log_section "RMK behavior"
for feats in "${RMK_TEST_FEATURESETS[@]}"; do
    args=(--no-default-features)
    [[ -z "$feats" ]] || args+=(--features "$feats")
    log_section "Tests: rmk, default features disabled, features: ${feats:-none}"
    cargo +stable "${nx[@]}" --manifest-path rmk/Cargo.toml "${args[@]}"
done
# Selectable Vial profiles: masks 4 (K:04/Micro), 1 (K:03/I44/OP36), 2 (Velvet).
for geometry in universal_symbols_iso universal_symbols_iso_bit0 universal_symbols_iso_bit1; do
    cargo +stable "${nx[@]}" --manifest-path rmk/Cargo.toml --no-default-features \
        --features "split,vial,host_lock,storage,async_matrix,_ble,$geometry" \
        --lib -E 'test(universal) or test(layout_options) or test(unbound) or test(a_timed_out) or test(staged)'
done
# The pointing executor tests size their event buses from a real K:04 profile;
# the other modules' tests assume the default sizes, so only pointing runs here.
KEYBOARD_TOML_PATH="$repo_root/keyboards/k04/keyboard.toml" CARGO_TARGET_DIR="$target_root/test-k04" \
    cargo +stable "${nx[@]}" --manifest-path rmk/Cargo.toml --no-default-features \
    --features "$RMK_K04_STANDALONE_FEATURES" -E 'test(/^input_device::pointing::/) or test(unbound_key_preserves)'
python3 scripts/test_k04_bt_profile_indicator.py

# The K:04 firmware bins set `test = false`, so build its dependency-free
# modules as standalone test harnesses.
k04_tests="$target_root/k04-tests"
mkdir -p "$k04_tests"
rustc +stable --edition 2021 --test keyboards/k04/src/battery_level.rs -o "$k04_tests/battery_level"
"$k04_tests/battery_level"
rustc +stable --edition 2021 --test keyboards/k04/src/trackball/motion_pacing.rs -o "$k04_tests/motion_pacing"
"$k04_tests/motion_pacing"
rustc +stable --edition 2021 --test --cfg 'feature="production_v22"' \
    keyboards/k04/src/trackball/motion_pacing.rs -o "$k04_tests/motion_pacing_production"
"$k04_tests/motion_pacing_production"
rustc +stable --edition 2021 --test keyboards/k04/src/media_label.rs -o "$k04_tests/media_label"
"$k04_tests/media_label"

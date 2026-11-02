#!/usr/bin/env bash
set -euo pipefail
# shellcheck source=_lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/_lib.sh"

export CARGO_NET_OFFLINE=false
export CARGO_BUILD_TARGET="${CARGO_BUILD_TARGET:-x86_64-unknown-linux-gnu}"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$target_root/test}"

# Each crate is its own cargo workspace in this repo, so nextest's default
# `<workspace>/.config/nextest.toml` lookup would miss our shared config at
# repo root. Pass it explicitly.
nextest_cfg="$repo_root/.config/nextest.toml"
nx=(nextest run --config-file "$nextest_cfg" --profile ci)

log_section "Running tests"
cargo +stable "${nx[@]}" --manifest-path rmk-config/Cargo.toml
cargo +stable "${nx[@]}" --manifest-path rmk-types/Cargo.toml
# Exercise the rynk protocol module (gated behind `rynk`) and the Ergohaven commands.
cargo +stable "${nx[@]}" --manifest-path rmk-types/Cargo.toml --features host,ergohaven
cargo +stable "${nx[@]}" --manifest-path rmk-types/Cargo.toml --features steno
cargo +stable "${nx[@]}" --manifest-path rmk-macro/Cargo.toml --features _simulator,steno
for feats in "${RMK_TEST_FEATURESETS[@]}"; do
    if [[ -z "$feats" ]]; then
        cargo +stable "${nx[@]}" --manifest-path rmk/Cargo.toml --no-default-features
    else
        cargo +stable "${nx[@]}" --manifest-path rmk/Cargo.toml --no-default-features --features "$feats"
    fi
done
# The pointing executor tests size their event buses from a real K:04 profile;
# the other modules' tests assume the default sizes, so only pointing runs here.
KEYBOARD_TOML_PATH="$repo_root/keyboards/k04/keyboard.toml" CARGO_TARGET_DIR="$target_root/test-k04" \
    cargo +stable "${nx[@]}" --manifest-path rmk/Cargo.toml --no-default-features \
    --features "$RMK_K04_STANDALONE_FEATURES" -E 'test(/^input_device::pointing::/)'
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

# Doctests: nextest does not run them. rmk/ and rmk-macro/ have `doctest = false`,
# so only rmk-types and rmk-config need a separate --doc pass.
log_section "Running doctests"
cargo +stable test --manifest-path rmk-types/Cargo.toml --doc
cargo +stable test --manifest-path rmk-types/Cargo.toml --features host,ergohaven --doc
cargo +stable test --manifest-path rmk-config/Cargo.toml --doc

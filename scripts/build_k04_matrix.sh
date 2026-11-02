#!/usr/bin/env bash

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

export RUST_MIN_STACK="${RUST_MIN_STACK:-33554432}"

if [[ -z "${BINDGEN_EXTRA_CLANG_ARGS:-}" ]]; then
    clang_args=(-ffreestanding --target=arm-none-eabi)

    if command -v arm-none-eabi-gcc >/dev/null 2>&1; then
        sysroot="$(arm-none-eabi-gcc -print-sysroot 2>/dev/null || true)"
        gcc_include="$(arm-none-eabi-gcc -print-file-name=include 2>/dev/null || true)"

        if [[ -n "$sysroot" && -d "$sysroot" ]]; then
            clang_args+=("--sysroot=$sysroot")
            if [[ -d "$sysroot/include" ]]; then
                clang_args+=("-I$sysroot/include")
            fi
        fi

        if [[ -n "$gcc_include" && -d "$gcc_include" ]]; then
            clang_args+=("-I$gcc_include")
        fi
    else
        xpack_root="${XPACK_ARM_GCC_ROOT:-}"
        if [[ -z "$xpack_root" ]]; then
            for candidate in "$HOME"/.local/xPacks/@xpack-dev-tools/arm-none-eabi-gcc/*/.content; do
                if [[ -d "$candidate/arm-none-eabi/include" ]]; then
                    xpack_root="$candidate"
                fi
            done
        fi

        if [[ -n "$xpack_root" && -d "$xpack_root" ]]; then
            clang_args+=("-I$xpack_root/arm-none-eabi/include")
            gcc_include="$(
                find "$xpack_root/lib/gcc/arm-none-eabi" \
                    -mindepth 2 \
                    -maxdepth 2 \
                    -type d \
                    -name include \
                    2>/dev/null \
                    | sort -V \
                    | tail -1
            )"
            if [[ -n "$gcc_include" ]]; then
                clang_args+=("-I$gcc_include")
            fi
        fi
    fi

    host_gcc_include="$(gcc -print-file-name=include 2>/dev/null || true)"
    if [[ -n "$host_gcc_include" && -d "$host_gcc_include" ]]; then
        clang_args+=("-I$host_gcc_include")
    fi

    export BINDGEN_EXTRA_CLANG_ARGS="${clang_args[*]}"
fi

run() {
    local dir="$1"
    shift
    echo
    echo "==> $dir: $*"
    (
        cd "$repo_root/$dir"
        "$@"
    )
}

build_split() {
    local keyboard="$1"
    local bins=(--bin central --bin peripheral)
    if grep -q 'name = "hardreset"' "$repo_root/keyboards/$keyboard/Cargo.toml"; then
        bins+=(--bin hardreset)
    fi
    run "keyboards/$keyboard" cargo build --release "${bins[@]}"
}

build_k04_series_profile() {
    local profile="$1"
    local keyboard_toml="$2"
    local vial_json="$3"
    local bins=(--bin central --bin peripheral --bin hardreset)

    run "keyboards/k04" env \
        "CARGO_TARGET_DIR=target/$profile/standalone" \
        "KEYBOARD_TOML_PATH=$repo_root/keyboards/k04/$keyboard_toml" \
        "VIAL_JSON_PATH=$repo_root/keyboards/k04/$vial_json" \
        cargo build --release "${bins[@]}" --features production_v30g
}

build_classic_qube_profile() {
    local profile="$1"
    local keyboard_toml="$2"
    local vial_json="$3"

    run "keyboards/classic_qube" env \
        "CARGO_TARGET_DIR=target/$profile/qube" \
        "KEYBOARD_TOML_PATH=$repo_root/keyboards/classic_qube/$keyboard_toml" \
        "VIAL_JSON_PATH=$repo_root/$vial_json" \
        cargo build --release --bin qube --features qube
    run "keyboards/classic_qube" env \
        "CARGO_TARGET_DIR=target/$profile/halves" \
        "KEYBOARD_TOML_PATH=$repo_root/keyboards/classic_qube/$keyboard_toml" \
        "VIAL_JSON_PATH=$repo_root/$vial_json" \
        cargo build --release --bin left --bin right
}

build_phenom_profile() {
    local profile="$1"
    local keyboard_toml="$2"
    local vial_json="$3"

    run "keyboards/phenom" env \
        "CARGO_TARGET_DIR=target/$profile" \
        "KEYBOARD_TOML_PATH=$repo_root/keyboards/phenom/$keyboard_toml" \
        "VIAL_JSON_PATH=$repo_root/keyboards/phenom/$vial_json" \
        cargo build --release --bin central --bin peripheral
}

build_k04_qube_profile() {
    local profile="$1"
    local keyboard_toml="$2"
    local vial_json="$3"

    run "keyboards/k04" env \
        "CARGO_TARGET_DIR=target/$profile/qube" \
        "KEYBOARD_TOML_PATH=$repo_root/keyboards/k04/$keyboard_toml" \
        "VIAL_JSON_PATH=$repo_root/keyboards/k04/$vial_json" \
        cargo build --release --bin qube --no-default-features --features qube
    run "keyboards/k04" env \
        "CARGO_TARGET_DIR=target/$profile/halves" \
        "KEYBOARD_TOML_PATH=$repo_root/keyboards/k04/$keyboard_toml" \
        "VIAL_JSON_PATH=$repo_root/keyboards/k04/$vial_json" \
        cargo build --release --bin left --bin right --no-default-features --features qube-half
}

echo "Using BINDGEN_EXTRA_CLANG_ARGS=$BINDGEN_EXTRA_CLANG_ARGS"

build_k04_series_profile k04 keyboard.toml vial.json
build_k04_series_profile mini keyboard_mini.toml vial_mini.json
build_k04_series_profile micro keyboard_micro.toml vial_micro.json
build_k04_qube_profile k04 keyboard_qube.toml vial_qube.json
build_k04_qube_profile mini keyboard_qube_mini.toml vial_qube_mini.json
build_k04_qube_profile micro keyboard_qube_micro.toml vial_qube_micro.json
build_split op36
build_split k03
build_split imperial44
build_split velvet
build_phenom_profile phenom keyboard.toml vial.json
build_phenom_profile mini keyboard_mini.toml vial_mini.json
build_phenom_profile micro keyboard_micro.toml vial_micro.json
build_classic_qube_profile op36 keyboard.toml keyboards/classic_qube/vial.json
build_classic_qube_profile k03 keyboard_k03.toml keyboards/k03/vial.json
build_classic_qube_profile velvet keyboard_velvet.toml keyboards/velvet/vial.json
build_classic_qube_profile imperial44 keyboard_imperial44.toml keyboards/imperial44/vial.json

echo
echo "Root RMK build matrix OK"

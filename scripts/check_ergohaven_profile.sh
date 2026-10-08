#!/usr/bin/env bash

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

failures=0

fail() {
    echo "profile contract: $*" >&2
    failures=$((failures + 1))
}

toml_value() {
    local file="$1"
    local key="$2"
    awk -v key="$key" '
        $0 ~ "^[[:space:]]*" key "[[:space:]]*=" {
            value = $0
            sub(/#.*/, "", value)
            sub(/^[^=]*=/, "", value)
            gsub(/^[[:space:]"]+|[[:space:]"]+$/, "", value)
            print value
            exit
        }
    ' "$file"
}

expect_toml() {
    local file="$1"
    local key="$2"
    local expected="$3"
    local actual
    actual="$(toml_value "$file" "$key")"
    if [[ "$actual" != "$expected" ]]; then
        fail "$file: $key=$actual, expected $expected"
    fi
}

expect_not_true() {
    local file="$1"
    local key="$2"
    local actual
    actual="$(toml_value "$file" "$key")"
    if [[ "$actual" == "true" ]]; then
        fail "$file: $key must not be true in production firmware"
    fi
}

profiles=(
    keyboards/imperial44/keyboard.toml
    keyboards/k03/keyboard.toml
    keyboards/k04/keyboard.toml
    keyboards/k04/keyboard_micro.toml
    keyboards/k04/keyboard_mini.toml
    keyboards/k04/keyboard_qube.toml
    keyboards/k04/keyboard_qube_micro.toml
    keyboards/k04/keyboard_qube_mini.toml
    keyboards/op36/keyboard.toml
    keyboards/classic_qube/keyboard.toml
    keyboards/classic_qube/keyboard_imperial44.toml
    keyboards/classic_qube/keyboard_k03.toml
    keyboards/classic_qube/keyboard_velvet.toml
    keyboards/velvet/keyboard.toml
)

split_profiles=(
    keyboards/imperial44/keyboard.toml
    keyboards/k03/keyboard.toml
    keyboards/k04/keyboard.toml
    keyboards/k04/keyboard_micro.toml
    keyboards/k04/keyboard_mini.toml
    keyboards/k04/keyboard_qube.toml
    keyboards/k04/keyboard_qube_micro.toml
    keyboards/k04/keyboard_qube_mini.toml
    keyboards/op36/keyboard.toml
    keyboards/classic_qube/keyboard.toml
    keyboards/classic_qube/keyboard_imperial44.toml
    keyboards/classic_qube/keyboard_k03.toml
    keyboards/classic_qube/keyboard_velvet.toml
    keyboards/velvet/keyboard.toml
)

standalone_split_profiles=(
    keyboards/imperial44/keyboard.toml
    keyboards/k03/keyboard.toml
    keyboards/k04/keyboard.toml
    keyboards/k04/keyboard_micro.toml
    keyboards/k04/keyboard_mini.toml
    keyboards/op36/keyboard.toml
    keyboards/velvet/keyboard.toml
)

qube_profiles=(
    keyboards/k04/keyboard_qube.toml
    keyboards/k04/keyboard_qube_micro.toml
    keyboards/k04/keyboard_qube_mini.toml
    keyboards/classic_qube/keyboard.toml
    keyboards/classic_qube/keyboard_imperial44.toml
    keyboards/classic_qube/keyboard_k03.toml
    keyboards/classic_qube/keyboard_velvet.toml
)

live_matrix_cargo_manifests=(
    keyboards/imperial44/Cargo.toml
    keyboards/k03/Cargo.toml
    keyboards/op36/Cargo.toml
    keyboards/velvet/Cargo.toml
    keyboards/classic_qube/Cargo.toml
)

battery_reader_sources=(
    keyboards/imperial44/src/battery_nrf.rs
    keyboards/k03/src/battery_nrf.rs
    keyboards/op36/src/battery_nrf.rs
    keyboards/velvet/src/battery_nrf.rs
    keyboards/classic_qube/src/battery_nrf.rs
)

non_k04_profiles=(
    keyboards/imperial44/keyboard.toml
    keyboards/k03/keyboard.toml
    keyboards/op36/keyboard.toml
    keyboards/classic_qube/keyboard.toml
    keyboards/classic_qube/keyboard_imperial44.toml
    keyboards/classic_qube/keyboard_k03.toml
    keyboards/classic_qube/keyboard_velvet.toml
    keyboards/velvet/keyboard.toml
)

for file in "${live_matrix_cargo_manifests[@]}"; do
    rg -Fq '"vial_lock"' "$file" \
        || fail "$file: Vial live matrix support requires vial_lock/host_security"
done

for file in "${battery_reader_sources[@]}"; do
    rg -Fq 'publish_battery_status(' "$file" \
        || fail "$file: custom battery reader must update the synchronous host cache"
done

for file in "${profiles[@]}"; do
    expect_toml "$file" manufacturer Ergohaven
    expect_toml "$file" layers 16
    expect_toml "$file" no_action_layer_start 5
    expect_toml "$file" combo_max_num 32
    expect_toml "$file" fork_max_num 32
    expect_toml "$file" morse_max_num 32
    expect_toml "$file" macro_space_size 2048
    expect_toml "$file" ble_profiles_num 5
    expect_toml "$file" ble_reconnect_timeout_seconds 60
    expect_toml "$file" ble_pairing_timeout_seconds 60
    expect_not_true "$file" clear_storage
    expect_not_true "$file" clear_layout
done

python3 - <<'PY' || fail "shared K:04 crate must keep all topology and model identities"
import json
import tomllib

profiles = [
    ("keyboards/k04/keyboard.toml", "keyboards/k04/vial.json", 0x0074, 5, 1),
    ("keyboards/k04/keyboard_mini.toml", "keyboards/k04/vial_mini.json", 0x0075, 4, 1),
    ("keyboards/k04/keyboard_micro.toml", "keyboards/k04/vial_micro.json", 0x0076, 4, 1),
    ("keyboards/k04/keyboard_qube.toml", "keyboards/k04/vial_qube.json", 0x0071, 0, 2),
    ("keyboards/k04/keyboard_qube_mini.toml", "keyboards/k04/vial_qube_mini.json", 0x0072, 0, 2),
    ("keyboards/k04/keyboard_qube_micro.toml", "keyboards/k04/vial_qube_micro.json", 0x0073, 0, 2),
]

for keyboard_path, vial_path, expected_product_id, central_rows, peripheral_count in profiles:
    with open(keyboard_path, "rb") as source:
        keyboard = tomllib.load(source)
    with open(vial_path, encoding="utf-8") as source:
        vial = json.load(source)

    assert keyboard["keyboard"]["product_id"] == expected_product_id
    assert int(vial["productId"], 16) == expected_product_id
    assert keyboard["split"]["central"]["rows"] == central_rows
    assert len(keyboard["split"]["peripheral"]) == peripheral_count
PY

if git ls-files 'keyboards/k04_qube/**' | rg -q .; then
    fail "keyboards/k04_qube must remain consolidated into keyboards/k04"
fi

python3 - "${profiles[@]}" <<'PY' || fail "factory keymaps must leave layers 5-15 for generated No actions"
import sys
import tomllib

for path in sys.argv[1:]:
    with open(path, "rb") as source:
        config = tomllib.load(source)
    explicit_layers = config.get("layer", [])
    if len(explicit_layers) > 5:
        raise SystemExit(
            f"{path}: defines {len(explicit_layers)} factory layers; "
            "layers 5-15 must be generated as No"
        )
PY

python3 - <<'PY' || fail "K:04 Micro and OP36 Adjust layers must use direct key actions"
import tomllib

profiles = [
    "keyboards/k04/keyboard_micro.toml",
    "keyboards/k04/keyboard_qube_micro.toml",
    "keyboards/op36/keyboard.toml",
    "keyboards/classic_qube/keyboard.toml",
]

for path in profiles:
    with open(path, "rb") as source:
        layers = tomllib.load(source).get("layer", [])
    adjust = next((layer for layer in layers if layer.get("name") == "Adjust"), None)
    if adjust is None:
        raise SystemExit(f"{path}: Adjust layer is missing")
    if any(action.startswith("MT(") for action in adjust["keys"].split()):
        raise SystemExit(f"{path}: Adjust layer must not contain Mod-Tap actions")
PY

rg -Fq 'KeymapTailV3' rmk/src/storage/mod.rs \
    || fail "rmk/src/storage/mod.rs: separate keymap tail namespace is missing"
rg -Fq 'EncoderTailV3' rmk/src/storage/mod.rs \
    || fail "rmk/src/storage/mod.rs: separate encoder tail namespace is missing"
rg -Fq 'uses_tail_key_namespace' rmk/src/host/storage.rs \
    || fail "rmk/src/host/storage.rs: legacy tail records are not filtered"
rg -Fq 'no_action_layer_start: #no_action_layer_start' rmk-macro/src/codegen/chip/flash.rs \
    || fail "rmk-macro: no-action boundary is not propagated to runtime storage"

python3 - <<'PY' || fail "projected factory layers must mirror their K:04 Series references"
import tomllib

def layers(path):
    with open(path, "rb") as source:
        return tomllib.load(source).get("layer", [])

replacements = {
    "User19": "User0",
    "User20": "User1",
    "User21": "User2",
    "User22": "User3",
    "User23": "User4",
    "User26": "User7",
    "User37": "OutputBluetooth",
    "User38": "OutputUsb",
    # Classic profiles have no K:04 status LED for the battery indication action.
    "User39": "No",
}

def project(layer, drop_indices, expected_source_actions):
    actions = layer["keys"].split()
    if len(actions) != expected_source_actions:
        raise SystemExit(
            f"K:04 reference layer {layer['name']} has {len(actions)} actions, "
            f"expected {expected_source_actions}"
        )
    projected = []
    for index, action in enumerate(actions):
        if index in drop_indices:
            continue
        for source, target in replacements.items():
            action = action.replace(source, target)
        projected.append(action)
    return projected

def check_projection(
    reference_path,
    target_paths,
    drop_indices,
    expected_source_actions,
    encoders_per_half,
    expected_target_layers,
):
    reference_layers = layers(reference_path)[:4]
    for layer in reference_layers:
        reference_encoders = layer.get("encoders", [])
        if len(reference_encoders) != 2:
            raise SystemExit(
                f"{reference_path}: layer {layer['name']} has "
                f"{len(reference_encoders)} encoders, expected two"
            )

    expected = [
        (
            layer["name"],
            project(layer, drop_indices, expected_source_actions),
            (
                [layer["encoders"][0]] * encoders_per_half
                + [layer["encoders"][1]] * encoders_per_half
                if encoders_per_half
                else None
            ),
        )
        for layer in reference_layers
    ]

    for path in target_paths:
        actual_layers = layers(path)
        if len(actual_layers) != expected_target_layers:
            raise SystemExit(
                f"{path}: expected {expected_target_layers} factory layers, "
                f"found {len(actual_layers)}"
            )
        actual = [
            (
                layer["name"],
                layer["keys"].split(),
                layer.get("encoders"),
            )
            for layer in actual_layers[:4]
        ]
        if actual != expected:
            raise SystemExit(
                f"{path}: common factory layers drifted from {reference_path}"
            )

check_projection(
    "keyboards/k04/keyboard_micro.toml",
    [
        "keyboards/op36/keyboard.toml",
        "keyboards/classic_qube/keyboard.toml",
    ],
    {25, 26},
    38,
    0,
    4,
)
check_projection(
    "keyboards/k04/keyboard_mini.toml",
    [
        "keyboards/imperial44/keyboard.toml",
        "keyboards/classic_qube/keyboard_imperial44.toml",
    ],
    {38, 39, 46, 47},
    48,
    1,
    4,
)
check_projection(
    "keyboards/k04/keyboard_mini.toml",
    [
        "keyboards/velvet/keyboard.toml",
        "keyboards/classic_qube/keyboard_velvet.toml",
    ],
    {30, 31},
    48,
    0,
    5,
)
check_projection(
    "keyboards/k04/keyboard.toml",
    [
        "keyboards/k03/keyboard.toml",
        "keyboards/classic_qube/keyboard_k03.toml",
    ],
    set(),
    60,
    3,
    4,
)
PY

for file in "${non_k04_profiles[@]}"; do
    expect_toml "$file" combo_max_length 4
    expect_toml "$file" max_patterns_per_key 8
    expect_toml "$file" protocol_max_bulk_size 8
    expect_toml "$file" protocol_macro_chunk_size 64
done

for file in "${split_profiles[@]}"; do
    expect_toml "$file" split_pairing_timeout_seconds 30
done

for file in "${standalone_split_profiles[@]}"; do
    expect_toml "$file" split_central_sleep_timeout_seconds 120
done

for file in "${qube_profiles[@]}"; do
    case "$file" in
        keyboards/k04/*) expected=300 ;;
        *) expected=900 ;;
    esac
    expect_toml "$file" split_central_sleep_timeout_seconds "$expected"
done

for file in "${profiles[@]}"; do
    expect_toml "$file" start_addr 0xCC000
    expect_toml "$file" num_sectors 32
done

memory_files=(
    keyboards/imperial44/memory.x
    keyboards/k03/memory.x
    keyboards/k04/memory_halves.x
    keyboards/k04/memory_qube.x
    keyboards/op36/memory.x
    keyboards/classic_qube/memory_halves.x
    keyboards/classic_qube/memory_qube.x
    keyboards/velvet/memory.x
)

if [[ -e keyboards/k04/memory.x ]]; then
    fail "keyboards/k04/memory.x: source file would shadow the generated topology linker script"
fi
rg -Fq 'include_bytes!("memory_halves.x")' keyboards/k04/build.rs \
    || fail "keyboards/k04/build.rs: Standalone halves must use memory_halves.x"
rg -Fq 'include_bytes!("memory_qube.x")' keyboards/k04/build.rs \
    || fail "keyboards/k04/build.rs: Qube dongle must use memory_qube.x"
for file in "${memory_files[@]}"; do
    rg -Fq 'Reserve 0xCC000..0xEC000 for RMK storage.' "$file" \
        || fail "$file: unified storage reservation is missing"
    rg -q 'FLASH[[:space:]]*:[[:space:]]*ORIGIN[[:space:]]*=[[:space:]]*0x000(26000|01000),[[:space:]]*LENGTH[[:space:]]*=[[:space:]]*(664|812)K' "$file" \
        || fail "$file: application linker must stop at 0xCC000"
done

python3 - <<'PY' || fail "K:04 standalone CI must build the production profile"
from pathlib import Path

workflow = Path(".github/workflows/build.yml").read_text(encoding="utf-8")
for output_name in ("k04_series_k04", "k04_series_mini", "k04_series_micro"):
    marker = f"output_name: {output_name}"
    start = workflow.index(marker)
    end = workflow.find("          - keyboard:", start + len(marker))
    stanza = workflow[start:] if end == -1 else workflow[start:end]
    assert 'build_features: "--features production_v30g"' in stanza, output_name

assert "cargo build --release --bin central ${{ matrix.build_features }}" in workflow
assert "cargo build --release --bin peripheral ${{ matrix.build_features }}" in workflow
assert workflow.count('build_features: "--features production_v30g"') == 3
PY

python3 - <<'PY' || fail "K:04 local build instructions drifted from the production profile"
from pathlib import Path

script = Path("scripts/build_k04_matrix.sh").read_text(encoding="utf-8")
readme = Path("keyboards/k04/README.md").read_text(encoding="utf-8")

standalone_body = script.split("build_k04_series_profile() {", 1)[1].split(
    "build_classic_qube_profile() {", 1
)[0]
assert 'cargo build --release "${bins[@]}" --features production_v30g' in standalone_body
assert standalone_body.count("--features production_v30g") == 1
for call in (
    "build_k04_series_profile k04 keyboard.toml vial.json",
    "build_k04_series_profile mini keyboard_mini.toml vial_mini.json",
    "build_k04_series_profile micro keyboard_micro.toml vial_micro.json",
):
    assert script.count(call) == 1, call

standalone_readme, qube_readme = readme.split("Qube K:04:", 1)
standalone_command = (
    "cargo build --release --bin central --bin peripheral --bin hardreset "
    "--features production_v30g"
)
assert standalone_readme.count(standalone_command) == 3

k04_qube_body = script.split("build_k04_qube_profile() {", 1)[1].split(
    'echo "Using BINDGEN_EXTRA_CLANG_ARGS=', 1
)[0]
assert k04_qube_body.count(
    "cargo build --release --bin qube --no-default-features --features qube"
) == 1
assert k04_qube_body.count(
    "cargo build --release --bin left --bin right --no-default-features --features qube-half"
) == 1
assert "production_v30g" not in k04_qube_body
assert qube_readme.count(
    "cargo build --release --bin qube --no-default-features --features qube"
) == 1
assert qube_readme.count(
    "cargo build --release --bin left --bin right --no-default-features --features qube-half"
) == 1
assert "production_v30g" not in qube_readme
PY

python3 - <<'PY' || fail "K:04 production_v30g feature scope is invalid"
from pathlib import Path

k04 = Path("keyboards/k04/Cargo.toml").read_text(encoding="utf-8")
rmk = Path("rmk/Cargo.toml").read_text(encoding="utf-8")
entry = Path("rmk-macro/src/codegen/entry.rs").read_text(encoding="utf-8")
ble = Path("rmk/src/ble/mod.rs").read_text(encoding="utf-8")
split = Path("rmk/src/split/ble/central.rs").read_text(encoding="utf-8")
central = Path("rmk/src/split/central.rs").read_text(encoding="utf-8")
assert 'production_v30g = ["production_v22", "fixed_mouse_pacing_15ms", "rmk/host_first_split_wake"]' in k04
assert 'fixed_mouse_pacing_15ms = ["mouse_vector_preserve", "rmk/fixed_mouse_pacing_15ms"]' in k04
assert 'host_first_split_wake = []' in rmk
assert 'production_v30g' not in rmk
assert "adaptive_mouse_pacing" not in k04 + rmk
assert entry.count("#use_2m_phy,") == 3
assert "run_peripheral_manager_with_profile_and_phy::<" in entry
legacy = central.split("pub async fn run_peripheral_manager_with_profile<", 1)[1].split(
    "pub async fn run_peripheral_manager_with_profile_and_phy<", 1
)[0]
assert "use_2m_phy: bool" not in legacy
assert "id, addr, stack, profile, true" in " ".join(legacy.split())
phy_aware = central.split("pub async fn run_peripheral_manager_with_profile_and_phy<", 1)[1]
assert "use_2m_phy: bool" in phy_aware
assert "new_with_host_power_and_phy_config" in entry
assert 'if split_phy_update_requested(use_2m_phy)' in split
assert '#[cfg(feature = "host_first_split_wake")]' in ble
assert '#[cfg(feature = "host_first_split_wake")]' in split
PY

rg -Fq 'usb_config.device_release = keyboard_config.device_release;' rmk/src/usb/mod.rs \
    || fail "rmk/src/usb/mod.rs: configured bcdDevice is not applied to the USB descriptor"
rg -Fq 'device_release: #device_release' rmk-macro/src/codegen/keyboard_config.rs \
    || fail "rmk-macro: generated keyboard identity does not carry bcdDevice"
if rg -Fq 'BLE_HOST_SESSION_SEQUENCE.fetch_add' rmk/src/ble/mod.rs; then
    fail "rmk/src/ble/mod.rs: generic BLE path must not require atomic read-modify-write"
fi
rg -Fq 'watch_split_link_health(stack, conn.raw(), id)' rmk/src/split/ble/peripheral.rs \
    || fail "rmk split peripheral: production split-health recovery is missing"
rg -Fq 'send_hid_mouse_report(buttons, x, y, wheel, pan).await;' rmk/src/input_device/pointing.rs \
    || fail "rmk pointing: native i16 motion must enter the HID queue as one item"
rg -Fq 'cfg!(feature = "production_v22") && !is_central' keyboards/k04/src/trackball/motion_pacing.rs \
    && fail "keyboards/k04: Qube cadence must be isolated from the standalone production profile"

mapfile -t build_scripts < <(
    git ls-files 'keyboards/*/build.rs' |
        while read -r file; do
            [[ -f "$file" ]] && printf '%s\n' "$file"
        done
)
for file in "${build_scripts[@]}"; do
    if [[ "$file" == "keyboards/k04/build.rs" ]]; then
        rg -q 'const STANDALONE_RELEASE_VERSION: &str = "0\.1\.10";' "$file" \
            || fail "$file: K:04 Standalone release version must be 0.1.10"
        rg -q 'const STANDALONE_FIRMWARE_VERSION: &str = "0\.1\.10";' "$file" \
            || fail "$file: K:04 Standalone numeric firmware version must be 0.1.10"
        rg -q 'const STANDALONE_FIRMWARE_VERSION_BCD: &str = "0x0110";' "$file" \
            || fail "$file: K:04 Standalone BCD firmware version must be 0x0110"
        rg -q 'const QUBE_FIRMWARE_VERSION: &str = "0\.1\.10";' "$file" \
            || fail "$file: K:04 Qube firmware version must be 0.1.10"
        rg -q 'const QUBE_FIRMWARE_VERSION_BCD: &str = "0x0110";' "$file" \
            || fail "$file: K:04 Qube BCD firmware version must be 0x0110"
    elif [[ "$file" == "keyboards/k03/build.rs" || "$file" == "keyboards/imperial44/build.rs" ]]; then
        rg -q 'const FIRMWARE_VERSION: &str = "0\.1\.9";' "$file" \
            || fail "$file: firmware version must be 0.1.9"
        rg -q 'const FIRMWARE_VERSION_BCD: &str = "0x0109";' "$file" \
            || fail "$file: BCD firmware version must be 0x0109"
    else
        rg -q 'const FIRMWARE_VERSION: &str = "0\.1\.8";' "$file" \
            || fail "$file: firmware version must be 0.1.8"
        rg -q 'const FIRMWARE_VERSION_BCD: &str = "0x0108";' "$file" \
            || fail "$file: BCD firmware version must be 0x0108"
    fi
done

mapfile -t vial_definitions < <(
    git ls-files 'keyboards/*/vial*.json' |
        while read -r file; do
            [[ -f "$file" ]] && printf '%s\n' "$file"
        done
)
for file in "${vial_definitions[@]}"; do
    expected_version="0.1.8"
    case "$file" in
        keyboards/k03/vial.json|keyboards/imperial44/vial.json)
            expected_version="0.1.9"
            ;;
        keyboards/k04/vial.json|keyboards/k04/vial_mini.json|keyboards/k04/vial_micro.json|\
        keyboards/k04/vial_qube.json|keyboards/k04/vial_qube_mini.json|keyboards/k04/vial_qube_micro.json)
            expected_version="0.1.10"
            ;;
    esac
    jq -e '.manufacturer == "Ergohaven"' "$file" >/dev/null \
        || fail "$file: manufacturer must be Ergohaven"
    jq -e --arg version "$expected_version" '
        .firmware.name == "RMK"
        and .firmware.version == $version
        and .firmwareVersion == $version
    ' "$file" >/dev/null \
        || fail "$file: RMK identity and both firmware versions must equal $expected_version"
done

python3 - <<'PY' || fail "all production profiles must advertise their release package identity"
import json
import re

static_profiles = [
    ("keyboards/imperial44/vial.json", "imperial44"),
    ("keyboards/k03/vial.json", "k03"),
    ("keyboards/k04/vial.json", "k04"),
    ("keyboards/k04/vial_micro.json", "k04-micro"),
    ("keyboards/k04/vial_mini.json", "k04-mini"),
    ("keyboards/k04/vial_qube.json", "k04-qube"),
    ("keyboards/k04/vial_qube_micro.json", "k04-micro-qube"),
    ("keyboards/k04/vial_qube_mini.json", "k04-mini-qube"),
    ("keyboards/op36/vial.json", "op36"),
    ("keyboards/classic_qube/vial.json", "op36-qube"),
    ("keyboards/velvet/vial.json", "velvet"),
]

for path, expected_asset in static_profiles:
    with open(path, encoding="utf-8") as source:
        definition = json.load(source)
    assert definition["firmware"]["name"] == "RMK", path
    assert definition["entropy"]["firmwareUpdate"]["asset"] == expected_asset, path

with open("keyboards/classic_qube/build.rs", encoding="utf-8") as source:
    classic_qube_build = source.read()
expected_generated_assets = {
    "0036": "op36-qube",
    "0044": "imperial44-qube",
    "0070": "k03-qube",
    "00BE": "velvet-qube",
}
for product_id, expected_asset in expected_generated_assets.items():
    pattern = rf'0x{product_id}\s*=>\s*"{re.escape(expected_asset)}"'
    assert re.search(pattern, classic_qube_build), (product_id, expected_asset)

# The static definitions cover 11 binaries. Three additional classic Qube
# variants reuse a standalone definition and are rewritten by build.rs.
assert len(static_profiles) + 3 == 14
PY

for file in "${vial_definitions[@]}"; do
    jq -e '.entropy.batteryHalves == true' "$file" >/dev/null \
        || fail "$file: split devices must advertise entropy.batteryHalves"
done

for file in keyboards/classic_qube/vial.json keyboards/k04/vial_qube{,_mini,_micro}.json; do
    jq -e '
        .entropy.batteryHalves == true
        and (.entropy.liveFeatures | index("time") != null)
        and (.entropy.liveFeatures | index("media") != null)
    ' "$file" >/dev/null || fail "$file: Qube must advertise time, media, and half batteries"
done

default_names_source=keyboards/common/default_layer_names.rs
python3 - "$default_names_source" <<'PY' || fail "$default_names_source: factory layer-name profiles drifted"
import ast
import re
import sys

source = open(sys.argv[1], encoding="utf-8").read()

def rust_array(name):
    match = re.search(
        rf"pub const {name}:.*?=\s*\[(.*?)\];",
        source,
        flags=re.DOTALL,
    )
    if not match:
        raise SystemExit(f"missing {name}")
    return ast.literal_eval("[" + match.group(1) + "]")

numeric_tail = [str(index) for index in range(5, 16)]
assert rust_array("STANDARD_NO_MOUSE") == [
    "Base", "Navigation", "Symbols", "Adjust", "4", *numeric_tail
]
assert rust_array("STANDARD_WITH_MOUSE") == [
    "Base", "Navigation", "Symbols", "Adjust", "Mouse", *numeric_tail
]
PY

standard_no_mouse_roots=(
    keyboards/imperial44/src/central.rs
    keyboards/k03/src/central.rs
    keyboards/op36/src/central.rs
)
for file in "${standard_no_mouse_roots[@]}"; do
    rg -Fq 'default_layer_names::STANDARD_NO_MOUSE' "$file" \
        || fail "$file: standard no-Mouse layer names are missing"
done

standard_with_mouse_roots=(
    keyboards/k04/src/central.rs
    keyboards/k04/src/qube.rs
    keyboards/velvet/src/central.rs
)
for file in "${standard_with_mouse_roots[@]}"; do
    rg -Fq 'default_layer_names::STANDARD_WITH_MOUSE' "$file" \
        || fail "$file: standard Mouse layer names are missing"
done

rg -Fq 'crate::default_layer_names::STANDARD_NO_MOUSE' keyboards/classic_qube/build.rs \
    || fail "keyboards/classic_qube/build.rs: generated non-pointing Qube defaults drifted"
rg -Fq 'crate::default_layer_names::STANDARD_WITH_MOUSE' keyboards/classic_qube/build.rs \
    || fail "keyboards/classic_qube/build.rs: generated Velvet Qube defaults drifted"
rg -Fq 'const STORAGE_VERSION: u8 = 2;' keyboards/common/layer_names.rs \
    || fail "keyboards/common/layer_names.rs: default-name migration version drifted"
for file in keyboards/k04/src/layer_names.rs; do
    rg -Fq 'const STORAGE_VERSION: u8 = 5;' "$file" \
        || fail "$file: K:04 settings migration version drifted"
    rg -Fq 'migrate_legacy_placeholders();' "$file" \
        || fail "$file: generated layer-name migration is missing"
    if rg -q '\b323\b' "$file"; then
        fail "$file: removed host disconnect QSID 323 is still handled"
    fi
    rg -Fq 'const IDX_RESERVED_HOST_DISCONNECT_TIMEOUT: usize = 37;' "$file" \
        || fail "$file: reserved host-timeout storage byte moved"
done

host_power_module=keyboards/common/ble_host_power.rs
rg -Fq 'BleHostPowerConfig::new(' "$host_power_module" \
    || fail "$host_power_module: shared host BLE power policy is missing"
rg -Fq 'SPLIT_CENTRAL_SLEEP_TIMEOUT_SECONDS' "$host_power_module" \
    || fail "$host_power_module: host idle deadline must follow the standalone 120-second sleep contract"
rg -Fq 'const HOST_DISCONNECT_TIMEOUT_SECONDS: u64 = 30 * 60;' "$host_power_module" \
    || fail "$host_power_module: host disconnect policy must remain fixed at 30 minutes"

standalone_central_roots=(
    keyboards/imperial44/src/central.rs
    keyboards/k03/src/central.rs
    keyboards/k04/src/central.rs
    keyboards/op36/src/central.rs
    keyboards/velvet/src/central.rs
)
for file in "${standalone_central_roots[@]}"; do
    rg -Fq '#[path = "../../common/ble_host_power.rs"]' "$file" \
        || fail "$file: shared standalone host BLE power module is missing"
done

standalone_build_scripts=(
    keyboards/imperial44/build.rs
    keyboards/k03/build.rs
    keyboards/k04/build.rs
    keyboards/op36/build.rs
    keyboards/velvet/build.rs
)
for file in "${standalone_build_scripts[@]}"; do
    rg -Fq 'RMK_BLE_HOST_POWER_CONFIG_FN=crate::ble_host_power::ble_host_power_config' "$file" \
        || fail "$file: standalone host BLE power callback is missing"
done
rg -Uq 'if is_standalone\(product_id\) \{\n[[:space:]]+println!\("cargo:rustc-env=RMK_BLE_HOST_POWER_CONFIG_FN=crate::ble_host_power::ble_host_power_config"\);\n[[:space:]]+\}' keyboards/k04/build.rs \
    || fail "keyboards/k04/build.rs: K:04 host BLE power callback must remain Standalone-only"
if rg -Fq 'RMK_BLE_HOST_POWER_CONFIG_FN' keyboards/classic_qube/build.rs; then
    fail "keyboards/classic_qube/build.rs: USB Qube must not enable the host BLE power callback"
fi
if rg -Fq 'ble_host_power_config' keyboards/k04/src/layer_names.rs; then
    fail "keyboards/k04/src/layer_names.rs: host BLE power policy must stay in the shared owner"
fi
if rg -Fq '../../common/ble_host_power.rs' keyboards/k04/src/qube.rs; then
    fail "keyboards/k04/src/qube.rs: USB Qube must not include the host BLE power module"
fi

k04_vial_definitions=(
    keyboards/k04/vial.json
    keyboards/k04/vial_micro.json
    keyboards/k04/vial_mini.json
    keyboards/k04/vial_qube.json
    keyboards/k04/vial_qube_micro.json
    keyboards/k04/vial_qube_mini.json
)
python3 - "${k04_vial_definitions[@]}" <<'PY' \
    || fail "K:04 USER00..USER40 registry drifted"
import json
import sys

expected_reserved = [f"EH_RSRV{index:02d}" for index in range(19)]
expected_active = [
    "BT0",
    "BT1",
    "BT2",
    "BT3",
    "BT4",
    "BT_NEXT",
    "BT_PREV",
    "BT_CLR",
    "BT_TOG",
    "EH_SNP",
    "EH_SCR",
    "EH_TXT",
    "EH_L_SNP",
    "EH_L_SCR",
    "EH_L_TXT",
    "EH_USR1",
    "EH_USR2",
    "EH_USR3",
    "BT_OUT",
    "USB_OUT",
    "BT_BATTERY",
    "BT_CLR_PEER",
]

for path in sys.argv[1:]:
    with open(path, encoding="utf-8") as source:
        definition = json.load(source)
        custom_keycodes = definition["customKeycodes"]

    assert [entry["name"] for entry in custom_keycodes[:19]] == expected_reserved
    assert all(entry["shortName"] == "" and entry["title"] == "" for entry in custom_keycodes[:19])
    assert [entry["name"] for entry in custom_keycodes[19:]] == expected_active
    assert all(field.get("qsid") != 323 for section in definition.get("settings", []) for field in section["fields"])
PY

classic_user_registry='BT0,BT1,BT2,BT3,BT4,BT_NEXT,BT_PREV,BT_CLR,BT_TOG,BT_PEER'
for file in \
    keyboards/imperial44/vial.json \
    keyboards/k03/vial.json \
    keyboards/op36/vial.json \
    keyboards/velvet/vial.json
do
    actual="$(jq -r '.customKeycodes[0:10] | map(.name) | join(",")' "$file")"
    if [[ "$actual" != "$classic_user_registry" ]]; then
        fail "$file: classic USER00..USER09 registry drifted"
    fi
done

if find keyboards/velvet_ui -type f -print -quit 2>/dev/null | grep -q .; then
    fail "keyboards/velvet_ui: obsolete duplicate profile must stay removed"
fi

for directory in keyboards/trackball keyboards/trackball_v30 keyboards/trackball_v31 keyboards/trackball_royale; do
    if find "$directory" -type f -print -quit 2>/dev/null | grep -q .; then
        fail "$directory: unsupported standalone trackball firmware must stay removed"
    fi
done

[[ "$(rg -c '^\[\[split\.central\.input_device\.pmw3610\]\]$' keyboards/velvet/keyboard.toml)" == "1" ]] \
    || fail "keyboards/velvet/keyboard.toml: right central must own one optional PMW3610"
[[ "$(rg -c '^\[\[split\.peripheral\.input_device\.pmw3610\]\]$' keyboards/classic_qube/keyboard_velvet.toml)" == "1" ]] \
    || fail "keyboards/classic_qube/keyboard_velvet.toml: right peripheral must own one optional PMW3610"
for file in keyboards/velvet/keyboard.toml keyboards/classic_qube/keyboard_velvet.toml; do
    [[ "$(rg -c '^\[\[behavior\.auto_mouse_layer\]\]$' "$file")" == "1" ]] \
        || fail "$file: persistent Velvet settings require one runtime-configurable auto Mouse entry"
    [[ "$(rg -c '^name = "Mouse"$' "$file")" == "1" ]] \
        || fail "$file: unified Velvet must define one Mouse factory layer"
done

python3 - <<'PY' || fail "Velvet pointing runtime contract drifted"
import tomllib

for path in (
    "keyboards/velvet/keyboard.toml",
    "keyboards/classic_qube/keyboard_velvet.toml",
):
    with open(path, "rb") as source:
        config = tomllib.load(source)
    refresh_subs = config["event"]["peripheral_settings_refresh"]["subs"]
    assert refresh_subs == 1, (
        f"{path}: event.peripheral_settings_refresh.subs={refresh_subs}, expected 1"
    )
    if path == "keyboards/velvet/keyboard.toml":
        layer_subs = config["event"]["layer_change"]["subs"]
        assert layer_subs >= 3, (
            f"{path}: event.layer_change.subs={layer_subs}, expected at least 3 "
            "for auto Mouse, Velvet mode, and the split manager"
        )
        central = config["split"]["central"]
        peripheral = config["split"]["peripheral"][0]
        assert central["row_offset"] == 4, f"{path}: right half must be central"
        assert peripheral["row_offset"] == 0, f"{path}: left half must be peripheral"
        devices = [central]
    else:
        devices = config["split"]["peripheral"]
    pointing = [
        sensor
        for peripheral in devices
        for sensor in peripheral.get("input_device", {}).get("pmw3610", [])
    ]
    assert len(pointing) == 1, f"{path}: expected one PMW3610, found {len(pointing)}"
    assert pointing[0]["smart_mode"] is True, f"{path}: PMW3610 smart_mode must be true"
    assert pointing[0]["report_hz"] == 125, f"{path}: PMW3610 report_hz must be 125"
    auto_mouse = config["behavior"]["auto_mouse_layer"]
    assert auto_mouse == [{
        "device_id": 0,
        "target_layer": 4,
        "timeout": "500ms",
        "threshold": 2,
    }], f"{path}: runtime-configurable auto Mouse seed drifted: {auto_mouse!r}"
PY

actual_velvet_modes="$(jq -r '.customKeycodes[10:13] | map(.name) | join(",")' keyboards/velvet/vial.json)"
if [[ "$actual_velvet_modes" != "EH_SNP,EH_SCR,EH_TXT" ]]; then
    fail "keyboards/velvet/vial.json: Velvet pointing USER10..USER12 registry drifted"
fi
jq -e '
    .productId == "0x00BE"
    and .layouts.labels == ["Right trackball instead of key"]
    and ([.layouts.keymap[][] | select(type == "string")] | index("7,1\n\n\n0,0") != null)
    and ([.settings[].fields[].qsid] | sort == [
        121, 127, 128, 129, 131, 135, 138, 139, 141, 142, 143, 144, 145, 146, 148, 324, 328, 330, 334
    ])
' keyboards/velvet/vial.json >/dev/null \
    || fail "keyboards/velvet/vial.json: unified layout or backed trackball settings drifted"
rg -Fq '#[path = "../../common/velvet_pointing.rs"]' keyboards/velvet/src/central.rs \
    || fail "keyboards/velvet/src/central.rs: shared Velvet pointing owner is missing"
rg -Fq '#[path = "../../common/velvet_device_settings.rs"]' keyboards/velvet/src/central.rs \
    || fail "keyboards/velvet/src/central.rs: persistent Velvet settings owner is missing"
rg -Fq 'crate::velvet_pointing::VelvetPointingSettingsSync' keyboards/velvet/src/central.rs \
    || fail "keyboards/velvet/src/central.rs: right-central PMW3610 settings sync is missing"
rg -Fq 'AutoMouseLayerConfigEvent' keyboards/common/velvet_pointing.rs \
    || fail "keyboards/common/velvet_pointing.rs: persistent settings no longer update the generic auto Mouse runner"
if rg -q 'VelvetPointingSettingsSync|velvet_pointing.rs' keyboards/velvet/src/peripheral.rs; then
    fail "keyboards/velvet/src/peripheral.rs: left peripheral must not own PMW3610 settings"
fi
rg -Fq 'crate::velvet_device_settings::vial_device_settings' keyboards/velvet/build.rs \
    || fail "keyboards/velvet/build.rs: standalone Velvet settings provider is missing"
rg -Fq 'crate::velvet_device_settings::vial_device_settings' keyboards/classic_qube/build.rs \
    || fail "keyboards/classic_qube/build.rs: Qube Velvet settings provider is missing"
rg -Fq '#[cfg(velvet_pointing)]' keyboards/classic_qube/src/qube.rs \
    || fail "keyboards/classic_qube/src/qube.rs: Velvet-only Qube pointing-mode registration is missing"

reset_source=tools/settings_reset/src/main.rs
rg -Fq 'const STORAGE_RANGE: (u32, u32) = (0xCC000, 0xEC000);' "$reset_source" \
    || fail "$reset_source: unified storage reset range drifted"
rg -q '0xA0000|0xC0000|ERASE_RANGES' "$reset_source" \
    && fail "$reset_source: reset must erase only the unified storage partition"

migration_source=tools/storage_migrate/src/main.rs
rg -Fq 'const LEGACY_START: u32 = 0xA0000;' "$migration_source" \
    || fail "$migration_source: legacy source address drifted"
rg -Fq 'const UNIFIED_START: u32 = 0xCC000;' "$migration_source" \
    || fail "$migration_source: unified destination address drifted"
rg -Fq 'fn destination_is_safe() -> bool' "$migration_source" \
    || fail "$migration_source: destination safety preflight is missing"

for tool in settings_reset storage_migrate; do
    if [[ -e "tools/$tool/memory.x" ]]; then
        fail "tools/$tool/memory.x: source file would shadow the generated Qube linker script"
    fi
    rg -Fq 'include_bytes!("memory_halves.x")' "tools/$tool/build.rs" \
        || fail "tools/$tool/build.rs: halves linker selection is missing"
    rg -Fq 'include_bytes!("memory_qube.x")' "tools/$tool/build.rs" \
        || fail "tools/$tool/build.rs: Qube linker selection is missing"
    rg -q 'FLASH[[:space:]]*:[[:space:]]*ORIGIN[[:space:]]*=[[:space:]]*0x00026000' "tools/$tool/memory_halves.x" \
        || fail "tools/$tool/memory_halves.x: application origin must be 0x26000"
    rg -q 'FLASH[[:space:]]*:[[:space:]]*ORIGIN[[:space:]]*=[[:space:]]*0x00001000' "tools/$tool/memory_qube.x" \
        || fail "tools/$tool/memory_qube.x: application origin must be 0x1000"
done

if ((failures > 0)); then
    echo "Ergohaven firmware profile contract failed with $failures error(s)." >&2
    exit 1
fi

echo "Ergohaven firmware profile contract OK (${#profiles[@]} production profiles)."

# Selectable Vial Universal Symbols profiles

Rynk remains the default protocol. Existing keyboard TOMLs, descriptors,
hardware mappings, topology and production features are unchanged. Vial builds
use separate TOMLs (only `host.vial_enabled` / `host.rynk_enabled` differ) and
separate descriptors. Mini and Qube do not opt into the new ISO contract.

From the repository root, select both files and all standalone options explicitly:

```sh
KEYBOARD_TOML_PATH="$PWD/keyboards/k04/keyboard_vial.toml" \
VIAL_JSON_PATH="$PWD/keyboards/k04/vial_universal.json" \
cargo check --manifest-path keyboards/k04/Cargo.toml \
  --target thumbv7em-none-eabihf --no-default-features \
  --features vial_standalone,production_v30g,universal_mac_iso --bin central
```

Use `--bin peripheral` for the other role. For K:04 Micro use
`keyboard_micro_vial.toml` and `vial_micro_universal.json` in the same K:04 crate.
For `k03`, `imperial44`, `op36`, and `velvet`, use `keyboard_vial.toml`,
`vial_universal.json`, and `--features vial_standalone,universal_mac_iso` in that
model's crate. Their hardware configurations and role offsets are not copied
from K:04. The ISO masks are respectively K:04/Micro=4, K:03/I44/OP36=1, Velvet=2.
Build scripts reject both/no protocols, Vial without its complete standalone
alias, Rynk with the ISO alias, and ISO without the matching descriptor.

`protocol_rynk` selects the existing Ergohaven Rynk features;
`protocol_vial` selects Vial plus the current `host_lock` feature. The
`vial_standalone` alias preserves K:04's nonprotocol `standalone` options when
Cargo defaults are disabled. Qube aliases retain Rynk explicitly. CI keeps every
existing Rynk row and adds six separately named Vial rows, each building both roles.
Board build scripts forward their active feature aliases to the configuration
macro through `RMK_BOARD_FEATURES`; this avoids mistaking manifest dependency
features for the actually selected protocol.

## Persistence boundary

The existing schema hash includes **all RMK features**. Enabling ISO changes that
hash and can trigger the existing storage reset policy. No hash/reset migration
is changed here. Export before flashing a different feature build; do not infer
upgrade persistence from the same-build restore tests. Within the same build,
platform and packed options restore without rewriting absent default records;
keymaps and default layers retain their independent records. Host layout-option
SET still updates the live keymap and returns a persistence failure rather than
a success ACK if storage fails. The staged keymap/macro and timed-flash APIs are
not replaced.

## Vial compatibility capabilities

Native-action wire revision stays **2**. This is not Rynk negotiation and is not a
V1 postcard compatibility endpoint. A paired configurator must negotiate the
native version and preserve the actual TapHold profile representation.

* `0x0100` (`ERGOHAVEN_NATIVE_KEY_ACTION_CAP_UNIVERSAL_NUMERO`) is advertised only
  by Vial with Universal Symbols. It certifies sparse User action `0xB4`;
  the older broad symbols bit alone does not certify that action.
* `0x0200` (`ERGOHAVEN_NATIVE_KEY_ACTION_CAP_PROFILE_RESOLUTION`) certifies the
  read-only profile lookup, independently of Universal Symbols.

Request: `[08, E8, 08, 02, index, ...]` (hex). Response is zero-initialized:

| Bytes | Meaning |
|---|---|
| 0..4 | `08 E8 08 02` |
| 4 | status: 0 success, 2 unsupported native version |
| 5 | requested index echo |
| 6 | 1 for an actual table entry; 0 for default fallback |
| 7 | reserved zero |
| 8..16 | resolved raw MorseProfile u64, little-endian |
| 16..24 | raw global default MorseProfile u64, little-endian |
| 24..26 | profile-table entry count u16, little-endian |
| 26..32 | reserved zeros |

For unsupported versions only the header, status and index echo are populated.
Lookup follows the actual keymap resolver: index zero is not implicitly default,
and a missing index falls back to the global default. Optional fields remain
optional; no per-field default merge, allocation, staging or write occurs.
Rynk's capability revision, handlers and native codec are unchanged.

Local compile checks and host tests do not establish hardware acceptance or a
working paired Entropy build. Paired version-aware client integration and its
verification are separate acceptance gates.

# Universal Symbols ANSI/ISO profiles

All 14 official RMK profiles use Rynk and expose the existing ANSI/ISO selector
in Entropy's Universal Symbols live features: seven standalone profiles and seven
Qube profiles. The geometry selector does not replace hardware mappings or
unrelated production features.

Phenom, Phenom Mini and Phenom Micro use official QMK/Vial firmware for now.
Their RMK production builds/releases are paused; their manual configurations
and existing geometry/descriptor checks remain available.

Separate Vial firmware artifacts are no longer built or published by the official
firmware matrix, including the former K:03, K:04 Micro, Imperial44, OP36 and
Velvet variants. Existing published releases are historical and are not removed.
Vial runtime support, Cargo aliases, TOMLs and descriptors remain available for
manual builds; this does not change QMK/Vial firmware. Official Rynk firmware
uses Entropy/Rynk, not the Vial application.

Manual Vial builds use separate TOMLs (only `host.vial_enabled` /
`host.rynk_enabled` differ). Each official Rynk row and manual Vial profile
selects a matching static `*_universal.json` descriptor.

For a manual Vial build, select both files and all standalone options explicitly
from the repository root:

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
from K:04. The ISO masks are K:04/Mini/Micro and Phenom/Mini/Micro=4,
K:03/I44/OP36=1, and Velvet=2, including their Qube profiles.
Build scripts reject both/no protocols, Vial without its complete standalone
alias, and ISO without the matching descriptor on the selectable-protocol boards.

`protocol_rynk` selects the existing Ergohaven Rynk features;
`protocol_vial` selects Vial plus the current `host_lock` feature. The
`vial_standalone` alias preserves K:04's nonprotocol `standalone` options when
Cargo defaults are disabled. Qube aliases retain Rynk explicitly. CI retains
the active Rynk rows and no separate Vial firmware rows. Core Vial protocol tests remain
enabled; manual availability does not imply an official Vial firmware release.
Board build scripts forward their active feature aliases to the configuration
macro through `RMK_BOARD_FEATURES`; this avoids mistaking manifest dependency
features for the actually selected protocol.

## Feature and layout-option ownership

Standalone rows use the board's `universal_mac_iso` alias without replacing
their existing feature flags. K:04 Qube dongle and both halves enable that same
alias. Classic Qube rows explicitly use `rmk/universal_symbols_iso_bit0` for
OP36/K:03/I44 or `rmk/universal_symbols_iso_bit1` for Velvet on all three roles;
no model-specific runtime switch is needed. `.github/workflows/build.yml` is the
official profile inventory; `scripts/build_k04_matrix.sh` mirrors the Rynk rows.

The geometry selector is prepended, not appended. Layout options pack from the
last label toward bit zero, so existing encoder-hide and trackball bits keep
their positions. KLE condition indices and Velvet's `layoutOption` setting
reference advance by one; coordinates, encoder markers and custom keycodes do
not change. ANSI is value zero and remains the default for existing stored
options. ISO changes only macOS Universal Symbols grave/tilde, including their
Russian fallback; ordinary keys and PC strokes are unchanged.

## Timed window switching on Phenom

Phenom Full, Mini and Micro expose `WNEXT` (`User(54)`) and `WPREV` (`User(55)`).
They send Tab or Shift+Tab while holding Alt in PC mode or Command in Mac mode.
Repeated presses extend a 650 ms timer; expiry releases the owned modifier and
commits the selection. Layer changes and universal symbol/language actions also
commit it. On macOS, Command+Tab selects applications rather than windows within
one application. Slots 50–53 remain reserved; these actions do not add the
separate Previous/Next Word implementation.

## Dedicated Russian letters and host layouts

Native actions `User(0xB5)`, `User(0xB6)` and `User(0xB7)` provide Ж, Э and Ё
while Russian is active; existing action IDs are unchanged. They use the existing
Shift and Caps Word handling. Configurators must require the extended-Russian
capability: Rynk `EXTENDED_RUSSIAN_LETTERS` or Vial native-actions bit `0x0800`.

PC mode retains the QMK-compatible Russian mapping, with Ё on Grave. Mac mode
targets Apple's Russian layout, with Ё on Backslash. The two-state platform
setting does not distinguish the Russian–PC input source on macOS: in PC mode,
Shift+Ё can produce Latin Ë (`U+00CB`) instead of Cyrillic Ё (`U+0401`). This
change accepts that host-layout limitation; it does not add a separate
Russian–PC-on-macOS mode or change other PC mappings. The ANSI/ISO selector
changes universal grave/tilde punctuation only and is not a fix for this letter.

## Persistence boundary

The Rynk storage feature signature excludes the three geometry flags and
`ergohaven_window_switching`: these change reports, not stored framing. Enabling
the transient window helper therefore does not by itself invalidate existing
Rynk storage. No new stored setting or format is introduced. Other feature
changes can still trigger the existing reset policy; Vial retains its established
feature-hash policy. This is not a general migration guarantee.
Export before flashing a different feature build; do not infer general upgrade
persistence from the same-build restore tests. Within the same build,
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

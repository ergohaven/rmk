# Universal actions in Vial macros

Firmware built with `universal_symbols` advertises capability `0x0400` in
the little-endian native-action capability flags (`0x03 / 0xE8 / 0x02`,
protocol version 1). A host must require this bit, Universal Symbols
support, and extended macro support before offering these macro actions.

Extended macro opcodes 5 (tap), 6 (press), and 7 (release) accept these
macro-only aliases as a little-endian 16-bit keycode:

| Macro alias | Native action |
| --- | --- |
| `0x7E80..=0x7E84` | Universal layout controls, `Action::User(0x80..=0x84)` |
| `0x7E90..=0x7EAF` | Universal symbols, `Action::User(0x90..=0xAF)` |
| `0x7EB0..=0x7EB3` | Universal Russian letters, `Action::User(0xB0..=0xB3)` |

For example, `01 05 82 7E 00` is a macro containing a tap of Universal EN
followed by the macro terminator. Encoding and decoding preserve these
aliases. Unknown Universal IDs remain unsupported. Normal Vial keymap
conversion is unchanged; native keymap actions still use the native-action
protocol.

The macro operations use the normal Universal action dispatcher. Universal
commands act on press and ignore release, so a tap executes once and a
release operation alone has no effect. Existing layout synchronization and
platform settings continue to apply.

Macro slots can also be selected by native Tap-Hold and Morse actions.
Macros execute on key release: crossing the hold threshold selects the
hold action, then releasing the key runs its macro. This capability does
not change that timing.

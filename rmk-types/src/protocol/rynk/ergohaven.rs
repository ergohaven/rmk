//! Ergohaven's Rynk extension: the commands in `0x7E00..=0x7EFF`, high in the
//! request range where RMK's own groups never reach, for what Ergohaven
//! firmware exposed over Vial. The table and its revision cover the whole
//! range: a command in it that the table does not name gets `UnknownCmd`, as
//! an unknown upstream command does.
//!
//! Frames, errors and the upstream table are untouched. The wire a build speaks
//! is named by [`ERGOHAVEN_RYNK_REVISION`], not `ProtocolVersion`, which RMK
//! keeps while reshaping payloads: hosts require an exact match, and a test
//! fails whenever a wire snapshot changes without a new revision.

use heapless::Vec;
use postcard::experimental::max_size::MaxSize;
use serde::{Deserialize, Serialize};

#[cfg(any(test, not(feature = "host")))]
use super::RynkError;
#[cfg(test)]
use super::command::EndpointMeta;
use super::command::{Cmd, Endpoint, assert_unique, endpoints};
use super::{FirmwareVersion, LayoutChunk};

/// The Rynk wire this build speaks, upstream and Ergohaven commands together.
/// Bump it whenever [`ERGOHAVEN_RYNK_WIRE_HASH`] changes.
pub const ERGOHAVEN_RYNK_REVISION: u16 = 2;

/// FNV-1a over the wire snapshots (`snapshots/wire_values.snap`,
/// `snapshots/wire_frames.snap`, `snapshots/ergohaven.snap`) at
/// [`ERGOHAVEN_RYNK_REVISION`].
pub const ERGOHAVEN_RYNK_WIRE_HASH: u32 = 0x2c6f_b985;

/// [`ErgohavenInfo::caps`] bits: what the firmware can do. Whether a board has
/// something to do it with (a screen, layout options) is in its definition.
pub mod caps {
    /// `Action::User` ids of Universal Symbols are understood.
    pub const UNIVERSAL_SYMBOLS: u32 = 1 << 0;
    /// Universal Symbols include the Russian letters.
    pub const RUSSIAN_LETTERS: u32 = 1 << 1;
    /// `GetBatteryHalves` reports both halves.
    pub const BATTERY_HALVES: u32 = 1 << 2;
    /// `SetHostData` takes the time.
    pub const HOST_TIME: u32 = 1 << 3;
    /// `SetHostData` takes the media artist and title.
    pub const HOST_MEDIA: u32 = 1 << 4;
    /// `SetHostData` takes the host keyboard layout.
    pub const HOST_KEYBOARD_LAYOUT: u32 = 1 << 5;
    /// `GetLayoutOption` and `SetLayoutOption` are served.
    pub const LAYOUT_OPTIONS: u32 = 1 << 6;
    /// Keymap and macro writes reach flash after the reply; `CommitWrites`
    /// and `GetWriteStatus` tell when.
    pub const STAGED_WRITES: u32 = 1 << 7;
}

/// What `GetErgohavenInfo` reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, MaxSize)]
pub struct ErgohavenInfo {
    /// The firmware's [`ERGOHAVEN_RYNK_REVISION`].
    pub revision: u16,
    /// The board firmware's version, not the `rmk` crate's.
    pub firmware: FirmwareVersion,
    /// The board's Vial keyboard id: the same 8 bytes, in the same order, as
    /// Vial's `GetKeyboardId` reply.
    pub keyboard_uid: [u8; 8],
    /// [`caps`] bits.
    pub caps: u32,
    /// FNV-1a of the board definition `GetBoardDefinition` pages through.
    pub definition_hash: u32,
}

/// Longest value of one setting.
pub const SETTING_VALUE_MAX_LEN: usize = 28;

/// Most setting ids in one `GetSettingKeys` page.
pub const SETTING_KEYS_PAGE_LEN: usize = 64;

/// One setting's value, in the bytes Vial carries for the same id:
/// little-endian at the id's width, a layer name as UTF-8 plus a NUL.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingValue(pub Vec<u8, SETTING_VALUE_MAX_LEN>);

impl MaxSize for SettingValue {
    const POSTCARD_MAX_SIZE: usize = crate::heapless_vec_max_size::<u8, SETTING_VALUE_MAX_LEN>();
}

/// `SetSetting`'s request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetSettingRequest {
    pub id: u16,
    pub value: SettingValue,
}

impl MaxSize for SetSettingRequest {
    const POSTCARD_MAX_SIZE: usize = u16::POSTCARD_MAX_SIZE + SettingValue::POSTCARD_MAX_SIZE;
}

/// The setting ids above `GetSettingKeys`'s cursor, ascending; empty past the
/// last one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingKeysPage(pub Vec<u16, SETTING_KEYS_PAGE_LEN>);

impl MaxSize for SettingKeysPage {
    const POSTCARD_MAX_SIZE: usize = crate::heapless_vec_max_size::<u16, SETTING_KEYS_PAGE_LEN>();
}

/// Longest media text `SetHostData` carries, in bytes: 32 Cyrillic characters.
pub const HOST_DATA_TEXT_MAX_LEN: usize = 64;

/// What the host tells the keyboard. Variants are only appended: postcard
/// numbers them in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HostDataUpdate {
    Time {
        hour: u8,
        minute: u8,
    },
    ClearTime,
    /// Accepted and ignored, as over Vial.
    Volume(u8),
    /// 0 for English, 1 for Russian.
    KeyboardLayout(u8),
    /// Raw bytes, as over Vial: the firmware keeps the valid UTF-8 prefix.
    MediaArtist(Vec<u8, HOST_DATA_TEXT_MAX_LEN>),
    MediaTitle(Vec<u8, HOST_DATA_TEXT_MAX_LEN>),
}

impl MaxSize for HostDataUpdate {
    // The variant tag, then the largest variant: a media text.
    const POSTCARD_MAX_SIZE: usize = 1 + crate::heapless_vec_max_size::<u8, HOST_DATA_TEXT_MAX_LEN>();
}

/// Battery levels of a split keyboard's halves, as Vial reported them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, MaxSize)]
pub struct BatteryHalves {
    /// Bit 0: `left` is known, bit 1: `right` is.
    pub mask: u8,
    /// Percent, 0xFF when unknown.
    pub left: u8,
    /// Percent, 0xFF when unknown.
    pub right: u8,
}

/// Writes waiting for flash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, MaxSize)]
pub struct WriteStatus {
    pub pending_keys: u16,
    pub pending_macro_chunks: u16,
    /// A staged write failed since the last `CommitWrites`.
    pub fault: bool,
}

endpoints! {
    // Ergohaven (0x7Exx).
    GetErgohavenInfo = 0x7E01: () => ErgohavenInfo;
    /// Pages through the board definition: Vial's xz-compressed JSON.
    GetBoardDefinition = 0x7E02: u32 => LayoutChunk;
    GetSettingKeys = 0x7E10: u16 => SettingKeysPage;
    GetSetting = 0x7E11: u16 => SettingValue;
    SetSetting = 0x7E12: SetSettingRequest => ();
    GetLayoutOption = 0x7E20: () => u32;
    SetLayoutOption = 0x7E21: u32 => ();
    SetHostData = 0x7E30: HostDataUpdate => ();
    /// Also asks the halves for fresh levels.
    GetBatteryHalves = 0x7E40: () => BatteryHalves;
    /// Starts writing staged keymap and macro changes now; the reply comes
    /// before they land.
    CommitWrites = 0x7E50: () => WriteStatus;
    GetWriteStatus = 0x7E51: () => WriteStatus;
}

#[cfg(not(feature = "host"))]
const _: () = core::assert!(
    super::RYNK_MAX_PAYLOAD_SIZE >= MAX_ENDPOINT_PAYLOAD,
    "rynk_buffer_size is too small to hold the largest Ergohaven rynk frame; increase it"
);

#[cfg(test)]
mod tests {
    extern crate alloc;

    use alloc::vec::Vec;

    use super::*;
    use crate::protocol::rynk::RynkHeader;
    use crate::protocol::rynk::tests::snapshot::{assert_snapshot, format_value_snapshot};
    use crate::protocol::rynk::tests::{assert_max_size_bound, round_trip};

    fn info() -> ErgohavenInfo {
        ErgohavenInfo {
            revision: 0x0102,
            firmware: FirmwareVersion {
                major: 3,
                minor: 4,
                patch: 5,
            },
            keyboard_uid: [6, 7, 8, 9, 10, 11, 12, 13],
            caps: 0x0E0F,
            definition_hash: 0x1011_1213,
        }
    }

    fn text(bytes: &[u8]) -> heapless::Vec<u8, HOST_DATA_TEXT_MAX_LEN> {
        heapless::Vec::from_slice(bytes).unwrap()
    }

    #[test]
    fn every_payload_round_trips_within_its_max_size() {
        assert_max_size_bound(&round_trip(&info()));
        let value = SettingValue(heapless::Vec::from_slice(&[0xAB; SETTING_VALUE_MAX_LEN]).unwrap());
        assert_max_size_bound(&round_trip(&value));
        assert_max_size_bound(&round_trip(&SetSettingRequest { id: u16::MAX, value }));
        let keys = SettingKeysPage((0..SETTING_KEYS_PAGE_LEN as u16).map(|k| k * 1000).collect());
        assert_max_size_bound(&round_trip(&keys));
        for update in [
            HostDataUpdate::Time { hour: 23, minute: 59 },
            HostDataUpdate::ClearTime,
            HostDataUpdate::Volume(100),
            HostDataUpdate::KeyboardLayout(1),
            HostDataUpdate::MediaArtist(text(&[0xFF; HOST_DATA_TEXT_MAX_LEN])),
            HostDataUpdate::MediaTitle(text(b"")),
        ] {
            assert_max_size_bound(&round_trip(&update));
        }
        assert_max_size_bound(&round_trip(&BatteryHalves {
            mask: 3,
            left: 0xFF,
            right: 100,
        }));
        assert_max_size_bound(&round_trip(&WriteStatus {
            pending_keys: u16::MAX,
            pending_macro_chunks: u16::MAX,
            fault: true,
        }));
    }

    #[test]
    fn commands_stay_in_the_ergohaven_range() {
        for row in ENDPOINT_META {
            assert!((0x7E00..=0x7EFF).contains(&row.cmd), "{} = {:#06x}", row.name, row.cmd);
        }
    }

    fn encode<T: Serialize>(value: &T) -> Vec<u8> {
        let mut buf = [0u8; 256];
        postcard::to_slice(value, &mut buf).unwrap().to_vec()
    }

    fn frame<T: Serialize>(cmd: Cmd, value: &T) -> Vec<u8> {
        let mut buf = [0u8; 256];
        let n = crate::protocol::rynk::encode_frame(&mut buf, RynkHeader { cmd, seq: 1 }, value).unwrap();
        buf[..n].to_vec()
    }

    fn ok<T>(value: T) -> Result<T, RynkError> {
        Ok(value)
    }

    /// Every Ergohaven request and its `Ok` reply as full frames, plus the
    /// payload exemplars, so a reshaped type or a renumbered command flips
    /// bytes. A change also changes [`ERGOHAVEN_RYNK_WIRE_HASH`].
    #[test]
    fn ergohaven_wire_locked() {
        let value = SettingValue(heapless::Vec::from_slice(&[0x2C, 0x01]).unwrap());
        let set = SetSettingRequest {
            id: 120,
            value: value.clone(),
        };
        let keys = SettingKeysPage(heapless::Vec::from_slice(&[2, 6, 120, 300]).unwrap());
        let chunk = LayoutChunk {
            total_len: 2000,
            bytes: heapless::Vec::from_slice(&[0xFD, 0x37, 0x7A]).unwrap(),
        };
        let artist = HostDataUpdate::MediaArtist(text("Ария".as_bytes()));
        let longest_title = HostDataUpdate::MediaTitle(text("Ж".repeat(HOST_DATA_TEXT_MAX_LEN / 2).as_bytes()));
        let halves = BatteryHalves {
            mask: 3,
            left: 80,
            right: 0xFF,
        };
        let status = WriteStatus {
            pending_keys: 960,
            pending_macro_chunks: 5,
            fault: false,
        };
        let entries: Vec<(&str, Vec<u8>)> = alloc::vec![
            ("ErgohavenInfo", encode(&info())),
            ("SettingValue", encode(&value)),
            ("SetSettingRequest", encode(&set)),
            ("SettingKeysPage", encode(&keys)),
            (
                "HostDataUpdate::Time",
                encode(&HostDataUpdate::Time { hour: 9, minute: 5 })
            ),
            ("HostDataUpdate::ClearTime", encode(&HostDataUpdate::ClearTime)),
            ("HostDataUpdate::Volume", encode(&HostDataUpdate::Volume(40))),
            (
                "HostDataUpdate::KeyboardLayout",
                encode(&HostDataUpdate::KeyboardLayout(1))
            ),
            ("HostDataUpdate::MediaArtist", encode(&artist)),
            (
                "HostDataUpdate::MediaTitle",
                encode(&HostDataUpdate::MediaTitle(text(b"Song")))
            ),
            ("HostDataUpdate::MediaTitle, longest", encode(&longest_title)),
            ("BatteryHalves", encode(&halves)),
            ("WriteStatus", encode(&status)),
            ("GetErgohavenInfo request", frame(Cmd::GetErgohavenInfo, &())),
            ("GetErgohavenInfo reply", frame(Cmd::GetErgohavenInfo, &ok(info()))),
            ("GetBoardDefinition request", frame(Cmd::GetBoardDefinition, &1952u32)),
            ("GetBoardDefinition reply", frame(Cmd::GetBoardDefinition, &ok(chunk))),
            ("GetSettingKeys request", frame(Cmd::GetSettingKeys, &0u16)),
            ("GetSettingKeys reply", frame(Cmd::GetSettingKeys, &ok(keys))),
            ("GetSetting request", frame(Cmd::GetSetting, &120u16)),
            ("GetSetting reply", frame(Cmd::GetSetting, &ok(value))),
            (
                "GetSetting reply Err(Invalid)",
                frame(Cmd::GetSetting, &Err::<SettingValue, _>(RynkError::Invalid))
            ),
            ("SetSetting request", frame(Cmd::SetSetting, &set)),
            ("SetSetting reply", frame(Cmd::SetSetting, &ok(()))),
            ("GetLayoutOption request", frame(Cmd::GetLayoutOption, &())),
            ("GetLayoutOption reply", frame(Cmd::GetLayoutOption, &ok(3u32))),
            ("SetLayoutOption request", frame(Cmd::SetLayoutOption, &3u32)),
            ("SetLayoutOption reply", frame(Cmd::SetLayoutOption, &ok(()))),
            ("SetHostData request", frame(Cmd::SetHostData, &artist)),
            ("SetHostData reply", frame(Cmd::SetHostData, &ok(()))),
            ("GetBatteryHalves request", frame(Cmd::GetBatteryHalves, &())),
            ("GetBatteryHalves reply", frame(Cmd::GetBatteryHalves, &ok(halves))),
            ("CommitWrites request", frame(Cmd::CommitWrites, &())),
            ("CommitWrites reply", frame(Cmd::CommitWrites, &ok(status))),
            ("GetWriteStatus request", frame(Cmd::GetWriteStatus, &())),
            ("GetWriteStatus reply", frame(Cmd::GetWriteStatus, &ok(status))),
        ];
        let view: Vec<(&str, &[u8])> = entries.iter().map(|(l, b)| (*l, b.as_slice())).collect();
        let actual = format_value_snapshot(
            "snapshots/ergohaven.snap",
            "Ergohaven wire snapshot",
            "# Payload exemplars and full frames (SEQ 1) of the Ergohaven commands. A diff\n\
             # here means the Ergohaven wire changed. If intentional, bump\n\
             # ERGOHAVEN_RYNK_REVISION, regenerate with `--features host,ergohaven` and\n\
             # update ERGOHAVEN_RYNK_WIRE_HASH:",
            "ergohaven_wire",
            &view,
        );
        assert_snapshot("snapshots/ergohaven.snap", actual);
    }

    /// The upstream snapshots are in the hash too: a sync that reshapes an RMK
    /// payload changes this build's wire as much as an Ergohaven change does.
    #[test]
    fn wire_revision_matches_the_snapshots() {
        let hash = [
            include_str!("snapshots/wire_values.snap"),
            include_str!("snapshots/wire_frames.snap"),
            include_str!("snapshots/ergohaven.snap"),
        ]
        .iter()
        .flat_map(|snapshot| snapshot.bytes())
        .fold(0x811c_9dc5_u32, |hash, byte| {
            (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
        });
        assert_eq!(
            hash, ERGOHAVEN_RYNK_WIRE_HASH,
            "the Rynk wire changed: bump ERGOHAVEN_RYNK_REVISION and set ERGOHAVEN_RYNK_WIRE_HASH to {hash:#010x}"
        );
    }
}

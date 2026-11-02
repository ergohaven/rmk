//! Ergohaven's commands (`rmk_types::protocol::rynk::ergohaven`), served next
//! to RMK's own table.

use heapless::Vec;
use rmk_types::protocol::rynk::ergohaven::{
    BatteryHalves, CommitWrites, ERGOHAVEN_RYNK_REVISION, ErgohavenInfo, GetBatteryHalves, GetBoardDefinition,
    GetErgohavenInfo, GetLayoutOption, GetSetting, GetSettingKeys, GetWriteStatus, HOST_DATA_TEXT_MAX_LEN,
    HostDataUpdate, SetHostData, SetLayoutOption, SetSetting, SetSettingRequest, SettingKeysPage, SettingValue,
    WriteStatus, caps,
};
use rmk_types::protocol::rynk::{Cmd, FirmwareVersion, LayoutChunk, RYNK_BLE_CHUNK_SIZE, RynkError, RynkMessage};

use super::RynkService;
use super::handlers::{Handle, serve};
use crate::host::settings::{self, SETTING_VALUE_LEN};

/// What this build can do, as `GetErgohavenInfo` reports it.
const CAPS: u32 = caps::LAYOUT_OPTIONS
    | caps::STAGED_WRITES
    | caps::HOST_TIME
    | caps::HOST_MEDIA
    | caps::HOST_KEYBOARD_LAYOUT
    | if cfg!(feature = "universal_symbols") {
        caps::UNIVERSAL_SYMBOLS | caps::RUSSIAN_LETTERS
    } else {
        0
    }
    | if cfg!(all(feature = "split", feature = "_ble")) {
        caps::BATTERY_HALVES
    } else {
        0
    };

impl RynkService<'_> {
    /// Serve an Ergohaven command; `None` for a command outside its table.
    pub(super) async fn dispatch_ergohaven(&self, msg: &mut RynkMessage<'_>) -> Option<Result<(), RynkError>> {
        Some(match msg.header().cmd {
            Cmd::GetErgohavenInfo => serve::<GetErgohavenInfo, _>(self, msg).await,
            Cmd::GetBoardDefinition => serve::<GetBoardDefinition, _>(self, msg).await,
            Cmd::GetLayoutOption => serve::<GetLayoutOption, _>(self, msg).await,
            Cmd::SetLayoutOption => serve::<SetLayoutOption, _>(self, msg).await,
            Cmd::GetSettingKeys => serve::<GetSettingKeys, _>(self, msg).await,
            Cmd::GetSetting => serve::<GetSetting, _>(self, msg).await,
            Cmd::SetSetting => serve::<SetSetting, _>(self, msg).await,
            Cmd::SetHostData => serve::<SetHostData, _>(self, msg).await,
            Cmd::GetBatteryHalves => serve::<GetBatteryHalves, _>(self, msg).await,
            Cmd::CommitWrites => serve::<CommitWrites, _>(self, msg).await,
            Cmd::GetWriteStatus => serve::<GetWriteStatus, _>(self, msg).await,
            _ => return None,
        })
    }

    /// The Ergohaven commands that persist, gated like RMK's own setters.
    pub(super) fn ergohaven_writes(cmd: Cmd) -> bool {
        matches!(cmd, Cmd::SetLayoutOption | Cmd::SetSetting)
    }
}

impl Handle<GetErgohavenInfo> for RynkService<'_> {
    async fn handle(&self, _: ()) -> Result<ErgohavenInfo, RynkError> {
        let config = &self.ergohaven;
        let mut keyboard_uid = [0; 8];
        let len = config.keyboard_uid.len().min(keyboard_uid.len());
        keyboard_uid[..len].copy_from_slice(&config.keyboard_uid[..len]);
        let version = config.firmware_version;
        Ok(ErgohavenInfo {
            revision: ERGOHAVEN_RYNK_REVISION,
            firmware: FirmwareVersion {
                major: (version >> 16) as u8,
                minor: (version >> 8) as u8,
                patch: version as u8,
            },
            keyboard_uid,
            caps: CAPS,
            // FNV-1a: a cache key for the host, not a checksum.
            definition_hash: config.board_definition.iter().fold(0x811c_9dc5, |hash, &byte| {
                (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
            }),
        })
    }
}

impl Handle<GetBoardDefinition> for RynkService<'_> {
    async fn handle(&self, offset: u32) -> Result<LayoutChunk, RynkError> {
        let blob = self.ergohaven.board_definition;
        let start = (offset as usize).min(blob.len());
        let end = (start + RYNK_BLE_CHUNK_SIZE).min(blob.len());
        let bytes = Vec::from_slice(&blob[start..end]).map_err(|_| RynkError::Internal)?;
        Ok(LayoutChunk {
            total_len: blob.len() as u32,
            bytes,
        })
    }
}

impl Handle<GetLayoutOption> for RynkService<'_> {
    async fn handle(&self, _: ()) -> Result<u32, RynkError> {
        Ok(self.ctx.layout_options())
    }
}

impl Handle<SetLayoutOption> for RynkService<'_> {
    async fn handle(&self, options: u32) -> Result<(), RynkError> {
        self.ctx
            .set_layout_options(options)
            .await
            .or(Err(RynkError::StorageFault))
    }
}

impl Handle<GetSettingKeys> for RynkService<'_> {
    async fn handle(&self, after: u16) -> Result<SettingKeysPage, RynkError> {
        let keys = settings::keys_after(after, self.ergohaven.device_settings);
        Ok(SettingKeysPage(
            keys.take(rmk_types::protocol::rynk::ergohaven::SETTING_KEYS_PAGE_LEN)
                .collect(),
        ))
    }
}

impl Handle<GetSetting> for RynkService<'_> {
    async fn handle(&self, id: u16) -> Result<SettingValue, RynkError> {
        let mut value = [0; SETTING_VALUE_LEN];
        let len = settings::get(&self.ctx, self.ergohaven.device_settings, id, &mut value).ok_or(RynkError::Invalid)?;
        Ok(SettingValue(
            Vec::from_slice(&value[..len]).map_err(|_| RynkError::Internal)?,
        ))
    }
}

impl Handle<SetSetting> for RynkService<'_> {
    async fn handle(&self, request: SetSettingRequest) -> Result<(), RynkError> {
        // Zero-padded like a Vial report, which is what the board hooks parse.
        let mut value = [0; SETTING_VALUE_LEN];
        value[..request.value.0.len()].copy_from_slice(&request.value.0);
        if settings::set(&self.ctx, self.ergohaven.device_settings, request.id, &value).await {
            Ok(())
        } else {
            Err(RynkError::Invalid)
        }
    }
}

// A media text the host may send fits whole where the screen reads it.
const _: () = core::assert!(crate::host_data::HOST_TEXT_LEN >= HOST_DATA_TEXT_MAX_LEN);

impl Handle<SetHostData> for RynkService<'_> {
    async fn handle(&self, update: HostDataUpdate) -> Result<(), RynkError> {
        match update {
            HostDataUpdate::Time { hour, minute } => crate::host_data::update_time(hour, minute),
            HostDataUpdate::ClearTime => crate::host_data::clear_time(),
            // The firmware has nothing to show it on, as over Vial.
            HostDataUpdate::Volume(_) => {}
            HostDataUpdate::KeyboardLayout(layout) => crate::host_data::update_layout(layout),
            HostDataUpdate::MediaArtist(text) => {
                crate::host_data::update_media_artist(crate::host_data::valid_text(&text))
            }
            HostDataUpdate::MediaTitle(text) => {
                crate::host_data::update_media_title(crate::host_data::valid_text(&text))
            }
        }
        Ok(())
    }
}

impl Handle<GetBatteryHalves> for RynkService<'_> {
    async fn handle(&self, _: ()) -> Result<BatteryHalves, RynkError> {
        let (mask, left, right) = self.ctx.battery_halves();
        Ok(BatteryHalves { mask, left, right })
    }
}

/// The staged writes still waiting for flash.
fn write_status() -> WriteStatus {
    #[cfg(feature = "storage")]
    let (keys, chunks, fault) = crate::storage::staged_write_status();
    #[cfg(not(feature = "storage"))]
    let (keys, chunks, fault) = (0, 0, false);
    WriteStatus {
        pending_keys: keys as u16,
        pending_macro_chunks: chunks as u16,
        fault,
    }
}

impl Handle<CommitWrites> for RynkService<'_> {
    async fn handle(&self, _: ()) -> Result<WriteStatus, RynkError> {
        #[cfg(feature = "storage")]
        crate::storage::commit_staged_writes();
        Ok(write_status())
    }
}

impl Handle<GetWriteStatus> for RynkService<'_> {
    async fn handle(&self, _: ()) -> Result<WriteStatus, RynkError> {
        Ok(write_status())
    }
}

#[cfg(test)]
mod tests {
    extern crate alloc;

    use alloc::collections::VecDeque;
    use alloc::vec::Vec;
    use core::sync::atomic::{AtomicU8, Ordering};

    use rmk_types::action::KeyAction;
    use rmk_types::protocol::rynk::{LockStatus, MatrixState};

    use super::super::tests::{ChunkRead, VecWrite, decode_frames, req, req_with};
    use super::*;
    use crate::config::{
        BehaviorConfig, ErgohavenConfig, LockConfig, PositionalConfig, RmkConfig, VialDeviceSettings,
        VialDeviceSettingsData,
    };
    use crate::event::KeyboardEvent;
    use crate::keymap::{KeyMap, KeymapData};
    use crate::test_support::test_block_on as block_on;

    const UID: [u8; 8] = [0x80, 0x04, 1, 2, 3, 4, 5, 6];

    /// A board with a one-byte setting 120 it accepts, a name at 200 it only
    /// reads, and 300 it lists but cannot read now.
    static BOARD_VALUE: AtomicU8 = AtomicU8::new(4);

    fn board_get(key: u16, out: &mut [u8]) -> Option<usize> {
        match key {
            120 => {
                out[0] = BOARD_VALUE.load(Ordering::Relaxed);
                Some(1)
            }
            200 => {
                out[..5].copy_from_slice(b"Base\0");
                Some(5)
            }
            _ => None,
        }
    }

    fn board_set(key: u16, value: &[u8]) -> bool {
        key == 120 && {
            BOARD_VALUE.store(value[0], Ordering::Relaxed);
            true
        }
    }

    const BOARD: VialDeviceSettings<'static> = VialDeviceSettings {
        setting_keys: &[120, 200, 300],
        get_setting: board_get,
        set_setting: board_set,
        serialize: VialDeviceSettingsData::empty,
        deserialize: |_| {},
    };

    /// Serve `requests` on one session of a service configured with `definition`
    /// and return each reply's payload.
    fn exchange(definition: &'static [u8], requests: impl IntoIterator<Item = Vec<u8>>) -> Vec<Vec<u8>> {
        let mut behavior = BehaviorConfig::default();
        let positional: PositionalConfig<1, 1> = PositionalConfig::default();
        let mut data: KeymapData<1, 1, 1, 0> = KeymapData::new([[[KeyAction::No]]]);
        let keymap = block_on(KeyMap::new(&mut data, &mut behavior, &positional));
        let config = RmkConfig {
            ergohaven_config: ErgohavenConfig {
                keyboard_uid: &UID,
                board_definition: definition,
                firmware_version: 0x0002_0301,
                device_settings: Some(BOARD),
            },
            ..Default::default()
        };
        let service = RynkService::new(&keymap, &config);
        let mut rx = ChunkRead {
            chunks: VecDeque::from_iter(requests),
        };
        let mut tx = VecWrite { captured: Vec::new() };
        block_on(service.run_session(&mut rx, &mut tx));
        decode_frames(&tx.captured)
            .into_iter()
            .map(|(_, _, payload)| payload)
            .collect()
    }

    #[test]
    fn info_reports_the_revision_firmware_uid_and_definition_hash() {
        let replies = exchange(b"abc", [req(Cmd::GetErgohavenInfo.raw(), 1)]);
        let info = postcard::from_bytes::<Result<ErgohavenInfo, RynkError>>(&replies[0])
            .unwrap()
            .unwrap();
        assert_eq!(info.revision, ERGOHAVEN_RYNK_REVISION);
        assert_eq!(
            info.firmware,
            FirmwareVersion {
                major: 2,
                minor: 3,
                patch: 1
            }
        );
        assert_eq!(info.keyboard_uid, UID);
        assert_ne!(info.caps & caps::LAYOUT_OPTIONS, 0);
        // FNV-1a("abc"), the published test vector.
        assert_eq!(info.definition_hash, 0x1a47_e90b);
    }

    fn reply<T: serde::de::DeserializeOwned>(payload: &[u8]) -> Result<T, RynkError> {
        postcard::from_bytes::<Result<T, RynkError>>(payload).unwrap()
    }

    /// The board's settings follow RMK's, as Vial lists them, and the cursor
    /// pages past both.
    #[test]
    fn setting_keys_page_by_cursor_and_skip_what_the_board_cannot_read() {
        let replies = exchange(
            b"",
            [0u16, 0x1B, 200]
                .iter()
                .enumerate()
                .map(|(seq, after)| req_with(Cmd::GetSettingKeys.raw(), seq as u8 + 1, after)),
        );
        let page = |i: usize| reply::<SettingKeysPage>(&replies[i]).unwrap().0.to_vec();
        assert_eq!(
            page(0),
            [0x02, 0x06, 0x07, 0x12, 0x13, 0x16, 0x17, 0x19, 0x1A, 0x1B, 120, 200]
        );
        assert_eq!(page(1), [120, 200]);
        assert!(page(2).is_empty());
    }

    #[test]
    fn settings_read_in_vial_bytes() {
        let replies = exchange(
            b"",
            [2u16, 120, 200, 999]
                .iter()
                .enumerate()
                .map(|(seq, id)| req_with(Cmd::GetSetting.raw(), seq as u8 + 1, id)),
        );
        // The combo timeout's default, little-endian.
        let combo = reply::<SettingValue>(&replies[0]).unwrap().0;
        assert_eq!(combo.len(), 2);
        assert_eq!(reply::<SettingValue>(&replies[1]).unwrap().0, [4]);
        assert_eq!(reply::<SettingValue>(&replies[2]).unwrap().0, *b"Base\0");
        assert_eq!(reply::<SettingValue>(&replies[3]), Err(RynkError::Invalid));
    }

    #[test]
    fn a_board_setting_is_set_or_turned_down() {
        let set = |id: u16, value: &[u8]| SetSettingRequest {
            id,
            value: SettingValue(heapless::Vec::from_slice(value).unwrap()),
        };
        let replies = exchange(
            b"",
            [
                req_with(Cmd::SetSetting.raw(), 1, &set(120, &[9])),
                req_with(Cmd::GetSetting.raw(), 2, &120u16),
                req_with(Cmd::SetSetting.raw(), 3, &set(200, b"Fn")),
            ],
        );
        assert_eq!(reply::<()>(&replies[0]), Ok(()));
        assert_eq!(reply::<SettingValue>(&replies[1]).unwrap().0, [9]);
        // The board reads 200 but does not take it.
        assert_eq!(reply::<()>(&replies[2]), Err(RynkError::Invalid));
    }

    /// What the host tells the keyboard lands where the screen and Universal
    /// Symbols read it; a media text cut inside a character keeps its valid start.
    #[test]
    fn host_data_reaches_the_shared_state() {
        let media = |text: &[u8]| heapless::Vec::from_slice(text).unwrap();
        let replies = exchange(
            b"",
            [
                HostDataUpdate::Time { hour: 9, minute: 5 },
                HostDataUpdate::KeyboardLayout(1),
                HostDataUpdate::MediaArtist(media("Ария".as_bytes())),
                // "Ж" is two bytes; the second is cut off.
                HostDataUpdate::MediaTitle(media(&"Ж".as_bytes()[..1])),
                HostDataUpdate::Volume(30),
            ]
            .iter()
            .enumerate()
            .map(|(seq, update)| req_with(Cmd::SetHostData.raw(), seq as u8 + 1, update)),
        );
        assert!(replies.iter().all(|r| reply::<()>(r) == Ok(())));
        let host = crate::host_data::snapshot();
        assert_eq!((host.hour, host.minute, host.layout), (Some(9), Some(5), Some(1)));
        assert_eq!(host.media_artist, "Ария");
        assert_eq!(host.media_title, "");

        exchange(b"", [req_with(Cmd::SetHostData.raw(), 1, &HostDataUpdate::ClearTime)]);
        let host = crate::host_data::snapshot();
        assert_eq!((host.hour, host.minute), (None, None));
    }

    // A 30-byte limit cut a Cyrillic title to 15 characters, too short for the
    // Qube header to scroll.
    #[test]
    fn a_cyrillic_title_at_the_limit_arrives_whole() {
        let title = "Ж".repeat(HOST_DATA_TEXT_MAX_LEN / 2);
        let update = HostDataUpdate::MediaTitle(heapless::Vec::from_slice(title.as_bytes()).unwrap());
        let replies = exchange(b"", [req_with(Cmd::SetHostData.raw(), 1, &update)]);
        assert_eq!(reply::<()>(&replies[0]), Ok(()));
        assert_eq!(crate::host_data::snapshot().media_title, title.as_str());
    }

    #[test]
    fn battery_halves_answer_with_unknown_levels_before_a_measurement() {
        let replies = exchange(b"", [req(Cmd::GetBatteryHalves.raw(), 1)]);
        assert_eq!(
            reply::<BatteryHalves>(&replies[0]),
            Ok(BatteryHalves {
                mask: 0,
                left: 0xFF,
                right: 0xFF
            })
        );
    }

    /// A key write answers before flash and waits in the stage; without a
    /// storage task here nothing drains it.
    #[cfg(feature = "storage")]
    #[test]
    fn a_key_write_answers_before_flash() {
        use rmk_types::action::{Action, KeyAction};
        use rmk_types::keycode::{HidKeyCode, KeyCode};
        use rmk_types::protocol::rynk::{KeyPosition, SetKeyRequest};

        let set = SetKeyRequest {
            position: KeyPosition {
                layer: 0,
                row: 0,
                col: 0,
            },
            action: KeyAction::Single(Action::Key(KeyCode::Hid(HidKeyCode::A))),
        };
        let replies = exchange(
            b"",
            [
                req_with(Cmd::SetKeyAction.raw(), 1, &set),
                req(Cmd::GetWriteStatus.raw(), 2),
            ],
        );
        assert_eq!(reply::<()>(&replies[0]), Ok(()));
        let status = reply::<WriteStatus>(&replies[1]).unwrap();
        assert_eq!((status.pending_keys, status.fault), (1, false));
    }

    #[cfg(feature = "storage")]
    mod staged {
        use embassy_futures::select::{Either, select};
        use embassy_time::{Duration, Timer};
        use rmk_types::action::{Action, KeyAction};
        use rmk_types::keyboard_macros::{Macro, MacroOp};
        use rmk_types::keycode::{HidKeyCode, KeyCode};
        use rmk_types::protocol::rynk::command::{SetKeyAction, SetMacro};
        use rmk_types::protocol::rynk::{KeyPosition, RYNK_HEADER_SIZE, SetKeyRequest, SetMacroRequest};

        use super::super::super::handlers::{Handle, HandleBulk};
        use super::*;
        use crate::config::StorageConfig;
        use crate::core_traits::Runnable;
        use crate::storage::{Storage, async_flash_wrapper};

        type Part = crate::test_support::InMemoryFlash<16_384, 4_096, 1>;

        /// Run `body` against a Rynk service whose keymap lives in `part`, with
        /// the storage task draining staged writes alongside.
        fn with_storage(part: &Part, body: impl AsyncFnOnce(&RynkService)) {
            crate::test_support::clear_flash_channel();
            block_on(async {
                let config = StorageConfig::default();
                let positional = PositionalConfig::<1, 1>::default();
                let mut storage = Box::pin(Storage::new(async_flash_wrapper(part.clone()), &config)).await;
                let mut data = KeymapData::new([[[KeyAction::No]]]);
                let mut behavior = BehaviorConfig::default();
                let keymap = KeyMap::new_from_storage(&mut data, Some(&mut storage), &mut behavior, &positional).await;
                let service = RynkService::new(&keymap, &RmkConfig::default());
                if let Either::First(never) = select(storage.run(), body(&service)).await {
                    never
                }
            });
        }

        async fn set_a(service: &RynkService<'_>) {
            let set = SetKeyRequest {
                position: KeyPosition {
                    layer: 0,
                    row: 0,
                    col: 0,
                },
                action: KeyAction::Single(Action::Key(KeyCode::Hid(HidKeyCode::A))),
            };
            Handle::<SetKeyAction>::handle(service, set).await.unwrap();
        }

        async fn status(service: &RynkService<'_>) -> WriteStatus {
            Handle::<GetWriteStatus>::handle(service, ()).await.unwrap()
        }

        /// A commit writes the stage now; the quiet period would hold it for 500 ms.
        #[test]
        fn commit_writes_the_stage_before_the_quiet_period_ends() {
            let part = Part::new();
            with_storage(&part, async |service| {
                let before = part.writes();
                set_a(service).await;
                Timer::after(Duration::from_millis(100)).await;
                assert_eq!(part.writes(), before, "written before the quiet period or a commit");
                assert_eq!(status(service).await.pending_keys, 1);

                Handle::<CommitWrites>::handle(service, ()).await.unwrap();
                Timer::after(Duration::from_millis(50)).await;
                assert!(part.writes() > before);
                assert_eq!(
                    status(service).await,
                    WriteStatus {
                        pending_keys: 0,
                        pending_macro_chunks: 0,
                        fault: false
                    }
                );
            });
        }

        /// A failed write is reported until the next commit starts over.
        #[test]
        fn a_failed_write_is_reported_until_the_next_commit() {
            let part = Part::new();
            with_storage(&part, async |service| {
                part.fail_writes(true);
                set_a(service).await;
                Handle::<CommitWrites>::handle(service, ()).await.unwrap();
                Timer::after(Duration::from_millis(50)).await;
                assert!(status(service).await.fault);

                part.fail_writes(false);
                set_a(service).await;
                Handle::<CommitWrites>::handle(service, ()).await.unwrap();
                Timer::after(Duration::from_millis(50)).await;
                assert!(!status(service).await.fault);
            });
        }

        /// A decoded `SetMacro` frame in `buf` that saves `A` to slot 0.
        fn set_macro_a(buf: &mut [u8]) -> RynkMessage<'_> {
            let set = SetMacroRequest {
                index: 0,
                macro_ops: Macro::from_slice(&[MacroOp::Tap(Action::Key(KeyCode::Hid(HidKeyCode::A)))]).unwrap(),
            };
            buf[..2].copy_from_slice(&Cmd::SetMacro.raw().to_le_bytes());
            let len = RYNK_HEADER_SIZE + postcard::to_slice(&set, &mut buf[RYNK_HEADER_SIZE..]).unwrap().len();
            RynkMessage::from_decoded(buf, len)
        }

        /// A macro save waits in the stage too, chunk by chunk.
        #[test]
        fn a_macro_save_waits_for_a_commit() {
            let part = Part::new();
            with_storage(&part, async |service| {
                let mut buf = [0; 64];
                HandleBulk::<SetMacro>::handle_bulk(service, &mut set_macro_a(&mut buf))
                    .await
                    .unwrap();
                assert!(status(service).await.pending_macro_chunks > 0);
                Handle::<CommitWrites>::handle(service, ()).await.unwrap();
                Timer::after(Duration::from_millis(50)).await;
                assert_eq!(status(service).await.pending_macro_chunks, 0);
            });
        }

        /// A session future is as big as its biggest handler, so the save must
        /// not hold the macro buffer it was sent.
        #[test]
        fn a_macro_save_does_not_hold_the_macro() {
            let part = Part::new();
            with_storage(&part, async |service| {
                let mut buf = [0; 64];
                let mut msg = set_macro_a(&mut buf);
                let save = HandleBulk::<SetMacro>::handle_bulk(service, &mut msg);
                assert!(core::mem::size_of_val(&save) < core::mem::size_of::<Macro>());
                save.await.unwrap();
            });
        }

        /// A reboot or a bootloader jump flushes first, so a key set just before
        /// it is in flash.
        #[test]
        fn flush_returns_once_the_stage_is_in_flash() {
            let part = Part::new();
            with_storage(&part, async |service| {
                let before = part.writes();
                set_a(service).await;
                crate::storage::flush_staged_writes().await;
                assert!(part.writes() > before);
                assert_eq!(status(service).await.pending_keys, 0);
            });
        }
    }

    /// Ergohaven's Vial firmware kept an unlock until `Lock` or a reboot, so a
    /// host that reconnects after the keyboard slept stays unlocked.
    #[test]
    fn an_unlock_outlives_the_session() {
        let mut behavior = BehaviorConfig::default();
        let positional: PositionalConfig<1, 1> = PositionalConfig::default();
        let mut data: KeymapData<1, 1, 1, 0> = KeymapData::new([[[KeyAction::No]]]);
        let keymap = block_on(KeyMap::new(&mut data, &mut behavior, &positional));
        const UNLOCK_KEYS: &[(u8, u8)] = &[(0, 0)];
        let config = RmkConfig {
            lock_config: LockConfig {
                unlock_keys: UNLOCK_KEYS,
                insecure: false,
                write_requires_unlock: false,
            },
            ..Default::default()
        };
        let service = RynkService::new(&keymap, &config);
        let session = |requests: Vec<Vec<u8>>| {
            let mut rx = ChunkRead {
                chunks: VecDeque::from(requests),
            };
            let mut tx = VecWrite { captured: Vec::new() };
            block_on(service.run_session(&mut rx, &mut tx));
            decode_frames(&tx.captured)
        };

        keymap.update_matrix_state(&KeyboardEvent::key(0, 0, true));
        let unlocked = session(alloc::vec![req(Cmd::UnlockPoll.raw(), 1)]);
        let status = postcard::from_bytes::<Result<LockStatus, RynkError>>(&unlocked[0].2)
            .unwrap()
            .unwrap();
        assert!(!status.locked);
        keymap.update_matrix_state(&KeyboardEvent::key(0, 0, false));

        let next = session(alloc::vec![req(Cmd::GetMatrixState.raw(), 1)]);
        assert!(
            postcard::from_bytes::<Result<MatrixState, RynkError>>(&next[0].2)
                .unwrap()
                .is_ok(),
            "a gated command is served in the next session without a new unlock"
        );
    }

    #[test]
    fn board_definition_pages_reassemble_into_the_blob() {
        static DEFINITION: [u8; 600] = {
            let mut blob = [0; 600];
            let mut i = 0;
            while i < blob.len() {
                blob[i] = i as u8;
                i += 1;
            }
            blob
        };
        let offsets = [0u32, 244, 488, 600];
        let replies = exchange(
            &DEFINITION,
            offsets
                .iter()
                .enumerate()
                .map(|(seq, offset)| req_with(Cmd::GetBoardDefinition.raw(), seq as u8 + 1, offset)),
        );
        let mut assembled = Vec::new();
        for reply in &replies {
            let chunk = postcard::from_bytes::<Result<LayoutChunk, RynkError>>(reply)
                .unwrap()
                .unwrap();
            assert_eq!(chunk.total_len, 600);
            assembled.extend_from_slice(&chunk.bytes);
        }
        assert_eq!(assembled, DEFINITION);
        // Past the end the page is empty.
        let last = postcard::from_bytes::<Result<LayoutChunk, RynkError>>(&replies[3])
            .unwrap()
            .unwrap();
        assert!(last.bytes.is_empty());
    }
}

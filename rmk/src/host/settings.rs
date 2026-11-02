//! Settings by QSID, the 16-bit ids of Vial's QMK settings: RMK's behavior
//! settings and the board's own. Vial and Ergohaven's Rynk commands serve the
//! same ids with the same value bytes.

use rmk_types::morse::MorseMode;
use rmk_types::protocol::vial::SettingKey;

use crate::config::VialDeviceSettings;
use crate::host::context::KeyboardContext;

/// Longest value a setting takes: what follows the id in a Vial report.
pub(crate) const SETTING_VALUE_LEN: usize = 28;

/// RMK's own settings, each always readable.
const BUILTIN_KEYS: &[u16] = &[0x02, 0x06, 0x07, 0x12, 0x13, 0x16, 0x17, 0x19, 0x1A, 0x1B];

/// The ids above `after`, in the order Vial lists them: RMK's, then those of
/// the board's the board can read now.
pub(crate) fn keys_after(after: u16, board: Option<VialDeviceSettings<'_>>) -> impl Iterator<Item = u16> + '_ {
    let board_keys = board.into_iter().flat_map(|board| {
        board
            .setting_keys
            .iter()
            .copied()
            .filter(move |&key| (board.get_setting)(key, &mut [0; SETTING_VALUE_LEN]).is_some())
    });
    BUILTIN_KEYS
        .iter()
        .copied()
        .chain(board_keys)
        .filter(move |&key| key > after)
}

fn put_u16(out: &mut [u8], value: u16) -> Option<usize> {
    out.get_mut(..2)?.copy_from_slice(&value.to_le_bytes());
    Some(2)
}

fn put_flag(out: &mut [u8], flag: bool) -> Option<usize> {
    *out.first_mut()? = flag as u8;
    Some(1)
}

/// Write setting `id`'s value into `out`: little-endian at its width, a flag
/// as one byte. Returns the length, or `None` for an id nobody serves.
pub(crate) fn get(
    ctx: &KeyboardContext<'_>,
    board: Option<VialDeviceSettings<'_>>,
    id: u16,
    out: &mut [u8],
) -> Option<usize> {
    let profile = ctx.morse_default_profile();
    match SettingKey::from(id) {
        SettingKey::None => board.and_then(|board| (board.get_setting)(id, out)),
        SettingKey::ComboTimeout => put_u16(out, ctx.combo_timeout().as_millis() as u16),
        SettingKey::MorseTimeout => put_u16(out, profile.hold_timeout_ms().unwrap_or(250)),
        SettingKey::OneShotTimeout => put_u16(out, ctx.one_shot_timeout().as_millis() as u16),
        SettingKey::TapInterval => put_u16(out, ctx.tap_interval()),
        SettingKey::TapCapslockInterval => put_u16(out, ctx.tap_capslock_interval()),
        SettingKey::PermissiveHold => put_flag(out, profile.mode() == Some(MorseMode::PermissiveHold)),
        SettingKey::HoldOnOtherKeyPress => put_flag(out, profile.mode() == Some(MorseMode::HoldOnOtherPress)),
        SettingKey::QuickTapTerm => put_u16(out, profile.quick_tap_timeout_ms().unwrap_or(0)),
        SettingKey::UnilateralTap => put_flag(out, profile.unilateral_tap().unwrap_or(false)),
        SettingKey::PriorIdleTime => put_u16(out, ctx.morse_prior_idle_time().as_millis() as u16),
    }
}

/// Set `id` from `value`, laid out as [`get`] writes it and zero-padded.
/// `false` for an id nobody serves or a value the board turns down. RMK's
/// settings persist through the context, the board's as its whole block.
pub(crate) async fn set(
    ctx: &KeyboardContext<'_>,
    board: Option<VialDeviceSettings<'_>>,
    id: u16,
    value: &[u8; SETTING_VALUE_LEN],
) -> bool {
    let number = u16::from_le_bytes([value[0], value[1]]);
    let flag = value[0] == 1;
    let profile = ctx.morse_default_profile();
    // A failed write leaves the live value set; Vial never reported it either.
    let _ = match SettingKey::from(id) {
        SettingKey::None => {
            let Some(board) = board.filter(|board| (board.set_setting)(id, value)) else {
                return false;
            };
            #[cfg(feature = "storage")]
            crate::storage::queue_device_settings_flash_write((board.serialize)());
            #[cfg(not(feature = "storage"))]
            let _ = board;
            Ok(())
        }
        SettingKey::ComboTimeout => ctx.set_combo_timeout(number).await,
        SettingKey::MorseTimeout => {
            ctx.set_morse_default_profile(profile.with_hold_timeout_ms(Some(number)))
                .await
        }
        SettingKey::OneShotTimeout => ctx.set_one_shot_timeout(number).await,
        SettingKey::TapInterval => ctx.set_tap_interval(number).await,
        SettingKey::TapCapslockInterval => ctx.set_tap_capslock_interval(number).await,
        SettingKey::PermissiveHold => {
            let mode = match (flag, profile.mode()) {
                // Hold On Other Key Press takes precedence.
                (true, Some(MorseMode::HoldOnOtherPress)) => profile.mode(),
                (true, _) => Some(MorseMode::PermissiveHold),
                (false, Some(MorseMode::PermissiveHold)) => Some(MorseMode::Normal),
                (false, mode) => mode,
            };
            ctx.set_morse_default_profile(profile.with_mode(mode)).await
        }
        SettingKey::HoldOnOtherKeyPress => {
            let mode = match (flag, profile.mode()) {
                (true, _) => Some(MorseMode::HoldOnOtherPress),
                (false, Some(MorseMode::HoldOnOtherPress)) => Some(MorseMode::Normal),
                (false, mode) => mode,
            };
            ctx.set_morse_default_profile(profile.with_mode(mode)).await
        }
        SettingKey::QuickTapTerm => {
            ctx.set_morse_default_profile(profile.with_quick_tap_timeout_ms(Some(number)))
                .await
        }
        SettingKey::UnilateralTap => {
            ctx.set_morse_default_profile(profile.with_unilateral_tap(Some(flag)))
                .await
        }
        SettingKey::PriorIdleTime => ctx.set_morse_prior_idle_time(number).await,
    };
    true
}

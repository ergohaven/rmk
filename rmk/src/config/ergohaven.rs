/// What the Ergohaven Rynk commands serve from the board build: the same
/// constants Vial's config takes, emitted by the macro.
#[derive(Clone, Copy, Debug, Default)]
pub struct ErgohavenConfig<'a> {
    /// The board's Vial keyboard id, `VIAL_KEYBOARD_ID`.
    pub keyboard_uid: &'a [u8],
    /// The board definition, `VIAL_KEYBOARD_DEF`: Vial's xz-compressed JSON.
    pub board_definition: &'a [u8],
    /// `RMK_FIRMWARE_VERSION` packed as `major << 16 | minor << 8 | patch`,
    /// like Vial's `firmware_version`.
    pub firmware_version: u32,
    /// The board's own settings, `RMK_VIAL_DEVICE_SETTINGS_FN`.
    pub device_settings: Option<super::VialDeviceSettings<'a>>,
}

/// Config for [vial](https://get.vial.today/).
///
/// You can generate automatically using [`build.rs`](https://github.com/HaoboGu/rmk/blob/main/examples/use_rust/stm32h7/build.rs).
pub const VIAL_DEVICE_SETTINGS_MAX_LEN: usize = 224;

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct VialDeviceSettingsData {
    pub len: u8,
    #[serde(with = "device_settings_data_serde")]
    pub data: [u8; VIAL_DEVICE_SETTINGS_MAX_LEN],
}

impl Default for VialDeviceSettingsData {
    fn default() -> Self {
        Self::empty()
    }
}

impl VialDeviceSettingsData {
    pub const fn empty() -> Self {
        Self {
            len: 0,
            data: [0; VIAL_DEVICE_SETTINGS_MAX_LEN],
        }
    }
}

mod device_settings_data_serde {
    use core::fmt;

    use serde::de::{Error, SeqAccess, Visitor};
    use serde::ser::SerializeTuple;
    use serde::{Deserializer, Serializer};

    use super::VIAL_DEVICE_SETTINGS_MAX_LEN;

    pub fn serialize<S>(data: &[u8; VIAL_DEVICE_SETTINGS_MAX_LEN], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut seq = serializer.serialize_tuple(VIAL_DEVICE_SETTINGS_MAX_LEN)?;
        for byte in data {
            seq.serialize_element(byte)?;
        }
        seq.end()
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<[u8; VIAL_DEVICE_SETTINGS_MAX_LEN], D::Error>
    where
        D: Deserializer<'de>,
    {
        struct BytesVisitor;

        impl<'de> Visitor<'de> for BytesVisitor {
            type Value = [u8; VIAL_DEVICE_SETTINGS_MAX_LEN];

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("Vial device settings byte array")
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut out = [0u8; VIAL_DEVICE_SETTINGS_MAX_LEN];
                for byte in &mut out {
                    *byte = seq
                        .next_element()?
                        .ok_or_else(|| A::Error::custom("short Vial device settings data"))?;
                }
                Ok(out)
            }
        }

        deserializer.deserialize_tuple(VIAL_DEVICE_SETTINGS_MAX_LEN, BytesVisitor)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct VialDeviceSettings<'a> {
    pub setting_keys: &'a [u16],
    pub get_setting: fn(u16, &mut [u8]) -> Option<usize>,
    pub set_setting: fn(u16, &[u8]) -> bool,
    pub serialize: fn() -> VialDeviceSettingsData,
    pub deserialize: fn(&[u8]),
}

#[derive(Clone, Copy, Debug, Default)]
pub struct VialConfig<'a> {
    pub vial_keyboard_id: &'a [u8],
    pub vial_keyboard_def: &'a [u8],
    pub unlock_keys: &'a [(u8, u8)],
    pub device_settings: Option<VialDeviceSettings<'a>>,
    pub vial_insecure: bool,
}

impl<'a> VialConfig<'a> {
    pub fn new(vial_keyboard_id: &'a [u8], vial_keyboard_def: &'a [u8], unlock_keys: &'a [(u8, u8)]) -> Self {
        Self {
            vial_keyboard_id,
            vial_keyboard_def,
            unlock_keys,
            device_settings: None,
            vial_insecure: false,
        }
    }
}

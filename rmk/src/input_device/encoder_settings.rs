//! Compact encoder setting codec shared by keyboards.

/// Runtime interval choices exposed to Entropy/Vial.
pub const ENCODER_INTERVALS_MS: [u16; 10] = [0, 5, 10, 15, 20, 30, 40, 60, 80, 100];

/// One-byte persistent representation for a physical encoder slot.
///
/// Bits 7..4 contain the interval table index and bits 2..0 contain steps - 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EncoderSetting {
    pub interval_index: u8,
    pub steps: u8,
}

impl Default for EncoderSetting {
    fn default() -> Self {
        Self {
            interval_index: 0,
            steps: 1,
        }
    }
}

impl EncoderSetting {
    pub const fn new(interval_index: u8, steps: u8) -> Self {
        Self {
            interval_index: if interval_index > 9 { 9 } else { interval_index },
            steps: if steps < 1 {
                1
            } else if steps > 8 {
                8
            } else {
                steps
            },
        }
    }

    pub const fn encode(self) -> u8 {
        (self.interval_index << 4) | (self.steps - 1)
    }

    pub const fn decode(value: u8) -> Self {
        Self::new(value >> 4, (value & 0x07) + 1)
    }

    pub const fn interval_ms(self) -> u16 {
        ENCODER_INTERVALS_MS[self.interval_index as usize]
    }
}

/// Encode one central-to-peripheral packet. Physical IDs are stored verbatim,
/// so a K:03 peripheral consumes IDs 3..5 rather than renumbering them.
pub fn encode_settings_packet<const N: usize>(marker: u8, settings: &[EncoderSetting; N]) -> [u8; 27] {
    assert!(N <= 25);
    let mut packet = [0u8; 27];
    packet[0] = marker;
    packet[1] = N as u8;
    let mut id = 0;
    while id < N {
        packet[2 + id] = settings[id].encode();
        id += 1;
    }
    packet
}

/// Decode the packed storage tail. Missing/short legacy data migrates to the
/// historical defaults (0 ms, one action per detent).
pub fn decode_storage_settings<const N: usize>(bytes: Option<&[u8]>) -> [EncoderSetting; N] {
    core::array::from_fn(|id| {
        bytes
            .and_then(|bytes| bytes.get(id))
            .copied()
            .map(EncoderSetting::decode)
            .unwrap_or_default()
    })
}

/// Decode and apply a packet through the RMK runtime APIs.
pub fn apply_settings_packet(marker: u8, packet: &[u8; 27]) -> bool {
    if packet[0] != marker || usize::from(packet[1]) > 25 {
        return false;
    }
    let mut id = 0usize;
    while id < usize::from(packet[1]) {
        let setting = EncoderSetting::decode(packet[2 + id]);
        let _ = super::rotary_encoder::set_encoder_interval_ms(id as u8, setting.interval_ms());
        let _ = super::rotary_encoder::set_encoder_steps(id as u8, setting.steps);
        id += 1;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_defaults_clamps_and_roundtrips() {
        assert_eq!(EncoderSetting::default().encode(), 0);
        for interval in 0..10 {
            for steps in 1..=8 {
                let setting = EncoderSetting::new(interval, steps);
                assert_eq!(EncoderSetting::decode(setting.encode()), setting);
            }
        }
        assert_eq!(EncoderSetting::new(250, 0), EncoderSetting::new(9, 1));
        assert_eq!(EncoderSetting::new(0, 250), EncoderSetting::new(0, 8));
    }

    #[test]
    fn packet_preserves_physical_ids_and_applies_runtime_values() {
        let settings = [
            EncoderSetting::new(0, 1),
            EncoderSetting::new(1, 2),
            EncoderSetting::new(2, 3),
            EncoderSetting::new(3, 4),
            EncoderSetting::new(4, 5),
            EncoderSetting::new(9, 8),
        ];
        let packet = encode_settings_packet(0xec, &settings);
        assert_eq!(&packet[2..8], &[0x00, 0x11, 0x22, 0x33, 0x44, 0x97]);
        assert!(apply_settings_packet(0xec, &packet));
        assert_eq!(super::super::rotary_encoder::encoder_interval_ms(3), Some(15));
        assert_eq!(super::super::rotary_encoder::encoder_steps(3), 4);
        assert_eq!(super::super::rotary_encoder::encoder_interval_ms(5), Some(100));
        assert_eq!(super::super::rotary_encoder::encoder_steps(5), 8);
        assert!(!apply_settings_packet(0xed, &packet));
    }

    #[test]
    fn storage_defaults_migration_and_roundtrip() {
        assert_eq!(decode_storage_settings::<6>(None), [EncoderSetting::default(); 6]);
        assert_eq!(
            decode_storage_settings::<2>(Some(&[EncoderSetting::new(9, 8).encode()])),
            [EncoderSetting::new(9, 8), EncoderSetting::default()]
        );
        let raw = [0x00, 0x11, 0x97];
        let decoded = decode_storage_settings::<3>(Some(&raw));
        assert_eq!(decoded.map(EncoderSetting::encode), raw);
    }
}

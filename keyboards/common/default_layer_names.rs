//! Factory layer-name profiles shared by Ergohaven firmware.

#![allow(dead_code)]

pub const LAYER_NAME_COUNT: usize = 16;

pub const STANDARD_NO_MOUSE: [&str; LAYER_NAME_COUNT] = {
    let mut names = [""; LAYER_NAME_COUNT];
    names[0] = "Base";
    names[1] = "Num + Nav";
    names[2] = "Symbols";
    names[3] = "Adjust";
    names
};

pub const STANDARD_WITH_MOUSE: [&str; LAYER_NAME_COUNT] = {
    let mut names = STANDARD_NO_MOUSE;
    names[4] = "Mouse";
    names
};

pub const PHENOM: [&str; LAYER_NAME_COUNT] = {
    let mut names = STANDARD_WITH_MOUSE;
    names[3] = "";
    names
};

const NUMERIC: [&str; LAYER_NAME_COUNT] = [
    "0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12", "13", "14", "15",
];

const GENERATED: [&str; LAYER_NAME_COUNT] = [
    "Layer 0", "Layer 1", "Layer 2", "Layer 3", "Layer 4", "Layer 5", "Layer 6", "Layer 7", "Layer 8", "Layer 9",
    "Layer 10", "Layer 11", "Layer 12", "Layer 13", "Layer 14", "Layer 15",
];

/// Numeric names were written by older Entropy builds as generated
/// placeholders. They are migrated once to the model's factory profile while
/// genuine user-defined names remain untouched.
pub fn is_legacy_placeholder(index: usize, bytes: &[u8]) -> bool {
    if index >= LAYER_NAME_COUNT {
        return false;
    }
    if bytes.is_empty() || core::str::from_utf8(bytes).is_err() {
        return true;
    }

    bytes == NUMERIC[index].as_bytes()
        || bytes.eq_ignore_ascii_case(GENERATED[index].as_bytes())
        || (index == 0 && bytes.eq_ignore_ascii_case(b"Main"))
}

/// Recognize factory text only as part of a complete generated profile.
/// Callers must check every layer before replacing any readable name.
pub fn is_factory_name(index: usize, bytes: &[u8], default_name: &str) -> bool {
    bytes.eq_ignore_ascii_case(default_name.as_bytes())
        || is_legacy_placeholder(index, bytes)
        || (index == 1 && default_name == "Num + Nav"
            && (bytes.eq_ignore_ascii_case(b"Navigation") || bytes.eq_ignore_ascii_case(b"Nav")))
        || (index == 4 && default_name == "Mouse" && bytes.eq_ignore_ascii_case(b"Pointing"))
        || (index == 3 && default_name.is_empty() && bytes.eq_ignore_ascii_case(b"Adjust"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factory_names_fit_firmware_limit() {
        for profile in [STANDARD_NO_MOUSE, STANDARD_WITH_MOUSE, PHENOM] {
            assert!(profile.iter().all(|name| name.len() <= 12));
        }
        assert_eq!(&STANDARD_NO_MOUSE[..4], &["Base", "Num + Nav", "Symbols", "Adjust"]);
        assert!(STANDARD_NO_MOUSE[4..].iter().all(|name| name.is_empty()));
        assert_eq!(STANDARD_WITH_MOUSE[4], "Mouse");
        assert!(STANDARD_WITH_MOUSE[5..].iter().all(|name| name.is_empty()));
        assert!(PHENOM[3].is_empty());
        assert_eq!(PHENOM[4], "Mouse");
    }

    #[test]
    fn recognizes_only_generated_placeholders() {
        assert!(is_legacy_placeholder(0, b""));
        assert!(is_legacy_placeholder(0, b"Main"));
        assert!(is_legacy_placeholder(4, b"4"));
        assert!(is_legacy_placeholder(15, b"layer 15"));
        assert!(is_legacy_placeholder(2, &[0xFF]));

        assert!(!is_legacy_placeholder(0, b"Base"));
        assert!(!is_legacy_placeholder(1, b"Navigation"));
        assert!(!is_legacy_placeholder(4, b"Mouse"));
        assert!(!is_legacy_placeholder(16, b"16"));
    }
}

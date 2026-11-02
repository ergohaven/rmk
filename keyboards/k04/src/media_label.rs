//! The media line in the Qube's header: what it says and how it scrolls.
//!
//! Counts are in characters, not bytes: a Cyrillic title takes two bytes a
//! character, and the header has room for a fixed number of characters.

/// Characters the header shows at once; a longer label scrolls.
pub(crate) const MEDIA_VISIBLE_CHARS: usize = 26;
/// Blank characters between the end of a scrolling label and its restart.
const MEDIA_GAP_CHARS: usize = 3;

/// "Artist - Title", or whichever of the two is there, without control
/// characters.
pub(crate) fn media_label<'a>(artist: &'a str, title: &'a str) -> impl Iterator<Item = char> + 'a {
    let shown = |text: &'a str| text.chars().filter(|ch| !ch.is_control());
    let separator = if shown(artist).next().is_some() && shown(title).next().is_some() {
        " - "
    } else {
        ""
    };
    shown(artist).chain(separator.chars()).chain(shown(title))
}

pub(crate) fn media_scrolls(artist: &str, title: &str) -> bool {
    media_label(artist, title).count() > MEDIA_VISIBLE_CHARS
}

/// The [`MEDIA_VISIBLE_CHARS`] characters of `label` shown `step` steps into
/// its scroll, wrapping around through a short gap.
pub(crate) fn marquee(label: &str, step: usize) -> impl Iterator<Item = char> + '_ {
    let len = label.chars().count();
    let cycle = len + MEDIA_GAP_CHARS;
    (0..MEDIA_VISIBLE_CHARS).map(move |i| {
        let idx = (step + i) % cycle;
        if idx < len { label.chars().nth(idx).unwrap_or(' ') } else { ' ' }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(artist: &str, title: &str) -> String {
        media_label(artist, title).collect()
    }

    // Hosts used to cut a title at 30 bytes, 15 Cyrillic characters, so a
    // Cyrillic title never got long enough to scroll.
    #[test]
    fn a_cyrillic_title_longer_than_the_header_scrolls() {
        let title = "Ж".repeat(MEDIA_VISIBLE_CHARS + 4);
        assert_eq!(title.len(), 60);
        assert!(media_scrolls("", &title));
        assert!(!media_scrolls("", &"Ж".repeat(MEDIA_VISIBLE_CHARS)));
    }

    #[test]
    fn the_separator_needs_both_parts() {
        assert_eq!(label("Artist", "Title"), "Artist - Title");
        assert_eq!(label("", "Title"), "Title");
        assert_eq!(label("Artist", ""), "Artist");
        assert_eq!(label("\n", "Title"), "Title");
    }

    #[test]
    fn control_characters_are_dropped() {
        assert_eq!(label("A\tB", "C\u{7}D"), "AB - CD");
    }

    #[test]
    fn the_marquee_shows_a_window_of_characters_and_wraps_through_the_gap() {
        let title: String = ('А'..='Я').collect();
        let window = |step| marquee(&title, step).collect::<String>();
        assert_eq!(window(0), title.chars().take(MEDIA_VISIBLE_CHARS).collect::<String>());
        let len = title.chars().count();
        let tail = window(len - 1);
        assert_eq!(tail.chars().next(), Some('Я'));
        assert_eq!(&tail[tail.char_indices().nth(1).unwrap().0..][..MEDIA_GAP_CHARS], "   ");
        assert_eq!(window(len + MEDIA_GAP_CHARS), window(0));
    }
}

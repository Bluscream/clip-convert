//! Colours written in the config file.
//!
//! Actions and size targets may each set their own button and text colour, so a
//! menu can be made scannable at a glance — the destructive entry red, the one
//! reached for every day in the app's own accent. The value is a CSS-style hex
//! string, which is what anyone picking a colour anywhere else already has in
//! their clipboard.
//!
//! Parsing lives here, in the UI-free crate, so that a malformed colour is
//! reported when the config loads rather than silently ignored when a dialog
//! opens.

/// A colour as red, green, blue and alpha.
pub type Rgba = [u8; 4];

/// Parses `#rgb`, `#rgba`, `#rrggbb` or `#rrggbbaa`, with or without the `#`.
///
/// Returns `None` for anything else, including an empty string, so a caller can
/// treat "not set" and "not understood" the same way after validation has
/// already rejected the latter.
#[must_use]
pub fn parse(raw: &str) -> Option<Rgba> {
    let digits = raw.trim().trim_start_matches('#');
    if !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }

    let nibble = |index: usize| -> Option<u8> {
        digits
            .as_bytes()
            .get(index)
            .and_then(|b| char::from(*b).to_digit(16))
            .and_then(|v| u8::try_from(v).ok())
    };
    // The short forms repeat each digit, so `#f80` is `#ff8800` — the same rule
    // CSS uses, which is what makes a copied colour work unchanged.
    let short = |index: usize| nibble(index).map(|v| v << 4 | v);
    let long = |index: usize| Some(nibble(index)? << 4 | nibble(index + 1)?);

    match digits.len() {
        3 => Some([short(0)?, short(1)?, short(2)?, 255]),
        4 => Some([short(0)?, short(1)?, short(2)?, short(3)?]),
        6 => Some([long(0)?, long(2)?, long(4)?, 255]),
        8 => Some([long(0)?, long(2)?, long(4)?, long(6)?]),
        _ => None,
    }
}

/// Whether a configured colour is usable, treating "not set" as fine.
#[must_use]
pub fn is_valid(raw: Option<&String>) -> bool {
    raw.is_none_or(|value| parse(value).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_long_form_is_read_channel_by_channel() {
        assert_eq!(parse("#1a2b3c"), Some([0x1a, 0x2b, 0x3c, 255]));
        assert_eq!(parse("1a2b3c"), Some([0x1a, 0x2b, 0x3c, 255]));
        assert_eq!(parse("#1a2b3c80"), Some([0x1a, 0x2b, 0x3c, 0x80]));
    }

    #[test]
    fn the_short_form_repeats_each_digit_as_css_does() {
        assert_eq!(parse("#f80"), Some([0xff, 0x88, 0x00, 255]));
        assert_eq!(parse("#f80a"), Some([0xff, 0x88, 0x00, 0xaa]));
    }

    #[test]
    fn case_does_not_matter() {
        assert_eq!(parse("#ABCDEF"), parse("#abcdef"));
    }

    #[test]
    fn surrounding_space_is_ignored() {
        assert_eq!(parse("  #fff  "), Some([255, 255, 255, 255]));
    }

    #[test]
    fn anything_that_is_not_a_hex_colour_is_rejected() {
        for bad in [
            "",
            "#",
            "red",
            "#12345",
            "#1234567",
            "#gg0000",
            "rgb(1,2,3)",
        ] {
            assert_eq!(parse(bad), None, "{bad} should not parse");
        }
    }

    #[test]
    fn an_unset_colour_is_valid_but_a_broken_one_is_not() {
        assert!(is_valid(None));
        assert!(is_valid(Some(&"#fff".to_string())));
        assert!(!is_valid(Some(&"octarine".to_string())));
    }
}

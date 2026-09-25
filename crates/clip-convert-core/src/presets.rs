//! Named size targets for the `resize` built-in.
//!
//! The factory list is transcribed from the sticker limits table at
//! `/var/mnt/nas/appdata/nginx-php-api/www/stickers/limits.php`. It is data, not
//! code: the whole list lives in the config file and can be edited, extended or
//! replaced without rebuilding.

use serde::{Deserialize, Serialize};

/// How a target's dimensions constrain the output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Fit {
    /// Scale to fit inside the box, keeping aspect ratio. The result may be
    /// smaller than the box on one axis.
    #[default]
    Inside,
    /// Scale to fit, then pad with transparency to exactly the box size.
    /// Needed
    /// by platforms that reject anything but an exact canvas.
    Exact,
}

/// One selectable size target.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Preset {
    pub id: String,
    pub label: String,
    /// Widest the result may be. Zero leaves the width alone.
    #[serde(default)]
    pub width: u32,
    /// Tallest the result may be. Zero leaves the height alone.
    #[serde(default)]
    pub height: u32,
    /// Upper bound on the encoded file size, in bytes. Zero means unconstrained.
    #[serde(default)]
    pub max_bytes: u64,
    /// Output container. Absent keeps whatever the source already was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    #[serde(default)]
    pub fit: Fit,
    /// A small picture shown on this target's button.
    ///
    /// May be written as base64, a `data:` URI, an `http(s)` URL or a local
    /// path; anything not already base64 is converted once and written back
    /// here, so it is never fetched again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Colour of this entry's button, as a hex string such as `#3b5bdb`.
    /// Absent uses the theme's own button colour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub button_color: Option<String>,
    /// Colour of the text on this entry's button. Absent uses the theme's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_color: Option<String>,
}

impl Preset {
    /// Whether this target constrains the picture's dimensions at all.
    #[must_use]
    pub fn constrains_size(&self) -> bool {
        self.width > 0 && self.height > 0
    }

    /// The box to fit inside, given the source's own dimensions.
    ///
    /// An unset axis is not a limit, so the source's own size stands in for it.
    #[must_use]
    pub fn box_for(&self, source_width: u32, source_height: u32) -> (u32, u32) {
        (
            if self.width == 0 {
                source_width
            } else {
                self.width
            },
            if self.height == 0 {
                source_height
            } else {
                self.height
            },
        )
    }
}

/// The factory presets as plain data: id, label, width, height, max bytes,
/// format, and whether the canvas size is mandatory.
const FACTORY: [(&str, &str, u32, u32, u64, &str, Fit); 11] = [
    (
        "discord",
        "Discord sticker",
        320,
        320,
        512 * 1024,
        "png",
        Fit::Exact,
    ),
    (
        "telegram",
        "Telegram sticker",
        512,
        512,
        512 * 1024,
        "webp",
        Fit::Inside,
    ),
    (
        "telegram-icon",
        "Telegram pack icon",
        100,
        100,
        128 * 1024,
        "png",
        Fit::Exact,
    ),
    (
        "signal",
        "Signal sticker",
        512,
        512,
        300 * 1024,
        "png",
        Fit::Exact,
    ),
    (
        "whatsapp",
        "WhatsApp sticker",
        512,
        512,
        100 * 1024,
        "webp",
        Fit::Exact,
    ),
    (
        "whatsapp-tray",
        "WhatsApp tray icon",
        96,
        96,
        50 * 1024,
        "png",
        Fit::Exact,
    ),
    (
        "vrchat",
        "VRChat sticker",
        1024,
        1024,
        8 * 1024 * 1024,
        "png",
        Fit::Exact,
    ),
    (
        "slack",
        "Slack emoji",
        128,
        128,
        128 * 1024,
        "png",
        Fit::Inside,
    ),
    (
        "line",
        "LINE sticker",
        320,
        270,
        1024 * 1024,
        "png",
        Fit::Inside,
    ),
    (
        "matrix",
        "Matrix sticker",
        512,
        512,
        512 * 1024,
        "webp",
        Fit::Inside,
    ),
    // Not a sticker: Discord's plain attachment limit. Caps the file size and
    // changes nothing else — no scaling, no re-encoding to another format.
    (
        "discord-file",
        "Discord file",
        0,
        0,
        10 * 1024 * 1024,
        "",
        Fit::Inside,
    ),
];

/// The presets shipped in a freshly written config file.
#[must_use]
pub fn factory() -> Vec<Preset> {
    FACTORY
        .into_iter()
        .map(
            |(id, label, width, height, max_bytes, format, fit)| Preset {
                id: id.to_string(),
                label: label.to_string(),
                width,
                height,
                max_bytes,
                format: (!format.is_empty()).then(|| format.to_string()),
                fit,
                icon: None,
                button_color: None,
                text_color: None,
            },
        )
        .collect()
}

impl Preset {
    /// A one-line summary for the chooser, e.g. `512×512 · WEBP · max 512 KB`.
    ///
    /// Only mentions what the target actually constrains, so a size-only target
    /// does not claim dimensions it leaves alone.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();

        if self.constrains_size() {
            parts.push(format!("{}×{}", self.width, self.height));
        }
        if let Some(format) = self.format.as_ref() {
            parts.push(format.to_uppercase());
        }
        if self.max_bytes > 0 {
            parts.push(format!(
                "max {}",
                crate::content::human_bytes(usize::try_from(self.max_bytes).unwrap_or(usize::MAX))
            ));
        }

        if parts.is_empty() {
            "unchanged".to_string()
        } else {
            parts.join(" · ")
        }
    }
}

/// Scales `(width, height)` to fit inside `(max_width, max_height)`, preserving
/// aspect ratio and never enlarging.
///
/// Returns at least 1×1 so the result is always a valid image geometry.
#[must_use]
pub fn fit_inside(width: u32, height: u32, max_width: u32, max_height: u32) -> (u32, u32) {
    if width == 0 || height == 0 || max_width == 0 || max_height == 0 {
        return (1, 1);
    }
    if width <= max_width && height <= max_height {
        return (width, height);
    }

    let scale = f64::from(max_width) / f64::from(width);
    let scale = scale.min(f64::from(max_height) / f64::from(height));

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    // Scale is in (0, 1] and the inputs are u32, so the products stay in range.
    let scaled = |v: u32| ((f64::from(v) * scale).round() as u32).max(1);

    (scaled(width), scaled(height))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factory_presets_are_uniquely_identified() {
        let presets = factory();
        let mut ids: Vec<&str> = presets.iter().map(|p| p.id.as_str()).collect();
        ids.sort_unstable();
        let count = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), count, "factory presets contain a duplicate id");
    }

    #[test]
    fn factory_presets_are_all_usable() {
        for preset in factory() {
            assert!(!preset.label.is_empty(), "{}", preset.id);
            assert!(
                preset.format.as_ref().is_none_or(|f| !f.is_empty()),
                "{} has an empty format, which is neither a format nor absent",
                preset.id
            );
            // Every target must constrain something, or choosing it does
            // nothing at all.
            assert!(
                preset.constrains_size() || preset.max_bytes > 0,
                "{} constrains neither size nor dimensions",
                preset.id
            );
            // A single axis is not a box; it is almost certainly a typo.
            assert_eq!(
                preset.width == 0,
                preset.height == 0,
                "{} sets only one dimension",
                preset.id
            );
        }
    }

    #[test]
    fn a_size_only_target_constrains_nothing_else() {
        let preset = factory()
            .into_iter()
            .find(|p| p.id == "discord-file")
            .expect("the size-only target should exist");

        assert!(!preset.constrains_size());
        assert_eq!(preset.format, None, "it must keep the source format");
        assert_eq!(preset.max_bytes, 10 * 1024 * 1024);
        // An unconstrained axis takes the source's own size.
        assert_eq!(preset.box_for(1920, 1080), (1920, 1080));
    }

    #[test]
    fn a_constrained_axis_overrides_the_source() {
        let preset = factory()
            .into_iter()
            .find(|p| p.id == "discord")
            .expect("discord sticker");
        assert_eq!(preset.box_for(1920, 1080), (320, 320));
    }

    #[derive(serde::Deserialize)]
    struct Wrapper {
        presets: Vec<Preset>,
    }

    #[test]
    fn factory_presets_survive_a_config_round_trip() {
        let original = factory();
        let encoded = toml::to_string(&toml::value::Table::from_iter([(
            "presets".to_string(),
            toml::Value::try_from(&original).expect("serialisable"),
        )]))
        .expect("encodable");

        let decoded: Wrapper = toml::from_str(&encoded).expect("decodable");
        assert_eq!(decoded.presets, original);
    }

    #[test]
    fn an_image_already_within_the_box_is_left_alone() {
        assert_eq!(fit_inside(100, 80, 512, 512), (100, 80));
    }

    #[test]
    fn scaling_preserves_aspect_ratio() {
        assert_eq!(fit_inside(1000, 500, 512, 512), (512, 256));
        assert_eq!(fit_inside(500, 1000, 512, 512), (256, 512));
    }

    #[test]
    fn the_tighter_axis_governs() {
        assert_eq!(fit_inside(1000, 1000, 320, 270), (270, 270));
    }

    #[test]
    fn scaling_never_returns_a_zero_dimension() {
        let (w, h) = fit_inside(10_000, 1, 32, 32);
        assert!(w >= 1 && h >= 1, "got {w}x{h}");
    }

    #[test]
    fn degenerate_input_is_handled_without_dividing_by_zero() {
        assert_eq!(fit_inside(0, 10, 32, 32), (1, 1));
        assert_eq!(fit_inside(10, 10, 0, 32), (1, 1));
    }

    #[test]
    fn summary_reports_the_size_cap() {
        let preset = &factory()[0];
        assert_eq!(preset.summary(), "320×320 · PNG · max 512.0 KB");

        // A size-only target mentions only what it constrains.
        let size_only = factory()
            .into_iter()
            .find(|p| p.id == "discord-file")
            .expect("size-only target");
        assert_eq!(size_only.summary(), "max 10.0 MB");
    }
}

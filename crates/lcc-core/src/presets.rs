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
    pub width: u32,
    pub height: u32,
    /// Upper bound on the encoded file size, in bytes. Zero means unconstrained.
    #[serde(default)]
    pub max_bytes: u64,
    /// Output container, as an `ImageMagick` format name.
    pub format: String,
    #[serde(default)]
    pub fit: Fit,
}

/// The presets shipped in a freshly written config file.
#[must_use]
pub fn factory() -> Vec<Preset> {
    let p = |id: &str, label: &str, width, height, max_bytes, format: &str, fit| Preset {
        id: id.to_string(),
        label: label.to_string(),
        width,
        height,
        max_bytes,
        format: format.to_string(),
        fit,
    };

    vec![
        p(
            "discord",
            "Discord sticker",
            320,
            320,
            512 * 1024,
            "png",
            Fit::Exact,
        ),
        p(
            "telegram",
            "Telegram sticker",
            512,
            512,
            512 * 1024,
            "webp",
            Fit::Inside,
        ),
        p(
            "telegram-icon",
            "Telegram pack icon",
            100,
            100,
            128 * 1024,
            "png",
            Fit::Exact,
        ),
        p(
            "signal",
            "Signal sticker",
            512,
            512,
            300 * 1024,
            "png",
            Fit::Exact,
        ),
        p(
            "whatsapp",
            "WhatsApp sticker",
            512,
            512,
            100 * 1024,
            "webp",
            Fit::Exact,
        ),
        p(
            "whatsapp-tray",
            "WhatsApp tray icon",
            96,
            96,
            50 * 1024,
            "png",
            Fit::Exact,
        ),
        p(
            "vrchat",
            "VRChat sticker",
            1024,
            1024,
            8 * 1024 * 1024,
            "png",
            Fit::Exact,
        ),
        p(
            "slack",
            "Slack emoji",
            128,
            128,
            128 * 1024,
            "png",
            Fit::Inside,
        ),
        p(
            "line",
            "LINE sticker",
            320,
            270,
            1024 * 1024,
            "png",
            Fit::Inside,
        ),
        p(
            "matrix",
            "Matrix sticker",
            512,
            512,
            512 * 1024,
            "webp",
            Fit::Inside,
        ),
    ]
}

impl Preset {
    /// A one-line summary for the chooser, e.g. `512×512 · WEBP · max 512 KB`.
    #[must_use]
    pub fn summary(&self) -> String {
        let format = self.format.to_uppercase();
        if self.max_bytes == 0 {
            format!("{}×{} · {format}", self.width, self.height)
        } else {
            format!(
                "{}×{} · {format} · max {} KB",
                self.width,
                self.height,
                self.max_bytes / 1024
            )
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
            assert!(preset.width > 0 && preset.height > 0, "{}", preset.id);
            assert!(!preset.format.is_empty(), "{}", preset.id);
            assert!(!preset.label.is_empty(), "{}", preset.id);
        }
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
        assert_eq!(preset.summary(), "320×320 · PNG · max 512 KB");
    }
}

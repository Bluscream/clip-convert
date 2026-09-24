//! Re-encoding images to fit a size target, in-process.
//!
//! Decoding, scaling and encoding are all done in Rust, so this works
//! identically on Linux, Windows and macOS with nothing to install. See
//! [`crate::encode`] for the format-specific side.
//!
//! Hitting a file-size cap is a search, not a calculation: no encoder can be
//! told "produce at most 100 KB" for an arbitrary image. Quality is lowered
//! first, since that preserves dimensions. Shrinking is a last resort and is
//! only available to [`Fit::Inside`] targets — a [`Fit::Exact`] preset exists
//! precisely because the platform mandates that canvas size, so quietly
//! returning a smaller image would produce a file it rejects.

use crate::encode::{self, Format};
use crate::presets::{self, Fit, Preset};
use image::imageops::FilterType;
use image::{DynamicImage, GenericImageView, Rgba, RgbaImage};

/// Quality settings tried in order before dimensions are reduced.
const QUALITY_LADDER: [u8; 6] = [95, 85, 75, 60, 45, 25];

/// How many shrink passes to try before giving up.
///
/// Few are needed because the next size is estimated from the last measurement
/// rather than stepped down by a fixed factor.
const MAX_SHRINK_PASSES: u32 = 5;

/// Damping applied to the estimated scale, so a pass aims slightly under the cap
/// rather than landing on it and overshooting on the next measurement.
const SHRINK_DAMPING: f64 = 0.95;

/// Bounds on a single pass's scale change, keeping the search from stalling
/// (too close to 1.0) or collapsing the image in one step.
const MIN_SHRINK: f64 = 0.35;
const MAX_SHRINK: f64 = 0.9;

#[derive(Debug, thiserror::Error)]
pub enum ImageError {
    #[error("the clipboard image could not be decoded: {0}")]
    Decode(#[source] image::ImageError),
    #[error(transparent)]
    Encode(#[from] encode::EncodeError),
    #[error("`{0}` is not a format this can write (use png, jpeg, webp or gif)")]
    UnknownFormat(String),
    #[error(
        "could not get below {} at {width}×{height}; smallest reached was {}{}",
        crate::content::human_bytes(*limit),
        crate::content::human_bytes(*best),
        if *canvas_is_fixed {
            " (this preset requires an exact canvas, so the image cannot be scaled down further)"
        } else {
            ""
        }
    )]
    TooLarge {
        limit: usize,
        best: usize,
        width: u32,
        height: u32,
        /// True for an exact-canvas preset, where shrinking was not an option.
        canvas_is_fixed: bool,
    },
}

/// The result of a successful resize.
#[derive(Debug, Clone)]
pub struct Resized {
    pub bytes: Vec<u8>,
    pub mime: String,
    pub width: u32,
    pub height: u32,
}

/// Re-encodes `source` to satisfy `target`.
///
/// # Errors
///
/// Returns [`ImageError::Decode`] if the source is not a readable image,
/// [`ImageError::UnknownFormat`] if the preset names a format this cannot
/// write, and [`ImageError::TooLarge`] if no combination of quality and scale
/// met the file-size cap.
pub fn resize(source: &[u8], target: &Preset) -> Result<Resized, ImageError> {
    let format = Format::parse(&target.format)
        .ok_or_else(|| ImageError::UnknownFormat(target.format.clone()))?;
    let decoded = image::load_from_memory(source).map_err(ImageError::Decode)?;
    let (source_width, source_height) = decoded.dimensions();

    let exact_canvas = target.fit == Fit::Exact;
    let mut best_seen = usize::MAX;
    let mut box_width = target.width;
    let mut box_height = target.height;

    // An exact-canvas preset gets a single pass: quality is the only lever it is
    // allowed to pull.
    let passes = if exact_canvas { 0 } else { MAX_SHRINK_PASSES };

    for pass in 0..=passes {
        let (width, height) =
            presets::fit_inside(source_width, source_height, box_width, box_height);
        let scaled = decoded.resize_exact(width, height, FilterType::Lanczos3);
        let canvas = if exact_canvas {
            pad_to(&scaled, target.width, target.height, format)
        } else {
            scaled
        };
        let (out_width, out_height) = canvas.dimensions();

        let mut best_this_pass = usize::MAX;
        for quality in QUALITY_LADDER {
            let bytes = encode::encode(&canvas, format, quality)?;
            best_seen = best_seen.min(bytes.len());
            best_this_pass = best_this_pass.min(bytes.len());

            if target.max_bytes == 0 || bytes.len() as u64 <= target.max_bytes {
                log::debug!(
                    "resized to {out_width}×{out_height} q{quality} = {} bytes (pass {pass})",
                    bytes.len()
                );
                return Ok(Resized {
                    bytes,
                    mime: format.mime().to_string(),
                    width: out_width,
                    height: out_height,
                });
            }
        }

        // Encoded size scales roughly with pixel count, so the linear scale that
        // would hit the cap is about sqrt(cap / achieved). Estimating it converges
        // in two or three passes where a fixed step needs a dozen.
        let scale = estimate_scale(target.max_bytes, best_this_pass);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        // Both dimensions are u32 and the scale is below 1, so the product fits.
        {
            box_width = ((f64::from(box_width) * scale).round() as u32).max(1);
            box_height = ((f64::from(box_height) * scale).round() as u32).max(1);
        }
    }

    Err(ImageError::TooLarge {
        limit: usize::try_from(target.max_bytes).unwrap_or(usize::MAX),
        best: best_seen,
        width: target.width,
        height: target.height,
        canvas_is_fixed: exact_canvas,
    })
}

/// Centres `image` on a `width`×`height` canvas.
///
/// The background is transparent where the format allows it and white where it
/// does not, so a padded JPEG does not come out with black bars.
fn pad_to(image: &DynamicImage, width: u32, height: u32, format: Format) -> DynamicImage {
    let background = if format.supports_transparency() {
        Rgba([0, 0, 0, 0])
    } else {
        Rgba([255, 255, 255, 255])
    };

    let mut canvas = RgbaImage::from_pixel(width, height, background);
    let (source_width, source_height) = image.dimensions();
    let left = i64::from(width.saturating_sub(source_width)) / 2;
    let top = i64::from(height.saturating_sub(source_height)) / 2;

    image::imageops::overlay(&mut canvas, &image.to_rgba8(), left, top);
    DynamicImage::ImageRgba8(canvas)
}

/// Estimates the linear scale needed to bring `achieved` bytes under `cap`.
///
/// Clamped so one pass can neither stall nor collapse the image.
fn estimate_scale(cap: u64, achieved: usize) -> f64 {
    if achieved == 0 || cap == 0 {
        return MIN_SHRINK;
    }
    #[allow(clippy::cast_precision_loss)] // Only steers the search; exactness is not needed.
    let ratio = cap as f64 / achieved as f64;
    (ratio.sqrt() * SHRINK_DAMPING).clamp(MIN_SHRINK, MAX_SHRINK)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic noise: compresses poorly, so the size-cap tests measure the
    /// search rather than the fixture being trivially compressible.
    fn fixture(width: u32, height: u32) -> Vec<u8> {
        let mut img = RgbaImage::new(width, height);
        for (x, y, pixel) in img.enumerate_pixels_mut() {
            let v = u8::try_from((x * 31 + y * 17) % 256).unwrap_or(0);
            *pixel = Rgba([v, v.wrapping_mul(7), v.wrapping_add(33), 255]);
        }
        encode::encode(&DynamicImage::ImageRgba8(img), Format::Png, 95).expect("fixture encodes")
    }

    fn preset(width: u32, height: u32, max_bytes: u64, format: &str, fit: Fit) -> Preset {
        Preset {
            id: "test".to_string(),
            label: "Test".to_string(),
            width,
            height,
            max_bytes,
            format: format.to_string(),
            fit,
        }
    }

    #[test]
    fn unreadable_input_is_rejected_before_any_work() {
        let err = resize(b"not an image", &preset(64, 64, 0, "png", Fit::Inside))
            .expect_err("should not decode");
        assert!(matches!(err, ImageError::Decode(_)), "{err:?}");
    }

    #[test]
    fn an_unwritable_format_is_named() {
        let err = resize(&fixture(32, 32), &preset(64, 64, 0, "avif", Fit::Inside))
            .expect_err("avif is not supported");
        assert!(err.to_string().contains("avif"), "{err}");
    }

    #[test]
    fn scaling_down_preserves_aspect_ratio() {
        let out = resize(
            &fixture(1000, 500),
            &preset(512, 512, 0, "png", Fit::Inside),
        )
        .expect("resizes");
        assert_eq!((out.width, out.height), (512, 256));
        assert_eq!(out.mime, "image/png");
    }

    #[test]
    fn exact_fit_pads_to_the_full_canvas() {
        let out =
            resize(&fixture(1000, 500), &preset(320, 320, 0, "png", Fit::Exact)).expect("resizes");
        assert_eq!((out.width, out.height), (320, 320));

        // The padding must be transparent, not black.
        let decoded = image::load_from_memory(&out.bytes)
            .expect("decodes")
            .to_rgba8();
        assert_eq!(
            decoded.get_pixel(160, 4).0[3],
            0,
            "padding should be transparent"
        );
    }

    #[test]
    fn a_file_size_cap_is_respected_by_shrinking() {
        let cap = 20 * 1024;
        let out = resize(
            &fixture(2000, 2000),
            &preset(512, 512, cap, "png", Fit::Inside),
        )
        .expect("resizes");
        assert!(
            out.bytes.len() as u64 <= cap,
            "output is {} bytes, cap was {cap}",
            out.bytes.len()
        );
        assert!(out.width <= 512 && out.height <= 512);
    }

    #[test]
    fn a_file_size_cap_is_respected_by_quality_for_a_lossy_format() {
        let cap = 30 * 1024;
        let out = resize(
            &fixture(2000, 2000),
            &preset(512, 512, cap, "webp", Fit::Exact),
        )
        .expect("resizes");
        assert!(out.bytes.len() as u64 <= cap, "{} bytes", out.bytes.len());
        // Quality alone had to do it: the canvas is mandatory.
        assert_eq!((out.width, out.height), (512, 512));
    }

    #[test]
    fn an_exact_canvas_is_never_traded_away_to_meet_a_cap() {
        let err = resize(
            &fixture(2000, 2000),
            &preset(512, 512, 200, "png", Fit::Exact),
        )
        .expect_err("cap cannot be met at a fixed canvas");
        match err {
            ImageError::TooLarge {
                canvas_is_fixed,
                width,
                height,
                ..
            } => {
                assert!(canvas_is_fixed);
                assert_eq!((width, height), (512, 512));
            }
            other => panic!("expected TooLarge, got {other:?}"),
        }
        let message = resize(
            &fixture(2000, 2000),
            &preset(512, 512, 200, "png", Fit::Exact),
        )
        .expect_err("same")
        .to_string();
        assert!(message.contains("exact canvas"), "{message}");
    }

    #[test]
    fn an_impossible_cap_reports_the_closest_it_got() {
        let err = resize(
            &fixture(1000, 1000),
            &preset(512, 512, 1, "png", Fit::Inside),
        )
        .expect_err("cannot be met");
        match err {
            ImageError::TooLarge { best, limit, .. } => {
                assert!(best > 0, "should report the smallest size actually reached");
                assert_eq!(limit, 1);
            }
            other => panic!("expected TooLarge, got {other:?}"),
        }
    }

    #[test]
    fn every_factory_preset_can_be_produced_from_a_realistic_image() {
        // The presets are the app's headline feature; a preset that cannot be
        // met for an ordinary image is a broken preset.
        let source = fixture(1200, 900);
        for target in presets::factory() {
            let out = resize(&source, &target)
                .unwrap_or_else(|e| panic!("preset {} failed: {e}", target.id));
            assert!(
                out.width <= target.width && out.height <= target.height,
                "{}",
                target.id
            );
            if target.max_bytes > 0 {
                assert!(out.bytes.len() as u64 <= target.max_bytes, "{}", target.id);
            }
            if target.fit == Fit::Exact {
                assert_eq!(
                    (out.width, out.height),
                    (target.width, target.height),
                    "{}",
                    target.id
                );
            }
        }
    }

    #[test]
    fn the_scale_estimate_targets_the_cap_in_one_step() {
        let scale = estimate_scale(25_000, 100_000);
        assert!((0.45..=0.5).contains(&scale), "got {scale}");
    }

    #[test]
    fn the_scale_estimate_stays_within_its_bounds() {
        assert!(estimate_scale(99_000, 100_000) <= MAX_SHRINK);
        assert!(estimate_scale(10, 100_000_000) >= MIN_SHRINK);
        assert!(estimate_scale(0, 100).is_finite());
        assert!(estimate_scale(100, 0) > 0.0);
    }
}

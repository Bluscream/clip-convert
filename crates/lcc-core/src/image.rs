//! Re-encoding images to fit a size target.
//!
//! The work is done by `ImageMagick` through the configurable command templates in
//! [`Commands`], so the pipeline can be retargeted (to `ffmpeg`, `cwebp`, or a
//! script) without touching this code.
//!
//! Hitting a file-size cap is a search, not a calculation: no encoder can be
//! told "produce at most 100 KB" for an arbitrary image. Quality is lowered
//! first, since that preserves dimensions. Shrinking is a last resort and is
//! only available to [`Fit::Inside`] targets — an [`Fit::Exact`] preset exists
//! precisely because the platform mandates that canvas size, so quietly
//! returning a smaller image would produce a file it rejects. An exact target
//! that cannot meet its cap is reported instead.

use crate::config::Commands;
use crate::presets::{self, Fit, Preset};
use std::path::Path;
use std::time::Duration;

/// Per-conversion timeout. Generous: a large animated source is slow.
const CONVERT_TIMEOUT: Duration = Duration::from_secs(30);

/// Quality settings tried in order before dimensions are reduced.
const QUALITY_LADDER: [u32; 6] = [92, 85, 75, 65, 55, 45];

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
    #[error("could not read the image's dimensions; it may not be a supported format")]
    Unreadable,
    #[error("could not create a temporary file: {0}")]
    TempFile(#[source] std::io::Error),
    #[error("image conversion failed: {0}")]
    Convert(#[from] crate::exec::ExecError),
    #[error("could not read the converted image: {0}")]
    ReadBack(#[source] std::io::Error),
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
/// Returns [`ImageError::Unreadable`] if the source header cannot be parsed,
/// [`ImageError::Convert`] if the converter failed or is not installed, and
/// [`ImageError::TooLarge`] if no combination of quality and scale met the
/// file-size cap.
pub fn resize(source: &[u8], target: &Preset, commands: &Commands) -> Result<Resized, ImageError> {
    let dimensions = imagesize::blob_size(source).map_err(|_| ImageError::Unreadable)?;
    let (source_width, source_height) = (
        u32::try_from(dimensions.width).unwrap_or(u32::MAX),
        u32::try_from(dimensions.height).unwrap_or(u32::MAX),
    );

    let dir = tempfile::tempdir().map_err(ImageError::TempFile)?;
    let input = dir.path().join("input");
    std::fs::write(&input, source).map_err(ImageError::TempFile)?;
    let output = dir.path().join(format!("output.{}", target.format));

    let mut best_seen = usize::MAX;
    let exact_canvas = target.fit == Fit::Exact;
    let mut box_width = target.width;
    let mut box_height = target.height;

    // An exact-canvas preset gets a single pass: quality is the only lever it is
    // allowed to pull.
    let passes = if exact_canvas { 0 } else { MAX_SHRINK_PASSES };

    for pass in 0..=passes {
        let (width, height) = if exact_canvas {
            (target.width, target.height)
        } else {
            presets::fit_inside(source_width, source_height, box_width, box_height)
        };

        let mut best_this_pass = usize::MAX;
        for quality in QUALITY_LADDER {
            let bytes = convert(&input, &output, width, height, quality, target, commands)?;
            let size = bytes.len();
            best_seen = best_seen.min(size);
            best_this_pass = best_this_pass.min(size);

            if target.max_bytes == 0 || size as u64 <= target.max_bytes {
                log::debug!("resized to {width}×{height} q{quality} = {size} bytes (pass {pass})");
                return Ok(Resized {
                    bytes,
                    mime: format!("image/{}", target.format),
                    width,
                    height,
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

/// Runs one conversion and returns the encoded bytes.
fn convert(
    input: &Path,
    output: &Path,
    width: u32,
    height: u32,
    quality: u32,
    target: &Preset,
    commands: &Commands,
) -> Result<Vec<u8>, ImageError> {
    let template = if target.fit == Fit::Exact {
        &commands.convert_exact
    } else {
        &commands.convert_inside
    };

    let argv = crate::exec::substitute(
        template,
        &[
            ("input", input.display().to_string()),
            ("output", output.display().to_string()),
            ("geometry", format!("{width}x{height}")),
            ("width", target.width.to_string()),
            ("height", target.height.to_string()),
            ("quality", quality.to_string()),
        ],
    );

    crate::exec::run(&argv, None, CONVERT_TIMEOUT)?;
    std::fs::read(output).map_err(ImageError::ReadBack)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Resize is implemented by driving `ImageMagick`, so these are integration
    /// tests against the real converter. If `magick` is missing the feature is
    /// broken, and the tests should say so rather than quietly passing.
    fn magick_available() -> bool {
        crate::exec::run(
            &["magick".to_string(), "-version".to_string()],
            None,
            Duration::from_secs(10),
        )
        .is_ok()
    }

    fn fixture(spec: &str) -> Vec<u8> {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("fixture.png");
        let argv: Vec<String> = [
            "magick", "-size", spec, "xc:", "+noise", "Random", "-depth", "8",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .chain(std::iter::once(path.display().to_string()))
        .collect();
        crate::exec::run(&argv, None, Duration::from_secs(20)).expect("magick creates a fixture");
        std::fs::read(&path).expect("read fixture")
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
    fn the_scale_estimate_targets_the_cap_in_one_step() {
        // Four times too big means half the linear size, modulo damping.
        let scale = estimate_scale(25_000, 100_000);
        assert!((0.45..=0.5).contains(&scale), "got {scale}");
    }

    #[test]
    fn the_scale_estimate_stays_within_its_bounds() {
        // Barely over the cap: must still make progress rather than stalling.
        assert!(estimate_scale(99_000, 100_000) <= MAX_SHRINK);
        // Wildly over the cap: must not collapse the image in one step.
        assert!(estimate_scale(10, 100_000_000) >= MIN_SHRINK);
        // Degenerate inputs must not produce NaN or a zero scale.
        assert!(estimate_scale(0, 100).is_finite());
        assert!(estimate_scale(100, 0) > 0.0);
    }

    #[test]
    fn unreadable_input_is_rejected_before_any_conversion() {
        let err = resize(
            b"not an image",
            &preset(64, 64, 0, "png", Fit::Inside),
            &Commands::default(),
        )
        .expect_err("should not be readable");
        assert!(matches!(err, ImageError::Unreadable), "{err:?}");
    }

    #[test]
    fn scaling_down_preserves_aspect_ratio() {
        assert!(magick_available(), "magick is required for resize");
        let source = fixture("1000x500");
        let out = resize(
            &source,
            &preset(512, 512, 0, "png", Fit::Inside),
            &Commands::default(),
        )
        .expect("resize works");

        let dim = imagesize::blob_size(&out.bytes).expect("output is an image");
        assert_eq!((dim.width, dim.height), (512, 256));
        assert_eq!(out.mime, "image/png");
    }

    #[test]
    fn exact_fit_pads_to_the_full_canvas() {
        assert!(magick_available(), "magick is required for resize");
        let source = fixture("1000x500");
        let out = resize(
            &source,
            &preset(320, 320, 0, "png", Fit::Exact),
            &Commands::default(),
        )
        .expect("resize works");

        let dim = imagesize::blob_size(&out.bytes).expect("output is an image");
        assert_eq!(
            (dim.width, dim.height),
            (320, 320),
            "exact fit must produce the full canvas, padded"
        );
    }

    #[test]
    fn a_file_size_cap_is_respected() {
        assert!(magick_available(), "magick is required for resize");
        // Random noise is close to incompressible, so meeting the cap really
        // requires the search to work rather than the fixture being trivial.
        let source = fixture("2000x2000");
        let cap = 20 * 1024;
        let out = resize(
            &source,
            &preset(512, 512, cap, "png", Fit::Inside),
            &Commands::default(),
        )
        .expect("resize works");

        assert!(
            out.bytes.len() as u64 <= cap,
            "output is {} bytes, cap was {cap}",
            out.bytes.len()
        );
        assert!(out.width <= 512 && out.height <= 512);
    }

    #[test]
    fn an_exact_canvas_is_never_traded_away_to_meet_a_cap() {
        assert!(magick_available(), "magick is required for resize");
        let source = fixture("2000x2000");
        // Far too small for 512x512 noise. The platform mandates the canvas, so
        // the right answer is to fail rather than return a smaller sticker.
        let err = resize(
            &source,
            &preset(512, 512, 8 * 1024, "png", Fit::Exact),
            &Commands::default(),
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
                assert!(err.to_string().contains("exact canvas"), "{err}");
            }
            other => panic!("expected TooLarge, got {other:?}"),
        }
    }

    #[test]
    fn an_impossible_cap_reports_the_closest_it_got() {
        assert!(magick_available(), "magick is required for resize");
        let source = fixture("2000x2000");
        // One byte is not reachable by any encoding.
        let err = resize(
            &source,
            &preset(512, 512, 1, "png", Fit::Inside),
            &Commands::default(),
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
    fn webp_output_is_produced_for_a_webp_target() {
        assert!(magick_available(), "magick is required for resize");
        let source = fixture("800x800");
        let out = resize(
            &source,
            &preset(512, 512, 0, "webp", Fit::Inside),
            &Commands::default(),
        )
        .expect("resize works");

        assert_eq!(out.mime, "image/webp");
        assert!(
            out.bytes.starts_with(b"RIFF"),
            "output should really be a WebP container"
        );
    }

    #[test]
    fn a_missing_converter_is_reported_rather_than_silently_skipped() {
        let source = fixture("64x64");
        let broken = Commands {
            convert_inside: vec!["lcc-no-such-converter".to_string(), "{input}".to_string()],
            ..Commands::default()
        };
        let err = resize(&source, &preset(32, 32, 0, "png", Fit::Inside), &broken)
            .expect_err("no converter");
        assert!(matches!(err, ImageError::Convert(_)), "{err:?}");
    }
}

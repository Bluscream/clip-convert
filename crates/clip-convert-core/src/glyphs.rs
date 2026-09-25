//! Icons for the built-in actions, drawn rather than shipped.
//!
//! A factory config that referenced icon files would break the moment those
//! files moved, and bundling binary blobs in the source would make them
//! unreviewable. So each built-in's mark is drawn here from rectangles, lines
//! and triangles, encoded as a PNG and written into the config as base64 —
//! where it is ordinary data the user can replace with anything they like.
//!
//! White on transparent, because the mark has to read on whatever background
//! the desktop theme gives a button.

use crate::action::Builtin;
use image::{Rgba, RgbaImage};

/// Edge length of a generated icon.
///
/// Larger than any button draws it, so it still looks right on a scaled
/// display, and small enough that the base64 stays a few hundred bytes.
const SIZE: u32 = 64;

/// The colour everything is drawn in.
const INK: Rgba<u8> = Rgba([255, 255, 255, 255]);

/// Thickness of a drawn stroke, in pixels.
const STROKE: f32 = 5.0;

/// The icon for a built-in action, as base64 PNG.
///
/// # Panics
///
/// Does not panic: the canvas is a fixed size and PNG encoding of an in-memory
/// image cannot fail for it.
#[must_use]
pub fn for_builtin(builtin: Builtin) -> String {
    let mut canvas = RgbaImage::new(SIZE, SIZE);
    match builtin {
        Builtin::Type => draw_type(&mut canvas),
        Builtin::Shorten => draw_shorten(&mut canvas),
        Builtin::Split => draw_split(&mut canvas),
        Builtin::Truncate => draw_truncate(&mut canvas),
        Builtin::Replace => draw_replace(&mut canvas),
        Builtin::Resize => draw_resize(&mut canvas),
        Builtin::Convert => draw_convert(&mut canvas),
    }
    encode(&canvas)
}

/// A text cursor: the I-beam every text field shows.
fn draw_type(canvas: &mut RgbaImage) {
    rect(canvas, 22.0, 10.0, 42.0, 15.0);
    rect(canvas, 22.0, 49.0, 42.0, 54.0);
    rect(canvas, 29.5, 10.0, 34.5, 54.0);
}

/// A long bar becoming a short one.
fn draw_shorten(canvas: &mut RgbaImage) {
    rect(canvas, 6.0, 17.0, 58.0, 24.0);
    rect(canvas, 6.0, 40.0, 30.0, 47.0);
    arrow_left(canvas, 34.0, 43.5, 10.0);
}

/// One block cut into three pieces.
fn draw_split(canvas: &mut RgbaImage) {
    rect(canvas, 8.0, 10.0, 56.0, 20.0);
    rect(canvas, 8.0, 27.0, 56.0, 37.0);
    rect(canvas, 8.0, 44.0, 56.0, 54.0);
}

/// A line of text stopping early, with the ellipsis that marks the cut.
fn draw_truncate(canvas: &mut RgbaImage) {
    rect(canvas, 8.0, 15.0, 56.0, 23.0);
    rect(canvas, 8.0, 38.0, 30.0, 46.0);
    for step in 0..3u8 {
        let x = 36.0 + f32::from(step) * 10.0;
        rect(canvas, x, 38.0, x + 6.0, 46.0);
    }
}

/// Two arrows swapping places: this for that.
fn draw_replace(canvas: &mut RgbaImage) {
    rect(canvas, 8.0, 17.0, 46.0, 23.0);
    arrow_right(canvas, 46.0, 20.0, 11.0);
    rect(canvas, 18.0, 41.0, 56.0, 47.0);
    arrow_left(canvas, 18.0, 44.0, 11.0);
}

/// A frame with a smaller frame inside it.
fn draw_resize(canvas: &mut RgbaImage) {
    frame(canvas, 6.0, 6.0, 58.0, 58.0);
    frame(canvas, 6.0, 6.0, 36.0, 36.0);
}

/// One shape becoming another.
fn draw_convert(canvas: &mut RgbaImage) {
    rect(canvas, 6.0, 22.0, 24.0, 42.0);
    rect(canvas, 26.0, 29.0, 44.0, 35.0);
    arrow_right(canvas, 44.0, 32.0, 11.0);
}

/// Fills an axis-aligned rectangle.
fn rect(canvas: &mut RgbaImage, x0: f32, y0: f32, x1: f32, y1: f32) {
    for y in clamp(y0)..clamp(y1) {
        for x in clamp(x0)..clamp(x1) {
            canvas.put_pixel(x, y, INK);
        }
    }
}

/// Draws the outline of a rectangle, [`STROKE`] thick.
fn frame(canvas: &mut RgbaImage, x0: f32, y0: f32, x1: f32, y1: f32) {
    rect(canvas, x0, y0, x1, y0 + STROKE);
    rect(canvas, x0, y1 - STROKE, x1, y1);
    rect(canvas, x0, y0, x0 + STROKE, y1);
    rect(canvas, x1 - STROKE, y0, x1, y1);
}

/// A solid triangle pointing right, its flat side at `x`.
fn arrow_right(canvas: &mut RgbaImage, x: f32, centre_y: f32, height: f32) {
    triangle(canvas, x, centre_y, height, 1.0);
}

/// A solid triangle pointing left, its flat side at `x`.
fn arrow_left(canvas: &mut RgbaImage, x: f32, centre_y: f32, height: f32) {
    triangle(canvas, x, centre_y, height, -1.0);
}

/// The shared body of both arrowheads: a triangle narrowing along `direction`.
fn triangle(canvas: &mut RgbaImage, x: f32, centre_y: f32, height: f32, direction: f32) {
    let rows = clamp(height * 2.0).max(1);
    for step in 0..rows {
        #[allow(clippy::cast_precision_loss)] // At most 64 rows.
        let offset = step as f32 / 2.0;
        let half = height - offset;
        if half <= 0.0 {
            break;
        }
        let edge = x + direction * offset;
        let (x0, x1) = if direction > 0.0 {
            (edge, edge + 1.0)
        } else {
            (edge - 1.0, edge)
        };
        rect(canvas, x0, centre_y - half, x1, centre_y + half);
    }
}

/// Rounds a coordinate onto the canvas.
fn clamp(value: f32) -> u32 {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    // Clamped to the canvas, so the cast is always in range, and SIZE is 64.
    {
        value.round().clamp(0.0, SIZE as f32) as u32
    }
}

/// Encodes the canvas as base64 PNG.
fn encode(canvas: &RgbaImage) -> String {
    use base64::Engine as _;

    let png = crate::encode::encode(
        &image::DynamicImage::ImageRgba8(canvas.clone()),
        crate::encode::Format::Png,
        crate::encode::TRUECOLOR_ABOVE,
    )
    .unwrap_or_default();
    base64::engine::general_purpose::STANDARD.encode(png)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered(builtin: Builtin) -> image::RgbaImage {
        let encoded = for_builtin(builtin);
        let bytes = crate::icons::decode(&encoded).expect("valid base64");
        image::load_from_memory(&bytes)
            .expect("a readable image")
            .to_rgba8()
    }

    #[test]
    fn every_builtin_has_a_drawable_icon() {
        for builtin in [
            Builtin::Type,
            Builtin::Shorten,
            Builtin::Split,
            Builtin::Truncate,
            Builtin::Replace,
            Builtin::Resize,
            Builtin::Convert,
        ] {
            let image = rendered(builtin);
            assert_eq!((image.width(), image.height()), (SIZE, SIZE), "{builtin:?}");

            let drawn = image.pixels().filter(|p| p.0[3] > 0).count();
            let total = (SIZE * SIZE) as usize;
            // Something, but not a solid block: either extreme means the glyph
            // is not a glyph.
            assert!(
                drawn > total / 40 && drawn < total * 3 / 4,
                "{builtin:?} covers {drawn} of {total} pixels"
            );
        }
    }

    #[test]
    fn icons_are_white_on_transparent() {
        let image = rendered(Builtin::Split);
        assert_eq!(image.get_pixel(0, 0).0[3], 0, "the corner must be clear");

        for pixel in image.pixels().filter(|p| p.0[3] > 0) {
            assert_eq!(
                [pixel.0[0], pixel.0[1], pixel.0[2]],
                [255, 255, 255],
                "every drawn pixel should be white"
            );
        }
    }

    #[test]
    fn each_builtin_gets_its_own_mark() {
        let all = [
            Builtin::Type,
            Builtin::Shorten,
            Builtin::Split,
            Builtin::Truncate,
            Builtin::Replace,
            Builtin::Resize,
            Builtin::Convert,
        ];
        let mut seen: Vec<String> = all.into_iter().map(for_builtin).collect();
        seen.sort_unstable();
        let count = seen.len();
        seen.dedup();
        assert_eq!(seen.len(), count, "two built-ins share an icon");
    }

    #[test]
    fn an_icon_is_small_enough_to_live_in_a_config_file() {
        for builtin in [Builtin::Type, Builtin::Resize, Builtin::Convert] {
            let encoded = for_builtin(builtin);
            assert!(
                encoded.len() < 4096,
                "{builtin:?} is {} bytes",
                encoded.len()
            );
        }
    }
}

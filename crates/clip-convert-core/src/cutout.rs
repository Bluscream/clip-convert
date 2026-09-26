//! Removing a flat background, and cropping away unused space.
//!
//! Both are best-effort operations on pixels that were never meant to be
//! analysed, so both are deliberately conservative: a screenshot or a sticker
//! with one flat background colour is handled well, a photograph is left
//! almost untouched rather than being punched full of holes.

use image::{Rgba, RgbaImage};

/// Extra work to do on an image before it is resized.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct ImageOptions {
    /// Replace a detected flat background with transparency.
    #[serde(default)]
    pub remove_background: bool,
    /// Crop every side until the content touches the edge.
    #[serde(default)]
    pub fill_to_borders: bool,
}

impl ImageOptions {
    /// Whether anything at all would be done.
    #[must_use]
    pub const fn is_noop(self) -> bool {
        !self.remove_background && !self.fill_to_borders
    }
}

/// How far a colour may differ from the background and still count as it.
///
/// Squared distance in RGB, so 32 per channel. Generous enough for the
/// gradients and JPEG artefacts in a real screenshot, tight enough that a
/// subject in a similar shade survives.
const TOLERANCE: i32 = 32 * 32 * 3;

/// A pixel is content once it is this opaque. Anything less reads as
/// background for cropping purposes.
const OPAQUE_ENOUGH: u8 = 8;

fn distance(a: Rgba<u8>, b: Rgba<u8>) -> i32 {
    let channel = |index: usize| i32::from(a.0[index]) - i32::from(b.0[index]);
    let (r, g, b_) = (channel(0), channel(1), channel(2));
    r * r + g * g + b_ * b_
}

/// The most common colour around the edge of the image.
///
/// The border is where a background is, by definition — a subject that
/// reaches every edge has no background to remove. Counting all four edges
/// rather than sampling one corner avoids being fooled by a logo or a
/// rounded corner.
#[must_use]
pub fn background_color(image: &RgbaImage) -> Option<Rgba<u8>> {
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 {
        return None;
    }

    let mut counts: std::collections::HashMap<[u8; 4], usize> = std::collections::HashMap::new();
    let mut count = |pixel: &Rgba<u8>| {
        *counts.entry(pixel.0).or_default() += 1;
    };

    for x in 0..width {
        count(image.get_pixel(x, 0));
        count(image.get_pixel(x, height - 1));
    }
    for y in 0..height {
        count(image.get_pixel(0, y));
        count(image.get_pixel(width - 1, y));
    }

    counts
        .into_iter()
        .max_by_key(|&(_, seen)| seen)
        .map(|(colour, _)| Rgba(colour))
}

/// Makes the background transparent.
///
/// Fills inward from the edges rather than replacing every matching pixel in
/// the image: the white of a page background should go, the white of an eye
/// in the middle of the subject should not. A pixel is only cleared if it
/// matches the background *and* is reachable from an edge through other
/// background pixels.
#[must_use]
pub fn remove_background(image: &RgbaImage) -> RgbaImage {
    let Some(background) = background_color(image) else {
        return image.clone();
    };

    // An already-transparent border means the work is done; doing it again
    // would eat into a subject that happens to touch the edge.
    if background.0[3] < OPAQUE_ENOUGH {
        return image.clone();
    }

    let (width, height) = image.dimensions();
    let mut out = image.clone();
    let mut seen = vec![false; (width as usize) * (height as usize)];
    let mut queue: std::collections::VecDeque<(u32, u32)> = std::collections::VecDeque::new();

    let index = |x: u32, y: u32| (y as usize) * (width as usize) + (x as usize);

    let consider = |x: u32,
                    y: u32,
                    seen: &mut Vec<bool>,
                    queue: &mut std::collections::VecDeque<(u32, u32)>| {
        if seen[index(x, y)] {
            return;
        }
        if distance(*image.get_pixel(x, y), background) > TOLERANCE {
            return;
        }
        seen[index(x, y)] = true;
        queue.push_back((x, y));
    };

    for x in 0..width {
        consider(x, 0, &mut seen, &mut queue);
        consider(x, height - 1, &mut seen, &mut queue);
    }
    for y in 0..height {
        consider(0, y, &mut seen, &mut queue);
        consider(width - 1, y, &mut seen, &mut queue);
    }

    while let Some((x, y)) = queue.pop_front() {
        out.put_pixel(x, y, Rgba([0, 0, 0, 0]));

        if x > 0 {
            consider(x - 1, y, &mut seen, &mut queue);
        }
        if y > 0 {
            consider(x, y - 1, &mut seen, &mut queue);
        }
        if x + 1 < width {
            consider(x + 1, y, &mut seen, &mut queue);
        }
        if y + 1 < height {
            consider(x, y + 1, &mut seen, &mut queue);
        }
    }

    out
}

/// The smallest rectangle holding everything that is not background.
///
/// Transparency decides it when the image has any, since that is what the
/// background became; otherwise the border colour does, so a screenshot with
/// a wide flat margin still crops.
///
/// Returns `None` when the whole image is background — an empty crop is not a
/// useful answer, so the caller keeps what it had.
#[must_use]
pub fn content_bounds(image: &RgbaImage) -> Option<(u32, u32, u32, u32)> {
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 {
        return None;
    }

    let transparent_anywhere = image.pixels().any(|p| p.0[3] < OPAQUE_ENOUGH);
    let background = background_color(image);

    let is_content = |pixel: &Rgba<u8>| {
        if transparent_anywhere {
            return pixel.0[3] >= OPAQUE_ENOUGH;
        }
        background.is_some_and(|colour| distance(*pixel, colour) > TOLERANCE)
    };

    let (mut min_x, mut min_y) = (u32::MAX, u32::MAX);
    let (mut max_x, mut max_y) = (0_u32, 0_u32);
    let mut found = false;

    for (x, y, pixel) in image.enumerate_pixels() {
        if !is_content(pixel) {
            continue;
        }
        found = true;
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
    }

    found.then_some((min_x, min_y, max_x, max_y))
}

/// Crops until the content touches every edge.
#[must_use]
pub fn fill_to_borders(image: &RgbaImage) -> RgbaImage {
    let Some((min_x, min_y, max_x, max_y)) = content_bounds(image) else {
        return image.clone();
    };
    let width = max_x - min_x + 1;
    let height = max_y - min_y + 1;
    if width == image.width() && height == image.height() {
        return image.clone();
    }
    image::imageops::crop_imm(image, min_x, min_y, width, height).to_image()
}

/// Applies whichever options are set, in the only order that makes sense:
/// the background is removed first, so cropping can then use the
/// transparency it produced.
#[must_use]
pub fn apply(image: &RgbaImage, options: ImageOptions) -> RgbaImage {
    let mut out = if options.remove_background {
        remove_background(image)
    } else {
        image.clone()
    };
    if options.fill_to_borders {
        out = fill_to_borders(&out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An image with a `colour` border and a `subject` block inside it.
    fn framed(width: u32, height: u32, colour: Rgba<u8>, subject: Rgba<u8>) -> RgbaImage {
        let mut image = RgbaImage::from_pixel(width, height, colour);
        for y in 2..height - 2 {
            for x in 2..width - 2 {
                image.put_pixel(x, y, subject);
            }
        }
        image
    }

    const WHITE: Rgba<u8> = Rgba([255, 255, 255, 255]);
    const RED: Rgba<u8> = Rgba([220, 20, 20, 255]);
    const CLEAR: Rgba<u8> = Rgba([0, 0, 0, 0]);

    #[test]
    fn the_border_colour_is_the_background() {
        let image = framed(10, 10, WHITE, RED);
        assert_eq!(background_color(&image), Some(WHITE));
    }

    #[test]
    fn the_background_becomes_transparent_and_the_subject_does_not() {
        let image = framed(10, 10, WHITE, RED);
        let out = remove_background(&image);

        assert_eq!(*out.get_pixel(0, 0), CLEAR, "corner should be cleared");
        assert_eq!(*out.get_pixel(5, 5), RED, "subject should survive");
    }

    #[test]
    fn a_hole_inside_the_subject_survives() {
        // The reason this fills from the edges instead of replacing every
        // matching pixel: white eyes in a white-backed sticker must stay.
        let mut image = framed(12, 12, WHITE, RED);
        image.put_pixel(6, 6, WHITE);

        let out = remove_background(&image);
        assert_eq!(*out.get_pixel(0, 0), CLEAR);
        assert_eq!(
            *out.get_pixel(6, 6),
            WHITE,
            "an enclosed background-coloured pixel is not background"
        );
    }

    #[test]
    fn a_near_miss_still_counts_as_background() {
        // JPEG artefacts and gradients mean a "flat" background is not flat.
        let mut image = framed(10, 10, WHITE, RED);
        image.put_pixel(0, 0, Rgba([250, 252, 255, 255]));

        let out = remove_background(&image);
        assert_eq!(out.get_pixel(0, 0).0[3], 0);
    }

    #[test]
    fn an_image_that_is_already_transparent_at_the_edges_is_left_alone() {
        let image = framed(10, 10, CLEAR, RED);
        let out = remove_background(&image);
        assert_eq!(out, image);
    }

    #[test]
    fn cropping_puts_the_content_against_every_edge() {
        let image = framed(10, 10, WHITE, RED);
        let out = fill_to_borders(&image);

        assert_eq!(out.dimensions(), (6, 6));
        for pixel in out.pixels() {
            assert_eq!(*pixel, RED);
        }
    }

    #[test]
    fn cropping_uses_transparency_when_there_is_any() {
        let mut image = RgbaImage::from_pixel(10, 10, CLEAR);
        image.put_pixel(4, 5, RED);
        image.put_pixel(5, 5, RED);

        let out = fill_to_borders(&image);
        assert_eq!(out.dimensions(), (2, 1));
    }

    #[test]
    fn an_image_with_no_margin_is_returned_unchanged() {
        let image = RgbaImage::from_pixel(8, 8, RED);
        assert_eq!(fill_to_borders(&image).dimensions(), (8, 8));
    }

    #[test]
    fn an_entirely_blank_image_is_not_cropped_to_nothing() {
        let image = RgbaImage::from_pixel(8, 8, WHITE);
        assert_eq!(content_bounds(&image), None);
        assert_eq!(fill_to_borders(&image).dimensions(), (8, 8));
    }

    #[test]
    fn the_two_together_remove_the_background_then_crop_to_what_is_left() {
        let image = framed(12, 12, WHITE, RED);
        let out = apply(
            &image,
            ImageOptions {
                remove_background: true,
                fill_to_borders: true,
            },
        );

        assert_eq!(out.dimensions(), (8, 8));
        assert!(out.pixels().all(|p| *p == RED));
    }

    #[test]
    fn doing_nothing_changes_nothing() {
        let image = framed(10, 10, WHITE, RED);
        assert_eq!(apply(&image, ImageOptions::default()), image);
        assert!(ImageOptions::default().is_noop());
    }
}

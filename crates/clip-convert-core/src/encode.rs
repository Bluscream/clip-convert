//! Encoding an image to a chosen container, in-process.
//!
//! This replaces shelling out to `ImageMagick`, so the same code runs on Linux,
//! Windows and macOS with no external program to install.
//!
//! `quality` means different things per format, because the formats differ in
//! what they can trade away:
//!
//! * **JPEG and WebP** are lossy and take it directly, 1–100.
//! * **PNG** is lossless, so quality instead selects a palette: above
//!   [`TRUECOLOR_ABOVE`] the image is left at full colour, and below it the
//!   image is quantised to fewer and fewer colours. This is the only lever that
//!   shrinks a PNG without changing its dimensions.
//! * **GIF** is always a 256-colour palette and ignores quality.

use image::{DynamicImage, ImageEncoder, RgbaImage};
use std::io::Cursor;

/// At or above this quality a PNG keeps full colour rather than being quantised.
pub const TRUECOLOR_ABOVE: u8 = 90;

/// Output containers the built-in resize can produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Png,
    Jpeg,
    Webp,
    Gif,
}

impl Format {
    /// Parses a format name as written in a preset.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "png" => Some(Self::Png),
            "jpeg" | "jpg" => Some(Self::Jpeg),
            "webp" => Some(Self::Webp),
            "gif" => Some(Self::Gif),
            _ => None,
        }
    }

    /// The MIME type to put on the clipboard.
    #[must_use]
    pub fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Webp => "image/webp",
            Self::Gif => "image/gif",
        }
    }

    /// Whether lowering quality actually reduces the encoded size.
    ///
    /// Drives the search in [`crate::image`]: a lossless format needs the
    /// palette or the dimensions reduced instead.
    #[must_use]
    pub fn is_lossy(self) -> bool {
        matches!(self, Self::Jpeg | Self::Webp)
    }

    /// Whether the format keeps an alpha channel.
    ///
    /// JPEG does not, so transparency has to be flattened before encoding or
    /// the transparent area comes out black.
    #[must_use]
    pub fn supports_transparency(self) -> bool {
        !matches!(self, Self::Jpeg)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EncodeError {
    #[error("`{0}` is not a format this can write (use png, jpeg, webp or gif)")]
    UnknownFormat(String),
    #[error("encoding as {format} failed: {source}")]
    Failed {
        format: &'static str,
        #[source]
        source: image::ImageError,
    },
    #[error("encoding as WebP failed")]
    Webp,
    #[error("writing the PNG failed")]
    Png,
}

/// Encodes `image` in `format` at `quality`.
///
/// # Errors
///
/// Returns [`EncodeError::Failed`] if the encoder rejected the image.
pub fn encode(image: &DynamicImage, format: Format, quality: u8) -> Result<Vec<u8>, EncodeError> {
    match format {
        Format::Jpeg => encode_jpeg(image, quality),
        Format::Webp => encode_webp(image, quality),
        Format::Png => encode_png(image, quality),
        Format::Gif => encode_gif(image),
    }
}

fn encode_jpeg(image: &DynamicImage, quality: u8) -> Result<Vec<u8>, EncodeError> {
    // JPEG has no alpha; compositing onto white avoids the black boxes that a
    // naive channel drop produces.
    let flattened = flatten_onto_white(image);
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut Cursor::new(&mut out), quality.max(1))
        .encode_image(&DynamicImage::ImageRgb8(flattened))
        .map_err(|source| EncodeError::Failed {
            format: "JPEG",
            source,
        })?;
    Ok(out)
}

fn encode_webp(image: &DynamicImage, quality: u8) -> Result<Vec<u8>, EncodeError> {
    let rgba = image.to_rgba8();
    let encoder = webp::Encoder::from_rgba(rgba.as_raw(), rgba.width(), rgba.height());
    let output = encoder.encode(f32::from(quality.max(1)));
    if output.is_empty() {
        return Err(EncodeError::Webp);
    }
    Ok(output.to_vec())
}

fn encode_png(image: &DynamicImage, quality: u8) -> Result<Vec<u8>, EncodeError> {
    let mut out = Vec::new();

    if quality >= TRUECOLOR_ABOVE {
        let rgba = image.to_rgba8();
        image::codecs::png::PngEncoder::new_with_quality(
            &mut out,
            image::codecs::png::CompressionType::Best,
            image::codecs::png::FilterType::Adaptive,
        )
        .write_image(
            rgba.as_raw(),
            rgba.width(),
            rgba.height(),
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|source| EncodeError::Failed {
            format: "PNG",
            source,
        })?;
        return Ok(out);
    }

    encode_png_indexed(&image.to_rgba8(), colours_for(quality))
}

/// Writes a palette PNG: one byte per pixel plus a small colour table.
///
/// This is where the saving actually comes from. Quantising the colours but
/// still writing RGBA would make the file *larger*, because it throws away the
/// smooth gradients PNG's filters compress well while still spending four bytes
/// on every pixel.
fn encode_png_indexed(rgba: &RgbaImage, colours: u8) -> Result<Vec<u8>, EncodeError> {
    let quantiser = color_quant::NeuQuant::new(10, usize::from(colours).max(2), rgba.as_raw());
    let map = quantiser.color_map_rgba();
    let entries = map.len() / 4;

    let mut palette = Vec::with_capacity(entries * 3);
    let mut alphas = Vec::with_capacity(entries);
    for chunk in map.chunks_exact(4) {
        palette.extend_from_slice(&chunk[..3]);
        alphas.push(chunk[3]);
    }

    let indices: Vec<u8> = rgba
        .pixels()
        .map(|p| u8::try_from(quantiser.index_of(&p.0)).unwrap_or(0))
        .collect();

    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, rgba.width(), rgba.height());
        encoder.set_color(png::ColorType::Indexed);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Best);
        encoder.set_palette(palette);
        // A tRNS chunk is only worth writing when something is actually
        // transparent; otherwise it is wasted bytes in every file.
        if alphas.iter().any(|a| *a != 255) {
            encoder.set_trns(alphas);
        }

        let mut writer = encoder.write_header().map_err(|_| EncodeError::Png)?;
        writer
            .write_image_data(&indices)
            .map_err(|_| EncodeError::Png)?;
    }
    Ok(out)
}

fn encode_gif(image: &DynamicImage) -> Result<Vec<u8>, EncodeError> {
    let mut out = Vec::new();
    {
        let mut encoder = image::codecs::gif::GifEncoder::new(&mut out);
        encoder
            .encode_frame(image::Frame::new(image.to_rgba8()))
            .map_err(|source| EncodeError::Failed {
                format: "GIF",
                source,
            })?;
    }
    Ok(out)
}

/// Maps a quality below [`TRUECOLOR_ABOVE`] onto a palette size.
///
/// Fewer colours means a smaller PNG. The floor of 8 keeps the image
/// recognisable rather than reducing it to noise.
fn colours_for(quality: u8) -> u8 {
    match quality {
        80..=89 => 255,
        60..=79 => 128,
        40..=59 => 64,
        20..=39 => 32,
        _ => 8,
    }
}

/// Composites an image onto white, for formats without an alpha channel.
fn flatten_onto_white(image: &DynamicImage) -> image::RgbImage {
    let rgba = image.to_rgba8();
    let mut out = image::RgbImage::new(rgba.width(), rgba.height());

    for (source, target) in rgba.pixels().zip(out.pixels_mut()) {
        let alpha = f32::from(source.0[3]) / 255.0;
        let blend = |channel: u8| {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            // The blend of two values in 0..=255 stays in range.
            {
                (f32::from(channel).mul_add(alpha, 255.0 * (1.0 - alpha))).round() as u8
            }
        };
        target.0 = [blend(source.0[0]), blend(source.0[1]), blend(source.0[2])];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    fn sample(width: u32, height: u32) -> DynamicImage {
        // Deterministic noise: compresses poorly, so size tests are meaningful.
        let mut img = RgbaImage::new(width, height);
        for (x, y, pixel) in img.enumerate_pixels_mut() {
            let v = u8::try_from((x * 7 + y * 13) % 256).unwrap_or(0);
            *pixel = Rgba([v, v.wrapping_mul(3), v.wrapping_add(90), 255]);
        }
        DynamicImage::ImageRgba8(img)
    }

    fn transparent(width: u32, height: u32) -> DynamicImage {
        let mut img = RgbaImage::new(width, height);
        for (x, _y, pixel) in img.enumerate_pixels_mut() {
            *pixel = Rgba([255, 0, 0, if x < width / 2 { 0 } else { 255 }]);
        }
        DynamicImage::ImageRgba8(img)
    }

    #[test]
    fn format_names_round_trip() {
        assert_eq!(Format::parse("PNG"), Some(Format::Png));
        assert_eq!(Format::parse(" jpg "), Some(Format::Jpeg));
        assert_eq!(Format::parse("jpeg"), Some(Format::Jpeg));
        assert_eq!(Format::parse("webp"), Some(Format::Webp));
        assert_eq!(Format::parse("gif"), Some(Format::Gif));
        assert_eq!(Format::parse("avif"), None);
    }

    #[test]
    fn every_format_encodes_something_decodable() {
        let source = sample(64, 48);
        for format in [Format::Png, Format::Jpeg, Format::Webp, Format::Gif] {
            let bytes = encode(&source, format, 80).expect("encodes");
            assert!(!bytes.is_empty(), "{format:?} produced nothing");

            let decoded = image::load_from_memory(&bytes)
                .unwrap_or_else(|e| panic!("{format:?} output is not readable: {e}"));
            assert_eq!(decoded.width(), 64, "{format:?} changed the width");
            assert_eq!(decoded.height(), 48, "{format:?} changed the height");
        }
    }

    #[test]
    fn encoded_output_carries_the_right_magic_bytes() {
        let source = sample(32, 32);
        assert_eq!(
            &encode(&source, Format::Png, 95).expect("png")[..8],
            b"\x89PNG\r\n\x1a\n"
        );
        assert_eq!(
            &encode(&source, Format::Jpeg, 80).expect("jpeg")[..2],
            b"\xff\xd8"
        );
        assert_eq!(
            &encode(&source, Format::Webp, 80).expect("webp")[..4],
            b"RIFF"
        );
        assert_eq!(&encode(&source, Format::Gif, 80).expect("gif")[..3], b"GIF");
    }

    #[test]
    fn lower_quality_makes_a_lossy_format_smaller() {
        let source = sample(256, 256);
        for format in [Format::Jpeg, Format::Webp] {
            let high = encode(&source, format, 95).expect("high").len();
            let low = encode(&source, format, 30).expect("low").len();
            assert!(
                low < high,
                "{format:?}: {low} should be smaller than {high}"
            );
        }
    }

    #[test]
    fn quantising_makes_a_png_smaller_without_changing_its_size() {
        let source = sample(128, 128);
        let truecolor = encode(&source, Format::Png, 95).expect("truecolor");
        let reduced = encode(&source, Format::Png, 30).expect("quantised");

        assert!(
            reduced.len() < truecolor.len(),
            "quantised PNG ({}) should be smaller than truecolor ({})",
            reduced.len(),
            truecolor.len()
        );
        let decoded = image::load_from_memory(&reduced).expect("still a valid png");
        assert_eq!((decoded.width(), decoded.height()), (128, 128));

        // Byte 25 of a PNG is the IHDR colour type; 3 means indexed. Without
        // this the file could be smaller by luck rather than by palette.
        assert_eq!(reduced[25], 3, "expected an indexed PNG");
    }

    #[test]
    fn transparency_survives_formats_that_support_it() {
        let source = transparent(32, 32);
        for format in [Format::Png, Format::Webp] {
            let bytes = encode(&source, format, 95).expect("encodes");
            let decoded = image::load_from_memory(&bytes).expect("decodes").to_rgba8();
            assert_eq!(
                decoded.get_pixel(2, 16).0[3],
                0,
                "{format:?} lost the transparent area"
            );
        }
    }

    #[test]
    fn jpeg_flattens_transparency_onto_white_rather_than_black() {
        // Dropping the alpha channel naively turns transparent pixels black,
        // which looks like a bug to anyone pasting the result.
        let bytes = encode(&transparent(32, 32), Format::Jpeg, 95).expect("encodes");
        let decoded = image::load_from_memory(&bytes).expect("decodes").to_rgb8();
        let pixel = decoded.get_pixel(2, 16).0;
        assert!(
            pixel.iter().all(|c| *c > 200),
            "transparent area became {pixel:?}, expected near-white"
        );
    }

    #[test]
    fn transparency_support_is_reported_correctly() {
        assert!(!Format::Jpeg.supports_transparency());
        for format in [Format::Png, Format::Webp, Format::Gif] {
            assert!(format.supports_transparency());
        }
    }

    #[test]
    fn lossiness_is_reported_correctly() {
        assert!(Format::Jpeg.is_lossy() && Format::Webp.is_lossy());
        assert!(!Format::Png.is_lossy() && !Format::Gif.is_lossy());
    }

    #[test]
    fn palette_size_shrinks_as_quality_falls() {
        let mut previous = u16::MAX;
        for quality in [89u8, 70, 50, 30, 10] {
            let colours = u16::from(colours_for(quality));
            assert!(colours <= previous, "quality {quality} widened the palette");
            assert!(colours >= 8, "palette must stay usable");
            previous = colours;
        }
    }

    #[test]
    fn mime_types_match_the_formats() {
        assert_eq!(Format::Png.mime(), "image/png");
        assert_eq!(Format::Webp.mime(), "image/webp");
    }
}

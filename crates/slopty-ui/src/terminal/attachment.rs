//! A pasted picture made fit for the agent: the types the model reads, sized so the prompt
//! stays under the wire's caps and the model's own limits.
//!
//! A phone screenshot is a 2–6 MB PNG and a photo a 3–12 MB JPEG; the model wants at most
//! 5 MB and reads nothing past ~1568 px on the long side anyway. So a picture over
//! [`LONGEST_SIDE`] or over [`IMAGE_BYTES_MAX`] is decoded, shrunk to the long side and
//! re-encoded: PNG when it has transparency (a screenshot rarely does), JPEG otherwise. A
//! picture already inside both bounds is passed through untouched, bytes and all.

use std::io::Cursor;

use image::imageops::FilterType;
use image::{DynamicImage, ImageFormat};
use slopty_proto::agent::{IMAGE_BYTES_MAX, Image};

/// The long side a picture is shrunk to: what the model reads at full detail.
pub const LONGEST_SIDE: u32 = 1568;
/// JPEG quality for a shrunk photo: visually clean, a fraction of the PNG.
const JPEG_QUALITY: u8 = 85;
/// A second try for a shrunk picture that still does not fit: fewer bytes, still legible.
const JPEG_QUALITY_SMALL: u8 = 70;

/// Why a picture was not attached.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Refused {
    /// A type the model does not read (a TIFF, a BMP, an SVG…).
    Type(String),
    /// The bytes do not decode as their type says.
    Unreadable,
    /// Even shrunk and recompressed it is over the wire's cap.
    TooLarge,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Type(media) => write!(f, "{media} is not a picture the agent can read"),
            Self::Unreadable => f.write_str("the picture could not be read"),
            Self::TooLarge => f.write_str("the picture is too large even shrunk"),
        }
    }
}

/// The picture as the agent should get it: unchanged when it already fits, shrunk and
/// re-encoded otherwise.
///
/// # Errors
///
/// When the type is not one the model reads, the bytes do not decode, or nothing fits.
pub fn fit(image: Image) -> Result<Image, Refused> {
    let format = match image.media_type.as_str() {
        "image/png" => ImageFormat::Png,
        "image/jpeg" => ImageFormat::Jpeg,
        "image/gif" => ImageFormat::Gif,
        "image/webp" => ImageFormat::WebP,
        other => return Err(Refused::Type(other.to_owned())),
    };
    let decoded = image::load_from_memory_with_format(&image.data, format)
        .map_err(|_undecodable| Refused::Unreadable)?;
    let long = decoded.width().max(decoded.height());
    if long <= LONGEST_SIDE && image.data.len() <= IMAGE_BYTES_MAX {
        return Ok(image);
    }
    let shrunk = if long > LONGEST_SIDE {
        // Triangle is a few times faster than Lanczos and the model cannot tell.
        decoded.resize(LONGEST_SIDE, LONGEST_SIDE, FilterType::Triangle)
    } else {
        decoded
    };
    let keep_alpha = format == ImageFormat::Png && shrunk.color().has_alpha();
    if keep_alpha {
        let png = encode(&shrunk, ImageFormat::Png, None)?;
        if png.len() <= IMAGE_BYTES_MAX {
            return Ok(Image { media_type: "image/png".to_owned(), data: png });
        }
    }
    for quality in [JPEG_QUALITY, JPEG_QUALITY_SMALL] {
        let jpeg = encode(&shrunk, ImageFormat::Jpeg, Some(quality))?;
        if jpeg.len() <= IMAGE_BYTES_MAX {
            return Ok(Image { media_type: "image/jpeg".to_owned(), data: jpeg });
        }
    }
    Err(Refused::TooLarge)
}

/// `picture` encoded as `format` (JPEG at `quality`, which drops transparency).
fn encode(
    picture: &DynamicImage,
    format: ImageFormat,
    quality: Option<u8>,
) -> Result<Vec<u8>, Refused> {
    let mut out = Cursor::new(Vec::new());
    let written = match (format, quality) {
        (ImageFormat::Jpeg, Some(quality)) => {
            let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality);
            picture.to_rgb8().write_with_encoder(encoder)
        }
        _ => picture.write_to(&mut out, format),
    };
    written.map_err(|_unencodable| Refused::Unreadable)?;
    Ok(out.into_inner())
}

#[cfg(test)]
mod tests {
    use image::{ImageBuffer, Rgb, Rgba};

    use super::*;

    fn png(width: u32, height: u32, alpha: bool) -> Image {
        let mut out = Cursor::new(Vec::new());
        if alpha {
            let buffer = ImageBuffer::from_fn(width, height, |x, _y| {
                Rgba([u8::try_from(x % 256).unwrap_or(0), 40, 200, 128])
            });
            DynamicImage::ImageRgba8(buffer).write_to(&mut out, ImageFormat::Png).expect("png");
        } else {
            let buffer = ImageBuffer::from_fn(width, height, |x, y| {
                Rgb([u8::try_from(x % 256).unwrap_or(0), u8::try_from(y % 256).unwrap_or(0), 90])
            });
            DynamicImage::ImageRgb8(buffer).write_to(&mut out, ImageFormat::Png).expect("png");
        }
        Image { media_type: "image/png".to_owned(), data: out.into_inner() }
    }

    fn size_of(image: &Image) -> (u32, u32) {
        let decoded = image::load_from_memory(&image.data).expect("decodes");
        (decoded.width(), decoded.height())
    }

    #[test]
    fn a_small_picture_passes_through_untouched() {
        let small = png(64, 48, false);
        let same = fit(small.clone()).expect("fits");
        assert_eq!(same, small, "bytes and type unchanged");
    }

    #[test]
    fn a_big_screenshot_is_shrunk_to_the_long_side_as_a_jpeg_and_one_with_alpha_as_a_png() {
        let wide = png(3200, 1400, false);
        let fitted = fit(wide).expect("fits");
        assert_eq!(fitted.media_type, "image/jpeg", "opaque: a JPEG, a fraction of the PNG");
        assert_eq!(size_of(&fitted), (LONGEST_SIDE, 686), "aspect kept, long side clamped");
        assert!(fitted.data.len() <= IMAGE_BYTES_MAX);

        let tall = png(600, 2000, true);
        let fitted = fit(tall).expect("fits");
        assert_eq!(fitted.media_type, "image/png", "transparency kept as a PNG");
        assert_eq!(size_of(&fitted), (470, LONGEST_SIDE));
    }

    #[test]
    fn what_the_model_cannot_read_is_refused_by_name() {
        let tiff = Image { media_type: "image/tiff".to_owned(), data: vec![0x4d; 10] };
        assert_eq!(fit(tiff), Err(Refused::Type("image/tiff".to_owned())));
        let garbage = Image { media_type: "image/png".to_owned(), data: vec![0x89; 10] };
        assert_eq!(fit(garbage), Err(Refused::Unreadable));
        assert_eq!(
            Refused::Type("image/tiff".to_owned()).to_string(),
            "image/tiff is not a picture the agent can read"
        );
    }
}

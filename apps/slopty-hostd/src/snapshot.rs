//! Pictures of host windows for an agent's prompt: taken as the prompt is sent, made fit for
//! the model, encoded here so the client never carries them.

use slopty_capture::{Picture, PixelOrder, Target};
use slopty_proto::agent::{IMAGE_BYTES_MAX, Image};
use slopty_proto::screen::CaptureTarget;

/// The model reads pictures up to this many pixels on the longest side; larger ones cost
/// tokens for nothing (the composer's own pictures are cut the same way).
pub const LONGEST_SIDE: u32 = 1568;

/// JPEG quality when a PNG of the picture would not fit the wire's limit.
const JPEG_QUALITY: u8 = 85;

/// Why a picture could not be made.
#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    /// The window or display could not be captured.
    #[error(transparent)]
    Capture(#[from] slopty_capture::CaptureError),
    /// The content list could not be read.
    #[error(transparent)]
    Screen(#[from] slopty_host::ScreenError),
    /// The picture's bytes do not describe an image of that size.
    #[error("picture of {0}×{1} does not fit its bytes")]
    Shape(u32, u32),
    /// The encoder refused.
    #[error("encode: {0}")]
    Encode(String),
    /// The capture never answered.
    #[error("no picture came back")]
    Silent,
}

/// One picture of each target, in order, skipping (and logging) those that fail.
pub async fn pictures(targets: &[CaptureTarget]) -> Vec<Image> {
    let mut out = Vec::with_capacity(targets.len());
    for &target in targets {
        match picture(target).await {
            Ok(image) => out.push(image),
            Err(e) => tracing::warn!(?target, error = %e, "no picture for the prompt"),
        }
    }
    out
}

async fn picture(target: CaptureTarget) -> Result<Image, SnapshotError> {
    let content = slopty_host::screen::shareable().await?;
    let resolved = Target::resolve(&content, target)?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    resolved.snapshot(move |result| {
        let _gone = tx.send(result);
    });
    let picture = rx.await.map_err(|_gone| SnapshotError::Silent)??;
    tokio::task::spawn_blocking(move || encode(&picture))
        .await
        .map_err(|e| SnapshotError::Encode(e.to_string()))?
}

/// The picture as the model takes it: RGBA, at most [`LONGEST_SIDE`] on the longest side,
/// PNG, else JPEG when the PNG would not fit [`IMAGE_BYTES_MAX`].
pub fn encode(picture: &Picture) -> Result<Image, SnapshotError> {
    let (w, h) = (picture.width, picture.height);
    let row = usize::try_from(w).unwrap_or(usize::MAX).saturating_mul(4);
    let rows = usize::try_from(h).unwrap_or(usize::MAX);
    if row > picture.bytes_per_row
        || picture.data.len() < picture.bytes_per_row.saturating_mul(rows)
    {
        return Err(SnapshotError::Shape(w, h));
    }
    let mut rgba = Vec::with_capacity(row.saturating_mul(rows));
    for line in picture.data.chunks_exact(picture.bytes_per_row).take(rows) {
        let (pixels, _pad) = line.get(..row).unwrap_or_default().as_chunks::<4>();
        for &[first, second, third, fourth] in pixels {
            match picture.order {
                PixelOrder::Bgra => rgba.extend_from_slice(&[third, second, first, 255]),
                PixelOrder::Argb => rgba.extend_from_slice(&[second, third, fourth, 255]),
            }
        }
    }
    let image = image::RgbaImage::from_raw(w, h, rgba).ok_or(SnapshotError::Shape(w, h))?;
    let longest = w.max(h);
    let image = if longest > LONGEST_SIDE {
        // Integer arithmetic: `side * LONGEST_SIDE / longest`, rounded, never above `side`.
        let fit = |side: u32| {
            let scaled = u64::from(side)
                .saturating_mul(u64::from(LONGEST_SIDE))
                .saturating_add(u64::from(longest).wrapping_div(2))
                .checked_div(u64::from(longest))
                .unwrap_or_else(|| u64::from(side));
            u32::try_from(scaled).unwrap_or(LONGEST_SIDE).max(1)
        };
        image::imageops::resize(&image, fit(w), fit(h), image::imageops::FilterType::Triangle)
    } else {
        image
    };
    let dynamic = image::DynamicImage::ImageRgba8(image);
    let mut png = std::io::Cursor::new(Vec::new());
    dynamic
        .write_to(&mut png, image::ImageFormat::Png)
        .map_err(|e| SnapshotError::Encode(e.to_string()))?;
    if png.get_ref().len() <= IMAGE_BYTES_MAX {
        return Ok(Image { media_type: "image/png".to_owned(), data: png.into_inner() });
    }
    let mut jpeg = std::io::Cursor::new(Vec::new());
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, JPEG_QUALITY);
    encoder.encode_image(&dynamic.to_rgb8()).map_err(|e| SnapshotError::Encode(e.to_string()))?;
    Ok(Image { media_type: "image/jpeg".to_owned(), data: jpeg.into_inner() })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A wide BGRA picture with padded rows comes out as a PNG no wider than the model
    /// wants, with its colours in the right order.
    #[test]
    fn a_wide_bgra_picture_is_scaled_and_recoloured() {
        let (width, height, stride) = (3136_u32, 200_u32, 3136_usize * 4 + 64);
        let mut data = vec![0_u8; stride * height as usize];
        for y in 0..height as usize {
            for x in 0..width as usize {
                // Blue, green, red, alpha: a red picture with a green left half.
                let bgra = if x < width as usize / 2 { [0, 255, 0, 255] } else { [0, 0, 255, 255] };
                data[y * stride + x * 4..y * stride + x * 4 + 4].copy_from_slice(&bgra);
            }
        }
        let picture =
            Picture { width, height, bytes_per_row: stride, order: PixelOrder::Bgra, data };
        let image = encode(&picture).unwrap();
        assert_eq!(image.media_type, "image/png");
        let decoded = image::load_from_memory(&image.data).unwrap().to_rgba8();
        assert_eq!(decoded.dimensions(), (LONGEST_SIDE, 100));
        assert_eq!(decoded.get_pixel(10, 50).0, [0, 255, 0, 255]);
        assert_eq!(decoded.get_pixel(LONGEST_SIDE - 10, 50).0, [255, 0, 0, 255]);
        let short = Picture { data: vec![0; 8], ..picture };
        assert!(matches!(encode(&short), Err(SnapshotError::Shape(3136, 200))));
    }
}

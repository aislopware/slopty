//! One still picture of a window or a display as PNG
//! ([`Verb::CaptureStill`](slopty_proto::orchestration::Verb::CaptureStill)).
//!
//! ScreenCaptureKit takes the picture at the target's native pixel size
//! (`slopty_capture::Target::snapshot`). It is encoded here, halved until the PNG fits one
//! reply. A worker that may not record its screen refuses with [`ErrorCode::Unsupported`]
//! before any picture is asked for: the preflight in `crate::screen::shareable` asks nobody,
//! so no consent prompt reaches whoever sits at the machine.

use slopty_proto::orchestration::{ErrorCode, Outcome};
use slopty_proto::screen::CaptureTarget;

use super::{Failure, MAX_FILE_BYTES};
use crate::screen::ScreenError;

/// Most bytes of a PNG one reply carries, as for a file read.
const PNG_BYTES: u64 = MAX_FILE_BYTES;

/// A picture as ScreenCaptureKit hands it over: 32 bits per pixel, rows `stride` bytes apart,
/// each pixel blue, green, red, alpha (or alpha, red, green, blue when `argb`), alpha
/// premultiplied.
#[derive(Clone, Copy, Debug)]
pub struct Raw<'a> {
    /// Pixels across.
    pub width: u32,
    /// Pixels down.
    pub height: u32,
    /// Bytes from one row to the next.
    pub stride: usize,
    /// The pixel order is ARGB rather than BGRA.
    pub argb: bool,
    /// `height` rows.
    pub data: &'a [u8],
}

/// The picture of `target` as [`Outcome::Still`].
///
/// # Errors
///
/// [`ErrorCode::Unsupported`] on a worker that may not record its screen or has none,
/// [`ErrorCode::Invalid`] for a window or display it does not have, [`ErrorCode::Failed`]
/// otherwise.
pub async fn capture(target: CaptureTarget) -> Result<Outcome, Failure> {
    let taken = take(target).await.map_err(|e| failure(target, e))?;
    tokio::task::spawn_blocking(move || {
        let raw = Raw {
            width: taken.width,
            height: taken.height,
            stride: taken.stride,
            argb: taken.argb,
            data: &taken.data,
        };
        encode_within(raw, PNG_BYTES)
    })
    .await
    .map_err(|e| Failure::new(ErrorCode::Failed, e.to_string()))?
    .map(|(png, width, height)| Outcome::Still { png, width, height })
}

/// A picture taken, its bytes as [`Raw`] reads them.
struct Taken {
    width: u32,
    height: u32,
    stride: usize,
    argb: bool,
    data: Vec<u8>,
}

#[cfg(target_os = "macos")]
async fn take(target: CaptureTarget) -> Result<Taken, ScreenError> {
    let content = crate::screen::shareable().await?;
    let resolved = slopty_capture::Target::resolve(&content, target)?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    resolved.snapshot(move |result| {
        let _receiver_gone = tx.send(result);
    });
    let picture = rx.await.map_err(|_dropped| ScreenError::Closed)??;
    Ok(Taken {
        width: picture.width,
        height: picture.height,
        stride: picture.bytes_per_row,
        argb: picture.order == slopty_capture::PixelOrder::Argb,
        data: picture.data,
    })
}

/// Only ScreenCaptureKit takes pictures.
#[cfg(not(target_os = "macos"))]
#[expect(clippy::unused_async, reason = "the same signature as the macOS one")]
async fn take(_target: CaptureTarget) -> Result<Taken, ScreenError> {
    Err(ScreenError::NotPermitted)
}

/// Why a picture was not taken, as the verb answers it.
fn failure(target: CaptureTarget, e: ScreenError) -> Failure {
    use slopty_capture::CaptureError;
    /// ScreenCaptureKit's "the user declined": no Screen Recording grant.
    const DECLINED: i64 = -3801;
    match e {
        ScreenError::NotPermitted
        | ScreenError::Capture(
            CaptureError::Unsupported | CaptureError::Sck { code: DECLINED, .. },
        ) => Failure::new(
            ErrorCode::Unsupported,
            "this worker may not record its screen (Screen Recording is not granted, or \
                 it has no desktop); `slopty worker doctor` on it says how to grant it",
        ),
        ScreenError::Capture(CaptureError::NotFound(_)) => Failure::new(
            ErrorCode::Invalid,
            format!("this worker has no {target:?} to capture; list_windows names what it has"),
        ),
        other => Failure::new(ErrorCode::Failed, other.to_string()),
    }
}

/// `raw` as a PNG of at most `budget` bytes, halved in each direction until it fits; the PNG
/// and its size in pixels.
///
/// # Errors
///
/// [`ErrorCode::Failed`] for rows shorter than the picture says, or an encoder failure.
pub fn encode_within(raw: Raw<'_>, budget: u64) -> Result<(Vec<u8>, u32, u32), Failure> {
    let short =
        || Failure::new(ErrorCode::Failed, "the picture holds fewer bytes than its size says");
    let mut rgba = straight_rgba(raw).ok_or_else(short)?;
    let (mut width, mut height) = (raw.width, raw.height);
    loop {
        let png = png(&rgba, width, height)
            .map_err(|e| Failure::new(ErrorCode::Failed, format!("PNG: {e}")))?;
        let fits = u64::try_from(png.len()).is_ok_and(|len| len <= budget);
        if fits || width <= 1 || height <= 1 {
            return Ok((png, width, height));
        }
        (rgba, width, height) = halve(&rgba, width, height);
    }
}

/// Tightly packed RGBA with straight alpha; `None` when `raw.data` is short.
fn straight_rgba(raw: Raw<'_>) -> Option<Vec<u8>> {
    let width = usize::try_from(raw.width).ok()?;
    let height = usize::try_from(raw.height).ok()?;
    let row_bytes = width.checked_mul(4)?;
    if raw.stride < row_bytes {
        return None;
    }
    let mut out = Vec::with_capacity(row_bytes.checked_mul(height)?);
    for row in raw.data.chunks(raw.stride).take(height) {
        let pixels = row.get(..row_bytes)?;
        for &[p0, p1, p2, p3] in pixels.as_chunks::<4>().0 {
            let (r, g, b, a) = if raw.argb { (p1, p2, p3, p0) } else { (p2, p1, p0, p3) };
            out.extend([unpremultiply(r, a), unpremultiply(g, a), unpremultiply(b, a), a]);
        }
    }
    (out.len() == row_bytes.checked_mul(height)?).then_some(out)
}

/// A premultiplied component as straight: `c * 255 / a`, at most 255.
fn unpremultiply(c: u8, a: u8) -> u8 {
    if a == 0 || a == u8::MAX {
        return c;
    }
    let straight = u16::from(c).saturating_mul(255).checked_div(u16::from(a)).unwrap_or(0);
    u8::try_from(straight).unwrap_or(u8::MAX)
}

/// Half the picture in each direction (rounded up), each pixel the mean of the up to four it
/// covers.
fn halve(rgba: &[u8], width: u32, height: u32) -> (Vec<u8>, u32, u32) {
    let size = |n: u32| usize::try_from(n).unwrap_or(usize::MAX);
    let (w, h) = (size(width), size(height));
    let (half_w, half_h) = (w.div_ceil(2), h.div_ceil(2));
    let at = |x: usize, y: usize, c: usize| -> Option<u16> {
        let i = y.checked_mul(w)?.checked_add(x)?.checked_mul(4)?.checked_add(c)?;
        rgba.get(i).copied().map(u16::from)
    };
    let mut out = Vec::with_capacity(half_w.saturating_mul(half_h).saturating_mul(4));
    for y in (0..h).step_by(2) {
        for x in (0..w).step_by(2) {
            for c in 0..4 {
                let near = [(x, y), (x.saturating_add(1), y), (x, y.saturating_add(1))];
                let far = (x.saturating_add(1), y.saturating_add(1));
                let samples: Vec<u16> = near
                    .into_iter()
                    .chain(std::iter::once(far))
                    .filter(|&(sx, sy)| sx < w && sy < h)
                    .filter_map(|(sx, sy)| at(sx, sy, c))
                    .collect();
                let sum = samples.iter().fold(0_u16, |sum, s| sum.saturating_add(*s));
                let count = u16::try_from(samples.len()).unwrap_or(1).max(1);
                out.push(u8::try_from(sum.checked_div(count).unwrap_or(0)).unwrap_or(u8::MAX));
            }
        }
    }
    let half = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
    (out, half(half_w), half(half_h))
}

fn png(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, png::EncodingError> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(rgba)?;
    writer.finish()?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(png: &[u8]) -> (u32, u32, Vec<u8>) {
        let mut reader = png::Decoder::new(std::io::Cursor::new(png)).read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        buf.truncate(info.buffer_size());
        (info.width, info.height, buf)
    }

    /// BGRA rows with padding come out as RGBA, premultiplied colours straightened; the
    /// padding is dropped.
    #[test]
    fn a_picture_is_encoded_as_straight_rgba() {
        // Two pixels a row, a padded stride of 12: opaque red then half-transparent grey.
        let row = [0, 0, 255, 255, 64, 64, 64, 128, 9, 9, 9, 9];
        let data: Vec<u8> = row.iter().chain(row.iter()).copied().collect();
        let raw = Raw { width: 2, height: 2, stride: 12, argb: false, data: &data };
        let (png, w, h) = encode_within(raw, u64::MAX).unwrap();
        let (dw, dh, pixels) = decode(&png);
        assert_eq!((w, h, dw, dh), (2, 2, 2, 2));
        assert_eq!(&pixels[..8], [255, 0, 0, 255, 127, 127, 127, 128]);
        let argb = [255, 10, 20, 30];
        let raw = Raw { width: 1, height: 1, stride: 4, argb: true, data: &argb };
        let (png, ..) = encode_within(raw, u64::MAX).unwrap();
        assert_eq!(decode(&png).2, [10, 20, 30, 255]);
    }

    /// A picture whose PNG is over the budget is halved until it fits; one that holds fewer
    /// bytes than its size says is refused.
    #[test]
    fn a_large_picture_is_halved_until_it_fits() {
        let (w, h) = (64_u32, 48_u32);
        // Noise compresses badly, so the whole picture is well over a small budget.
        let data: Vec<u8> = (0..w.saturating_mul(h).saturating_mul(4))
            .map(|i| blake3::hash(&i.to_le_bytes()).as_bytes()[0])
            .collect();
        let raw = Raw { width: w, height: h, stride: 256, argb: false, data: &data };
        let (whole, ..) = encode_within(raw, u64::MAX).unwrap();
        let budget = u64::try_from(whole.len().checked_div(8).unwrap()).unwrap();
        let (png, fw, fh) = encode_within(raw, budget).unwrap();
        assert!(u64::try_from(png.len()).unwrap() <= budget, "{} > {budget}", png.len());
        assert_eq!((fw, fh), (16, 12), "halved twice");
        assert_eq!(decode(&png).0, 16);
        let short = Raw { width: w, height: h, stride: 256, argb: false, data: &data[..100] };
        assert_eq!(encode_within(short, u64::MAX).unwrap_err().code, ErrorCode::Failed);
    }

    /// No Screen Recording (or no desktop) is Unsupported, a window it lacks is Invalid.
    #[test]
    fn a_worker_that_may_not_capture_says_unsupported() {
        let target = CaptureTarget::Window(slopty_core::WindowId(7));
        assert_eq!(failure(target, ScreenError::NotPermitted).code, ErrorCode::Unsupported);
        let declined =
            slopty_capture::CaptureError::Sck { code: -3801, message: "declined".to_owned() };
        assert_eq!(failure(target, declined.into()).code, ErrorCode::Unsupported);
        let missing = slopty_capture::CaptureError::NotFound(target);
        assert_eq!(failure(target, missing.into()).code, ErrorCode::Invalid);
        assert_eq!(failure(target, ScreenError::Closed).code, ErrorCode::Failed);
    }
}

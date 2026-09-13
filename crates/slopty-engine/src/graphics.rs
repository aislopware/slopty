//! Kitty graphics on the host side: a program's images made into what a client paints.
//!
//! libghostty keeps the images and lays the placements out; this module turns an image into
//! wire RGBA (converted, shrunk under the frame cap) and keeps the ledger of what each
//! client already holds, mirrored by `slopty_client` with the same budget.

use std::collections::BTreeMap;

use libghostty_vt::alloc::{Allocator, Bytes};
use libghostty_vt::kitty::graphics::{DecodePng, DecodedImage, ImageFormat};
use slopty_proto::terminal::IMAGE_CACHE_BYTES;

/// Largest RGBA payload shipped in one `TermEvent::Image`.
///
/// Under the codec's frame cap with room for the envelope. A bigger image is sampled down by
/// a whole factor.
pub const IMAGE_WIRE_BYTES: usize = 12 * 1024 * 1024;

/// Bytes of decoded pixels libghostty keeps per screen before it refuses a transmission
/// (its own default is 320 MB; a coding session shows previews, not a film).
pub const KITTY_STORAGE_BYTES: u64 = 64 * 1024 * 1024;

/// An image's pixels for the wire.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ImageUpload {
    /// The kitty image id.
    pub id: u32,
    /// The pixels' generation.
    pub generation: u64,
    /// Width in pixels, after any shrink.
    pub width: u32,
    /// Height in pixels, after any shrink.
    pub height: u32,
    /// RGBA, row-major.
    pub rgba: Vec<u8>,
}

/// What the clients hold of one image id.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Shipped {
    /// The generation they hold.
    pub generation: u64,
    /// The whole factor the image was sampled down by (1 = as transmitted).
    pub shrink: u32,
    /// Bytes of RGBA they hold for it.
    pub bytes: usize,
    /// The frame that last placed it.
    pub last: u64,
}

/// The images the clients hold, bounded like their cache so both sides drop the same ones.
#[derive(Default, Debug)]
pub struct Ledger {
    shipped: BTreeMap<u32, Shipped>,
    bytes: usize,
}

impl Ledger {
    /// The shrink factor the clients hold `id` at, when they hold this generation.
    #[must_use]
    pub fn held(&self, id: u32, generation: u64) -> Option<u32> {
        self.shipped.get(&id).filter(|s| s.generation == generation).map(|s| s.shrink)
    }

    /// Note `id` was placed by frame `seq`.
    pub fn touch(&mut self, id: u32, seq: u64) {
        if let Some(s) = self.shipped.get_mut(&id) {
            s.last = seq;
        }
    }

    /// Record an upload the clients now hold.
    pub fn insert(&mut self, id: u32, shipped: Shipped) {
        if let Some(old) = self.shipped.insert(id, shipped) {
            self.bytes = self.bytes.saturating_sub(old.bytes);
        }
        self.bytes = self.bytes.saturating_add(shipped.bytes);
    }

    /// Forget everything: the next frame ships every placed image again (attach, resync).
    pub fn clear(&mut self) {
        self.shipped.clear();
        self.bytes = 0;
    }

    /// Drop the least recently placed images until the cache budget holds, as the clients do.
    pub fn prune(&mut self) {
        while self.bytes > IMAGE_CACHE_BYTES {
            let Some((&id, _)) = self.shipped.iter().min_by_key(|(_, s)| s.last) else { break };
            if let Some(gone) = self.shipped.remove(&id) {
                self.bytes = self.bytes.saturating_sub(gone.bytes);
            }
        }
    }

    /// Bytes the clients hold.
    #[must_use]
    pub const fn bytes(&self) -> usize {
        self.bytes
    }
}

/// The wire image for pixels libghostty holds in `format`, with the shrink factor applied.
///
/// `None` when the data does not fit the format (a truncated transmission).
#[must_use]
pub fn upload(
    id: u32,
    generation: u64,
    format: ImageFormat,
    width: u32,
    height: u32,
    data: &[u8],
) -> Option<(ImageUpload, u32)> {
    let rgba = to_rgba(format, width, height, data)?;
    let shrink = shrink_factor(rgba.len());
    let (width, height, rgba) = if shrink > 1 {
        let (w, h) = (width.div_ceil(shrink).max(1), height.div_ceil(shrink).max(1));
        (w, h, sample_down(&rgba, width, w, h, shrink))
    } else {
        (width, height, rgba)
    };
    Some((ImageUpload { id, generation, width, height, rgba }, shrink))
}

/// The pixels as RGBA, whatever libghostty stored (PNG is decoded to RGBA on transmission).
fn to_rgba(format: ImageFormat, width: u32, height: u32, data: &[u8]) -> Option<Vec<u8>> {
    let pixels = usize::try_from(width).ok()?.checked_mul(usize::try_from(height).ok()?)?;
    let channels = match format {
        ImageFormat::Rgba | ImageFormat::Png => 4,
        ImageFormat::Rgb => 3,
        ImageFormat::GrayAlpha => 2,
        ImageFormat::Gray => 1,
        _ => return None,
    };
    let data = data.get(..pixels.checked_mul(channels)?)?;
    Some(match channels {
        4 => data.to_vec(),
        3 => data.as_chunks::<3>().0.iter().flat_map(|&[r, g, b]| [r, g, b, 255]).collect(),
        2 => data.as_chunks::<2>().0.iter().flat_map(|&[g, a]| [g, g, g, a]).collect(),
        _ => data.iter().flat_map(|&g| [g, g, g, 255]).collect(),
    })
}

/// The most an image is sampled down by: 64× covers every size libghostty would store.
const MAX_SHRINK: u32 = 64;

/// The whole factor that brings `bytes` of RGBA under the wire cap.
#[must_use]
pub fn shrink_factor(bytes: usize) -> u32 {
    (1_u32..=MAX_SHRINK)
        .find(|&f| {
            usize::try_from(f)
                .ok()
                .and_then(|f| f.checked_pow(2))
                .and_then(|sq| bytes.checked_div(sq))
                .is_some_and(|per| per <= IMAGE_WIRE_BYTES)
        })
        .unwrap_or(MAX_SHRINK)
}

/// Every `f`th pixel of each `f`th row, `w` by `h` of them.
fn sample_down(rgba: &[u8], src_width: u32, w: u32, h: u32, f: u32) -> Vec<u8> {
    let stride = usize::try_from(src_width).unwrap_or(usize::MAX).saturating_mul(4).max(4);
    let (w, h, f) = (
        usize::try_from(w).unwrap_or(usize::MAX),
        usize::try_from(h).unwrap_or(usize::MAX),
        usize::try_from(f).unwrap_or(1).max(1),
    );
    rgba.chunks_exact(stride)
        .step_by(f)
        .take(h)
        .flat_map(|row| row.as_chunks::<4>().0.iter().step_by(f).take(w))
        .flatten()
        .copied()
        .collect()
}

/// The PNG decoder libghostty calls on a `f=100` transmission.
///
/// The `png` crate, every colour type expanded to 8-bit RGBA.
#[derive(Clone, Copy, Default, Debug)]
pub struct PngDecoder;

impl DecodePng for PngDecoder {
    fn decode_png<'alloc>(
        &mut self,
        alloc: &'alloc Allocator<'_>,
        data: &[u8],
    ) -> Option<DecodedImage<'alloc>> {
        let mut decoder = png::Decoder::new(std::io::Cursor::new(data));
        decoder.set_transformations(
            png::Transformations::EXPAND
                | png::Transformations::ALPHA
                | png::Transformations::STRIP_16,
        );
        let mut reader = decoder.read_info().ok()?;
        let mut buf = vec![0; reader.output_buffer_size()?];
        let info = reader.next_frame(&mut buf).ok()?;
        let format = match info.color_type {
            png::ColorType::Rgba => ImageFormat::Rgba,
            png::ColorType::Rgb => ImageFormat::Rgb,
            png::ColorType::GrayscaleAlpha => ImageFormat::GrayAlpha,
            png::ColorType::Grayscale => ImageFormat::Gray,
            png::ColorType::Indexed => return None,
        };
        let rgba = to_rgba(format, info.width, info.height, buf.get(..info.buffer_size())?)?;
        let mut bytes = Bytes::new_with_alloc(alloc, rgba.len()).ok()?;
        bytes.copy_from_slice(&rgba);
        Some(DecodedImage { width: info.width, height: info.height, data: bytes })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_stored_format_becomes_rgba() {
        let up = |f, d: &[u8]| upload(1, 1, f, 2, 1, d).map(|(u, s)| (u.rgba, s));
        assert_eq!(
            up(ImageFormat::Rgba, &[1, 2, 3, 4, 5, 6, 7, 8]),
            Some((vec![1, 2, 3, 4, 5, 6, 7, 8], 1))
        );
        assert_eq!(
            up(ImageFormat::Rgb, &[1, 2, 3, 4, 5, 6]),
            Some((vec![1, 2, 3, 255, 4, 5, 6, 255], 1))
        );
        assert_eq!(
            up(ImageFormat::GrayAlpha, &[9, 8, 7, 6]),
            Some((vec![9, 9, 9, 8, 7, 7, 7, 6], 1))
        );
        assert_eq!(up(ImageFormat::Gray, &[9, 7]), Some((vec![9, 9, 9, 255, 7, 7, 7, 255], 1)));
        // Short data is a transmission still in flight, not an image.
        assert_eq!(up(ImageFormat::Rgb, &[1, 2, 3]), None);
    }

    #[test]
    fn a_large_image_is_sampled_down_by_a_whole_factor() {
        assert_eq!(shrink_factor(IMAGE_WIRE_BYTES), 1);
        assert_eq!(shrink_factor(IMAGE_WIRE_BYTES + 1), 2);
        assert_eq!(shrink_factor(IMAGE_WIRE_BYTES * 9), 3);
        // 4 × 2 sampled by 2 keeps pixels (0,0) and (2,0).
        let rgba: Vec<u8> = (0..32).collect();
        assert_eq!(sample_down(&rgba, 4, 2, 1, 2), vec![0, 1, 2, 3, 8, 9, 10, 11]);
    }

    #[test]
    fn the_ledger_drops_the_least_recently_placed_over_budget() {
        let mut ledger = Ledger::default();
        let half = IMAGE_CACHE_BYTES / 2;
        ledger.insert(1, Shipped { generation: 1, shrink: 1, bytes: half, last: 1 });
        ledger.insert(2, Shipped { generation: 2, shrink: 1, bytes: half, last: 2 });
        ledger.touch(1, 3);
        ledger.insert(3, Shipped { generation: 3, shrink: 2, bytes: 1, last: 3 });
        ledger.prune();
        assert_eq!(ledger.held(2, 2), None, "placed longest ago");
        assert_eq!(ledger.held(1, 1), Some(1));
        assert_eq!(ledger.held(3, 3), Some(2));
        assert_eq!(ledger.held(3, 4), None, "a newer generation is not held");
        assert_eq!(ledger.bytes(), half + 1);
        ledger.clear();
        assert_eq!((ledger.bytes(), ledger.held(1, 1)), (0, None));
    }
}

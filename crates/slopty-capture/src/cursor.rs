//! The pointer's picture on the worker: which cursor the window server is showing, as
//! premultiplied BGRA pixels with the hotspot, for a client to draw at the pointer.
//!
//! `NSCursor.currentSystemCursor` is the one public reading of a cursor another process set.
//! Apple has deprecated it in favour of baking the cursor into ScreenCaptureKit's frames,
//! which would tie pointer latency to the video pipeline; the cursor keeps its own channel
//! (ARCHITECTURE §3), so the deprecated reading is used while it answers, and a client draws
//! its own arrow when it does not.

use objc2::rc::Retained;
use objc2_app_kit::{NSCursor, NSImageRep};
use objc2_core_graphics::{CGDataProvider, CGImage, CGImageAlphaInfo, CGImageByteOrderInfo};
use slopty_proto::screen::CursorShape;

/// The cursor on screen right now, or `None` when it is hidden, the system will not say, or
/// its picture is in a layout this reader does not take.
///
/// `scale` is the backing scale the picture is wanted at (the display's, 1–4): a cursor
/// image carries representations at 1×, 2×, 5× and 10× for the pointer-size setting, and
/// the smallest one at or above the wanted scale is read. The first call in a process is
/// slow (AppKit connects to the window server); [`crate::warm_cursor`] pays that early.
#[must_use]
pub fn cursor_shape(scale: u8) -> Option<CursorShape> {
    #[expect(
        deprecated,
        reason = "the only public reading of the cursor another process set; the replacement bakes the cursor into the video, which the cursor channel exists to avoid"
    )]
    let cursor = NSCursor::currentSystemCursor()?;
    let image = cursor.image();
    let hot = cursor.hotSpot();
    let points = image.size();
    let reps = image.representations();
    if points.width <= 0.0 {
        return None;
    }
    let wanted = (points.width * f64::from(scale.clamp(1, 4))).round();
    #[expect(clippy::cast_possible_truncation, reason = "a cursor is a few hundred pixels")]
    let wanted = wanted as isize;
    let at_least: Vec<Retained<NSImageRep>> =
        reps.iter().filter(|rep| rep.pixelsWide() >= wanted).collect();
    let rep = match at_least.into_iter().min_by_key(|rep| rep.pixelsWide()) {
        Some(rep) => rep,
        None => reps.iter().max_by_key(|rep| rep.pixelsWide())?,
    };
    let wide = rep.pixelsWide();
    if wide <= 0 {
        return None;
    }
    #[expect(clippy::cast_precision_loss, reason = "cursor pixel counts are tiny")]
    let ratio = wide as f64 / points.width;
    #[expect(clippy::cast_possible_truncation, reason = "clamped to a byte")]
    #[expect(clippy::cast_sign_loss, reason = "clamped to at least 1")]
    let scale = ratio.round().clamp(1.0, 4.0) as u8;
    // SAFETY: a null proposed rect asks for the whole image at the representation's own
    // pixel size; no context and no hints is the documented way to read its pixels.
    let cg = unsafe { rep.CGImageForProposedRect_context_hints(std::ptr::null_mut(), None, None) }?;
    let layout = Layout::of(&cg)?;
    let provider = CGImage::data_provider(Some(&cg))?;
    let data = CGDataProvider::data(Some(&provider))?.to_vec();
    let bgra = bgra_premultiplied(&layout, &data)?;
    let to_px = |v: f64| -> u16 {
        #[expect(clippy::cast_possible_truncation, reason = "clamped")]
        #[expect(clippy::cast_sign_loss, reason = "clamped to zero")]
        let p = (v * f64::from(scale)).round().clamp(0.0, f64::from(u16::MAX)) as u16;
        p
    };
    Some(CursorShape {
        w: layout.width,
        h: layout.height,
        hot_x: to_px(hot.x),
        hot_y: to_px(hot.y),
        bgra,
        scale,
    })
}

/// Read the cursor once and drop it: the first read in a process has been measured at 11 s
/// (AppKit's connection to the window server), the second at 200 µs. Returns how long it took.
#[must_use]
pub fn warm_cursor() -> std::time::Duration {
    let started = std::time::Instant::now();
    let _shape = cursor_shape(2);
    started.elapsed()
}

/// Where the alpha byte sits in a pixel's four bytes as CoreGraphics describes it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AlphaAt {
    /// The alpha component comes first in the pixel's logical order (`ARGB`).
    First,
    /// The alpha component comes last (`RGBA`).
    Last,
}

/// How a 32-bit picture's bytes are laid out, enough to read every pixel.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Layout {
    /// Pixels across.
    pub width: u16,
    /// Pixels down.
    pub height: u16,
    /// Bytes from one row to the next (at least `width * 4`).
    pub bytes_per_row: usize,
    /// The bytes of a pixel are the logical components reversed (`kCGImageByteOrder32Little`).
    pub little_endian: bool,
    /// Where alpha sits in the logical order.
    pub alpha: AlphaAt,
    /// Colour is already multiplied by alpha.
    pub premultiplied: bool,
    /// The alpha byte is padding: the pixel is opaque.
    pub opaque: bool,
}

impl Layout {
    /// The layout of a `CGImage`, or `None` for one that is not 8 bits × 4 components.
    fn of(image: &CGImage) -> Option<Self> {
        let some = Some(image);
        if CGImage::bits_per_pixel(some) != 32 || CGImage::bits_per_component(some) != 8 {
            return None;
        }
        let (alpha, premultiplied, opaque) = match CGImage::alpha_info(some) {
            CGImageAlphaInfo::PremultipliedLast => (AlphaAt::Last, true, false),
            CGImageAlphaInfo::PremultipliedFirst => (AlphaAt::First, true, false),
            CGImageAlphaInfo::Last => (AlphaAt::Last, false, false),
            CGImageAlphaInfo::First => (AlphaAt::First, false, false),
            CGImageAlphaInfo::NoneSkipLast => (AlphaAt::Last, true, true),
            CGImageAlphaInfo::NoneSkipFirst => (AlphaAt::First, true, true),
            _ => return None,
        };
        Some(Self {
            width: u16::try_from(CGImage::width(some)).ok()?,
            height: u16::try_from(CGImage::height(some)).ok()?,
            bytes_per_row: CGImage::bytes_per_row(some),
            little_endian: CGImage::byte_order_info(some) == CGImageByteOrderInfo::Order32Little,
            alpha,
            premultiplied,
            opaque,
        })
    }

    /// Byte offsets of blue, green, red and alpha within a pixel.
    const fn offsets(self) -> [usize; 4] {
        // Logical order is RGBA or ARGB; a little-endian picture stores it reversed.
        match (self.alpha, self.little_endian) {
            (AlphaAt::Last, false) => [2, 1, 0, 3],
            (AlphaAt::First, false) => [3, 2, 1, 0],
            (AlphaAt::Last, true) => [1, 2, 3, 0],
            (AlphaAt::First, true) => [0, 1, 2, 3],
        }
    }
}

/// `data` as tightly packed premultiplied BGRA rows, or `None` when it is shorter than the
/// layout says.
#[must_use]
pub fn bgra_premultiplied(layout: &Layout, data: &[u8]) -> Option<Vec<u8>> {
    let (width, height) = (usize::from(layout.width), usize::from(layout.height));
    if layout.bytes_per_row < width.checked_mul(4)? {
        return None;
    }
    let needed = layout.bytes_per_row.checked_mul(height)?;
    if data.len() < needed {
        return None;
    }
    let [ob, og, or, oa] = layout.offsets();
    let mut out = Vec::with_capacity(width.checked_mul(height)?.checked_mul(4)?);
    for row in 0..height {
        let start = row.checked_mul(layout.bytes_per_row)?;
        let cells = data.get(start..start.checked_add(width.checked_mul(4)?)?)?;
        for px in cells.as_chunks::<4>().0 {
            let at = |o: usize| px.get(o).copied().unwrap_or(0);
            let (b, g, r) = (at(ob), at(og), at(or));
            let a = if layout.opaque { 255 } else { at(oa) };
            let (b, g, r) = if layout.premultiplied || layout.opaque {
                (b, g, r)
            } else {
                (premultiply(b, a), premultiply(g, a), premultiply(r, a))
            };
            out.extend_from_slice(&[b, g, r, a]);
        }
    }
    Some(out)
}

/// `c × a / 255`, rounded.
const fn premultiply(c: u8, a: u8) -> u8 {
    let p = (c as u32).wrapping_mul(a as u32).wrapping_add(127).wrapping_div(255);
    #[expect(clippy::cast_possible_truncation, reason = "at most 255 by construction")]
    let p = p as u8;
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(little_endian: bool, alpha: AlphaAt, premultiplied: bool, opaque: bool) -> Layout {
        Layout {
            width: 2,
            height: 1,
            bytes_per_row: 8,
            little_endian,
            alpha,
            premultiplied,
            opaque,
        }
    }

    #[test]
    fn every_byte_order_comes_out_as_bgra() {
        // One pixel (r 10, g 20, b 30, a 255) and one (r 1, g 2, b 3, a 255), premultiplied.
        let want = vec![30, 20, 10, 255, 3, 2, 1, 255];
        let rgba = [10, 20, 30, 255, 1, 2, 3, 255];
        assert_eq!(
            bgra_premultiplied(&layout(false, AlphaAt::Last, true, false), &rgba).unwrap(),
            want
        );
        let argb = [255, 10, 20, 30, 255, 1, 2, 3];
        assert_eq!(
            bgra_premultiplied(&layout(false, AlphaAt::First, true, false), &argb).unwrap(),
            want
        );
        let bgra = [30, 20, 10, 255, 3, 2, 1, 255];
        assert_eq!(
            bgra_premultiplied(&layout(true, AlphaAt::First, true, false), &bgra).unwrap(),
            want,
            "the common macOS layout is a copy"
        );
        let abgr = [255, 30, 20, 10, 255, 3, 2, 1];
        assert_eq!(
            bgra_premultiplied(&layout(true, AlphaAt::Last, true, false), &abgr).unwrap(),
            want
        );
    }

    #[test]
    fn straight_alpha_is_multiplied_in_and_padding_alpha_reads_opaque() {
        let straight = [200, 100, 50, 128, 0, 0, 0, 0];
        let got =
            bgra_premultiplied(&layout(false, AlphaAt::Last, false, false), &straight).unwrap();
        assert_eq!(got, vec![25, 50, 100, 128, 0, 0, 0, 0], "each colour halves with the alpha");
        let padded = [200, 100, 50, 7, 1, 2, 3, 0];
        let got = bgra_premultiplied(&layout(false, AlphaAt::Last, true, true), &padded).unwrap();
        assert_eq!(got, vec![50, 100, 200, 255, 3, 2, 1, 255], "the pad byte is not an alpha");
    }

    #[test]
    fn padded_rows_are_cut_and_short_data_is_refused() {
        let mut l = layout(true, AlphaAt::First, true, false);
        l.bytes_per_row = 12;
        let data = [30, 20, 10, 255, 3, 2, 1, 255, 9, 9, 9, 9];
        assert_eq!(bgra_premultiplied(&l, &data).unwrap(), vec![30, 20, 10, 255, 3, 2, 1, 255]);
        assert_eq!(bgra_premultiplied(&l, &data[..11]), None, "a row short of its stride");
        l.bytes_per_row = 4;
        assert_eq!(bgra_premultiplied(&l, &data), None, "a stride narrower than the row");
    }

    #[test]
    fn premultiply_rounds_to_nearest() {
        assert_eq!(premultiply(255, 255), 255);
        assert_eq!(premultiply(255, 0), 0);
        assert_eq!(premultiply(1, 128), 1, "0.502 rounds up");
        assert_eq!(premultiply(1, 127), 0, "0.498 rounds down");
    }

    /// The window server has a cursor whenever a session is on screen; this reads it and
    /// checks the picture is a plausible pointer at the wanted scale. Needs a window session
    /// and takes seconds on the first read, so it runs only with `SLOPTY_SCREEN_E2E=1` like
    /// the stream tests.
    #[test]
    fn the_system_cursor_reads_as_a_small_picture_with_its_hotspot_inside() {
        if std::env::var_os("SLOPTY_SCREEN_E2E").is_none() {
            eprintln!("SLOPTY_SCREEN_E2E unset; skipping");
            return;
        }
        let warm = warm_cursor();
        eprintln!("first read took {warm:?}");
        let started = std::time::Instant::now();
        let shape = cursor_shape(2).expect("the system says which cursor it shows");
        eprintln!("the second took {:?}: {shape:?}", started.elapsed());
        assert!(started.elapsed() < std::time::Duration::from_millis(50), "warm reads are quick");
        assert_eq!(shape.scale, 2, "the 2× representation was picked: {shape:?}");
        assert!((1..=512).contains(&shape.w) && (1..=512).contains(&shape.h), "{shape:?}");
        assert_eq!(shape.bgra.len(), usize::from(shape.w) * usize::from(shape.h) * 4);
        assert!(shape.hot_x < shape.w && shape.hot_y < shape.h, "hotspot inside: {shape:?}");
        assert!((1..=4).contains(&shape.scale));
        assert!(shape.bgra.as_chunks::<4>().0.iter().any(|px| px[3] > 0), "something is drawn");
    }
}

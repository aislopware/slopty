//! A PDF's pages as pictures, drawn by `PDFKit` on macOS and iOS alike.
//!
//! A file an agent shows inline has its first page in its card and every page in the pane
//! (`slopty_ui`'s pages).
//!
//! `PDFKit` draws a page into a bitmap context of the size asked, on white as paper is, with the
//! page's rotation applied; the pixels are BGRA, premultiplied, as GPUI's images take them. A
//! document is opened and drawn on the thread that asks, which needs no main thread, so the app
//! draws off it. Nothing is drawn from a locked (encrypted) document.

use std::ffi::c_void;
use std::fmt::Write as _;

use objc2::msg_send;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject};
use objc2_core_foundation::{CGFloat, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGColorSpace, CGContext, CGImageAlphaInfo, CGImageByteOrderInfo,
};
use objc2_foundation::NSData;

// `PDFKit` is linked for its classes, which are looked up by name.
#[link(name = "PDFKit", kind = "framework")]
unsafe extern "C" {}

/// `kPDFDisplayBoxCropBox` (PDFPage.h): the part of a page a reader shows.
const CROP_BOX: isize = 1;

/// The widest picture of a page drawn, in pixels: a page fills a 5K pane at 2×.
pub const MAX_WIDTH: u32 = 4096;

/// Why a PDF could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PdfError {
    /// The bytes are not a PDF `PDFKit` reads.
    #[error("not a PDF")]
    NotPdf,
    /// It is encrypted, and Slopty has no password for it.
    #[error("the PDF is locked")]
    Locked,
    /// It has no pages.
    #[error("the PDF has no pages")]
    Empty,
}

/// One page drawn.
#[derive(Clone, PartialEq, Eq)]
pub struct PdfPicture {
    /// Width, in pixels.
    pub width: u32,
    /// Height, in pixels.
    pub height: u32,
    /// The pixels, BGRA, premultiplied, rows top first.
    pub bgra: Vec<u8>,
}

impl std::fmt::Debug for PdfPicture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PdfPicture")
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

/// An open PDF. Used on the thread that opened it.
pub struct Pdf {
    document: Retained<AnyObject>,
    count: usize,
}

impl std::fmt::Debug for Pdf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pdf").field("count", &self.count).finish_non_exhaustive()
    }
}

impl Pdf {
    /// The PDF in `bytes`.
    ///
    /// # Errors
    ///
    /// When it is not a PDF, is locked, or has no pages.
    pub fn open(bytes: &[u8]) -> Result<Self, PdfError> {
        let class = AnyClass::get(c"PDFDocument").ok_or(PdfError::NotPdf)?;
        let data = NSData::with_bytes(bytes);
        // SAFETY: PDFKit rule (PDFDocument.h): `+alloc` then `-initWithData:` with an `NSData`,
        // which answers nil for bytes that are not a PDF.
        let allocated: Allocated<AnyObject> = unsafe { msg_send![class, alloc] };
        // SAFETY: as above.
        let document: Option<Retained<AnyObject>> =
            unsafe { msg_send![allocated, initWithData: &*data] };
        let document = document.ok_or(PdfError::NotPdf)?;
        // SAFETY: PDFKit rule (PDFDocument.h): `isLocked` is a BOOL property.
        let locked: bool = unsafe { msg_send![&*document, isLocked] };
        if locked {
            return Err(PdfError::Locked);
        }
        // SAFETY: PDFKit rule (PDFDocument.h): `pageCount` is an `NSUInteger` property.
        let count: usize = unsafe { msg_send![&*document, pageCount] };
        if count == 0 {
            return Err(PdfError::Empty);
        }
        Ok(Self { document, count })
    }

    /// How many pages it has.
    #[must_use]
    pub const fn page_count(&self) -> usize {
        self.count
    }

    /// Page `index` (from 0), and its size in points as shown (rotation applied).
    fn page(&self, index: usize) -> Option<(Retained<AnyObject>, f64, f64)> {
        if index >= self.count {
            return None;
        }
        // SAFETY: PDFKit rule (PDFDocument.h): `-pageAtIndex:` takes an index below
        // `pageCount`, checked above.
        let page: Option<Retained<AnyObject>> =
            unsafe { msg_send![&*self.document, pageAtIndex: index] };
        let page = page?;
        // SAFETY: PDFKit rule (PDFPage.h): `-boundsForBox:` takes a `PDFDisplayBox` (an
        // `NSInteger`) and answers a `CGRect`.
        let bounds: CGRect = unsafe { msg_send![&*page, boundsForBox: CROP_BOX] };
        // SAFETY: PDFKit rule (PDFPage.h): `rotation` is an `NSInteger` of degrees.
        let rotation: isize = unsafe { msg_send![&*page, rotation] };
        let (width, height) = (bounds.size.width, bounds.size.height);
        let turned = rotation.rem_euclid(180) == 90;
        let (width, height) = if turned { (height, width) } else { (width, height) };
        (width > 0.0 && height > 0.0).then_some((page, width, height))
    }

    /// Page `index`'s size in points as shown.
    #[must_use]
    pub fn page_size(&self, index: usize) -> Option<(f64, f64)> {
        self.page(index).map(|(_, width, height)| (width, height))
    }

    /// Page `index` (from 0) drawn `width` pixels wide (at most [`MAX_WIDTH`]), its height in
    /// its own proportion.
    #[must_use]
    pub fn draw(&self, index: usize, width: u32) -> Option<PdfPicture> {
        let (page, page_width, page_height) = self.page(index)?;
        let width = width.clamp(1, MAX_WIDTH);
        let scale = f64::from(width) / page_width;
        let height = whole(page_height * scale)?;
        let (columns, rows) = (usize::try_from(width).ok()?, usize::try_from(height).ok()?);
        let mut bgra = vec![0_u8; columns.checked_mul(rows)?.checked_mul(4)?];
        let space = CGColorSpace::new_device_rgb()?;
        // BGRA in memory, alpha first in a little-endian 32-bit word, premultiplied.
        let info = CGImageAlphaInfo::PremultipliedFirst.0 | CGImageByteOrderInfo::Order32Little.0;
        // SAFETY: `CGBitmapContextCreate` (CGBitmapContext.h) draws into `bgra` for the
        // context's life: it holds `bytes_per_row × height` bytes (four a pixel) and outlives the
        // context, which is dropped before `bgra` is read. 8-bit premultiplied ARGB, 32-bit
        // little-endian, in device RGB is one of its supported formats.
        let context = unsafe {
            CGBitmapContextCreate(
                bgra.as_mut_ptr().cast::<c_void>(),
                columns,
                rows,
                8,
                columns.checked_mul(4)?,
                Some(&space),
                info,
            )
        }?;
        let ctx = Some(&*context);
        CGContext::set_rgb_fill_color(ctx, 1.0, 1.0, 1.0, 1.0);
        CGContext::fill_rect(
            ctx,
            CGRect::new(
                CGPoint::new(0.0, 0.0),
                CGSize::new(CGFloat::from(width), CGFloat::from(height)),
            ),
        );
        CGContext::scale_ctm(ctx, scale, scale);
        // SAFETY: PDFKit rule (PDFPage.h): `-drawWithBox:toContext:` draws the page, its
        // rotation applied, into a `CGContextRef` the caller holds for the call.
        let () = unsafe { msg_send![&*page, drawWithBox: CROP_BOX, toContext: &*context] };
        drop(context);
        Some(PdfPicture { width, height, bgra })
    }
}

/// `pixels` rounded to a whole count of at least one; `None` when not finite or too large.
fn whole(pixels: f64) -> Option<u32> {
    let pixels = pixels.round().max(1.0);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "checked finite, at least one and at most u32::MAX"
    )]
    (pixels.is_finite() && pixels <= f64::from(u32::MAX)).then_some(pixels as u32)
}

/// Whether `PDFKit`'s document class is there to draw with.
#[must_use]
pub fn available() -> bool {
    AnyClass::get(c"PDFDocument").is_some()
}

/// A PDF for tests, written by hand so no tool is needed.
///
/// It has `pages` pages, `width` × `height` points each, page `n` with a band at its foot whose
/// shade says its number.
#[doc(hidden)]
#[must_use]
#[expect(
    clippy::arithmetic_side_effects,
    reason = "a test document of a few small pages, far from any overflow"
)]
pub fn sample(pages: usize, width: u32, height: u32) -> Vec<u8> {
    let mut objects: Vec<String> = Vec::new();
    let kids: Vec<String> = (0..pages).map(|n| format!("{} 0 R", 3 + n * 2)).collect();
    objects.push("<< /Type /Catalog /Pages 2 0 R >>".to_owned());
    objects.push(format!("<< /Type /Pages /Kids [{}] /Count {pages} >>", kids.join(" ")));
    for n in 0..pages {
        let content = 4 + n * 2;
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {width} {height}] /Contents {content} \
             0 R >>"
        ));
        #[expect(clippy::cast_precision_loss, reason = "a test PDF has a few pages")]
        let shade = 0.2 + 0.6 * (n as f64) / (pages.max(1) as f64);
        let stream = format!("{shade:.3} g 0 0 {width} {} re f", height / 4);
        objects.push(format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()));
    }
    let mut out = String::from("%PDF-1.4\n");
    let mut offsets = Vec::with_capacity(objects.len());
    for (n, object) in objects.iter().enumerate() {
        offsets.push(out.len());
        let _wrote = writeln!(out, "{} 0 obj\n{object}\nendobj", n + 1);
    }
    let xref = out.len();
    let _wrote = writeln!(out, "xref\n0 {}\n0000000000 65535 f ", objects.len() + 1);
    for offset in offsets {
        let _wrote = writeln!(out, "{offset:010} 00000 n ");
    }
    let _wrote = writeln!(
        out,
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF",
        objects.len() + 1
    );
    out.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A three-page PDF opens with its count, and each page draws at the width asked, in its
    /// own proportion, on white, its band where the page put it.
    #[test]
    fn a_pdf_draws_each_page_at_the_width_asked() {
        assert!(available());
        let pdf = Pdf::open(&sample(3, 200, 100)).expect("a PDF");
        assert_eq!(pdf.page_count(), 3);
        assert_eq!(pdf.page_size(0), Some((200.0, 100.0)));
        let picture = pdf.draw(1, 400).expect("page 2");
        assert_eq!((picture.width, picture.height), (400, 200));
        assert_eq!(picture.bgra.len(), 400 * 200 * 4);
        let pixel = |x: usize, y: usize| {
            let at = (y * 400 + x) * 4;
            picture.bgra[at..at + 4].to_vec()
        };
        assert_eq!(pixel(200, 10), [255, 255, 255, 255], "white paper at the top");
        let band = pixel(200, 190);
        assert!(band[0] < 200 && band[0] == band[1] && band[1] == band[2], "grey band: {band:?}");
        assert!(pdf.draw(3, 400).is_none(), "no fourth page");
    }

    /// What is not a PDF says so, and an absurd width is held to the cap.
    #[test]
    fn what_is_not_a_pdf_says_so() {
        assert_eq!(Pdf::open(b"%PDF-1.4 garbage").err(), Some(PdfError::NotPdf));
        assert_eq!(Pdf::open(b"").err(), Some(PdfError::NotPdf));
        let pdf = Pdf::open(&sample(1, 100, 100)).expect("a PDF");
        assert_eq!(pdf.draw(0, 100_000).map(|p| p.width), Some(MAX_WIDTH));
    }
}

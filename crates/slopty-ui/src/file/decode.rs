//! A file tile's picture or PDF, decoded and drawn by the platform at the size the tile shows
//! it.
//!
//! A picture goes through `ImageIO`, which reads every format the system does (PNG, JPEG, GIF,
//! WebP, HEIC, AVIF, TIFF, JPEG XL, …) and decodes straight to the size asked
//! (`CGImageSourceCreateThumbnailAtIndex`), so a 48-megapixel photo in a small tile costs a
//! small decode, a JPEG's scaled DCT and a HEIC's own tiles doing most of it. An animation's
//! frames come with their delays. A PDF is a `CGPDFDocument`, each page drawn on its own at the
//! width it is shown, only when asked.
//!
//! What comes out is [`Pixels`]: straight-alpha BGRA rows, as GPUI's `RenderImage` takes them.
//! The file's bytes are never copied: the platform reads them through a data provider that
//! holds the [`Bytes`] until it lets go.

use std::ffi::c_void;
use std::ptr::NonNull;

use bytes::Bytes;
use objc2_core_foundation::{
    CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType, CGFloat, CGPoint, CGRect,
    CGSize,
};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGColorSpace, CGContext, CGDataProvider, CGImage, CGImageAlphaInfo,
    CGImageByteOrderInfo, CGInterpolationQuality, CGPDFBox, CGPDFDocument, CGPDFPage,
    kCGColorSpaceSRGB,
};
use objc2_image_io::{
    CGImageSource, kCGImagePropertyAPNGUnclampedDelayTime, kCGImagePropertyDPIHeight,
    kCGImagePropertyDPIWidth, kCGImagePropertyGIFDelayTime, kCGImagePropertyGIFDictionary,
    kCGImagePropertyGIFUnclampedDelayTime, kCGImagePropertyHEICSDictionary,
    kCGImagePropertyHEICSUnclampedDelayTime, kCGImagePropertyOrientation,
    kCGImagePropertyPNGDictionary, kCGImagePropertyPixelHeight, kCGImagePropertyPixelWidth,
    kCGImagePropertyWebPDictionary, kCGImagePropertyWebPUnclampedDelayTime,
    kCGImageSourceCreateThumbnailFromImageAlways, kCGImageSourceCreateThumbnailWithTransform,
    kCGImageSourceShouldCacheImmediately, kCGImageSourceThumbnailMaxPixelSize,
};

/// Decoded bytes an animation keeps at most; one past it shows its first frame only.
pub const ANIMATION_BYTES: usize = 256 << 20;

/// The widest or tallest a drawn page or a decoded picture gets, in pixels: Metal's texture
/// limit on every Apple GPU the floor supports.
pub const LARGEST: u32 = 16_384;

/// Why a file cannot be shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewError {
    /// The platform does not read it: damaged, or a format it lacks.
    Unreadable,
    /// A PDF with no pages, or a picture with no image in it.
    Empty,
    /// A PDF that asks for a password.
    Locked,
    /// Drawing it failed (out of memory for its size).
    Draw,
}

impl PreviewError {
    /// What the tile says of it, under "Cannot show this file".
    #[must_use]
    pub const fn say(self) -> &'static str {
        match self {
            Self::Unreadable => "This device cannot read it",
            Self::Empty => "There is nothing in it to show",
            Self::Locked => "It is locked with a password",
            Self::Draw => "It could not be drawn",
        }
    }
}

/// Pixels as GPUI's `RenderImage` keeps them: BGRA, straight alpha, rows packed from the top.
#[derive(Clone, PartialEq, Eq)]
pub struct Pixels {
    /// Columns.
    pub width: u32,
    /// Rows.
    pub height: u32,
    /// `width × height × 4` bytes.
    pub bgra: Vec<u8>,
}

impl std::fmt::Debug for Pixels {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Pixels({}×{})", self.width, self.height)
    }
}

/// How large a picture is, from its header.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PictureSize {
    /// Its pixels, as shown: turned by its orientation.
    pub pixels: (u32, u32),
    /// Its size in points at its own resolution: a 144-dpi Retina screenshot is half its
    /// pixels, a picture that names none is one point a pixel.
    pub points: (f64, f64),
}

/// One frame of a picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// Its pixels.
    pub pixels: Pixels,
    /// How long it shows, in milliseconds; zero for a still.
    pub delay_ms: u32,
}

/// A picture decoded for a tile.
#[derive(Debug, Clone, PartialEq)]
pub struct Picture {
    /// What its header says.
    pub size: PictureSize,
    /// One frame for a still; every frame of an animation within [`ANIMATION_BYTES`].
    pub frames: Vec<Frame>,
}

/// Decode `bytes` as a picture whose longer side is `longest(size)` pixels at most, `size` being
/// what its header says; never larger than the picture itself.
///
/// # Errors
///
/// [`PreviewError::Unreadable`] when `ImageIO` cannot read it, [`PreviewError::Empty`] when it
/// holds no image, [`PreviewError::Draw`] when a frame could not be drawn.
pub fn picture(
    bytes: Bytes,
    longest: impl FnOnce(PictureSize) -> u32,
) -> Result<Picture, PreviewError> {
    let provider = provider(bytes).ok_or(PreviewError::Unreadable)?;
    // SAFETY: `CGImageSourceCreateWithDataProvider` (ImageIO/CGImageSource.h) retains the
    // provider for as long as it reads from it; no options.
    let source = unsafe { CGImageSource::with_data_provider(&provider, None) }
        .ok_or(PreviewError::Unreadable)?;
    // SAFETY: counting the images of a source made above (CGImageSource.h).
    let count = unsafe { source.count() };
    if count == 0 {
        return Err(PreviewError::Empty);
    }
    // SAFETY: as above; the primary image's index is below `count`.
    let primary = unsafe { source.primary_image_index() }.min(count.saturating_sub(1));
    let size = picture_size(&source, primary).ok_or(PreviewError::Unreadable)?;
    let most = size.pixels.0.max(size.pixels.1).min(LARGEST);
    let want = longest(size).clamp(1, most.max(1));
    let two_fifths = (most / 5).saturating_mul(2);
    let decode_at = if want < two_fifths && shrinks_slowly(&source) { two_fifths } else { want };
    let options = thumbnail_options(decode_at);
    let still = || -> Result<Vec<Frame>, PreviewError> {
        let pixels = frame(&source, primary, &options, want)?;
        Ok(vec![Frame { pixels, delay_ms: 0 }])
    };
    if count == 1 {
        return Ok(Picture { size, frames: still()? });
    }
    let mut frames = Vec::with_capacity(count);
    let mut kept = 0_usize;
    for index in 0..count {
        let Some(delay_ms) = delay_ms(&source, index) else {
            // Several images that are not an animation (a HEIC's burst, a TIFF's pages): the
            // primary one.
            return Ok(Picture { size, frames: still()? });
        };
        let pixels = frame(&source, index, &options, want)?;
        kept = kept.saturating_add(pixels.bgra.len());
        if kept > ANIMATION_BYTES {
            frames.truncate(1);
            break;
        }
        frames.push(Frame { pixels, delay_ms });
    }
    Ok(Picture { size, frames })
}

/// A data provider over `bytes`, holding them until the platform lets it go.
fn provider(bytes: Bytes) -> Option<CFRetained<CGDataProvider>> {
    /// Hands back the `Bytes` [`provider`] lent the platform.
    unsafe extern "C-unwind" fn release(info: *mut c_void, _data: NonNull<c_void>, _size: usize) {
        // SAFETY: `info` is the box `provider` leaked, which the provider hands back here once
        // it is freed (CGDataProvider.h, `CGDataProviderReleaseDataCallback`).
        drop(unsafe { Box::from_raw(info.cast::<Bytes>()) });
    }
    if bytes.is_empty() {
        return None;
    }
    let (data, size) = (bytes.as_ptr().cast::<c_void>(), bytes.len());
    let info = Box::into_raw(Box::new(bytes)).cast::<c_void>();
    // SAFETY: `CGDataProviderCreateWithData` (CGDataProvider.h) reads `size` bytes at `data`
    // until it calls `release` with `info`. `data` points into the boxed `Bytes`, whose storage
    // does not move and lives until that call.
    let made = unsafe { CGDataProvider::with_data(info, data, size, Some(release)) };
    if made.is_none() {
        // SAFETY: no provider was made, so no release comes for `info`: it is still ours.
        drop(unsafe { Box::from_raw(info.cast::<Bytes>()) });
    }
    made
}

/// A number from a property dictionary.
fn number(dict: &CFDictionary<CFString, CFType>, key: &CFString) -> Option<f64> {
    dict.get(key)?.downcast::<CFNumber>().ok()?.as_f64()
}

/// The properties of image `index`, keyed by name.
fn properties(source: &CGImageSource, index: usize) -> Option<CFRetained<CFDictionary>> {
    // SAFETY: `CGImageSourceCopyPropertiesAtIndex` (CGImageSource.h) on an index below the
    // source's count; no options.
    unsafe { source.properties_at_index(index, None) }
}

/// Cast an image property dictionary to what it holds.
fn typed(dict: &CFDictionary) -> &CFDictionary<CFString, CFType> {
    // SAFETY: ImageIO's property dictionaries are keyed by `CFString` (CGImageProperties.h);
    // values are any CF type, read through `downcast`.
    unsafe { dict.cast_unchecked() }
}

/// What image `index`'s header says of its size.
fn picture_size(source: &CGImageSource, index: usize) -> Option<PictureSize> {
    let props = properties(source, index)?;
    let props = typed(&props);
    // SAFETY: reading ImageIO's exported key constants (CGImageProperties.h).
    let (w, h, dpi_w, dpi_h, turn) = unsafe {
        (
            number(props, kCGImagePropertyPixelWidth)?,
            number(props, kCGImagePropertyPixelHeight)?,
            number(props, kCGImagePropertyDPIWidth),
            number(props, kCGImagePropertyDPIHeight),
            number(props, kCGImagePropertyOrientation),
        )
    };
    // EXIF orientations 5 to 8 turn the picture a quarter (CGImagePropertyOrientation).
    let quarter = turn.is_some_and(|o| (5.0..=8.0).contains(&o));
    let (w, h) = if quarter { (h, w) } else { (w, h) };
    let (dpi_w, dpi_h) = if quarter { (dpi_h, dpi_w) } else { (dpi_w, dpi_h) };
    let points = |px: f64, dpi: Option<f64>| px * 72.0 / dpi.filter(|d| *d >= 1.0).unwrap_or(72.0);
    Some(PictureSize {
        pixels: (whole(w)?, whole(h)?),
        points: (points(w, dpi_w), points(h, dpi_h)),
    })
}

/// A header's pixel count as one.
fn whole(value: f64) -> Option<u32> {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a positive whole number of pixels, checked below u32's range"
    )]
    (value >= 1.0 && value < f64::from(u32::MAX)).then(|| value.round() as u32)
}

/// How long frame `index` of an animation shows, or `None` when the source is not one.
///
/// The unclamped delay where the format has one, else the clamped one; below 11 ms it is
/// 100 ms, as browsers take a GIF's "as fast as possible".
fn delay_ms(source: &CGImageSource, index: usize) -> Option<u32> {
    let props = properties(source, index)?;
    let props = typed(&props);
    // SAFETY: reading ImageIO's exported key constants (CGImageProperties.h).
    let formats: [(&CFString, &[&CFString]); 4] = unsafe {
        [
            (
                kCGImagePropertyGIFDictionary,
                &[kCGImagePropertyGIFUnclampedDelayTime, kCGImagePropertyGIFDelayTime],
            ),
            (kCGImagePropertyPNGDictionary, &[kCGImagePropertyAPNGUnclampedDelayTime]),
            (kCGImagePropertyWebPDictionary, &[kCGImagePropertyWebPUnclampedDelayTime]),
            (kCGImagePropertyHEICSDictionary, &[kCGImagePropertyHEICSUnclampedDelayTime]),
        ]
    };
    let seconds = formats.iter().find_map(|(format, keys)| {
        let inner = props.get(format)?.downcast::<CFDictionary>().ok()?;
        keys.iter().find_map(|key| number(typed(&inner), key))
    })?;
    let ms = (seconds * 1000.0).round();
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a delay under a minute, checked"
    )]
    Some(if ms < 11.0 || ms > 60_000.0 { 100 } else { ms as u32 })
}

/// Whether `source` is HEIF-coded (HEIC, HEIF and their sequences), where `ImageIO` decodes every
/// pixel whatever the size asked and then shrinks them itself, at a cost that grows with how far
/// (MEASUREMENTS.md, "HEIC's thumbnail cost"): shrinking it to two fifths there and the rest in
/// the draw is cheaper.
fn shrinks_slowly(source: &CGImageSource) -> bool {
    // SAFETY: `CGImageSourceGetType` (CGImageSource.h) on a source made from data.
    let kind = unsafe { source.r#type() };
    kind.is_some_and(|k| k.to_string().starts_with("public.hei"))
}

/// The thumbnail options: made from the full picture, turned by its orientation, decoded now,
/// and no larger than `longest` pixels on its longer side.
fn thumbnail_options(longest: u32) -> CFRetained<CFDictionary<CFString, CFType>> {
    let yes: &CFType = CFBoolean::new(true);
    let size = CFNumber::new_i64(i64::from(longest));
    // SAFETY: reading ImageIO's exported key constants (CGImageSource.h).
    let keys = unsafe {
        [
            kCGImageSourceCreateThumbnailFromImageAlways,
            kCGImageSourceCreateThumbnailWithTransform,
            kCGImageSourceShouldCacheImmediately,
            kCGImageSourceThumbnailMaxPixelSize,
        ]
    };
    CFDictionary::from_slices(&keys, &[yes, yes, yes, &size])
}

/// Image `index` of `source`, decoded as `options` say and drawn with its longer side `want`
/// pixels at most.
fn frame(
    source: &CGImageSource,
    index: usize,
    options: &CFDictionary<CFString, CFType>,
    want: u32,
) -> Result<Pixels, PreviewError> {
    // SAFETY: `CGImageSourceCreateThumbnailAtIndex` (CGImageSource.h) on an index below the
    // source's count, with a dictionary of the thumbnail keys.
    let image = unsafe { source.thumbnail_at_index(index, Some(options.as_opaque())) }
        .ok_or(PreviewError::Unreadable)?;
    let (width, height) = (CGImage::width(Some(&image)), CGImage::height(Some(&image)));
    let size = (u32::try_from(width), u32::try_from(height));
    let (Ok(w), Ok(h)) = size else { return Err(PreviewError::Draw) };
    let (w, h) = within(w, h, want);
    let alpha = CGImage::alpha_info(Some(&image));
    let opaque =
        [CGImageAlphaInfo::None, CGImageAlphaInfo::NoneSkipFirst, CGImageAlphaInfo::NoneSkipLast]
            .contains(&alpha);
    draw(w, h, if opaque { Ground::Opaque } else { Ground::Clear }, |ctx| {
        let rect = CGRect::new(CGPoint::ZERO, CGSize::new(cg(w), cg(h)));
        CGContext::draw_image(Some(ctx), rect, Some(&image));
    })
}

/// `w × h` shrunk to a longer side of `want` at most, each side a whole pixel at least.
fn within(w: u32, h: u32, want: u32) -> (u32, u32) {
    let longer = w.max(h);
    if longer <= want || longer == 0 {
        return (w, h);
    }
    let side = |n: u32| {
        let scaled =
            u64::from(n).saturating_mul(u64::from(want)).saturating_add(u64::from(longer / 2));
        let side = scaled.checked_div(u64::from(longer)).unwrap_or(0);
        u32::try_from(side).unwrap_or(want).max(1)
    };
    (side(w), side(h))
}

/// A pixel count as Core Graphics measures.
fn cg(n: u32) -> CGFloat {
    CGFloat::from(n)
}

/// What a drawing goes over.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Ground {
    /// White, as a page's paper.
    Paper,
    /// Nothing, for a picture that covers every pixel: no alpha to undo.
    Opaque,
    /// Nothing, for a picture with see-through pixels.
    Clear,
}

/// Draw `w × h` pixels with `paint` on a bitmap context over `ground`, and read them out as
/// straight-alpha BGRA.
fn draw(
    w: u32,
    h: u32,
    ground: Ground,
    paint: impl FnOnce(&CGContext),
) -> Result<Pixels, PreviewError> {
    if w == 0 || h == 0 || w > LARGEST || h > LARGEST {
        return Err(PreviewError::Draw);
    }
    let (Ok(cols), Ok(rows)) = (usize::try_from(w), usize::try_from(h)) else {
        return Err(PreviewError::Draw);
    };
    let stride = cols.checked_mul(4).ok_or(PreviewError::Draw)?;
    let len = stride.checked_mul(rows).ok_or(PreviewError::Draw)?;
    let mut bgra = Vec::new();
    bgra.try_reserve_exact(len).map_err(|_full| PreviewError::Draw)?;
    bgra.resize(len, 0);
    // SAFETY: reading CoreGraphics' exported constant (CGColorSpace.h).
    let srgb = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }));
    // Little-endian 32-bit words with alpha first are B, G, R, A in memory, the one 8-bit
    // layout with alpha a bitmap context draws into besides RGBA (CGBitmapContext.h).
    let info = CGImageAlphaInfo::PremultipliedFirst.0 | CGImageByteOrderInfo::Order32Little.0;
    // SAFETY: `CGBitmapContextCreate` (CGBitmapContext.h) draws into `rows × stride` bytes at
    // the pointer, which `bgra` holds, unmoved, until the context is dropped at the end of this
    // function.
    let ctx = unsafe {
        CGBitmapContextCreate(
            bgra.as_mut_ptr().cast(),
            cols,
            rows,
            8,
            stride,
            srgb.as_deref(),
            info,
        )
    }
    .ok_or(PreviewError::Draw)?;
    CGContext::set_interpolation_quality(Some(&ctx), CGInterpolationQuality::High);
    if ground == Ground::Paper {
        CGContext::set_rgb_fill_color(Some(&ctx), 1.0, 1.0, 1.0, 1.0);
        CGContext::fill_rect(Some(&ctx), CGRect::new(CGPoint::ZERO, CGSize::new(cg(w), cg(h))));
    }
    paint(&ctx);
    drop(ctx);
    if ground == Ground::Clear {
        unpremultiply(&mut bgra);
    }
    Ok(Pixels { width: w, height: h, bgra })
}

/// Undo the context's premultiplied alpha, since GPUI blends a picture's colour by its alpha.
fn unpremultiply(bgra: &mut [u8]) {
    for [b, g, r, a] in bgra.as_chunks_mut::<4>().0 {
        let alpha = u32::from(*a);
        if alpha == 0 || alpha == 255 {
            continue;
        }
        for c in [b, g, r] {
            // A premultiplied channel is at most its alpha, so this stays within a byte.
            let rounded = u32::from(*c).saturating_mul(255).saturating_add(alpha / 2);
            let straight = rounded.checked_div(alpha).unwrap_or(u32::MAX);
            *c = u8::try_from(straight).unwrap_or(u8::MAX);
        }
    }
}

/// A PDF, open for its pages to be drawn one at a time.
///
/// `Send`, so it can go to the thread that draws, but not `Sync`: one page is drawn at a time.
#[derive(Debug)]
pub struct Pdf {
    doc: CFRetained<CGPDFDocument>,
    pages: usize,
}

#[expect(
    clippy::non_send_fields_in_send_ty,
    reason = "the document is the one field, sent whole under the rule below"
)]
// SAFETY: a `CGPDFDocument` is an immutable Core Foundation object with no thread affinity; CF's
// retain and release are atomic, and `Pdf` is not `Sync`, so the document is used from one
// thread at a time (Core Foundation's thread-safety rules for immutable objects).
unsafe impl Send for Pdf {}

impl Pdf {
    /// Open `bytes` as a PDF, unlocking one with an empty password as Preview does.
    ///
    /// # Errors
    ///
    /// [`PreviewError::Unreadable`] when it is not a PDF Core Graphics reads,
    /// [`PreviewError::Locked`] when it needs a password, [`PreviewError::Empty`] with no pages.
    pub fn open(bytes: Bytes) -> Result<Self, PreviewError> {
        let provider = provider(bytes).ok_or(PreviewError::Unreadable)?;
        let doc = CGPDFDocument::with_provider(Some(&provider)).ok_or(PreviewError::Unreadable)?;
        if CGPDFDocument::is_encrypted(Some(&doc)) && !CGPDFDocument::is_unlocked(Some(&doc)) {
            let empty = NonNull::from(c"").cast();
            // SAFETY: `CGPDFDocumentUnlockWithPassword` (CGPDFDocument.h) with a NUL-terminated
            // password.
            let opened = unsafe { CGPDFDocument::unlock_with_password(Some(&doc), empty) };
            if !opened {
                return Err(PreviewError::Locked);
            }
        }
        let pages = CGPDFDocument::number_of_pages(Some(&doc));
        if pages == 0 {
            return Err(PreviewError::Empty);
        }
        Ok(Self { doc, pages })
    }

    /// Its pages.
    #[must_use]
    pub const fn pages(&self) -> usize {
        self.pages
    }

    /// Page `index` (from 0), if there is one.
    fn page(&self, index: usize) -> Option<CFRetained<CGPDFPage>> {
        (index < self.pages)
            .then(|| CGPDFDocument::page(Some(&self.doc), index.saturating_add(1)))
            .flatten()
    }

    /// The size of page `index` in points as it is read: its crop box, turned by its rotation.
    #[must_use]
    pub fn page_size(&self, index: usize) -> Option<(f64, f64)> {
        let page = self.page(index)?;
        let crop = CGPDFPage::box_rect(Some(&page), CGPDFBox::CropBox);
        let (w, h) = (crop.size.width.abs(), crop.size.height.abs());
        let quarter = CGPDFPage::rotation_angle(Some(&page)).rem_euclid(180) == 90;
        Some(if quarter { (h, w) } else { (w, h) })
    }

    /// Draw page `index` on white, `width` pixels wide and as tall as its shape makes it.
    ///
    /// # Errors
    ///
    /// [`PreviewError::Empty`] past the last page, [`PreviewError::Draw`] when the bitmap cannot
    /// be made.
    pub fn render(&self, index: usize, width: u32) -> Result<Pixels, PreviewError> {
        let page = self.page(index).ok_or(PreviewError::Empty)?;
        let (pw, ph) = self.page_size(index).ok_or(PreviewError::Empty)?;
        if pw < 1.0 || ph < 1.0 {
            return Err(PreviewError::Empty);
        }
        let width = width.clamp(1, LARGEST);
        let height = whole((f64::from(width) * ph / pw).max(1.0)).ok_or(PreviewError::Draw)?;
        draw(width, height.min(LARGEST), Ground::Paper, |ctx| {
            // The page's own transform into a rect of its size: its crop box to the origin,
            // turned by its rotation; then scaled to the bitmap.
            let rect = CGRect::new(CGPoint::ZERO, CGSize::new(pw, ph));
            let fit = CGPDFPage::drawing_transform(Some(&page), CGPDFBox::CropBox, rect, 0, true);
            CGContext::scale_ctm(Some(ctx), f64::from(width) / pw, f64::from(height) / ph);
            CGContext::concat_ctm(Some(ctx), fit);
            CGContext::draw_pdf_page(Some(ctx), Some(&page));
        })
    }
}

#[cfg(test)]
#[expect(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "fixtures and timings of small, known sizes"
)]
pub(crate) mod tests {
    use std::fmt::Write as _;
    use std::io::Cursor;

    use image::codecs::gif::GifEncoder;
    use image::{Delay, ImageFormat, Rgba, RgbaImage};

    use super::*;

    /// `w × h` pixels: opaque red on the left half, half-clear blue on the right.
    pub(crate) fn halves(w: u32, h: u32) -> RgbaImage {
        RgbaImage::from_fn(w, h, |x, _| {
            if x < w / 2 { Rgba([255, 0, 0, 255]) } else { Rgba([0, 0, 255, 128]) }
        })
    }

    /// `image` as a PNG file's bytes.
    pub(crate) fn png(image: &RgbaImage) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        image.write_to(&mut out, ImageFormat::Png).unwrap();
        out.into_inner()
    }

    /// A PNG's bytes with a `pHYs` chunk saying `dpi`, after its header chunk.
    fn with_dpi(png: &[u8], dpi: u32) -> Vec<u8> {
        let per_metre = (f64::from(dpi) / 0.0254).round() as u32;
        let mut body = b"pHYs".to_vec();
        body.extend_from_slice(&per_metre.to_be_bytes());
        body.extend_from_slice(&per_metre.to_be_bytes());
        body.push(1);
        let mut chunk = 9_u32.to_be_bytes().to_vec();
        chunk.extend_from_slice(&body);
        chunk.extend_from_slice(&crc32(&body).to_be_bytes());
        let header_end = 8 + 25;
        [&png[..header_end], &chunk, &png[header_end..]].concat()
    }

    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = u32::MAX;
        for byte in bytes {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = if crc & 1 == 1 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
            }
        }
        !crc
    }

    /// A PDF of `pages`, each a media box `[0 0 w h]`, a rotation and a content stream, with
    /// Helvetica as font `F1`.
    pub(crate) fn pdf(pages: &[((u32, u32), u32, &str)]) -> Vec<u8> {
        let mut objects = vec![String::new(), String::new()];
        let kids: Vec<String> = (0..pages.len()).map(|n| format!("{} 0 R", 3 + 2 * n)).collect();
        objects[0] = "<< /Type /Catalog /Pages 2 0 R >>".to_owned();
        objects[1] =
            format!("<< /Type /Pages /Kids [{}] /Count {} >>", kids.join(" "), pages.len());
        for (n, ((w, h), rotate, content)) in pages.iter().enumerate() {
            objects.push(format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w} {h}] /Rotate {rotate} /Resources << /Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >> >> /Contents {} 0 R >>",
                4 + 2 * n
            ));
            objects
                .push(format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len() + 1));
        }
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (n, object) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", n + 1).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for at in offsets {
            out.extend_from_slice(format!("{at:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    fn bgra_at(pixels: &Pixels, x: u32, y: u32) -> [u8; 4] {
        let at = ((y * pixels.width + x) * 4) as usize;
        pixels.bgra[at..at + 4].try_into().unwrap()
    }

    #[test]
    fn unpremultiplying_restores_straight_colour_and_leaves_opaque_and_clear_alone() {
        let mut px = [64, 32, 0, 128, 10, 20, 30, 255, 0, 0, 0, 0];
        unpremultiply(&mut px);
        assert_eq!(px, [128, 64, 0, 128, 10, 20, 30, 255, 0, 0, 0, 0]);
    }

    #[test]
    fn nothing_and_garbage_are_unreadable() {
        assert_eq!(picture(Bytes::new(), |_| 64).err(), Some(PreviewError::Unreadable));
        let junk = Bytes::from_static(b"\x89PNG\r\n\x1a\nnot really");
        assert_eq!(picture(junk, |_| 64).err(), Some(PreviewError::Unreadable));
        assert_eq!(
            Pdf::open(Bytes::from_static(b"%PDF-1.7 and nothing")).err(),
            Some(PreviewError::Unreadable)
        );
    }

    /// A picture decodes to the size asked, never past its own, in straight-alpha BGRA, and its
    /// header's resolution says its size in points.
    /// `bytes` converted to HEIC by the system's own `sips`.
    fn heic(png: &[u8]) -> Bytes {
        let dir = tempfile::tempdir().unwrap();
        let (from, to) = (dir.path().join("in.png"), dir.path().join("out.heic"));
        std::fs::write(&from, png).unwrap();
        let made = std::process::Command::new("/usr/bin/sips")
            .args(["-s", "format", "heic"])
            .arg(&from)
            .arg("--out")
            .arg(&to)
            .output()
            .unwrap();
        assert!(made.status.success(), "{made:?}");
        Bytes::from(std::fs::read(&to).unwrap())
    }

    /// A HEIC is shrunk partly by `ImageIO` and the rest in the draw, and comes out at the size
    /// asked with its colours all the same.
    #[test]
    fn a_heic_is_shrunk_in_two_steps_to_the_size_asked() {
        let bytes = heic(&png(&halves(1000, 500)));
        let source = |bytes: Bytes| {
            let provider = provider(bytes).unwrap();
            // SAFETY: as in `picture`.
            unsafe { CGImageSource::with_data_provider(&provider, None) }.unwrap()
        };
        assert!(shrinks_slowly(&source(bytes.clone())));
        assert!(!shrinks_slowly(&source(Bytes::from(png(&halves(10, 10))))));
        let got = picture(bytes, |_| 100).unwrap();
        let [frame] = got.frames.as_slice() else { panic!("one frame: {:?}", got.frames) };
        assert_eq!((frame.pixels.width, frame.pixels.height), (100, 50));
        let [b, g, r, a] = bgra_at(&frame.pixels, 10, 25);
        assert!(r >= 240 && g <= 20 && b <= 20 && a == 255, "red: {b} {g} {r} {a}");
        assert_eq!(within(4032, 3024, 400), (400, 300));
        assert_eq!(within(300, 200, 400), (300, 200), "never past its own size");
        assert_eq!(within(10_000, 1, 100), (100, 1), "a side a pixel at least");
    }

    #[test]
    fn a_picture_decodes_to_the_size_asked_with_its_colours() {
        let bytes = Bytes::from(png(&halves(300, 200)));
        let mut seen = None;
        let got = picture(bytes.clone(), |size| {
            seen = Some(size);
            150
        })
        .unwrap();
        assert_eq!(seen, Some(PictureSize { pixels: (300, 200), points: (300.0, 200.0) }));
        let [frame] = got.frames.as_slice() else { panic!("one frame: {:?}", got.frames) };
        assert_eq!((frame.pixels.width, frame.pixels.height, frame.delay_ms), (150, 100, 0));
        assert_eq!(bgra_at(&frame.pixels, 10, 50), [0, 0, 255, 255], "opaque red");
        let [b, g, r, a] = bgra_at(&frame.pixels, 140, 50);
        assert!(
            b >= 250 && g <= 2 && r <= 2 && a.abs_diff(128) <= 1,
            "straight blue: {b} {g} {r} {a}"
        );

        let whole = picture(bytes, |_| 10_000).unwrap();
        assert_eq!(
            (whole.frames[0].pixels.width, whole.frames[0].pixels.height),
            (300, 200),
            "never past its own size"
        );

        let retina = Bytes::from(with_dpi(&png(&halves(300, 200)), 144));
        let got = picture(retina, |_| 300).unwrap();
        assert_eq!(
            got.size.points,
            (150.0, 100.0),
            "a 144-dpi picture is half its pixels in points"
        );
    }

    /// An animated GIF comes with every frame and its delay; GIF's "as fast as it goes" is
    /// 100 ms.
    #[test]
    fn an_animation_keeps_its_frames_and_delays() {
        let mut out = Vec::new();
        {
            let mut encoder = GifEncoder::new(&mut out);
            let frames = [
                (Rgba([255, 0, 0, 255]), 50),
                (Rgba([0, 255, 0, 255]), 200),
                (Rgba([0, 0, 255, 255]), 0),
            ]
            .map(|(colour, ms)| {
                image::Frame::from_parts(
                    RgbaImage::from_pixel(8, 8, colour),
                    0,
                    0,
                    Delay::from_numer_denom_ms(ms, 1),
                )
            });
            encoder.encode_frames(frames).unwrap();
        }
        let got = picture(Bytes::from(out), |_| 8).unwrap();
        let delays: Vec<u32> = got.frames.iter().map(|f| f.delay_ms).collect();
        assert_eq!(delays, [50, 200, 100]);
        assert_eq!(bgra_at(&got.frames[1].pixels, 4, 4), [0, 255, 0, 255]);
    }

    /// The process's CPU time so far, in ms: every thread's, `ImageIO`'s own included.
    fn cpu_ms() -> f64 {
        unsafe extern "C" {
            /// clock(3): processor time used, in `CLOCKS_PER_SEC`, which is 1 000 000 on
            /// Apple platforms (time.h).
            safe fn clock() -> u64;
        }
        #[expect(clippy::cast_precision_loss, reason = "microseconds of one test run")]
        let micros = clock() as f64;
        micros / 1000.0
    }

    /// The median wall and CPU time of `runs` calls of `f`, in ms. Under another session's
    /// load the wall time varies threefold; the CPU time is what the call itself costs.
    fn medians_ms(runs: usize, mut f: impl FnMut()) -> (f64, f64) {
        let (mut wall, mut cpu): (Vec<f64>, Vec<f64>) = std::iter::repeat_with(|| {
            let (at, used) = (std::time::Instant::now(), cpu_ms());
            f();
            (at.elapsed().as_secs_f64() * 1000.0, cpu_ms() - used)
        })
        .take(runs)
        .unzip();
        wall.sort_by(f64::total_cmp);
        cpu.sort_by(f64::total_cmp);
        (wall[runs / 2], cpu[runs / 2])
    }

    /// The median of `runs` timings of `f`, in milliseconds.
    fn median_ms(runs: usize, mut f: impl FnMut()) -> f64 {
        let mut took: Vec<f64> = std::iter::repeat_with(|| {
            let at = std::time::Instant::now();
            f();
            at.elapsed().as_secs_f64() * 1000.0
        })
        .take(runs)
        .collect();
        took.sort_by(f64::total_cmp);
        took[runs / 2]
    }

    /// A 12-megapixel photo-like picture: smooth light and fine grain, as a camera's.
    fn photo() -> image::RgbImage {
        let mut seed = 0x2545_f491_u32;
        image::RgbImage::from_fn(4032, 3024, |x, y| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let grain = (seed & 15) as u8;
            let r = ((x * 200 / 4032) as u8).saturating_add(grain);
            let g = ((y * 180 / 3024) as u8).saturating_add(grain);
            let b = (((x + y) * 120 / 7056) as u8).saturating_add(grain);
            image::Rgb([r, g, b])
        })
    }

    /// Decode and draw costs (MEASUREMENTS.md, "file tile pictures and PDFs"):
    ///
    /// ```sh
    /// cargo nextest run -p slopty-ui --run-ignored only --no-capture -E 'test(decode_and_draw_costs)'
    /// ```
    #[test]
    #[ignore = "measurement, run by hand"]
    fn decode_and_draw_costs() {
        let dir = tempfile::tempdir().unwrap();
        let jpeg_path = dir.path().join("photo.jpg");
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90)
            .encode_image(&photo())
            .unwrap();
        std::fs::write(&jpeg_path, &jpeg).unwrap();
        let heic_path = dir.path().join("photo.heic");
        let made = std::process::Command::new("/usr/bin/sips")
            .args(["-s", "format", "heic"])
            .arg(&jpeg_path)
            .arg("--out")
            .arg(&heic_path)
            .output()
            .unwrap();
        assert!(made.status.success(), "{made:?}");
        let heic = std::fs::read(&heic_path).unwrap();
        let shot = png(&halves(2880, 1800));
        println!("| file | bytes | decoded to | wall ms | CPU ms |");
        println!("| --- | --- | --- | --- | --- |");
        for (name, bytes) in
            [("JPEG 4032×3024", jpeg), ("HEIC 4032×3024", heic), ("PNG 2880×1800", shot)]
        {
            let bytes = Bytes::from(bytes);
            for longest in [100, 400, 1600, 4032] {
                let mut got = (0, 0);
                let (wall, cpu) = medians_ms(9, || {
                    let p = picture(bytes.clone(), |_| longest).unwrap();
                    got = (p.frames[0].pixels.width, p.frames[0].pixels.height);
                });
                println!(
                    "| {name} | {} | {}×{} | {wall:.1} | {cpu:.1} |",
                    bytes.len(),
                    got.0,
                    got.1
                );
                if name.starts_with("HEIC") && longest < 1600 {
                    // ImageIO asked for the size itself, as before `shrinks_slowly`.
                    let (wall, cpu) = medians_ms(9, || {
                        let provider = provider(bytes.clone()).unwrap();
                        // SAFETY: as in `picture`.
                        let s = unsafe { CGImageSource::with_data_provider(&provider, None) };
                        let options = thumbnail_options(longest);
                        drop(frame(&s.unwrap(), 0, &options, longest).unwrap());
                    });
                    println!("| {name}, one step | | | {wall:.1} | {cpu:.1} |");
                }
            }
        }
        let text = (0..60).fold(String::new(), |mut text, line| {
            let y = 760 - line * 12;
            write!(text, "BT /F1 10 Tf 40 {y} Td (Line {line} of prose, in Helvetica.) Tj ET ")
                .unwrap();
            text
        });
        let page = ((612, 792), 0, text.as_str());
        let doc = Bytes::from(pdf(&[page; 20]));
        let open_ms = median_ms(5, || drop(Pdf::open(doc.clone()).unwrap()));
        println!("PDF open (20 pages, {} bytes): {open_ms:.2} ms", doc.len());
        let pdf = Pdf::open(doc).unwrap();
        for width in [800, 1600, 3200] {
            let ms = median_ms(5, || drop(pdf.render(3, width).unwrap()));
            println!("| PDF letter page of text | - | {width} wide | {ms:.1} |");
        }
    }

    /// A PDF's pages are sized by their crop box and rotation, and one is drawn on white at the
    /// width asked, its marks where the page puts them.
    #[test]
    fn a_pdf_page_is_drawn_at_the_width_asked() {
        let doc = pdf(&[
            ((200, 100), 0, "0 0 0 rg 0 0 100 100 re f"),
            ((200, 100), 90, "0 0 0 rg 0 0 100 100 re f"),
        ]);
        let pdf = Pdf::open(Bytes::from(doc)).unwrap();
        assert_eq!(pdf.pages(), 2);
        assert_eq!(pdf.page_size(0), Some((200.0, 100.0)));
        assert_eq!(pdf.page_size(1), Some((100.0, 200.0)), "turned a quarter");
        assert_eq!(pdf.page_size(2), None);
        let page = pdf.render(0, 400).unwrap();
        assert_eq!((page.width, page.height), (400, 200));
        assert_eq!(bgra_at(&page, 100, 100), [0, 0, 0, 255], "the mark on the left");
        assert_eq!(bgra_at(&page, 300, 100), [255, 255, 255, 255], "paper on the right");
        let turned = pdf.render(1, 100).unwrap();
        assert_eq!((turned.width, turned.height), (100, 200));
        assert_eq!(pdf.render(2, 100).err(), Some(PreviewError::Empty));
    }
}

//! A capture source that draws its own pictures: [`Canvas`].
//!
//! It is the instrument for timing the frame path end to end where ScreenCaptureKit cannot run.
//! A worker a test starts from a shell has no Screen Recording grant, and nothing may ask for
//! one. [`Canvas`] stands in for every active display (CoreGraphics lists them without a grant).
//! On the display's beat it hands the stream an `IOSurface`-backed full-range NV12 picture of the
//! size asked for, stamped on the host time clock ScreenCaptureKit stamps with, into the same
//! sink a real capture feeds. Nothing in the product names it: only a stream built on it draws.
//!
//! The picture is a dark desktop with a window of text scrolling in its middle, so every frame
//! changes as a scrolling page does. A strip of [`MARK_BITS`] blocks along the top spells how
//! many inputs the process has taken ([`take_input`]), and [`inputs_shown`] reads it back off a
//! decoded picture. That is what times input to glass: the first picture presented whose strip
//! counts an input is the one that shows it.

use std::ptr::{self, NonNull};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use dispatch2::{DispatchQoS, DispatchQueue, DispatchQueueAttr, DispatchRetained, DispatchTime};
use objc2_core_foundation::{CFDictionary, CFNumber, CFRetained, CFString, CFType, Type as _};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferCreate, CVPixelBufferGetBaseAddressOfPlane,
    CVPixelBufferGetBytesPerRowOfPlane, CVPixelBufferGetHeightOfPlane,
    CVPixelBufferGetWidthOfPlane, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags,
    CVPixelBufferUnlockBaseAddress, kCVPixelBufferIOSurfacePropertiesKey,
    kCVPixelBufferPixelFormatTypeKey, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
};
use parking_lot::Mutex;
use slopty_codec::PixelBuffer;
use slopty_core::WindowId;
use slopty_proto::screen::{CaptureTarget, CursorShape, DisplayInfo, WindowInfo};

use crate::geometry;
use crate::source::{
    AudioSink, AxError, CaptureConfig, CaptureError, CaptureSource, CapturedFrame, Crop, Rect,
    TargetWindow, Went, WindowState,
};
use crate::stream::host_now_us;

/// Blocks in the input strip: the low bits of the input count it spells.
pub const MARK_BITS: u32 = 16;
/// The desktop behind the window.
const DESKTOP: u8 = 40;
/// The window's page and its text.
const PAPER: u8 = 24;
const INK: u8 = 225;
/// A strip block that is a one or a zero, and the level between them a decoded one is read at.
const MARK_ON: u8 = 235;
const MARK_OFF: u8 = 20;
const MARK_THRESHOLD: u8 = 128;
/// Neutral chroma.
const GREY: u8 = 128;
/// Rows the page scrolls each frame.
const SCROLL_ROWS: usize = 3;
/// A character cell of the page, in pixels.
const CELL_W: usize = 8;
const CELL_H: usize = 16;
/// Pictures the canvas keeps at most. A picture the stream or the encoder still holds is never
/// drawn over; past this many held at once the beat drops its frame, as a capture whose pool is
/// exhausted does.
const SLOTS: usize = 12;
/// The beat when the display reports no refresh rate.
const FALLBACK_HZ: f64 = 60.0;

/// Inputs this process has taken, all canvases together.
static INPUTS: AtomicU64 = AtomicU64::new(0);

/// Take one input: every picture drawn from now on counts it. Returns the new count.
pub fn take_input() -> u64 {
    INPUTS.fetch_add(1, Ordering::AcqRel).wrapping_add(1)
}

/// Inputs taken so far.
#[must_use]
pub fn inputs_taken() -> u64 {
    INPUTS.load(Ordering::Acquire)
}

/// The input count a canvas picture spells in its strip, modulo `2^MARK_BITS`; `None` when the
/// buffer is too small to carry a strip or its planes cannot be read.
#[must_use]
pub fn inputs_shown(buffer: &CVPixelBuffer) -> Option<u16> {
    // SAFETY: CoreVideo rule: the base address may only be read between a lock and its unlock;
    // a read-only lock is what a reader that writes nothing takes.
    let locked = unsafe { CVPixelBufferLockBaseAddress(buffer, CVPixelBufferLockFlags::ReadOnly) };
    if locked != 0 {
        return None;
    }
    let shown = read_strip(buffer);
    // SAFETY: matches the lock above, with the same flags as CoreVideo requires.
    let _unlocked =
        unsafe { CVPixelBufferUnlockBaseAddress(buffer, CVPixelBufferLockFlags::ReadOnly) };
    shown
}

/// The strip's bits off a locked buffer.
fn read_strip(buffer: &CVPixelBuffer) -> Option<u16> {
    let width = CVPixelBufferGetWidthOfPlane(buffer, 0);
    let height = CVPixelBufferGetHeightOfPlane(buffer, 0);
    let stride = CVPixelBufferGetBytesPerRowOfPlane(buffer, 0);
    let base = NonNull::new(CVPixelBufferGetBaseAddressOfPlane(buffer, 0).cast::<u8>())?;
    let layout = Layout::new(width, height)?;
    let row = layout.strip_h.checked_div(2)?;
    let len = stride.checked_mul(height)?;
    // SAFETY: the buffer is locked, so its luma plane is mapped for `stride * height` bytes from
    // its base address and nothing writes it while the lock is held.
    let plane = unsafe { std::slice::from_raw_parts(base.as_ptr(), len) };
    let line = plane.chunks_exact(stride).nth(row)?;
    let mut shown = 0_u16;
    for bit in 0..MARK_BITS {
        let x = layout.mark_centre(bit)?;
        if *line.get(x)? > MARK_THRESHOLD {
            shown |= 1_u16.checked_shl(bit)?;
        }
    }
    Some(shown)
}

/// Where things are on a picture of one size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Layout {
    width: usize,
    height: usize,
    /// Rows of the input strip, from the top.
    strip_h: usize,
    /// Width of one strip block.
    mark_w: usize,
    /// The scrolling window: left edge, top edge, width, height.
    page: (usize, usize, usize, usize),
}

impl Layout {
    fn new(width: usize, height: usize) -> Option<Self> {
        let strip_h = height.checked_div(24)?.max(8);
        let mark_w = width.checked_div(usize::try_from(MARK_BITS).ok()?)?;
        if mark_w == 0 || strip_h >= height {
            return None;
        }
        let page_w = width.checked_div(2)?;
        let page_h = height.checked_div(2)?;
        let page = (width.checked_div(4)?, height.checked_div(4)?.max(strip_h), page_w, page_h);
        Some(Self { width, height, strip_h, mark_w, page })
    }

    /// The column at the middle of strip block `bit`.
    fn mark_centre(&self, bit: u32) -> Option<usize> {
        let index = usize::try_from(bit).ok()?;
        self.mark_w.checked_mul(index)?.checked_add(self.mark_w.checked_div(2)?)
    }
}

/// A picture the canvas draws into, and whether its unchanging parts are drawn yet.
struct Slot {
    buffer: PixelBuffer,
    /// Desktop and chroma are drawn; a new buffer starts with neither.
    ready: bool,
}

/// What the beat draws with.
struct Painter {
    layout: Layout,
    /// The page's text, twice the window's height, scrolled through a frame at a time.
    text: Vec<u8>,
    slots: Vec<Slot>,
    frame: usize,
}

impl Painter {
    fn new(width: u32, height: u32) -> Option<Self> {
        let layout = Layout::new(usize::try_from(width).ok()?, usize::try_from(height).ok()?)?;
        let (_, _, page_w, page_h) = layout.page;
        Some(Self {
            layout,
            text: text(page_w, page_h.checked_mul(2)?),
            slots: Vec::new(),
            frame: 0,
        })
    }

    /// Draw the next picture, counting `inputs`; `None` when every picture is still held.
    fn paint(&mut self, inputs: u64) -> Option<PixelBuffer> {
        let slot = self.free_slot()?;
        let slot = self.slots.get_mut(slot)?;
        let buffer = slot.buffer.as_cv().retain();
        // SAFETY: CoreVideo rule: the planes are written only between a lock and its unlock.
        let locked =
            unsafe { CVPixelBufferLockBaseAddress(&buffer, CVPixelBufferLockFlags::empty()) };
        if locked != 0 {
            return None;
        }
        if !slot.ready {
            fill_plane(&buffer, 0, DESKTOP);
            fill_plane(&buffer, 1, GREY);
            slot.ready = true;
        }
        let drawn = draw(&buffer, &self.layout, &self.text, self.frame, inputs);
        // SAFETY: matches the lock above.
        let _unlocked =
            unsafe { CVPixelBufferUnlockBaseAddress(&buffer, CVPixelBufferLockFlags::empty()) };
        self.frame = self.frame.wrapping_add(1);
        drawn.then(|| PixelBuffer::from_retained(buffer))
    }

    /// A picture nobody else holds, made when there is none and room for one.
    fn free_slot(&mut self) -> Option<usize> {
        // A reference beyond the canvas's own is the stream's held frame or the encoder's.
        if let Some(free) = self.slots.iter().position(|s| s.buffer.as_cv().retain_count() == 1) {
            return Some(free);
        }
        if self.slots.len() >= SLOTS {
            return None;
        }
        let buffer = PixelBuffer::from_retained(surface(self.layout.width, self.layout.height)?);
        self.slots.push(Slot { buffer, ready: false });
        Some(self.slots.len().saturating_sub(1))
    }
}

/// Text on a page `width` wide and `rows` tall: lines of 5×7 glyphs in 8×16 cells, each line a
/// random length, from a fixed seed so every run draws the same page.
fn text(width: usize, rows: usize) -> Vec<u8> {
    let mut page = vec![PAPER; width.saturating_mul(rows)];
    let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let columns = width.checked_div(CELL_W).unwrap_or(0);
    for band in page.chunks_mut(width.saturating_mul(CELL_H)) {
        let length = usize::try_from(next() % 97).unwrap_or(0).saturating_mul(columns) / 97;
        for cell in 0..length.min(columns) {
            let glyph = next();
            // A space now and then, so the line reads as words.
            if glyph.is_multiple_of(7) {
                continue;
            }
            let x0 = cell.saturating_mul(CELL_W).saturating_add(1);
            for (row, pixels) in band.chunks_exact_mut(width).enumerate().skip(4).take(7) {
                for col in 0..5_usize {
                    let bit = row.saturating_sub(4).saturating_mul(5).saturating_add(col);
                    if glyph.checked_shr(u32::try_from(bit).unwrap_or(63)).unwrap_or(0) & 1 == 1
                        && let Some(p) = pixels.get_mut(x0.saturating_add(col))
                    {
                        *p = INK;
                    }
                }
            }
        }
    }
    page
}

/// An `IOSurface`-backed full-range NV12 buffer, as ScreenCaptureKit delivers.
fn surface(width: usize, height: usize) -> Option<CFRetained<CVPixelBuffer>> {
    let format = CFNumber::new_i32(i32::from_ne_bytes(
        kCVPixelFormatType_420YpCbCr8BiPlanarFullRange.to_ne_bytes(),
    ));
    let attrs = CFDictionary::<CFString, CFType>::from_slices(
        &[
            // SAFETY: framework-provided constant string.
            unsafe { kCVPixelBufferIOSurfacePropertiesKey },
            // SAFETY: framework-provided constant string.
            unsafe { kCVPixelBufferPixelFormatTypeKey },
        ],
        &[&CFDictionary::<CFString, CFType>::empty(), &format],
    );
    let mut raw: *mut CVPixelBuffer = ptr::null_mut();
    // SAFETY: CoreVideo rule: a valid out-pointer and an attributes dictionary of CFString keys.
    let status = unsafe {
        CVPixelBufferCreate(
            None,
            width,
            height,
            kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
            Some(attrs.as_opaque()),
            NonNull::from(&mut raw),
        )
    };
    if status != 0 {
        tracing::warn!(status, width, height, "canvas: CVPixelBufferCreate failed");
        return None;
    }
    // SAFETY: `CVPixelBufferCreate` returned a +1 reference, which this takes over.
    Some(unsafe { CFRetained::from_raw(NonNull::new(raw)?) })
}

/// Run `write` over plane `plane` of a buffer the caller holds the write lock of, as its bytes
/// and its row stride.
fn with_plane<R>(
    buffer: &CVPixelBuffer,
    plane: usize,
    write: impl FnOnce(&mut [u8], usize) -> R,
) -> Option<R> {
    let stride = CVPixelBufferGetBytesPerRowOfPlane(buffer, plane);
    let rows = CVPixelBufferGetHeightOfPlane(buffer, plane);
    let base = NonNull::new(CVPixelBufferGetBaseAddressOfPlane(buffer, plane).cast::<u8>())?;
    let len = stride.checked_mul(rows)?;
    // SAFETY: the caller holds the buffer's write lock, so the plane is mapped for
    // `stride * rows` bytes from its base address, and only this beat writes it (a picture
    // another holder retains is never drawn into).
    let bytes = unsafe { std::slice::from_raw_parts_mut(base.as_ptr(), len) };
    Some(write(bytes, stride))
}

fn fill_plane(buffer: &CVPixelBuffer, plane: usize, value: u8) {
    let _filled = with_plane(buffer, plane, |bytes, _stride| bytes.fill(value));
}

/// The page scrolled to `frame`, and the strip spelling `inputs`. `false` when the planes
/// could not be read.
fn draw(buffer: &CVPixelBuffer, layout: &Layout, text: &[u8], frame: usize, inputs: u64) -> bool {
    with_plane(buffer, 0, |luma, stride| draw_luma(luma, stride, layout, text, frame, inputs))
        .is_some()
}

fn draw_luma(
    luma: &mut [u8],
    stride: usize,
    layout: &Layout,
    text: &[u8],
    frame: usize,
    inputs: u64,
) {
    let (x0, y0, page_w, page_h) = layout.page;
    let text_rows = text.len().checked_div(page_w).unwrap_or(0);
    let scrolled = frame.saturating_mul(SCROLL_ROWS);
    for (y, row) in luma.chunks_exact_mut(stride).enumerate().skip(y0).take(page_h) {
        let from = y.saturating_sub(y0).wrapping_add(scrolled).checked_rem(text_rows).unwrap_or(0);
        let source = from.checked_mul(page_w).and_then(|at| text.get(at..at.checked_add(page_w)?));
        if let (Some(source), Some(dest)) = (source, row.get_mut(x0..x0.saturating_add(page_w))) {
            dest.copy_from_slice(source);
        }
    }
    for row in luma.chunks_exact_mut(stride).take(layout.strip_h) {
        for bit in 0..MARK_BITS {
            let on = inputs.checked_shr(bit).unwrap_or(0) & 1 == 1;
            let start = layout.mark_w.saturating_mul(usize::try_from(bit).unwrap_or(0));
            if let Some(block) = row.get_mut(start..start.saturating_add(layout.mark_w)) {
                block.fill(if on { MARK_ON } else { MARK_OFF });
            }
        }
    }
}

/// The capture platform that draws instead of capturing.
#[derive(Clone, Copy, Debug)]
pub enum Canvas {}

/// A display the canvas stands in for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CanvasTarget {
    display: u32,
    native: (u32, u32),
    scale: f32,
}

/// A running canvas: its beat and the queue it runs on.
pub struct CanvasStream {
    beat: Arc<Beat>,
    queue: DispatchRetained<DispatchQueue>,
}

impl std::fmt::Debug for CanvasStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CanvasStream")
            .field("period_us", &self.beat.period_us)
            .finish_non_exhaustive()
    }
}

/// What the beat shares with the stream that owns it.
struct Beat {
    stopped: AtomicBool,
    painter: Mutex<Option<Painter>>,
    period_us: u64,
    sink: Box<dyn Fn(CapturedFrame) + Send + Sync>,
}

impl Beat {
    /// Draw and deliver one picture, then schedule the next beat after `due_us`.
    fn tick(self: Arc<Self>, queue: &DispatchRetained<DispatchQueue>, due_us: u64) {
        if self.stopped.load(Ordering::Acquire) {
            return;
        }
        // The picture exists from here on, as a display time does for ScreenCaptureKit's.
        let shown_us = host_now_us();
        let picture = self.painter.lock().as_mut().and_then(|p| p.paint(inputs_taken()));
        if let Some(image) = picture {
            let now_us = host_now_us();
            let age_us = now_us.saturating_sub(shown_us);
            (self.sink)(CapturedFrame {
                image,
                capture_ts_us: shown_us,
                display_ts_us: Some(shown_us),
                age_us,
                latency_us: age_us,
            });
        }
        // Stay in phase, skipping the beats a stall ran past rather than bunching them.
        let now_us = host_now_us();
        let mut next = due_us.saturating_add(self.period_us);
        if next <= now_us {
            let behind = now_us.saturating_sub(next);
            let skipped = behind.checked_div(self.period_us).unwrap_or(0).saturating_add(1);
            next = next.saturating_add(skipped.saturating_mul(self.period_us));
        }
        let wait_ns = next.saturating_sub(now_us).saturating_mul(1_000);
        let when = DispatchTime::NOW.time(i64::try_from(wait_ns).unwrap_or(i64::MAX));
        let again = queue.clone();
        let _scheduled = queue.after(when, move || self.tick(&again, next));
    }
}

impl CaptureSource for Canvas {
    type Content = Vec<DisplayInfo>;
    type HideWatch = ();
    type Image = PixelBuffer;
    type Stream = CanvasStream;
    type Target = CanvasTarget;

    fn can_capture() -> bool {
        true
    }

    fn enumerate(done: impl FnOnce(Result<Vec<DisplayInfo>, CaptureError>) + Send + 'static) {
        done(Ok(geometry::active_displays()));
    }

    fn windows(_content: &Vec<DisplayInfo>) -> Vec<WindowInfo> {
        Vec::new()
    }

    fn displays(content: &Vec<DisplayInfo>) -> Vec<DisplayInfo> {
        content.clone()
    }

    fn resolve(
        content: &Vec<DisplayInfo>,
        kind: CaptureTarget,
    ) -> Result<CanvasTarget, CaptureError> {
        let CaptureTarget::Display(id) = kind else { return Err(CaptureError::NotFound(kind)) };
        let display = content.iter().find(|d| d.id == id).ok_or(CaptureError::NotFound(kind))?;
        let px = |points: f32| {
            #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped")]
            let v =
                (f64::from(points) * f64::from(display.scale)).round().clamp(2.0, 16_384.0) as u32;
            v.next_multiple_of(2)
        };
        Ok(CanvasTarget {
            display: id.0,
            native: (px(display.w), px(display.h)),
            scale: display.scale,
        })
    }

    fn resolve_crop(
        _content: &Vec<DisplayInfo>,
        id: WindowId,
    ) -> Result<Option<CanvasTarget>, CaptureError> {
        Err(CaptureError::NotFound(CaptureTarget::Window(id)))
    }

    fn crop(_target: &CanvasTarget) -> Option<Crop> {
        None
    }

    fn pixel_size(target: &CanvasTarget) -> (u32, u32) {
        target.native
    }

    fn point_scale(target: &CanvasTarget) -> f32 {
        target.scale
    }

    fn start(
        target: &CanvasTarget,
        config: &CaptureConfig,
        sink: impl Fn(CapturedFrame) + Send + Sync + 'static,
        _audio: Option<AudioSink>,
        _on_stop: impl Fn(CaptureError) + Send + Sync + 'static,
        done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
    ) -> Result<CanvasStream, CaptureError> {
        let hz = if config.fps == 0 {
            geometry::display_refresh_hz(target.display).unwrap_or(FALLBACK_HZ)
        } else {
            f64::from(config.fps)
        };
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped")]
        let period_us = (1e6 / hz.clamp(1.0, 240.0)).round() as u64;
        let beat = Arc::new(Beat {
            stopped: AtomicBool::new(false),
            painter: Mutex::new(Painter::new(config.width, config.height)),
            period_us,
            sink: Box::new(sink),
        });
        // User-interactive, as the capture queue it stands in for is.
        let interactive = DispatchQueueAttr::with_qos_class(
            DispatchQueueAttr::SERIAL,
            DispatchQoS::UserInteractive,
            0,
        );
        let queue = DispatchQueue::new("io.slopty.canvas", Some(&interactive));
        let first = Arc::clone(&beat);
        let again = queue.clone();
        queue.exec_async(move || {
            done(Ok(()));
            first.tick(&again, host_now_us());
        });
        Ok(CanvasStream { beat, queue })
    }

    fn update(
        stream: &CanvasStream,
        config: &CaptureConfig,
        done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
    ) {
        let beat = Arc::clone(&stream.beat);
        let (width, height) = (config.width, config.height);
        stream.queue.exec_async(move || {
            *beat.painter.lock() = Painter::new(width, height);
            done(Ok(()));
        });
    }

    fn retarget(
        stream: &CanvasStream,
        _target: &CanvasTarget,
        done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
    ) {
        stream.queue.exec_async(move || done(Ok(())));
    }

    fn stop(stream: &CanvasStream, done: impl FnOnce(Result<(), CaptureError>) + Send + 'static) {
        stream.beat.stopped.store(true, Ordering::Release);
        // Behind any beat already running, so no picture follows the answer.
        stream.queue.exec_async(move || done(Ok(())));
    }

    fn now_us() -> u64 {
        host_now_us()
    }

    fn target_bounds(target: CaptureTarget) -> Option<Rect> {
        geometry::target_bounds(target)
    }

    fn refresh_hz(target: CaptureTarget) -> Option<f64> {
        geometry::target_refresh_hz(target)
    }

    fn window_state(_id: WindowId) -> Option<WindowState> {
        None
    }

    fn window_bounds(_id: WindowId) -> Option<Rect> {
        None
    }

    fn window_owner(_id: WindowId) -> Option<i32> {
        None
    }

    fn window_on_screen(_id: WindowId) -> bool {
        false
    }

    fn window_title(_id: WindowId) -> Option<String> {
        None
    }

    fn occluded(_id: WindowId, _bounds: &Rect, _owner: i32) -> bool {
        false
    }

    fn display_enclosing(rect: &Rect) -> Option<u32> {
        geometry::display_enclosing(rect)
    }

    fn display_bounds(id: u32) -> Rect {
        geometry::display_bounds(id)
    }

    fn resize_window(
        _pid: i32,
        _target: &TargetWindow,
        _width: f64,
        _height: f64,
    ) -> Result<(), AxError> {
        Err(AxError::Unsupported)
    }

    fn watch_hides(
        _pid: i32,
        _target: TargetWindow,
        _on_went: impl Fn(Went) + Send + Sync + 'static,
    ) -> Result<(), AxError> {
        Err(AxError::Unsupported)
    }

    fn watch_targeted((): &()) -> bool {
        false
    }

    fn pointer_moves() -> u32 {
        0
    }

    fn pointer_location() -> (f64, f64) {
        (0.0, 0.0)
    }

    fn cursor_shape(_scale: u8) -> Option<CursorShape> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A picture spells the input count in its strip, and the reader gets it back; a picture too
    /// small for a strip reads as none.
    #[test]
    fn the_strip_reads_back_what_was_drawn() {
        let mut painter = Painter::new(640, 360).unwrap();
        for inputs in [0_u64, 1, 0x5a5a, 0xffff, 0x1_0003] {
            let picture = painter.paint(inputs).unwrap();
            assert_eq!(
                inputs_shown(picture.as_cv()),
                Some(u16::try_from(inputs & 0xffff).unwrap())
            );
        }
        let tiny = surface(8, 8).unwrap();
        assert_eq!(inputs_shown(&tiny), None, "8 px has no room for 16 blocks");
    }

    /// A picture something else still holds is never drawn over: the next beat takes another,
    /// and once all are held the beat drops its frame instead of tearing one.
    #[test]
    fn a_held_picture_is_never_drawn_over() {
        let mut painter = Painter::new(320, 180).unwrap();
        let first = painter.paint(1).unwrap();
        let second = painter.paint(2).unwrap();
        assert!(!ptr::eq(first.as_cv(), second.as_cv()), "the held picture was reused");
        assert_eq!(inputs_shown(first.as_cv()), Some(1), "drawing the second changed the first");
        let mut held = vec![first, second];
        while let Some(p) = painter.paint(3) {
            held.push(p);
        }
        assert_eq!(held.len(), SLOTS);
        drop(held.pop());
        assert!(painter.paint(4).is_some(), "a picture let go of is drawn into again");
    }

    /// The scrolling page changes every frame, as a scrolling window does, so the encoder sees
    /// motion on every beat rather than a still desktop.
    #[test]
    fn the_page_moves_every_frame() {
        let mut painter = Painter::new(320, 180).unwrap();
        let row = |p: &CVPixelBuffer| {
            // SAFETY: CoreVideo rule: read between a lock and its unlock.
            let locked =
                unsafe { CVPixelBufferLockBaseAddress(p, CVPixelBufferLockFlags::empty()) };
            assert_eq!(locked, 0);
            let (bytes, stride) =
                with_plane(p, 0, |plane, stride| (plane.to_vec(), stride)).unwrap();
            // SAFETY: matches the lock above.
            let _unlocked =
                unsafe { CVPixelBufferUnlockBaseAddress(p, CVPixelBufferLockFlags::empty()) };
            bytes.chunks_exact(stride).skip(45).take(90).flatten().copied().collect::<Vec<u8>>()
        };
        let a = painter.paint(0).unwrap();
        let before = row(a.as_cv());
        drop(a);
        let b = painter.paint(0).unwrap();
        assert_ne!(before, row(b.as_cv()), "the page did not scroll");
    }
}

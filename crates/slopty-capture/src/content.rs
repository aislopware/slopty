//! Shareable content: what can be captured.

use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2_foundation::NSError;
use objc2_screen_capture_kit::{SCDisplay, SCShareableContent, SCWindow};
use parking_lot::Mutex;
use slopty_core::WindowId;
use slopty_proto::screen::{DisplayInfo, WindowInfo};

use crate::CaptureError;

/// A snapshot of the windows and displays on the host, with the ScreenCaptureKit objects
/// needed to open a stream on any of them.
pub struct Shareable {
    inner: Retained<SCShareableContent>,
}

// SAFETY: `SCShareableContent`, `SCWindow` and `SCDisplay` are immutable value objects that
// ScreenCaptureKit itself hands to arbitrary queues; nothing here mutates them.
#[expect(clippy::non_send_fields_in_send_ty, reason = "immutable SCK value objects")]
unsafe impl Send for Shareable {}
// SAFETY: as above; all accessors are reads.
unsafe impl Sync for Shareable {}

impl std::fmt::Debug for Shareable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shareable")
            .field("windows", &self.windows().len())
            .field("displays", &self.displays().len())
            .finish()
    }
}

impl Shareable {
    /// Windows that are on screen or minimised, excluding desktop furniture.
    #[must_use]
    pub fn windows(&self) -> Vec<WindowInfo> {
        // SAFETY: valid object; the array is a snapshot.
        let windows = unsafe { self.inner.windows() };
        windows.iter().filter_map(|w| window_info(&w)).collect()
    }

    /// Attached displays.
    #[must_use]
    pub fn displays(&self) -> Vec<DisplayInfo> {
        // SAFETY: valid object; the array is a snapshot.
        let displays = unsafe { self.inner.displays() };
        displays.iter().map(|d| display_info(&d)).collect()
    }

    pub(crate) fn window(&self, id: WindowId) -> Option<Retained<SCWindow>> {
        // SAFETY: valid object.
        let windows = unsafe { self.inner.windows() };
        // SAFETY: `windowID` is a plain getter.
        windows.iter().find(|w| unsafe { w.windowID() } == id.0)
    }

    pub(crate) fn display(&self, id: u32) -> Option<Retained<SCDisplay>> {
        // SAFETY: valid object.
        let displays = unsafe { self.inner.displays() };
        // SAFETY: `displayID` is a plain getter.
        displays.iter().find(|d| unsafe { d.displayID() } == id)
    }
}

fn window_info(w: &SCWindow) -> Option<WindowInfo> {
    // SAFETY: plain property read on a valid object.
    let layer = unsafe { w.windowLayer() };
    if layer != 0 {
        // Menu bar, dock, desktop pictures and overlays live on other layers.
        return None;
    }
    // SAFETY: plain property read on a valid object.
    let frame = unsafe { w.frame() };
    if frame.size.width < 8.0 || frame.size.height < 8.0 {
        return None;
    }
    // SAFETY: plain property read on a valid object.
    let app = unsafe { w.owningApplication() };
    let (app_name, bundle_id) = app.map_or_else(
        || (String::new(), None),
        |a| {
            // SAFETY: plain getters on a valid object.
            let name = unsafe { a.applicationName() }.to_string();
            // SAFETY: as above.
            let bundle = unsafe { a.bundleIdentifier() }.to_string();
            (name, (!bundle.is_empty()).then_some(bundle))
        },
    );
    // SAFETY: plain property read on a valid object.
    let title = unsafe { w.title() }.map(|t| t.to_string()).unwrap_or_default();
    // SAFETY: plain property read on a valid object.
    let id = unsafe { w.windowID() };
    // SAFETY: plain property read on a valid object.
    let on_screen = unsafe { w.isOnScreen() };
    Some(WindowInfo {
        id: WindowId(id),
        app: app_name,
        bundle_id,
        title,
        x: to_f32(frame.origin.x),
        y: to_f32(frame.origin.y),
        w: to_f32(frame.size.width),
        h: to_f32(frame.size.height),
        display: 0,
        on_screen,
    })
}

fn display_info(d: &SCDisplay) -> DisplayInfo {
    // SAFETY: plain property read on a valid object.
    let id = unsafe { d.displayID() };
    // SAFETY: plain property read on a valid object.
    let frame = unsafe { d.frame() };
    let pixels_wide = objc2_core_graphics::CGDisplayPixelsWide(id);
    let scale = if frame.size.width > 0.0 {
        to_f32(f64::from(u32::try_from(pixels_wide).unwrap_or(u32::MAX)) / frame.size.width)
    } else {
        1.0
    };
    let hz = objc2_core_graphics::CGDisplayCopyDisplayMode(id)
        .map_or(60.0, |mode| objc2_core_graphics::CGDisplayMode::refresh_rate(Some(&mode)));
    DisplayInfo {
        id,
        w: to_f32(frame.size.width),
        h: to_f32(frame.size.height),
        scale,
        hz: to_f32(if hz > 0.0 { hz } else { 60.0 }),
        hdr: false,
    }
}

#[expect(clippy::cast_possible_truncation, reason = "geometry in points fits f32 exactly")]
const fn to_f32(v: f64) -> f32 {
    v as f32
}

/// Ask ScreenCaptureKit for the current shareable content. `done` runs on a ScreenCaptureKit
/// queue. The first call in a process triggers the Screen Recording permission prompt.
pub fn enumerate(done: impl FnOnce(Result<Shareable, CaptureError>) + Send + 'static) {
    let done = Mutex::new(Some(done));
    let block = RcBlock::new(move |content: *mut SCShareableContent, error: *mut NSError| {
        let Some(done) = done.lock().take() else { return };
        // SAFETY: ScreenCaptureKit passes a live, autoreleased object (or NULL);
        // retaining it gives us our own reference.
        let content = unsafe { Retained::retain(content) };
        let result = match (content, NonNull::new(error)) {
            (Some(inner), _) => Ok(Shareable { inner }),
            // SAFETY: a live error object for the callback's duration.
            (None, Some(error)) => Err(CaptureError::from_ns(unsafe { error.as_ref() })),
            (None, None) => Err(CaptureError::Stopped("no content, no error".to_owned())),
        };
        done(result);
    });
    // SAFETY: the block is copied by the framework before this returns; it captures only
    // `Send` data.
    unsafe {
        SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(
            true, false, &block,
        );
    }
}

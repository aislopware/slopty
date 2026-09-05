//! Cheap WindowServer queries the cursor channel needs: pointer position and the current
//! bounds of a window or display, without re-enumerating shareable content.

use std::ffi::c_void;
use std::ptr;

use objc2_core_foundation::{
    CFArray, CFDictionary, CFNumber, CFRetained, CFString, CFType, CGRect,
};
use objc2_core_graphics::{
    CGDisplayBounds, CGEvent, CGPreflightScreenCaptureAccess,
    CGRectMakeWithDictionaryRepresentation, CGWindowListCreateDescriptionFromArray,
    kCGWindowBounds, kCGWindowOwnerPID,
};
use slopty_core::WindowId;
use slopty_proto::screen::CaptureTarget;

/// A rectangle in global display points (origin top-left, y down, as `CGWindow` reports).
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Rect {
    /// Left edge.
    pub x: f64,
    /// Top edge.
    pub y: f64,
    /// Width.
    pub w: f64,
    /// Height.
    pub h: f64,
}

impl Rect {
    const fn from_cg(r: CGRect) -> Self {
        Self { x: r.origin.x, y: r.origin.y, w: r.size.width, h: r.size.height }
    }

    /// Whether `(x, y)` lies inside.
    #[must_use]
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }
}

/// The pointer's position in global display points.
#[must_use]
pub fn pointer_location() -> (f64, f64) {
    // `CGEventCreate(NULL)` snapshots the current event state, including the pointer.
    let event = CGEvent::new(None);
    let p = CGEvent::location(event.as_deref());
    (p.x, p.y)
}

/// Current bounds of a capture target, or `None` when a window is gone.
#[must_use]
pub fn target_bounds(target: CaptureTarget) -> Option<Rect> {
    match target {
        CaptureTarget::Window(id) => window_bounds(id),
        CaptureTarget::Display(id) => Some(Rect::from_cg(CGDisplayBounds(id))),
    }
}

/// The window-list description of one window (one WindowServer round trip).
fn window_description(id: WindowId) -> Option<CFRetained<CFDictionary<CFString, CFType>>> {
    // `CGWindowListCreateDescriptionFromArray` wants the raw `CGWindowID`s *as the array
    // values* (an array built with NULL callbacks), not boxed `CFNumber`s: with numbers it
    // returns an empty array (observed macOS 26.5).
    let mut values: [*const c_void; 1] = [id.0 as usize as *const c_void];
    // SAFETY: `values` outlives the call, `num_values` is its exact length and NULL callbacks
    // mean the array stores the pointer-sized integers verbatim, which is what the window
    // list API documents for its id array.
    let ids = unsafe { CFArray::new(None, values.as_mut_ptr(), 1, ptr::null()) }?;
    // SAFETY: the array holds `CGWindowID`s, which is what the function documents.
    let descriptions = unsafe { CGWindowListCreateDescriptionFromArray(Some(&ids)) }?;
    // SAFETY: `CGWindowListCreateDescriptionFromArray` documents an array of dictionaries keyed
    // by the `kCGWindow*` strings.
    let descriptions: CFRetained<CFArray<CFDictionary<CFString, CFType>>> =
        unsafe { CFRetained::cast_unchecked(descriptions) };
    descriptions.get(0)
}

/// Bounds of one window from the window list (cheap: one WindowServer round trip).
#[must_use]
pub fn window_bounds(id: WindowId) -> Option<Rect> {
    let description = window_description(id)?;
    // SAFETY: framework-provided constant string.
    let key: &CFString = unsafe { kCGWindowBounds };
    let bounds = description.get(key)?;
    let bounds: CFRetained<CFDictionary> = bounds.downcast().ok()?;
    let mut rect = CGRect::default();
    // SAFETY: `bounds` is the dictionary form of a rect; `rect` is a valid out pointer.
    let ok =
        unsafe { CGRectMakeWithDictionaryRepresentation(Some(&bounds), ptr::from_mut(&mut rect)) };
    ok.then(|| Rect::from_cg(rect))
}

/// Process id of the application that owns a window.
#[must_use]
pub fn window_owner_pid(id: WindowId) -> Option<i32> {
    let description = window_description(id)?;
    // SAFETY: framework-provided constant string.
    let key: &CFString = unsafe { kCGWindowOwnerPID };
    let pid = description.get(key)?;
    let pid: CFRetained<CFNumber> = pid.downcast().ok()?;
    pid.as_i32()
}

/// Whether this process has Screen Recording (TCC) access; without it ScreenCaptureKit
/// returns no windows and every stream fails to start. Answers without prompting.
#[must_use]
pub fn can_capture() -> bool {
    CGPreflightScreenCaptureAccess()
}

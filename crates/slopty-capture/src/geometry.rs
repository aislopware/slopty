//! Cheap WindowServer queries the cursor channel needs: pointer position and the current
//! bounds of a window or display, without re-enumerating shareable content.

use std::ptr;

use objc2_core_foundation::{
    CFArray, CFDictionary, CFNumber, CFRetained, CFString, CFType, CGRect,
};
use objc2_core_graphics::{
    CGDisplayBounds, CGEvent, CGRectMakeWithDictionaryRepresentation,
    CGWindowListCreateDescriptionFromArray, kCGWindowBounds,
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

/// Bounds of one window from the window list (cheap: one WindowServer round trip).
#[must_use]
pub fn window_bounds(id: WindowId) -> Option<Rect> {
    let number = CFNumber::new_i64(i64::from(id.0));
    let ids: CFRetained<CFArray<CFNumber>> = CFArray::from_retained_objects(&[number]);
    // SAFETY: the array holds `CFNumber` window ids, which is what the function documents.
    let descriptions = unsafe { CGWindowListCreateDescriptionFromArray(Some(ids.as_opaque())) }?;
    // SAFETY: `CGWindowListCreateDescriptionFromArray` documents an array of dictionaries keyed
    // by the `kCGWindow*` strings.
    let descriptions: CFRetained<CFArray<CFDictionary<CFString, CFType>>> =
        unsafe { CFRetained::cast_unchecked(descriptions) };
    let description = descriptions.get(0)?;
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

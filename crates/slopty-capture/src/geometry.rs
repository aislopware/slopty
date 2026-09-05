//! Cheap WindowServer queries the cursor channel needs: pointer position and the current
//! bounds of a window or display, without re-enumerating shareable content.

use std::ffi::c_void;
use std::ptr;

use objc2_core_foundation::{
    CFArray, CFDictionary, CFNumber, CFRetained, CFString, CFType, CGRect,
};
use objc2_core_graphics::{
    CGDisplayBounds, CGEvent, CGGetDisplaysWithRect, CGPreflightScreenCaptureAccess,
    CGRectMakeWithDictionaryRepresentation, CGWindowListCopyWindowInfo,
    CGWindowListCreateDescriptionFromArray, CGWindowListOption, kCGWindowAlpha, kCGWindowBounds,
    kCGWindowLayer, kCGWindowOwnerPID,
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

    /// Whether `other` lies entirely inside.
    #[must_use]
    pub fn encloses(&self, other: &Self) -> bool {
        other.x >= self.x
            && other.y >= self.y
            && other.x + other.w <= self.x + self.w
            && other.y + other.h <= self.y + self.h
    }

    /// Whether the interiors overlap (touching edges do not count).
    #[must_use]
    pub fn overlaps(&self, other: &Self) -> bool {
        self.x < other.x + other.w
            && other.x < self.x + self.w
            && self.y < other.y + other.h
            && other.y < self.y + self.h
    }
}

/// The part of a display a stream samples: a window's frame in the display's own point space
/// (origin at the display's top-left corner), what `SCStreamConfiguration.sourceRect` wants.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Crop {
    /// Left edge, display points.
    pub x: f64,
    /// Top edge, display points.
    pub y: f64,
    /// Width, display points.
    pub w: f64,
    /// Height, display points.
    pub h: f64,
}

/// Where a window sits on a display, for the display-crop capture path.
///
/// Returns the crop in the display's point space and the output size in pixels at the
/// display's `scale`; `None` when the window is not entirely on that display (partly
/// off-screen, straddling two displays): a crop would show a slice of the desktop where the
/// window continues, so those keep the window filter.
#[must_use]
pub fn crop_for(window: &Rect, display: &Rect, scale: f64) -> Option<(Crop, (u32, u32))> {
    if window.w <= 0.0 || window.h <= 0.0 || scale <= 0.0 || !display.encloses(window) {
        return None;
    }
    let crop = Crop { x: window.x - display.x, y: window.y - display.y, w: window.w, h: window.h };
    let even = |points: f64| -> u32 {
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped")]
        let px = (points * scale).round().clamp(2.0, 16_384.0) as u32;
        px.next_multiple_of(2)
    };
    Some((crop, (even(window.w), even(window.h))))
}

/// Bounds of a display in global points.
#[must_use]
pub fn display_bounds(id: u32) -> Rect {
    Rect::from_cg(CGDisplayBounds(id))
}

/// The display whose bounds enclose `rect` entirely, if there is one.
#[must_use]
pub fn display_enclosing(rect: &Rect) -> Option<u32> {
    let cg = CGRect {
        origin: objc2_core_foundation::CGPoint { x: rect.x, y: rect.y },
        size: objc2_core_foundation::CGSize { width: rect.w, height: rect.h },
    };
    let mut ids = [0_u32; 8];
    let mut count = 0_u32;
    // SAFETY: `ids` has room for `max_displays` entries and `count` is a valid out pointer.
    let err = unsafe { CGGetDisplaysWithRect(cg, 8, ids.as_mut_ptr(), &raw mut count) };
    if err != objc2_core_graphics::CGError::Success {
        return None;
    }
    let n = usize::try_from(count).unwrap_or(0).min(ids.len());
    ids.iter().take(n).copied().find(|&id| Rect::from_cg(CGDisplayBounds(id)).encloses(rect))
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

/// Window levels that count as occluders: normal windows (0) up to modal panels (8).
/// Above that live the Dock (20, whose window is a transparent full-screen hit region),
/// the menu bar (24), status items (25), popup menus, overlays and the cursor: none of
/// them is another app's window sitting on top of this one.
const OCCLUDER_LAYERS: std::ops::RangeInclusive<i32> = 0..=8;

/// Whether an on-screen window of another process overlaps `id` from above.
///
/// A display crop of an occluded window would show the occluder, so that case keeps the
/// window filter. Windows of the owner itself (menus, sheets, popovers) are its content and
/// do not count.
#[must_use]
pub fn occluded(id: WindowId, bounds: &Rect, owner_pid: i32) -> bool {
    let Some(list) =
        CGWindowListCopyWindowInfo(CGWindowListOption::OptionOnScreenAboveWindow, id.0)
    else {
        return false;
    };
    // SAFETY: the list is documented as dictionaries keyed by the `kCGWindow*` strings.
    let list: CFRetained<CFArray<CFDictionary<CFString, CFType>>> =
        unsafe { CFRetained::cast_unchecked(list) };
    // SAFETY: framework-provided constant strings.
    let (bounds_key, pid_key, alpha_key, layer_key): (&CFString, &CFString, &CFString, &CFString) =
        unsafe { (kCGWindowBounds, kCGWindowOwnerPID, kCGWindowAlpha, kCGWindowLayer) };
    list.iter().any(|d| {
        let pid =
            d.get(pid_key).and_then(|v| v.downcast::<CFNumber>().ok()).and_then(|n| n.as_i32());
        if pid.is_none_or(|p| p == owner_pid) {
            return false;
        }
        let layer =
            d.get(layer_key).and_then(|v| v.downcast::<CFNumber>().ok()).and_then(|n| n.as_i32());
        if !layer.is_some_and(|l| OCCLUDER_LAYERS.contains(&l)) {
            return false;
        }
        let alpha = d
            .get(alpha_key)
            .and_then(|v| v.downcast::<CFNumber>().ok())
            .and_then(|n| n.as_f64())
            .unwrap_or(1.0);
        if alpha <= 0.0 {
            return false;
        }
        let Some(rect) = d.get(bounds_key).and_then(|v| v.downcast::<CFDictionary>().ok()) else {
            return false;
        };
        let mut cg = CGRect::default();
        // SAFETY: `rect` is the dictionary form of a rect; `cg` is a valid out pointer.
        let ok =
            unsafe { CGRectMakeWithDictionaryRepresentation(Some(&rect), ptr::from_mut(&mut cg)) };
        ok && Rect::from_cg(cg).overlaps(bounds)
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    const DISPLAY: Rect = Rect { x: 0.0, y: 0.0, w: 1920.0, h: 1080.0 };
    /// A second display to the right, as CoreGraphics lays them out (global points).
    const RIGHT: Rect = Rect { x: 1920.0, y: -200.0, w: 2560.0, h: 1440.0 };

    #[test]
    fn a_window_on_the_display_crops_to_its_frame_at_dpr_1_and_2() {
        let window = Rect { x: 100.0, y: 45.0, w: 900.0, h: 500.0 };
        let (crop, pixels) = crop_for(&window, &DISPLAY, 1.0).expect("on the display");
        assert_eq!(crop, Crop { x: 100.0, y: 45.0, w: 900.0, h: 500.0 });
        assert_eq!(pixels, (900, 500));
        let (crop2, pixels2) = crop_for(&window, &DISPLAY, 2.0).expect("on the display");
        assert_eq!(crop2, crop, "sourceRect is in points, not pixels");
        assert_eq!(pixels2, (1800, 1000));
        // Odd point sizes round up to even pixels.
        let odd = Rect { x: 0.0, y: 0.0, w: 901.0, h: 501.0 };
        assert_eq!(crop_for(&odd, &DISPLAY, 1.0).expect("on").1, (902, 502));
    }

    #[test]
    fn a_window_on_a_second_display_crops_in_that_displays_space() {
        let window = Rect { x: 2000.0, y: 0.0, w: 800.0, h: 600.0 };
        assert!(crop_for(&window, &DISPLAY, 1.0).is_none());
        let (crop, pixels) = crop_for(&window, &RIGHT, 2.0).expect("on the right display");
        assert_eq!(crop, Crop { x: 80.0, y: 200.0, w: 800.0, h: 600.0 });
        assert_eq!(pixels, (1600, 1200));
    }

    #[test]
    fn a_window_partly_off_screen_or_across_displays_keeps_the_window_filter() {
        let off_left = Rect { x: -10.0, y: 45.0, w: 900.0, h: 500.0 };
        assert!(crop_for(&off_left, &DISPLAY, 1.0).is_none());
        let off_bottom = Rect { x: 100.0, y: 700.0, w: 900.0, h: 500.0 };
        assert!(crop_for(&off_bottom, &DISPLAY, 1.0).is_none());
        let straddling = Rect { x: 1500.0, y: 100.0, w: 900.0, h: 500.0 };
        assert!(crop_for(&straddling, &DISPLAY, 1.0).is_none());
        assert!(crop_for(&straddling, &RIGHT, 1.0).is_none());
        // Flush with the edges is still on the display.
        let flush = Rect { x: 1020.0, y: 580.0, w: 900.0, h: 500.0 };
        assert!(crop_for(&flush, &DISPLAY, 1.0).is_some());
        // Degenerate input never crops.
        assert!(crop_for(&Rect { x: 0.0, y: 0.0, w: 0.0, h: 500.0 }, &DISPLAY, 1.0).is_none());
        assert!(crop_for(&flush, &DISPLAY, 0.0).is_none());
    }

    #[test]
    fn a_moved_window_moves_the_crop_and_keeps_the_size() {
        let before = Rect { x: 100.0, y: 45.0, w: 900.0, h: 500.0 };
        let after = Rect { x: 340.0, y: 200.0, w: 900.0, h: 500.0 };
        let (c0, p0) = crop_for(&before, &DISPLAY, 2.0).expect("on");
        let (c1, p1) = crop_for(&after, &DISPLAY, 2.0).expect("on");
        assert_eq!(p0, p1, "a move is a configuration update, not a new encoder");
        assert_eq!((c1.x - c0.x, c1.y - c0.y), (240.0, 155.0));
        assert_eq!((c1.w, c1.h), (c0.w, c0.h));
    }

    #[test]
    fn rect_encloses_and_overlaps() {
        let a = Rect { x: 0.0, y: 0.0, w: 10.0, h: 10.0 };
        let inside = Rect { x: 2.0, y: 2.0, w: 5.0, h: 5.0 };
        let touching = Rect { x: 10.0, y: 0.0, w: 5.0, h: 5.0 };
        let crossing = Rect { x: 8.0, y: 8.0, w: 5.0, h: 5.0 };
        assert!(a.encloses(&inside));
        assert!(!a.encloses(&crossing));
        assert!(a.overlaps(&inside));
        assert!(a.overlaps(&crossing));
        assert!(!a.overlaps(&touching), "a shared edge is not an overlap");
    }
}

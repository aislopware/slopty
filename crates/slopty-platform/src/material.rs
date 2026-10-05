//! The system's sidebar material under a window.
//!
//! It is real glass from AppKit, which blurs what lies behind the window, takes its tint from
//! the desktop and goes flat while the window is inactive, as Finder's and Mail's sidebars do.
//! Nothing of it is drawn in the app.
//!
//! [`Glass::under`] puts one `NSVisualEffectView` across the whole window, below GPUI's own
//! view. GPUI then has to draw on a non-opaque layer (`WindowBackgroundAppearance::Transparent`)
//! and paint every region opaque except where the material is meant to show. One view the size
//! of the window, rather than one placed under the sidebar, never has to follow the sidebar's
//! width or motion a frame late. Under Reduce Transparency (which Increase Contrast turns on)
//! AppKit draws the view as a solid colour of its own, so the window stays opaque. The material
//! leans light or dark as the app's chrome over it does ([`Glass::set_dark`]), not as the
//! system's appearance would have it, so a light theme on a dark Mac stands on light glass.
//!
//! macOS only; iOS has no counterpart here yet.

use std::ffi::c_void;
use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly as _};
use objc2_app_kit::{
    NSAppearance, NSAppearanceCustomization as _, NSAppearanceNameAqua, NSAppearanceNameDarkAqua,
    NSAutoresizingMaskOptions, NSView, NSVisualEffectBlendingMode, NSVisualEffectMaterial,
    NSVisualEffectState, NSVisualEffectView, NSWindowOrderingMode,
};

/// The sidebar material under a window's GPUI view. Main thread only; dropping it takes the
/// material away.
pub struct Glass {
    view: Retained<NSVisualEffectView>,
}

impl std::fmt::Debug for Glass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Glass").finish_non_exhaustive()
    }
}

impl Glass {
    /// The material across the window whose GPUI view is `gpui` (its `raw_window_handle`
    /// AppKit handle), below that view, leaning dark or light as `dark` says. `None` off the
    /// main thread, or for a view in no superview.
    #[must_use]
    pub fn under(gpui: NonNull<c_void>, dark: bool) -> Option<Self> {
        let mtm = MainThreadMarker::new()?;
        // SAFETY: `raw_window_handle`'s AppKit rule: the handle is a live `NSView` of the
        // window, valid while the window is; retaining it keeps it valid past that.
        let gpui: Retained<NSView> = unsafe { Retained::retain(gpui.as_ptr().cast::<NSView>()) }?;
        // SAFETY: AppKit rule: a view's superview is read on the main thread; GPUI's view is a
        // subview of the window's content view.
        let host = unsafe { gpui.superview() }?;
        let view = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), host.bounds());
        view.setMaterial(NSVisualEffectMaterial::Sidebar);
        view.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
        view.setState(NSVisualEffectState::FollowsWindowActiveState);
        view.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        host.addSubview_positioned_relativeTo(&view, NSWindowOrderingMode::Below, Some(&gpui));
        let glass = Self { view };
        glass.set_dark(dark);
        Some(glass)
    }

    /// Lean the material dark or light, as the chrome over it does.
    pub fn set_dark(&self, dark: bool) {
        // SAFETY: immutable `NSString` statics AppKit defines (NSAppearance.h).
        let name = unsafe { if dark { NSAppearanceNameDarkAqua } else { NSAppearanceNameAqua } };
        self.view.setAppearance(NSAppearance::appearanceNamed(name).as_deref());
    }
}

impl Drop for Glass {
    fn drop(&mut self) {
        self.view.removeFromSuperview();
    }
}

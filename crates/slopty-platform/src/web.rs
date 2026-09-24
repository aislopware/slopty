//! The browser tile's page: a `WKWebView` over the GPUI window (macOS and iOS).
//!
//! GPUI draws every tile into one Metal layer, so a web page cannot be one of its elements.
//! It is a native view instead: a clipping view (the strip's area) inside the GPUI view, and
//! the web view inside that, placed over the tile's body every frame the strip draws. A
//! native view always draws above GPUI, so the caller hides it whenever something GPUI
//! draws should be on top (an overlay, the overview, the tile scrolled away).
//!
//! The keyboard differs by platform; each module says how.

use std::rc::Rc;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{DefinedClass as _, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_foundation::{NSError, NSObject};
use objc2_web_kit::{WKNavigation, WKNavigationDelegate};

#[cfg(target_os = "ios")]
mod ios;
#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "ios")]
pub use ios::WebView;
#[cfg(target_os = "macos")]
pub use macos::WebView;

/// What a page tells the tile that shows it. Delivered on the main thread, from inside a
/// framework callback: the receiver must only queue it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WebEvent {
    /// A navigation finished: the title, address and history may have changed.
    Loaded,
    /// A navigation failed; the system's word for why.
    Failed(String),
    /// The page was clicked or tapped: it has the keyboard now.
    Clicked,
    /// The keyboard went back to the GPUI view (a click outside, ⌃Tab, Esc twice).
    Released,
    /// A picture of the page, PNG, for when the view is hidden.
    Snapshot(Vec<u8>),
}

/// A rectangle in the GPUI view's coordinates: points, origin top left.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Frame {
    /// Left edge.
    pub x: f64,
    /// Top edge.
    pub y: f64,
    /// Width.
    pub w: f64,
    /// Height.
    pub h: f64,
}

/// What the page shows now.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Page {
    /// The document's title, empty before one loads.
    pub title: String,
    /// The address shown, which a redirect or a link may have changed.
    pub url: String,
    /// A navigation is under way.
    pub loading: bool,
    /// There is a page to go back to.
    pub can_go_back: bool,
}

type Sink = Rc<dyn Fn(WebEvent)>;

/// An editing command a page with the keyboard takes from a ⌘ key. The GPUI view answers
/// every key equivalent before its subviews see one, so these are handed to the page
/// directly instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edit {
    /// ⌘C.
    Copy,
    /// ⌘X.
    Cut,
    /// ⌘V.
    Paste,
    /// ⌘A.
    SelectAll,
    /// ⌘Z.
    Undo,
    /// ⇧⌘Z.
    Redo,
}

/// The editing command of a key: `key` is the character it types without modifiers (so the
/// layout decides, as it does for the Edit menu), `other` any of ⌃ and ⌥.
#[must_use]
pub fn edit_for(key: &str, command: bool, shift: bool, other: bool) -> Option<Edit> {
    if !command || other {
        return None;
    }
    match (key.to_lowercase().as_str(), shift) {
        ("c", false) => Some(Edit::Copy),
        ("x", false) => Some(Edit::Cut),
        ("v", false) => Some(Edit::Paste),
        ("a", false) => Some(Edit::SelectAll),
        ("z", false) => Some(Edit::Undo),
        ("z", true) => Some(Edit::Redo),
        _ => None,
    }
}

struct DelegateIvars {
    sink: Sink,
}

define_class!(
    // SAFETY:
    // - `NSObject` has no subclassing requirements.
    // - `Delegate` does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SloptyWebDelegate"]
    #[ivars = DelegateIvars]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    // The web view comes in as a plain object: the bindings type `WKWebView` for macOS only,
    // and the delegate reads nothing from it.
    unsafe impl WKNavigationDelegate for Delegate {
        #[unsafe(method(webView:didFinishNavigation:))]
        fn finished(&self, _web: &AnyObject, _navigation: Option<&WKNavigation>) {
            (self.ivars().sink)(WebEvent::Loaded);
        }

        #[unsafe(method(webView:didFailProvisionalNavigation:withError:))]
        fn failed_early(&self, _web: &AnyObject, _nav: Option<&WKNavigation>, error: &NSError) {
            (self.ivars().sink)(WebEvent::Failed(error.localizedDescription().to_string()));
        }

        #[unsafe(method(webView:didFailNavigation:withError:))]
        fn failed(&self, _web: &AnyObject, _nav: Option<&WKNavigation>, error: &NSError) {
            (self.ivars().sink)(WebEvent::Failed(error.localizedDescription().to_string()));
        }
    }
);

impl Delegate {
    fn new(sink: Sink, mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(DelegateIvars { sink });
        // SAFETY: `NSObject`'s `init` on a freshly allocated instance with ivars set.
        unsafe { msg_send![super(this), init] }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_edit_keys_go_to_the_page_and_nothing_else_does() {
        assert_eq!(edit_for("c", true, false, false), Some(Edit::Copy));
        assert_eq!(edit_for("x", true, false, false), Some(Edit::Cut));
        assert_eq!(edit_for("v", true, false, false), Some(Edit::Paste));
        assert_eq!(edit_for("a", true, false, false), Some(Edit::SelectAll));
        assert_eq!(edit_for("z", true, false, false), Some(Edit::Undo));
        assert_eq!(edit_for("Z", true, true, false), Some(Edit::Redo), "⇧ reports a capital");
        assert_eq!(edit_for("c", false, false, false), None, "a plain c is typing");
        assert_eq!(edit_for("c", true, false, true), None, "⌥⌘C is not copy");
        assert_eq!(edit_for("v", true, true, false), None, "⇧⌘V is not paste");
        assert_eq!(edit_for("t", true, false, false), None, "⌘T stays the workspace's");
    }
}

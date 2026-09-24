//! The page on iOS: a `UIView` subtree of the GPUI view.
//!
//! The keyboard: UIKit gives the page's text fields the keyboard when they are tapped, and
//! GPUI takes it back when one of its own inputs is focused. A tap on the page tells the tile
//! (so its tile takes the focus); hiding the page ends its editing. A hardware keyboard's
//! ⌃Tab and Esc twice are the page's own keys here, since UIKit has no monitor to see them
//! first.
//!
//! The bindings type `WKWebView` for macOS only, so the view is held as the `UIView` it is
//! and spoken to by selector.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::rc::Rc;

use block2::RcBlock;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject};
use objc2::{DefinedClass as _, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSError, NSObject, NSString, NSURL, NSURLRequest};
use objc2_ui_kit::{UIEvent, UIEventType, UIImage, UIResponder, UIView};
use objc2_web_kit::{WKSnapshotConfiguration, WKWebViewConfiguration};

use super::{Delegate, Frame, Page, Sink, WebEvent};

struct ClipIvars {
    sink: Sink,
}

define_class!(
    // SAFETY:
    // - `UIView` may be subclassed; this one only adds a hit test that defers to it.
    // - `Clip` does not implement `Drop`.
    #[unsafe(super(UIView, UIResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SloptyWebClip"]
    #[ivars = ClipIvars]
    struct Clip;

    impl Clip {
        /// A touch that lands on the page is the tile's cue to take the focus.
        #[unsafe(method(hitTest:withEvent:))]
        fn hit_test(&self, point: CGPoint, event: Option<&UIEvent>) -> *mut UIView {
            // SAFETY: UIKit rule: an override may call the superclass's hit test with the
            // same arguments.
            let hit: Option<Retained<UIView>> =
                unsafe { msg_send![super(self), hitTest: point, withEvent: event] };
            if hit.is_some() && event.is_some_and(|e| e.r#type() == UIEventType::Touches) {
                (self.ivars().sink)(WebEvent::Clicked);
            }
            // The hit test returns its view autoreleased, as UIKit's own does.
            hit.map_or(std::ptr::null_mut(), Retained::autorelease_return)
        }
    }
);

impl Clip {
    fn new(sink: Sink, mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ClipIvars { sink });
        // SAFETY: `UIView`'s designated initialiser on a freshly allocated instance with its
        // ivars set.
        unsafe { msg_send![super(this), initWithFrame: zero()] }
    }
}

const fn zero() -> CGRect {
    CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(0.0, 0.0))
}

const fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
    CGRect::new(CGPoint::new(x, y), CGSize::new(w.max(0.0), h.max(0.0)))
}

/// One page in a browser tile. Main thread only; dropping it takes the view away.
pub struct WebView {
    /// The `WKWebView`.
    web: Retained<UIView>,
    clip: Retained<Clip>,
    sink: Sink,
    _delegate: Retained<Delegate>,
    mtm: MainThreadMarker,
}

impl std::fmt::Debug for WebView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebView").finish_non_exhaustive()
    }
}

impl WebView {
    /// A hidden page loading `url`, inside `host`, the `UIView` a GPUI window draws into (its
    /// `raw_window_handle` UIKit handle). Events go to `sink`. `None` off the main thread,
    /// for an address `NSURL` refuses, or without `WebKit`.
    #[must_use]
    pub fn new(host: NonNull<c_void>, url: &str, sink: Rc<dyn Fn(WebEvent)>) -> Option<Self> {
        let mtm = MainThreadMarker::new()?;
        let address = NSURL::URLWithString(&NSString::from_str(url))?;
        // SAFETY: `raw_window_handle`'s UIKit rule: the handle is a live `UIView` of the
        // window, valid while the window is; the tile drops this view before its window.
        let host: Retained<UIView> = unsafe { Retained::retain(host.as_ptr().cast::<UIView>()) }?;
        let class = AnyClass::get(c"WKWebView")?;
        let clip = Clip::new(Rc::clone(&sink), mtm);
        clip.setClipsToBounds(true);
        clip.setHidden(true);
        // SAFETY: WebKit rule: a configuration made by `new` is complete.
        let config = unsafe { WKWebViewConfiguration::new(mtm) };
        // SAFETY: `NSObject` rule: `alloc` on a class returns a fresh instance to initialise.
        let allocated: Allocated<UIView> = unsafe { msg_send![class, alloc] };
        // SAFETY: WebKit rule: `WKWebView` is a `UIView` whose designated initialiser takes a
        // frame and a configuration, which it copies.
        let web: Option<Retained<UIView>> =
            unsafe { msg_send![allocated, initWithFrame: zero(), configuration: &*config] };
        let web = web?;
        let delegate = Delegate::new(Rc::clone(&sink), mtm);
        // SAFETY: WebKit rule: the navigation delegate is any object conforming to
        // `WKNavigationDelegate`, held weakly; `self` keeps it alive as long as the view.
        let () = unsafe { msg_send![&*web, setNavigationDelegate: &*delegate] };
        clip.addSubview(&web);
        host.addSubview(&clip);
        let this = Self { web, clip, sink, _delegate: delegate, mtm };
        this.load_address(&address);
        Some(this)
    }

    fn load_address(&self, address: &NSURL) {
        let request = NSURLRequest::requestWithURL(address);
        // SAFETY: WebKit rule: any `NSURLRequest` may be loaded; the navigation it returns
        // may be ignored.
        let _navigation: Option<Retained<AnyObject>> =
            unsafe { msg_send![&*self.web, loadRequest: &*request] };
    }

    /// Show the page at `frame`, cut to `clip` (both in the GPUI view's coordinates, which
    /// are UIKit's: points, origin top left), at `alpha`.
    pub fn place(&self, clip: Frame, frame: Frame, alpha: f64) {
        self.clip.setFrame(rect(clip.x, clip.y, clip.w, clip.h));
        self.web.setFrame(rect(frame.x - clip.x, frame.y - clip.y, frame.w, frame.h));
        self.clip.setAlpha(alpha.clamp(0.0, 1.0));
        self.clip.setHidden(false);
    }

    /// Take the page out of sight (it keeps running), ending any editing in it.
    pub fn hide(&self) {
        self.release();
        self.clip.setHidden(true);
    }

    /// Whether the page is shown.
    #[must_use]
    pub fn shown(&self) -> bool {
        !self.clip.isHidden()
    }

    /// Go to `url`.
    pub fn load(&self, url: &str) {
        if let Some(address) = NSURL::URLWithString(&NSString::from_str(url)) {
            self.load_address(&address);
        }
    }

    /// Back one page, when there is one.
    pub fn back(&self) {
        // SAFETY: WebKit rule: `goBack` with no history does nothing and returns nil.
        let _navigation: Option<Retained<AnyObject>> = unsafe { msg_send![&*self.web, goBack] };
    }

    /// Load the page again.
    pub fn reload(&self) {
        // SAFETY: WebKit rule: `reload` may be called at any time.
        let _navigation: Option<Retained<AnyObject>> = unsafe { msg_send![&*self.web, reload] };
    }

    /// What the page shows now.
    #[must_use]
    pub fn page(&self) -> Page {
        let web = &*self.web;
        // SAFETY: WebKit rule: plain `WKWebView` property reads on the main thread, of the
        // types its header declares (and so below).
        let title: Option<Retained<NSString>> = unsafe { msg_send![web, title] };
        // SAFETY: as above.
        let url: Option<Retained<NSURL>> = unsafe { msg_send![web, URL] };
        // SAFETY: as above.
        let loading: bool = unsafe { msg_send![web, isLoading] };
        // SAFETY: as above.
        let can_go_back: bool = unsafe { msg_send![web, canGoBack] };
        Page {
            title: title.map(|t| t.to_string()).unwrap_or_default(),
            url: url.and_then(|u| u.absoluteString()).map(|u| u.to_string()).unwrap_or_default(),
            loading,
            can_go_back,
        }
    }

    /// Give the page the keyboard.
    pub fn focus(&self) {
        let _took = self.web.becomeFirstResponder();
    }

    /// End any editing in the page, which hides its keyboard.
    pub fn release(&self) {
        // SAFETY: UIKit rule: `endEditing:` (the `UITextField` category on `UIView`) resigns
        // whichever view below the receiver is first responder.
        let _ended: bool = unsafe { msg_send![&*self.web, endEditing: true] };
    }

    /// Whether the page (or a view inside it) has the keyboard.
    #[must_use]
    pub fn focused(&self) -> bool {
        let mut stack = vec![Retained::clone(&self.web)];
        while let Some(view) = stack.pop() {
            if view.isFirstResponder() {
                return true;
            }
            stack.extend(view.subviews().iter());
        }
        false
    }

    /// Ask for a picture of the page; it comes back as [`WebEvent::Snapshot`].
    pub fn snapshot(&self) {
        let sink = Rc::clone(&self.sink);
        let done = RcBlock::new(move |image: *mut UIImage, _error: *mut NSError| {
            // SAFETY: WebKit rule: the image is nil or a valid `UIImage` for the duration of
            // the completion handler.
            let Some(image) = (unsafe { image.as_ref() }) else { return };
            if let Some(png) = image.png_representation() {
                sink(WebEvent::Snapshot(png.to_vec()));
            }
        });
        // SAFETY: WebKit rule: a default snapshot configuration captures the visible page.
        let config = unsafe { WKSnapshotConfiguration::new(self.mtm) };
        // SAFETY: WebKit rule: `takeSnapshotWithConfiguration:completionHandler:` takes a
        // block of `(UIImage *, NSError *)` and runs it once, on the main thread.
        unsafe {
            let (): () = msg_send![
                &*self.web,
                takeSnapshotWithConfiguration: &*config,
                completionHandler: &*done
            ];
        }
    }
}

impl Drop for WebView {
    fn drop(&mut self) {
        self.release();
        // SAFETY: WebKit rule: a delegate is cleared before it goes.
        let () = unsafe { msg_send![&*self.web, setNavigationDelegate: None::<&AnyObject>] };
        self.clip.removeFromSuperview();
    }
}

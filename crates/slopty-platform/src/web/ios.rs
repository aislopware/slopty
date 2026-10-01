//! The page on iOS: a `WKWebView` the window composes with GPUI's content.
//!
//! The tile hands [`WebView::view`] to a GPUI native host, which places it under GPUI's
//! layer and ties UIKit's first responder to GPUI's focus: UIKit gives the page's text
//! fields the keyboard when they are tapped, and GPUI focus leaving the page takes it back.
//!
//! The bindings type `WKWebView` for macOS only, so the view is held as the `UIView` it is
//! and spoken to by selector.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::rc::Rc;

use block2::RcBlock;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject};
use objc2::{MainThreadMarker, msg_send};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSError, NSString, NSURL, NSURLRequest};
use objc2_ui_kit::{UIImage, UIView};
use objc2_web_kit::{WKSnapshotConfiguration, WKWebViewConfiguration};

use super::{Delegate, Page, Sink, WebEvent};

const fn zero() -> CGRect {
    CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(0.0, 0.0))
}

/// One page in a browser tile. Main thread only; dropping it takes the view away.
pub struct WebView {
    /// The `WKWebView`.
    web: Retained<UIView>,
    sink: Sink,
    delegate: Retained<Delegate>,
    mtm: MainThreadMarker,
}

impl std::fmt::Debug for WebView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebView").finish_non_exhaustive()
    }
}

impl WebView {
    /// A page of `worker` loading `url`, in that worker's data store (`super::route`), in no
    /// view until the tile's native host adopts [`Self::view`]. Events go to `sink`. `None`
    /// off the main thread, for an address `NSURL` refuses, or without `WebKit`.
    #[must_use]
    pub fn new(worker: u128, url: &str, sink: Rc<dyn Fn(WebEvent)>) -> Option<Self> {
        let mtm = MainThreadMarker::new()?;
        let address = NSURL::URLWithString(&NSString::from_str(url))?;
        let class = AnyClass::get(c"WKWebView")?;
        // SAFETY: WebKit rule: a configuration made by `new` is complete.
        let config = unsafe { WKWebViewConfiguration::new(mtm) };
        // SAFETY: WebKit rule: a configuration takes any store before the view is made.
        unsafe {
            config.setWebsiteDataStore(&super::store(worker, mtm));
        }
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
        super::adopt_view(&web, &delegate);
        let this = Self { web, sink, delegate, mtm };
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

    /// The `WKWebView`, a `UIView *` for a native host to adopt. Valid while `self` is.
    #[must_use]
    pub fn view(&self) -> NonNull<c_void> {
        NonNull::from(&*self.web).cast()
    }

    /// Whether the page is on screen: in a window, and neither it nor a view it is in hidden.
    #[must_use]
    pub fn shown(&self) -> bool {
        if self.web.window().is_none() {
            return false;
        }
        let mut view = Some(Retained::clone(&self.web));
        while let Some(v) = view {
            if v.isHidden() {
                return false;
            }
            view = v.superview();
        }
        true
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

    /// Forward one page, when there is one.
    pub fn forward(&self) {
        // SAFETY: WebKit rule: `goForward` with no forward history does nothing and returns nil.
        let _navigation: Option<Retained<AnyObject>> = unsafe { msg_send![&*self.web, goForward] };
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
        // SAFETY: as above.
        let can_go_forward: bool = unsafe { msg_send![web, canGoForward] };
        Page {
            title: title.map(|t| t.to_string()).unwrap_or_default(),
            url: url.and_then(|u| u.absoluteString()).map(|u| u.to_string()).unwrap_or_default(),
            loading,
            can_go_back,
            can_go_forward,
        }
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

    /// Find `text` in the page, after the current match or (`backwards`) before it; the
    /// answer comes back as [`WebEvent::Found`].
    pub fn find(&self, text: &str, backwards: bool) {
        super::find_in(&self.web, text, backwards, Rc::clone(&self.sink));
    }

    /// Count `text` in the page; the answer comes back as [`WebEvent::Counted`].
    pub fn count(&self, text: &str) {
        super::count_in(&self.web, text, Rc::clone(&self.sink));
    }

    /// An iOS page has no inspector of its own: a debug build's is Safari's, on a Mac, in
    /// its Develop menu. Never opens one.
    #[must_use]
    #[expect(clippy::unused_self, reason = "the Mac twin opens its page's inspector")]
    pub const fn inspect(&self) -> bool {
        false
    }

    /// Show the page at `zoom`, 1 being its own size.
    pub fn set_zoom(&self, zoom: f64) {
        super::zoom_in(&self.web, zoom);
    }

    /// How much of download `id` has come, and of how much (0 while that is unknown); `None`
    /// once it is over.
    #[must_use]
    pub fn received(&self, id: u64) -> Option<(u64, u64)> {
        self.delegate.received(id)
    }

    /// Stop download `id`.
    pub fn cancel_download(&self, id: u64) {
        self.delegate.cancel(id);
    }
}

impl Drop for WebView {
    fn drop(&mut self) {
        // SAFETY: UIKit rule: `endEditing:` (the `UITextField` category on `UIView`) resigns
        // whichever view below the receiver is first responder.
        let _ended: bool = unsafe { msg_send![&*self.web, endEditing: true] };
        // SAFETY: WebKit rule: a delegate is cleared before it goes.
        let () = unsafe { msg_send![&*self.web, setNavigationDelegate: None::<&AnyObject>] };
        super::forget_view(&self.web, &self.delegate);
        self.web.removeFromSuperview();
    }
}

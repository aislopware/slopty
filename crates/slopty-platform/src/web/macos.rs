//! The page on macOS: an `NSView` subtree of the GPUI view.
//!
//! The keyboard: a click on the page makes the web view first responder, as AppKit does for
//! any view that accepts it; the GPUI view accepts none, so a click anywhere else, ⌃Tab, or
//! Esc twice makes the GPUI view first responder again. One local event monitor per process
//! watches for those, before AppKit dispatches the event.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::ptr::NonNull;
use std::rc::Rc;
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{MainThreadMarker, MainThreadOnly as _};
use objc2_app_kit::{
    NSBitmapImageFileType, NSBitmapImageRep, NSEvent, NSEventMask, NSEventModifierFlags,
    NSEventType, NSImage, NSResponder, NSView,
};
use objc2_foundation::{
    NSDictionary, NSError, NSPoint, NSRect, NSSize, NSString, NSURL, NSURLRequest,
};
use objc2_web_kit::{WKSnapshotConfiguration, WKWebView, WKWebViewConfiguration};

use super::{Delegate, Frame, Page, Sink, WebEvent};

/// Two presses of Esc closer than this give the keyboard back; one alone reaches the page.
const DOUBLE_ESCAPE: Duration = Duration::from_millis(400);
/// `kVK_Tab`, from `HIToolbox/Events.h`.
const KEY_TAB: u16 = 0x30;
/// `kVK_Escape`, from `HIToolbox/Events.h`.
const KEY_ESCAPE: u16 = 0x35;

/// A live page, as the monitor sees it.
struct Watched {
    web: Retained<WKWebView>,
    clip: Retained<NSView>,
    host: Retained<NSView>,
    sink: Sink,
    last_escape: Cell<Option<Instant>>,
}

thread_local! {
    static WATCHED: RefCell<Vec<Rc<Watched>>> = const { RefCell::new(Vec::new()) };
    static MONITOR: RefCell<Option<Retained<AnyObject>>> = const { RefCell::new(None) };
}

/// One page in a browser tile. Main thread only; dropping it takes the view away.
pub struct WebView {
    watched: Rc<Watched>,
    _delegate: Retained<Delegate>,
    mtm: MainThreadMarker,
}

impl std::fmt::Debug for WebView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebView").finish_non_exhaustive()
    }
}

impl WebView {
    /// A hidden page loading `url`, inside `host`, the `NSView` a GPUI window draws into (its
    /// `raw_window_handle` AppKit handle). Events go to `sink`. `None` off the main thread or
    /// for an address `NSURL` refuses.
    #[must_use]
    pub fn new(host: NonNull<c_void>, url: &str, sink: Rc<dyn Fn(WebEvent)>) -> Option<Self> {
        let mtm = MainThreadMarker::new()?;
        let address = NSURL::URLWithString(&NSString::from_str(url))?;
        // SAFETY: `raw_window_handle`'s AppKit rule: the handle is a live `NSView` of the
        // window, valid while the window is; the tile drops this view before its window.
        let host: Retained<NSView> = unsafe { Retained::retain(host.as_ptr().cast::<NSView>()) }?;
        let zero = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0));
        let clip = NSView::initWithFrame(NSView::alloc(mtm), zero);
        clip.setClipsToBounds(true);
        clip.setHidden(true);
        // SAFETY: WebKit rule: a configuration made by `new` is complete.
        let config = unsafe { WKWebViewConfiguration::new(mtm) };
        // SAFETY: WebKit rule: the view copies the configuration at init.
        let web =
            unsafe { WKWebView::initWithFrame_configuration(WKWebView::alloc(mtm), zero, &config) };
        let delegate = Delegate::new(Rc::clone(&sink), mtm);
        // SAFETY: WebKit rule: the navigation delegate is held weakly; `self` keeps it alive
        // as long as the view it is set on.
        unsafe {
            web.setNavigationDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        }
        clip.addSubview(&web);
        host.addSubview(&clip);
        let request = NSURLRequest::requestWithURL(&address);
        // SAFETY: WebKit rule: any `NSURLRequest` may be loaded; the navigation it returns
        // may be ignored.
        let _navigation = unsafe { web.loadRequest(&request) };
        let watched = Rc::new(Watched { web, clip, host, sink, last_escape: Cell::new(None) });
        watch(Rc::clone(&watched));
        Some(Self { watched, _delegate: delegate, mtm })
    }

    /// Show the page at `frame`, cut to `clip` (both in the GPUI view's coordinates), at
    /// `alpha`.
    pub fn place(&self, clip: Frame, frame: Frame, alpha: f64) {
        let w = &self.watched;
        let host = w.host.bounds();
        let flip = |f: Frame| {
            let y = if w.host.isFlipped() { f.y } else { host.size.height - f.y - f.h };
            NSRect::new(NSPoint::new(f.x, y), NSSize::new(f.w.max(0.0), f.h.max(0.0)))
        };
        w.clip.setFrame(flip(clip));
        // The clip view is not flipped: its origin is its bottom left.
        let inner = NSRect::new(
            NSPoint::new(frame.x - clip.x, (clip.y + clip.h) - (frame.y + frame.h)),
            NSSize::new(frame.w.max(0.0), frame.h.max(0.0)),
        );
        w.web.setFrame(inner);
        w.clip.setAlphaValue(alpha.clamp(0.0, 1.0));
        w.clip.setHidden(false);
    }

    /// Take the page out of sight (it keeps running). A page with the keyboard gives it back.
    pub fn hide(&self) {
        if self.focused() {
            self.release();
        }
        self.watched.clip.setHidden(true);
    }

    /// Whether the page is shown.
    #[must_use]
    pub fn shown(&self) -> bool {
        !self.watched.clip.isHidden()
    }

    /// Go to `url`.
    pub fn load(&self, url: &str) {
        let Some(address) = NSURL::URLWithString(&NSString::from_str(url)) else { return };
        let request = NSURLRequest::requestWithURL(&address);
        // SAFETY: as in `new`.
        let _navigation = unsafe { self.watched.web.loadRequest(&request) };
    }

    /// Back one page, when there is one.
    pub fn back(&self) {
        // SAFETY: WebKit rule: `goBack` with no history does nothing and returns nil.
        let _navigation = unsafe { self.watched.web.goBack() };
    }

    /// Load the page again.
    pub fn reload(&self) {
        // SAFETY: WebKit rule: `reload` may be called at any time.
        let _navigation = unsafe { self.watched.web.reload() };
    }

    /// What the page shows now.
    #[must_use]
    pub fn page(&self) -> Page {
        let web = &self.watched.web;
        // SAFETY: WebKit rule: a plain property read on the main thread (and so below).
        let title = unsafe { web.title() };
        // SAFETY: as above.
        let url = unsafe { web.URL() };
        // SAFETY: as above.
        let loading = unsafe { web.isLoading() };
        // SAFETY: as above.
        let can_go_back = unsafe { web.canGoBack() };
        Page {
            title: title.map(|t| t.to_string()).unwrap_or_default(),
            url: url.and_then(|u| u.absoluteString()).map(|u| u.to_string()).unwrap_or_default(),
            loading,
            can_go_back,
        }
    }

    /// Give the page the keyboard.
    pub fn focus(&self) {
        if let Some(window) = self.watched.web.window() {
            let _took = window.makeFirstResponder(Some(&self.watched.web));
        }
    }

    /// Give the keyboard back to the GPUI view.
    pub fn release(&self) {
        release(&self.watched);
    }

    /// Whether the page (or a view inside it) has the keyboard.
    #[must_use]
    pub fn focused(&self) -> bool {
        has_keyboard(&self.watched)
    }

    /// Ask for a picture of the page; it comes back as [`WebEvent::Snapshot`].
    pub fn snapshot(&self) {
        let sink = Rc::clone(&self.watched.sink);
        let done = RcBlock::new(move |image: *mut NSImage, _error: *mut NSError| {
            // SAFETY: WebKit rule: the image is nil or a valid `NSImage` for the duration of
            // the completion handler.
            let Some(image) = (unsafe { image.as_ref() }) else { return };
            if let Some(png) = png(image) {
                sink(WebEvent::Snapshot(png));
            }
        });
        // SAFETY: WebKit rule: a default snapshot configuration captures the visible page.
        let config = unsafe { WKSnapshotConfiguration::new(self.mtm) };
        // SAFETY: WebKit rule: the handler runs once, on the main thread.
        unsafe {
            self.watched.web.takeSnapshotWithConfiguration_completionHandler(Some(&config), &done);
        }
    }
}

impl Drop for WebView {
    fn drop(&mut self) {
        let w = &self.watched;
        if has_keyboard(w) {
            release(w);
        }
        // SAFETY: WebKit rule: a delegate is cleared before it goes.
        unsafe {
            w.web.setNavigationDelegate(None);
        }
        w.clip.removeFromSuperview();
        WATCHED.with(|all| all.borrow_mut().retain(|o| !Rc::ptr_eq(o, w)));
    }
}

/// The image as PNG bytes.
fn png(image: &NSImage) -> Option<Vec<u8>> {
    let tiff = image.TIFFRepresentation()?;
    let rep = NSBitmapImageRep::imageRepWithData(&tiff)?;
    // SAFETY: AppKit rule: an empty property dictionary asks for the type's defaults.
    let data = unsafe {
        rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new())
    }?;
    Some(data.to_vec())
}

/// Whether the window's first responder is `w`'s web view or inside it.
fn has_keyboard(w: &Watched) -> bool {
    let Some(window) = w.web.window() else { return false };
    let Some(responder) = window.firstResponder() else { return false };
    responder.downcast::<NSView>().is_ok_and(|view| view.isDescendantOf(&w.web))
}

fn release(w: &Watched) {
    if let Some(window) = w.host.window() {
        let host: &NSResponder = &w.host;
        let _took = window.makeFirstResponder(Some(host));
    }
}

/// Start watching `w`'s clicks and keys, installing the process's monitor on first use.
fn watch(w: Rc<Watched>) {
    WATCHED.with(|all| all.borrow_mut().push(w));
    MONITOR.with(|monitor| {
        let mut monitor = monitor.borrow_mut();
        if monitor.is_some() {
            return;
        }
        let mask = NSEventMask::LeftMouseDown
            | NSEventMask::RightMouseDown
            | NSEventMask::OtherMouseDown
            | NSEventMask::KeyDown;
        let block = RcBlock::new(|event: NonNull<NSEvent>| -> *mut NSEvent {
            // SAFETY: AppKit rule: the monitor's event is valid for the call.
            let event = unsafe { event.as_ref() };
            if screen(event) { std::ptr::null_mut() } else { std::ptr::from_ref(event).cast_mut() }
        });
        // SAFETY: AppKit rule: the handler returns the event, or nil to swallow it; the
        // monitor object is kept for the life of the process.
        *monitor = unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(mask, &block) };
    });
}

/// Look at `event` before AppKit dispatches it; `true` swallows it.
fn screen(event: &NSEvent) -> bool {
    let all: Vec<Rc<Watched>> = WATCHED.with(|all| all.borrow().clone());
    let Some(mtm) = MainThreadMarker::new() else { return false };
    let window = event.window(mtm);
    let mine: Vec<&Rc<Watched>> = all
        .iter()
        .filter(|w| {
            w.web
                .window()
                .zip(window.as_ref())
                .is_some_and(|(a, b)| std::ptr::eq(&raw const *a, &raw const **b))
        })
        .collect();
    if event.r#type() == NSEventType::KeyDown {
        mine.iter().find(|w| has_keyboard(w)).is_some_and(|w| key_down(w, event))
    } else {
        mouse_down(&mine, event);
        false
    }
}

/// A key while `w` has the keyboard: ⌃Tab, or a second Esc soon after the first, gives it
/// back and is swallowed; anything else reaches the page.
fn key_down(w: &Watched, event: &NSEvent) -> bool {
    let flags = event.modifierFlags();
    let code = event.keyCode();
    let control = flags.contains(NSEventModifierFlags::Control);
    let bare = !flags.intersects(
        NSEventModifierFlags::Control
            | NSEventModifierFlags::Command
            | NSEventModifierFlags::Option
            | NSEventModifierFlags::Shift,
    );
    let give_back = if code == KEY_TAB && control {
        true
    } else if code == KEY_ESCAPE && bare {
        let now = Instant::now();
        let twice =
            w.last_escape.get().is_some_and(|at| now.saturating_duration_since(at) < DOUBLE_ESCAPE);
        w.last_escape.set(if twice { None } else { Some(now) });
        twice
    } else {
        false
    };
    if give_back {
        release(w);
        (w.sink)(WebEvent::Released);
    }
    give_back
}

/// A click in the window: on a shown page it takes the keyboard (AppKit does that), and the
/// tile hears of it; anywhere else it takes the keyboard back from a page that had it.
fn mouse_down(mine: &[&Rc<Watched>], event: &NSEvent) {
    let at = event.locationInWindow();
    let hit = |w: &Watched| {
        if w.clip.isHidden() {
            return false;
        }
        let in_clip = w.clip.convertPoint_fromView(at, None);
        let in_web = w.web.convertPoint_fromView(at, None);
        contains(w.clip.bounds(), in_clip) && contains(w.web.bounds(), in_web)
    };
    if let Some(w) = mine.iter().find(|w| hit(w)) {
        (w.sink)(WebEvent::Clicked);
    } else if let Some(w) = mine.iter().find(|w| has_keyboard(w)) {
        release(w);
        (w.sink)(WebEvent::Released);
    }
}

fn contains(rect: NSRect, p: NSPoint) -> bool {
    p.x >= rect.origin.x
        && p.y >= rect.origin.y
        && p.x < rect.origin.x + rect.size.width
        && p.y < rect.origin.y + rect.size.height
}

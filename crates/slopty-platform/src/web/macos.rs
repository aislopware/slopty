//! The page on macOS: a `WKWebView` the window composes with GPUI's content.
//!
//! The tile hands [`WebView::view`] to a GPUI native host, which places, clips and stacks it
//! under GPUI's layer and ties AppKit's first responder to GPUI's focus: a click on the page
//! makes the web view first responder as AppKit does for any view that accepts it, and GPUI
//! focus leaving the page gives the keyboard back to the GPUI view. Keys reach GPUI's keymap
//! before the page; what no binding takes goes to `WebKit`.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::rc::Rc;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{MainThreadMarker, MainThreadOnly as _, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSBitmapImageFileType, NSBitmapImageRep, NSEvent, NSEventModifierFlags,
    NSEventType, NSImage, NSResponder, NSView,
};
use objc2_foundation::{
    NSDictionary, NSError, NSPoint, NSRect, NSSize, NSString, NSURL, NSURLRequest,
};
use objc2_web_kit::{WKSnapshotConfiguration, WKWebView, WKWebViewConfiguration};

use super::{Delegate, Edit, Page, Sink, WebEvent};

/// One page in a browser tile. Main thread only; dropping it takes the view away.
pub struct WebView {
    web: Retained<WKWebView>,
    /// The GPUI view of the page's window, which takes the keyboard back when the page goes.
    gpui: Retained<NSView>,
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
    /// A page of `worker` loading `url`, for the window whose GPUI view is `gpui` (its
    /// `raw_window_handle` AppKit handle), in that worker's data store (`super::route`). It is
    /// in no view until the tile's native host adopts [`Self::view`]. Events go to `sink`.
    /// Web Inspector opens on it. `None` off the main thread or for an address `NSURL`
    /// refuses.
    #[must_use]
    pub fn new(
        gpui: NonNull<c_void>,
        worker: u128,
        url: &str,
        sink: Rc<dyn Fn(WebEvent)>,
    ) -> Option<Self> {
        let mtm = MainThreadMarker::new()?;
        let address = NSURL::URLWithString(&NSString::from_str(url))?;
        // SAFETY: `raw_window_handle`'s AppKit rule: the handle is a live `NSView` of the
        // window, valid while the window is; retaining it keeps it valid past that.
        let gpui: Retained<NSView> = unsafe { Retained::retain(gpui.as_ptr().cast::<NSView>()) }?;
        let zero = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0));
        // SAFETY: WebKit rule: a configuration made by `new` is complete.
        let config = unsafe { WKWebViewConfiguration::new(mtm) };
        // SAFETY: WebKit rule: a configuration takes any store before the view is made.
        unsafe {
            config.setWebsiteDataStore(&super::store(worker, mtm));
        }
        developer_extras(&config);
        // SAFETY: WebKit rule: the view copies the configuration at init.
        let web =
            unsafe { WKWebView::initWithFrame_configuration(WKWebView::alloc(mtm), zero, &config) };
        let delegate = Delegate::new(Rc::clone(&sink), mtm);
        // SAFETY: WebKit rule: the navigation delegate is held weakly; `self` keeps it alive
        // as long as the view it is set on.
        unsafe {
            web.setNavigationDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        }
        super::adopt_view(&web, &delegate);
        super::opened(worker, &web);
        let request = NSURLRequest::requestWithURL(&address);
        // SAFETY: WebKit rule: any `NSURLRequest` may be loaded; the navigation it returns
        // may be ignored.
        let _navigation = unsafe { web.loadRequest(&request) };
        Some(Self { web, gpui, sink, delegate, mtm })
    }

    /// The `WKWebView`, an `NSView *` for a native host to adopt. Valid while `self` is.
    #[must_use]
    pub fn view(&self) -> NonNull<c_void> {
        NonNull::from(&*self.web).cast()
    }

    /// Whether the page is on screen: in a window, and neither it nor a view it is in hidden.
    #[must_use]
    pub fn shown(&self) -> bool {
        self.web.window().is_some() && !self.web.isHiddenOrHasHiddenAncestor()
    }

    /// Go to `url`.
    pub fn load(&self, url: &str) {
        let Some(address) = NSURL::URLWithString(&NSString::from_str(url)) else { return };
        let request = NSURLRequest::requestWithURL(&address);
        // SAFETY: as in `new`.
        let _navigation = unsafe { self.web.loadRequest(&request) };
    }

    /// Back one page, when there is one.
    pub fn back(&self) {
        // SAFETY: WebKit rule: `goBack` with no history does nothing and returns nil.
        let _navigation = unsafe { self.web.goBack() };
    }

    /// Forward one page, when there is one.
    pub fn forward(&self) {
        // SAFETY: WebKit rule: `goForward` with no forward history does nothing and returns nil.
        let _navigation = unsafe { self.web.goForward() };
    }

    /// Load the page again.
    pub fn reload(&self) {
        // SAFETY: WebKit rule: `reload` may be called at any time.
        let _navigation = unsafe { self.web.reload() };
    }

    /// What the page shows now.
    #[must_use]
    pub fn page(&self) -> Page {
        let web = &self.web;
        // SAFETY: WebKit rule: a plain property read on the main thread (and so below).
        let title = unsafe { web.title() };
        // SAFETY: as above.
        let url = unsafe { web.URL() };
        // SAFETY: as above.
        let loading = unsafe { web.isLoading() };
        // SAFETY: as above.
        let can_go_back = unsafe { web.canGoBack() };
        // SAFETY: as above.
        let can_go_forward = unsafe { web.canGoForward() };
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
        has_keyboard(&self.web)
    }

    /// The number of the window the page is in, for [`press`].
    #[must_use]
    pub fn window_number(&self) -> Option<isize> {
        self.web.window().map(|w| w.windowNumber())
    }

    /// Do `edit` in the page, as the Edit menu would: the action goes up the responder chain
    /// from the page's first responder, and undo to the web view's own undo manager.
    pub fn perform(&self, edit: Edit) {
        let action = match edit {
            Edit::Cut => sel!(cut:),
            Edit::SelectAll => sel!(selectAll:),
            Edit::Undo | Edit::Redo => {
                if let Some(undo) = self.web.undoManager() {
                    if edit == Edit::Undo { undo.undo() } else { undo.redo() }
                }
                return;
            }
        };
        let Some(responder) = self.web.window().and_then(|w| w.firstResponder()) else { return };
        // SAFETY: AppKit rule: a standard edit action takes its sender as the argument, and nil
        // is a valid sender; `tryToPerform:with:` walks the responder chain from the receiver.
        let _done = unsafe { responder.tryToPerform_with(action, None) };
    }

    /// Ask for a picture of the page; it comes back as [`WebEvent::Snapshot`].
    pub fn snapshot(&self) {
        let sink = Rc::clone(&self.sink);
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
            self.web.takeSnapshotWithConfiguration_completionHandler(Some(&config), &done);
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

    /// Open Web Inspector on the page, in a window of its own. Whether it opened.
    #[must_use]
    pub fn inspect(&self) -> bool {
        show_inspector(&self.web)
    }

    /// Whether the page is open to Web Inspector now.
    #[must_use]
    pub fn inspectable(&self) -> bool {
        // SAFETY: WebKit rule: `inspectable` is a BOOL property of `WKWebView`.
        unsafe { msg_send![&*self.web, isInspectable] }
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
        // A first responder taken out of its window leaves the window itself as the first
        // responder, where no key reaches GPUI.
        if has_keyboard(&self.web)
            && let Some(window) = self.gpui.window()
        {
            let gpui: &NSResponder = &self.gpui;
            let _took = window.makeFirstResponder(Some(gpui));
        }
        // SAFETY: WebKit rule: a delegate is cleared before it goes.
        unsafe {
            self.web.setNavigationDelegate(None);
        }
        super::forget_view(&self.web, &self.delegate);
        self.web.removeFromSuperview();
        super::close_page(&self.web);
    }
}

/// Whether `object` answers `selector`, asked before a selector of `WebKit`'s private headers.
fn responds(object: &AnyObject, selector: Sel) -> bool {
    // SAFETY: `NSObject` rule: `respondsToSelector:` may be asked of any object.
    unsafe { msg_send![object, respondsToSelector: selector] }
}

/// Turn on the developer extras of the pages `config` makes: the local Web Inspector, and
/// "Inspect Element" in a page's menu. `isInspectable` alone opens a page to Safari's
/// Develop menu, not to an inspector of its own.
fn developer_extras(config: &WKWebViewConfiguration) {
    // SAFETY: WebKit rule: `preferences` is a `WKPreferences` property of every configuration.
    let preferences: Option<Retained<AnyObject>> = unsafe { msg_send![config, preferences] };
    let Some(preferences) = preferences.filter(|p| responds(p, sel!(_setDeveloperExtrasEnabled:)))
    else {
        return;
    };
    // SAFETY: WebKit rule (`WKPreferencesPrivate.h`): `_developerExtrasEnabled` is a BOOL
    // property of `WKPreferences`; its setter is present, asked above.
    let () = unsafe { msg_send![&*preferences, _setDeveloperExtrasEnabled: true] };
}

/// Show Web Inspector for `web`'s page. Whether it could.
fn show_inspector(web: &WKWebView) -> bool {
    if !responds(web, sel!(_inspector)) {
        return false;
    }
    // SAFETY: WebKit rule (`WKWebViewPrivate.h`): `_inspector` is a `_WKInspector` property of
    // a macOS `WKWebView`, present as asked above.
    let inspector: Option<Retained<AnyObject>> = unsafe { msg_send![web, _inspector] };
    let Some(inspector) = inspector.filter(|i| responds(i, sel!(show))) else { return false };
    // SAFETY: WebKit rule (`_WKInspector.h`): `show` opens the inspector's window and takes
    // no arguments; present as asked above.
    let () = unsafe { msg_send![&*inspector, show] };
    true
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

/// Deliver a key press to window `number` as AppKit delivers one from the keyboard.
///
/// Through the application when the window is the key window, so its key equivalents and the
/// menu bar have it first; else straight to the window, which is where the application sends a
/// key its menu did not take. `key` is the character the key types without modifiers, with ⌘
/// and ⇧ as given. The self-test's way to prove where a key goes while a page holds the
/// keyboard. Whether there was such a window (off the main thread there is none).
#[must_use]
pub fn press(number: isize, key: &str, command: bool, shift: bool) -> bool {
    let Some(mtm) = MainThreadMarker::new() else { return false };
    let app = NSApplication::sharedApplication(mtm);
    let Some(window) = app.windowWithWindowNumber(number) else { return false };
    let mut flags = NSEventModifierFlags::empty();
    if command {
        flags |= NSEventModifierFlags::Command;
    }
    if shift {
        flags |= NSEventModifierFlags::Shift;
    }
    let typed = if shift { key.to_uppercase() } else { key.to_owned() };
    let chars = NSString::from_str(&typed);
    let event = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
        NSEventType::KeyDown,
        NSPoint::new(0.0, 0.0),
        flags,
        0.0,
        window.windowNumber(),
        None,
        &chars,
        &chars,
        false,
        key_code(key),
    );
    let Some(event) = event else { return false };
    if window.isKeyWindow() {
        app.sendEvent(&event);
    } else {
        window.sendEvent(&event);
    }
    true
}

/// Whether the window's first responder is `web` or a view inside it.
fn has_keyboard(web: &WKWebView) -> bool {
    let Some(window) = web.window() else { return false };
    let Some(responder) = window.firstResponder() else { return false };
    responder.downcast::<NSView>().is_ok_and(|view| view.isDescendantOf(web))
}

/// The virtual key code of the key typing `key` on an ANSI keyboard, from
/// `HIToolbox/Events.h` (`kVK_ANSI_A`…); [`NO_KEY`] for a character off that table.
fn key_code(key: &str) -> u16 {
    const LETTERS: [u16; 26] = [
        0x00, 0x0b, 0x08, 0x02, 0x0e, 0x03, 0x05, 0x04, 0x22, 0x26, 0x28, 0x25, 0x2e, 0x2d, 0x1f,
        0x23, 0x0c, 0x0f, 0x01, 0x11, 0x20, 0x09, 0x0d, 0x07, 0x10, 0x06,
    ];
    let mut chars = key.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else { return NO_KEY };
    let at = u32::from(c.to_ascii_lowercase()).wrapping_sub(u32::from('a'));
    usize::try_from(at).ok().and_then(|at| LETTERS.get(at)).copied().unwrap_or(NO_KEY)
}

/// A key code no key has.
const NO_KEY: u16 = 0xffff;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_letter_is_its_ansi_key_and_anything_else_no_key() {
        assert_eq!(key_code("a"), 0x00);
        assert_eq!(key_code("z"), 0x06);
        assert_eq!(key_code("Z"), 0x06, "⇧ types a capital on the same key");
        assert_eq!(key_code("c"), 0x08);
        assert_eq!(key_code("v"), 0x09);
        assert_eq!(key_code("x"), 0x07);
        assert_eq!(key_code("m"), 0x2e);
        for other in ["", "ab", "1", "é", "`"] {
            assert_eq!(key_code(other), NO_KEY, "{other:?}");
        }
    }
}

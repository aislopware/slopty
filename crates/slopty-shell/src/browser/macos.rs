// Derived from zeron (MIT, github.com/zeronsh/zeron @ 3d2f1e6), see THIRD_PARTY_NOTICES.md
//! AppKit boundary for the browser. Wry owns the native child and its UI
//! delegate; our navigation delegate supplies browser policy and state. All
//! callbacks enqueue events, never re-enter GPUI. No page-to-engine IPC.
// One page-to-shell message only, the page's own favicon (`FAVICON_SCRIPT`).
use std::{
    cell::{Cell, RefCell},
    mem::ManuallyDrop,
    ptr::NonNull, // the page goes to gpui-fast's native host
    rc::Rc,
};

use gpui::{App, Window};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{
    DefinedClass as _, MainThreadMarker, MainThreadOnly, class, define_class, msg_send, sel,
};
use objc2_app_kit::{
    NSColor, NSEvent, NSEventMask, NSEventModifierFlags, NSEventType, NSScreen, NSView,
};
use objc2_foundation::{
    NSDictionary, NSError, NSKeyValueChangeKey, NSKeyValueObservingOptions, NSObject,
    NSObjectNSKeyValueObserverRegistration as _, NSObjectProtocol, NSString,
};
use objc2_web_kit::{
    WKNavigation, WKNavigationAction, WKNavigationActionPolicy, WKNavigationDelegate,
    WKNavigationResponse, WKNavigationResponsePolicy, WKWebView,
};
use wry::{WebView, WebViewBuilderExtMacos as _, WebViewExtMacOS as _};

use super::model::{PageState, Presentation, allowed_navigation};

#[derive(Default)]
struct BrowserStore {
    profile: Option<super::profile::BrowserProfile>,
    store: Option<Retained<objc2_web_kit::WKWebsiteDataStore>>,
}
#[derive(Clone, Default)]
pub(super) struct BrowserData(Rc<RefCell<BrowserStore>>);
impl BrowserData {
    pub fn for_profile(profile: super::profile::BrowserProfile) -> Self {
        thread_local! {
            // Keep named stores alive through process termination, including
            // after the last window closes or the active identity changes.
            // macOS runs Rust TLS destructors during AppKit's exit(). Releasing
            // a WKWebsiteDataStore there removes its network session before
            // WebKit's UI-process-disconnect handler can flush its cookies.
            // In particular, deleted cookies can reappear on the next launch.
            // Intentionally skip this registry's destructor: WebKit must still
            // own the sessions when our process disconnects, and the OS reclaims
            // these process-lifetime references on exit.
            static PROFILES: ManuallyDrop<RefCell<std::collections::HashMap<std::path::PathBuf, Rc<RefCell<BrowserStore>>>>> =
                ManuallyDrop::new(RefCell::new(std::collections::HashMap::new()));
        }
        PROFILES.with(|profiles| {
            let mut profiles = profiles.borrow_mut();
            if let Some(store) = profiles.get(&profile.root) {
                return Self(Rc::clone(store));
            }
            let root = profile.root.clone();
            let store = Rc::new(RefCell::new(BrowserStore {
                profile: Some(profile),
                ..Default::default()
            }));
            profiles.insert(root, Rc::clone(&store));
            Self(store)
        })
    }

    fn configuration(
        &self,
        mtm: MainThreadMarker,
    ) -> Retained<objc2_web_kit::WKWebViewConfiguration> {
        let mut data = self.0.borrow_mut();
        let BrowserStore { profile, store } = &mut *data;
        let store = store.get_or_insert_with(|| website_store(profile.as_ref(), mtm));
        // SAFETY: WebKit's configuration is main-thread only, which `mtm` proves.
        let configuration = unsafe { objc2_web_kit::WKWebViewConfiguration::new(mtm) };
        // SAFETY: the store is a live WKWebsiteDataStore, set on a configuration no web view
        // has taken yet, on the main thread.
        unsafe {
            configuration.setWebsiteDataStore(store);
        }
        // WebKit's bare user agent names no browser, and sites (Google's among them)
        // serve it their pages for old browsers; it says what Safari says.
        let name = NSString::from_str(&safari_user_agent());
        // SAFETY: an NSString the configuration copies, set on the main thread.
        unsafe {
            configuration.setApplicationNameForUserAgent(Some(&name));
        }
        configuration
    }
}

/// The website data store a page of `profile` keeps its data in: the profile's own where
/// macOS keeps stores by identifier, else a temporary one.
fn website_store(
    profile: Option<&super::profile::BrowserProfile>,
    mtm: MainThreadMarker,
) -> Retained<objc2_web_kit::WKWebsiteDataStore> {
    match profile {
        Some(profile) if persistent_profiles_supported() => {
            let identifier = objc2_foundation::NSUUID::from_bytes(profile.data_store_identifier());
            // SAFETY: WebKit's data stores are main-thread only, which `mtm` proves; the
            // class answers `dataStoreForIdentifier:` (`persistent_profiles_supported`).
            unsafe { objc2_web_kit::WKWebsiteDataStore::dataStoreForIdentifier(&identifier, mtm) }
        }
        // SAFETY: WebKit's data stores are main-thread only, which `mtm` proves.
        _ => unsafe { objc2_web_kit::WKWebsiteDataStore::nonPersistentDataStore(mtm) },
    }
}

/// What Safari adds to `WebKit`'s user agent: its version, read from the Safari on this
/// Mac, and the `WebKit` build Safari has said since it froze it.
fn safari_user_agent() -> String {
    const PLIST: &str = "/Applications/Safari.app/Contents/Info.plist";
    const KEY: &str = "<key>CFBundleShortVersionString</key>";
    let version = std::fs::read_to_string(PLIST).ok().and_then(|plist| {
        let (_, after) = plist.split_once(KEY)?;
        let value = after.split_once("<string>")?.1.split_once("</string>")?.0.trim();
        let mut parts = value.split('.');
        let major = parts.next().filter(|p| p.parse::<u32>().is_ok())?;
        let minor = parts.next().filter(|p| p.parse::<u32>().is_ok()).unwrap_or("0");
        Some(format!("{major}.{minor}"))
    });
    let version = version.as_deref().unwrap_or("26.0");
    format!("Version/{version} Safari/605.1.15")
}

/// macOS 12/13 have only a single default persistent store. Do not silently
/// share it across accounts; retain isolated temporary stores on those systems.
pub(super) fn persistent_profiles_supported() -> bool {
    // SAFETY: `respondsToSelector:` is NSObject's, taking a selector and answering a BOOL, the
    // types `msg_send!` gives it here.
    unsafe {
        msg_send![class!(WKWebsiteDataStore), respondsToSelector: sel!(dataStoreForIdentifier:)]
    }
}

impl BrowserData {
    pub(super) fn persistence_notice(&self) -> Option<&'static str> {
        (self.0.borrow().profile.is_some() && !persistent_profiles_supported())
            .then_some("Logins are temporary on this macOS version. Use macOS 14 or later, or open this page in your default browser, to keep them.")
    }
}

type Sender = tokio::sync::mpsc::Sender<NativeEvent>;
const OBSERVED: [&str; 6] =
    ["URL", "title", "loading", "canGoBack", "canGoForward", "underPageBackgroundColor"];

pub(super) enum NativeEvent {
    Changed,
    Finished,
    NewTab(String),
    Key(gpui::Keystroke),
    // The icon itself, read inside the page as a PNG `data:` URL (no HTTP client of
    // the shell's own), for the page at `page`.
    Favicon { page: String, data: String },
}

struct ObserverState {
    tx: Sender,
    pending: Cell<bool>,
    error: RefCell<Option<String>>,
    requested_url: RefCell<Option<String>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SloptyBrowserObserver"]
    #[ivars = ObserverState]
    struct Observer;
    unsafe impl NSObjectProtocol for Observer {}
    impl Observer {
        #[unsafe(method(observeValueForKeyPath:ofObject:change:context:))]
        fn observe(&self, _key: Option<&NSString>, _object: Option<&AnyObject>, _change: Option<&NSDictionary<NSKeyValueChangeKey, AnyObject>>, _context: *mut std::ffi::c_void) {
            self.changed();
        }
    }
    unsafe impl WKNavigationDelegate for Observer {
        #[unsafe(method(webView:decidePolicyForNavigationAction:decisionHandler:))]
        fn policy(&self, _view: &WKWebView, action: &WKNavigationAction, decision: &block2::Block<dyn Fn(WKNavigationActionPolicy)>) {
            // SAFETY: the action WebKit hands its navigation delegate, read on the main thread
            // during the call.
            let url = unsafe { action.request().URL() }.and_then(|u| u.absoluteString()).map(|u| u.to_string()).unwrap_or_default();
            // SAFETY: as above.
            let frame = unsafe { action.targetFrame() };
            let main_frame = frame.is_some_and(|frame| {
                // SAFETY: the action's frame, read on the main thread during the call.
                unsafe { frame.isMainFrame() }
            });
            if allowed_navigation(&url) && main_frame {
                *self.ivars().requested_url.borrow_mut() = Some(url.clone());
            }
            decision.call((if allowed_navigation(&url) { WKNavigationActionPolicy::Allow } else { WKNavigationActionPolicy::Cancel },));
        }
        #[unsafe(method(webView:decidePolicyForNavigationResponse:decisionHandler:))]
        fn response(&self, _view: &WKWebView, response: &WKNavigationResponse, decision: &block2::Block<dyn Fn(WKNavigationResponsePolicy)>) {
            // SAFETY: the response WebKit hands its navigation delegate, read on the main thread
            // during the call.
            let displayable = unsafe { response.canShowMIMEType() };
            if !displayable { self.fail("This file can’t be previewed here. Open it in your default browser."); }
            decision.call((if displayable { WKNavigationResponsePolicy::Allow } else { WKNavigationResponsePolicy::Cancel },));
        }
        #[unsafe(method(webView:didStartProvisionalNavigation:))]
        fn start(&self, _view: &WKWebView, _navigation: Option<&WKNavigation>) {
            self.ivars().error.borrow_mut().take(); self.changed();
        }
        #[unsafe(method(webView:didCommitNavigation:))]
        fn commit(&self, _view: &WKWebView, _navigation: Option<&WKNavigation>) {
            self.ivars().requested_url.borrow_mut().take(); self.changed();
        }
        #[unsafe(method(webView:didFinishNavigation:))]
        fn finish(&self, _view: &WKWebView, _navigation: Option<&WKNavigation>) {
            self.changed();
            // A full or closed queue: the surface is behind or gone, and its next read catches up.
            let _full_or_gone = self.ivars().tx.try_send(NativeEvent::Finished);
        }
        #[unsafe(method(webView:didFailProvisionalNavigation:withError:))]
        fn provisional_error(&self, _view: &WKWebView, _navigation: Option<&WKNavigation>, error: &NSError) {
            if error.code() != -999 {
                tracing::warn!(domain = %error.domain(), code = error.code(), "browser provisional navigation failed");
                self.fail("Check the address and make sure your server is running, then try again.");
            }
        }
        #[unsafe(method(webView:didFailNavigation:withError:))]
        fn navigation_error(&self, _view: &WKWebView, _navigation: Option<&WKNavigation>, error: &NSError) {
            if error.code() != -999 {
                tracing::warn!(domain = %error.domain(), code = error.code(), "browser navigation failed");
                self.fail("The connection was interrupted. Try loading this page again.");
            }
        }
        #[unsafe(method(webViewWebContentProcessDidTerminate:))]
        fn terminated(&self, _view: &WKWebView) {
            self.fail("The page stopped responding. Reload to continue.");
        }
    }
);

impl Observer {
    fn new(tx: Sender, mtm: MainThreadMarker) -> Retained<Self> {
        let object = mtm.alloc().set_ivars(ObserverState {
            tx,
            pending: Cell::new(false),
            error: RefCell::new(None),
            requested_url: RefCell::new(None),
        });
        // SAFETY: NSObject's `init` on the allocation `set_ivars` just filled, returning it.
        unsafe { msg_send![super(object), init] }
    }

    fn changed(&self) {
        if !self.ivars().pending.replace(true)
            && self.ivars().tx.try_send(NativeEvent::Changed).is_err()
        {
            self.ivars().pending.set(false);
        }
    }

    fn fail(&self, message: &str) {
        *self.ivars().error.borrow_mut() = Some(message.into());
        self.changed();
    }
}

// gpui-fast natives under one layer (docs/composition.md): the native host cuts the hole, clips
// and routes hits and focus.

pub(super) struct NativePage(Rc<RefCell<Host>>);

pub(super) struct Host {
    web: WebView,
    view: Retained<WKWebView>,
    observer: Retained<Observer>,
    // gpui-fast's native host, which places, clips and routes the page.
    native: gpui::composition::NativeHost,
    background_color: Option<Retained<NSColor>>,
    monitor: Option<Retained<AnyObject>>,
    wheel_monitor: Option<Retained<AnyObject>>,
    wheel: Rc<PageGlide>,
    shortcuts: Rc<RefCell<Vec<String>>>,
    presentation: Presentation,
}

impl NativePage {
    // gpui-fast natives under one layer (docs/composition.md): the host needs the
    // window mutably and the app.
    pub fn new(
        window: &mut Window,
        data: &BrowserData,
        tx: Sender,
        cx: &mut App,
    ) -> Result<Self, String> {
        let mtm = MainThreadMarker::new().ok_or("Browser must be created on the main thread")?;
        let new_tab = tx.clone();
        // The page's favicon, posted by `FAVICON_SCRIPT`; anything else is dropped.
        let favicon = tx.clone();
        let web = wry::WebViewBuilder::new()
            .with_webview_configuration(data.configuration(mtm))
            .with_visible(false)
            .with_focused(false)
            .with_new_window_req_handler(move |url, _| {
                if allowed_navigation(&url) {
                    // A full or closed queue drops the request, as a closed tab would.
                    let _full_or_gone = new_tab.try_send(NativeEvent::NewTab(url));
                }
                wry::NewWindowResponse::Deny
            })
            .with_download_started_handler(|_, _| false)
            .with_ipc_handler(move |request| {
                if let Some((page, data)) = super::favicon_message(request.body()) {
                    // A full or closed queue drops the icon; the page posts it again.
                    let _full_or_gone = favicon.try_send(NativeEvent::Favicon { page, data });
                }
            })
            .build_as_child(window)
            .map_err(|e| e.to_string())?;
        let view = Retained::into_super(web.webview());
        // gpui-fast natives under one layer (docs/composition.md).
        let native = window
            .create_native_host(
                gpui::composition::NativeHostOptions {
                    opaque: true,
                    interactive: true,
                    label: Some("Browser".into()),
                },
                cx,
            )
            .map_err(|error| error.to_string())?;
        // SAFETY: `view` is the live WKWebView wry just made, and this runs on the main thread.
        unsafe { native.attach_view(NonNull::from(&*view).cast()) }
            .map_err(|error| error.to_string())?;

        let observer = Observer::new(tx.clone(), mtm);
        // SAFETY: the delegate is weak; `Host` keeps the observer and `Host::drop` clears the
        // delegate before letting the observer go. Main thread.
        unsafe {
            view.setNavigationDelegate(Some(ProtocolObject::from_ref(&*observer)));
        }
        for key in OBSERVED {
            // SAFETY: KVO on the web view's own key paths with no context pointer, removed in
            // `Host::drop` while the observer still lives. Main thread.
            unsafe {
                view.addObserver_forKeyPath_options_context(
                    &observer,
                    &NSString::from_str(key),
                    NSKeyValueObservingOptions::empty(),
                    std::ptr::null_mut(),
                );
            }
        }
        let shortcuts = Rc::new(RefCell::new(Vec::<String>::new()));
        let monitor_view = view.clone();
        let monitor_tx = tx;
        let monitor_shortcuts = Rc::clone(&shortcuts);
        let callback = block2::RcBlock::new(move |event: NonNull<NSEvent>| {
            // SAFETY: AppKit hands a local monitor a live event for the length of the call.
            let e = unsafe { event.as_ref() };
            if monitor_view.isHidden()
                || !has_focus(&monitor_view)
                || e.window(mtm) != monitor_view.window()
            {
                return event.as_ptr();
            }
            let mods = e.modifierFlags();
            // A dead key (AZERTY's ^, where ⌘[ lives) reports no characters
            // with ⌘ down; ask for the key as typed alone, as gpui does.
            let key = e
                .charactersIgnoringModifiers()
                .map(|s| s.to_string())
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    e.charactersByApplyingModifiers(mods & NSEventModifierFlags::Shift)
                        .map(|s| s.to_string())
                })
                .unwrap_or_default()
                .to_lowercase();
            let mut combo = String::new();
            if mods.contains(NSEventModifierFlags::Command) {
                combo.push_str("cmd-");
            }
            if mods.contains(NSEventModifierFlags::Control) {
                combo.push_str("ctrl-");
            }
            if mods.contains(NSEventModifierFlags::Option) {
                combo.push_str("alt-");
            }
            if mods.contains(NSEventModifierFlags::Shift) {
                combo.push_str("shift-");
            }
            combo.push_str(super::model::native_shortcut_key(&key));
            let Ok(keystroke) = gpui::Keystroke::parse_with_os(&combo, crate::keys::OS) else {
                return event.as_ptr();
            };
            // Shortcuts are written in US keys and bound where the layout
            // types them (AZERTY ⌘1 is ⌘&): match the US spelling, then
            // forward the keystroke as typed.
            let us = crate::settings::bound_canonical_combo(&combo);
            let browser_key = matches!(
                us.as_deref(),
                Some(
                    "cmd-l"
                        | "cmd-t"
                        | "cmd-w"
                        | "cmd-["
                        | "cmd-]"
                        | "cmd-shift-r"
                        | "cmd-k"
                        | "cmd-,"
                )
            );
            let app_key = us
                .as_deref()
                .and_then(|us| gpui::Keystroke::parse_with_os(us, crate::keys::OS).ok())
                .is_some_and(|us| {
                    monitor_shortcuts.borrow().iter().any(|s| {
                        gpui::Keystroke::parse_with_os(s, crate::keys::OS).is_ok_and(|s| s == us)
                    })
                });
            if browser_key || app_key {
                if monitor_tx.try_send(NativeEvent::Key(keystroke)).is_ok() {
                    std::ptr::null_mut()
                } else {
                    event.as_ptr()
                }
            } else {
                event.as_ptr()
            }
        });
        // SAFETY: AppKit copies the block and keeps it until `Host::drop` removes the monitor;
        // the block answers each event with it, or with null to swallow it. Main thread.
        let monitor = unsafe {
            NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &callback)
        };
        // Wheel ticks AppKit routes to the page glide like the rest of the
        // window (`PageGlide`); a press anywhere takes over, as in GPUI.
        let wheel = Rc::new(PageGlide::default());
        let wheel_view = view.clone();
        let page_glide = Rc::clone(&wheel);
        let wheel_callback = block2::RcBlock::new(move |event: NonNull<NSEvent>| {
            // SAFETY: AppKit hands a local monitor a live event for the length of the call.
            let e = unsafe { event.as_ref() };
            if e.r#type() != NSEventType::ScrollWheel {
                page_glide.stop();
                return event.as_ptr();
            }
            if receives_wheel(&wheel_view, e, mtm) && page_glide.take(&wheel_view, e) {
                std::ptr::null_mut()
            } else {
                event.as_ptr()
            }
        });
        // SAFETY: as the key monitor above: AppKit keeps the block until `Host::drop` removes it.
        let wheel_monitor = unsafe {
            NSEvent::addLocalMonitorForEventsMatchingMask_handler(
                NSEventMask::ScrollWheel
                    | NSEventMask::LeftMouseDown
                    | NSEventMask::RightMouseDown
                    | NSEventMask::OtherMouseDown,
                &wheel_callback,
            )
        };
        Ok(Self(Rc::new(RefCell::new(Host {
            web,
            view,
            observer,
            native,
            background_color: None,
            monitor,
            wheel_monitor,
            wheel,
            shortcuts,
            presentation: Presentation::Hidden,
        }))))
    }

    pub fn handle(&self) -> Rc<RefCell<Host>> {
        Rc::clone(&self.0)
    }

    pub fn focus_chrome(&self) {
        self.0.borrow().focus_chrome();
    }

    pub fn set_shortcuts(&self, shortcuts: Vec<String>) {
        *self.0.borrow().shortcuts.borrow_mut() = shortcuts;
    }

    pub fn present(&self, presentation: Presentation) {
        self.0.borrow_mut().present(presentation);
    }

    pub fn load(&self, url: &str) -> Result<(), String> {
        let host = self.0.borrow();
        host.observer.ivars().error.borrow_mut().take();
        *host.observer.ivars().requested_url.borrow_mut() = Some(url.into());
        host.web.load_url(url).map_err(|e| e.to_string())
    }

    pub fn reload(&self) {
        let host = self.0.borrow();
        host.observer.ivars().error.borrow_mut().take();
        // Retrying the requested URL also works after a failed provisional load.
        // SAFETY: the page's own web view, told on the main thread it lives on.
        unsafe {
            host.view.reload();
        }
        host.observer.changed();
    }

    pub fn history(&self, forward: bool) {
        let host = self.0.borrow();
        if forward {
            // SAFETY: the page's own web view, told on the main thread it lives on.
            unsafe {
                host.view.goForward();
            }
        } else {
            // SAFETY: as above.
            unsafe {
                host.view.goBack();
            }
        }
    }

    pub fn state(&self) -> PageState {
        let host = self.0.borrow();
        host.observer.ivars().pending.set(false);
        let view = &host.view;
        // Each read is one of WKWebView's getters, on the main thread the page lives on.
        let url = host.observer.ivars().requested_url.borrow().clone().or_else(|| {
            // SAFETY: as said above.
            unsafe { view.URL() }.and_then(|u| u.absoluteString()).map(|u| u.to_string())
        });
        // SAFETY: as said above.
        let title = unsafe { view.title() }.map(|s| s.to_string()).unwrap_or_default();
        // SAFETY: as said above.
        let loading = unsafe { view.isLoading() };
        // SAFETY: as said above.
        let can_back = unsafe { view.canGoBack() };
        // SAFETY: as said above.
        let can_forward = unsafe { view.canGoForward() };
        PageState {
            url,
            title,
            loading,
            can_back,
            can_forward,
            error: host.observer.ivars().error.borrow().clone(),
        }
    }

    // The page reads its own icon in its own session and cookies, draws it to a
    // 64 px canvas and posts it as a PNG `data:` URL (zeron fetched the link with reqwest).
    pub fn discover_favicon(&self) {
        let host = self.0.borrow();
        // Our navigation delegate owns readiness, so Wry's private pending-
        // script queue is intentionally bypassed. This runs only after finish.
        // SAFETY: a script string and no completion handler, given to the page's own web view on
        // the main thread.
        unsafe {
            host.view
                .evaluateJavaScript_completionHandler(&NSString::from_str(FAVICON_SCRIPT), None);
        }
    }
}

// Finds the icon link (else `/favicon.ico`), fetches it as the page would, decodes it
// in WebKit (ICO and SVG included), and posts it once; then again whenever the page changes its
// icon link. Failures (no icon, CORS, CSP) post nothing and the tab keeps the globe. Where the
// canvas cannot be read back (`data:,`, as in a virtual Mac's web view), the icon's own bytes go
// instead, a small file only, for the shell to decode (`super::favicon_png`).
const FAVICON_SCRIPT: &str = r#"(() => {
  const ipc = window.ipc;
  if (!ipc) return;
  const send = async () => {
    const page = location.href;
    const link = document.querySelector('link[rel~="icon" i]');
    const href = link && link.href ? link.href : new URL('/favicon.ico', location.href).href;
    try {
      const response = await fetch(href, { credentials: 'include' });
      if (!response.ok) return;
      const blob = await response.blob();
      if (blob.size === 0 || blob.size > 1048576) return;
      const url = URL.createObjectURL(blob);
      try {
        const image = new Image();
        image.src = url;
        await image.decode();
        const side = 64;
        const w = image.naturalWidth || side, h = image.naturalHeight || side;
        const scale = Math.min(side / w, side / h);
        const canvas = document.createElement('canvas');
        canvas.width = side;
        canvas.height = side;
        canvas.getContext('2d').drawImage(
          image, (side - w * scale) / 2, (side - h * scale) / 2, w * scale, h * scale);
        let data = canvas.toDataURL('image/png');
        if (data.length <= 'data:,'.length) {
          if (blob.size > 262144) return;
          data = await new Promise((resolve, reject) => {
            const reader = new FileReader();
            reader.onload = () => resolve(reader.result);
            reader.onerror = () => reject(reader.error);
            reader.readAsDataURL(blob);
          });
        }
        ipc.postMessage(JSON.stringify({ sloptyFavicon: page, data }));
      } finally {
        URL.revokeObjectURL(url);
      }
    } catch (_) {}
  };
  send();
  if (window.__sloptyFaviconWatch || !document.head) return;
  let timer = 0;
  const watch = new MutationObserver((records) => {
    const icon = (node) => node.nodeName === 'LINK' && /(^|\s)icon(\s|$)/i.test(node.rel || '');
    if (!records.some((r) => icon(r.target) || [...r.addedNodes].some(icon))) return;
    clearTimeout(timer);
    timer = setTimeout(send, 250);
  });
  watch.observe(document.head, { childList: true, subtree: true, attributes: true, attributeFilter: ['href', 'rel'] });
  Object.defineProperty(window, '__sloptyFaviconWatch', { value: watch });
})();"#;

fn has_focus(view: &NSView) -> bool {
    view.window()
        .and_then(|w| w.firstResponder())
        .and_then(|r| r.downcast::<NSView>().ok())
        .is_some_and(|v| v.isDescendantOf(view))
}

/// `WebKit`'s pixels per wheel line: a 3-line tick scrolls a page 120px.
const PAGE_PX_PER_LINE: f64 = 40.0;

/// A page's mouse-wheel glide. AppKit hands the page its wheel directly, so
/// it never reaches GPUI's root (`smooth_scroll`), and `WebKit` applies a tick
/// at once. The wheel monitor holds ticks back here; a timer replays the
/// glide as precise pixel deltas, which `WebKit` routes like a trackpad's: to
/// the scroller under the pointer, clamped at its edges.
#[derive(Default)]
struct PageGlide {
    glide: RefCell<crate::smooth_scroll::Glide>,
    /// The pointer, in window coordinates, where the replay lands.
    at: Cell<objc2_foundation::NSPoint>,
    /// Pixels owed below a whole pixel, per axis.
    carry: Cell<(f64, f64)>,
    timer: RefCell<Option<Retained<objc2_foundation::NSTimer>>>,
}

impl PageGlide {
    /// Take a wheel event AppKit routes to the page: hold a tick back to
    /// glide it, or let anything else through, ending a glide in flight.
    fn take(self: &Rc<Self>, view: &Retained<WKWebView>, event: &NSEvent) -> bool {
        // Zoom and other modified gestures keep their discrete steps.
        let commanded = event.modifierFlags().intersects(
            NSEventModifierFlags::Control
                | NSEventModifierFlags::Option
                | NSEventModifierFlags::Command
                | NSEventModifierFlags::Function,
        );
        if event.hasPreciseScrollingDeltas() || commanded || !crate::smooth_scroll::gliding() {
            self.stop();
            return false;
        }
        let lines =
            gpui::point(wheel_lines(event.scrollingDeltaX()), wheel_lines(event.scrollingDeltaY()));
        if lines == gpui::Point::default() {
            return false;
        }
        self.push(view, lines, event.locationInWindow());
        true
    }

    fn push(
        self: &Rc<Self>,
        view: &Retained<WKWebView>,
        lines: gpui::Point<f32>,
        at: objc2_foundation::NSPoint,
    ) {
        self.at.set(at);
        self.glide.borrow_mut().push(lines, crate::clock::event_now());
        if self.timer.borrow().is_some() {
            return;
        }
        let glide = Rc::downgrade(self);
        let view = view.clone();
        let tick = block2::RcBlock::new(move |_: NonNull<objc2_foundation::NSTimer>| {
            if let Some(glide) = glide.upgrade() {
                glide.frame(&view);
            }
        });
        // A frame at 120Hz; the curve runs on the clock, not on ticks.
        // SAFETY: the run loop keeps the timer and its copied block until `stop` invalidates it;
        // the block holds the glide weakly. Main thread.
        let timer = unsafe {
            objc2_foundation::NSTimer::scheduledTimerWithTimeInterval_repeats_block(
                1.0 / 120.0,
                true,
                &tick,
            )
        };
        *self.timer.borrow_mut() = Some(timer);
    }

    fn frame(&self, view: &WKWebView) {
        let lines = if crate::smooth_scroll::gliding() {
            self.glide.borrow_mut().advance(crate::clock::event_now())
        } else {
            self.glide.borrow_mut().finish()
        };
        let (dx, dy, carry) = whole_pixels(self.carry.get(), lines);
        self.carry.set(carry);
        if (dx, dy) != (0, 0) {
            let height = NSScreen::screens(view.mtm())
                .firstObject()
                .map(|screen| screen.frame().size.height);
            if let Some(event) =
                height.and_then(|height| precise_wheel(dx, dy, self.at.get(), height))
            {
                view.scrollWheel(&event);
            }
        }
        if !self.glide.borrow().active() {
            self.stop();
        }
    }

    fn stop(&self) {
        self.glide.borrow_mut().stop();
        self.carry.set((0.0, 0.0));
        if let Some(timer) = self.timer.borrow_mut().take() {
            timer.invalidate();
        }
    }
}

/// A wheel event's delta in lines, as the glide takes it.
#[expect(clippy::cast_possible_truncation, reason = "a wheel moves a handful of lines")]
const fn wheel_lines(delta: f64) -> f32 {
    delta as f32
}

/// A slice of lines as whole page pixels, carrying the fractions to the next
/// slice, so a tick still scrolls exactly its 40px a line.
#[expect(clippy::cast_possible_truncation, reason = "a glide's slice is a few pixels, rounded")]
fn whole_pixels(carry: (f64, f64), lines: gpui::Point<f32>) -> (i32, i32, (f64, f64)) {
    let x = f64::from(lines.x).mul_add(PAGE_PX_PER_LINE, carry.0);
    let y = f64::from(lines.y).mul_add(PAGE_PX_PER_LINE, carry.1);
    let (dx, dy) = (x.round(), y.round());
    (dx as i32, dy as i32, (x - dx, y - dy))
}

/// A precise (trackpad-style) wheel event of whole pixels at `at`, a point
/// in a window. A synthesized `NSEvent` has no window and reports its
/// location, flipped by the primary screen's `height`, as `locationInWindow`,
/// which is the point `WebKit` hit-tests for the scroller: so it is placed
/// where `at` would be in a window at the screen's origin.
fn precise_wheel(
    dx: i32,
    dy: i32,
    at: objc2_foundation::NSPoint,
    height: f64,
) -> Option<Retained<NSEvent>> {
    use objc2_core_graphics::{CGEvent, CGEventField, CGScrollEventUnit};
    let event = CGEvent::new_scroll_wheel_event2(None, CGScrollEventUnit::Pixel, 2, dy, dx, 0)?;
    CGEvent::set_integer_value_field(Some(&*event), CGEventField::ScrollWheelEventIsContinuous, 1);
    CGEvent::set_location(Some(&*event), objc2_foundation::NSPoint::new(at.x, height - at.y));
    NSEvent::eventWithCGEvent(&event)
}

/// Whether AppKit routes `event` to `view`: the view under the pointer is it
/// or inside it.
fn receives_wheel(view: &NSView, event: &NSEvent, mtm: MainThreadMarker) -> bool {
    let Some(window) = view.window() else {
        return false;
    };
    if event.window(mtm).as_deref() != Some(&*window) {
        return false;
    }
    // The content view hit-tests points in its superview's space: the window's.
    window
        .contentView()
        .and_then(|content| content.hitTest(event.locationInWindow()))
        .is_some_and(|hit| hit.isDescendantOf(view))
}

impl Host {
    /// The page's own background under freshly exposed tiles, all that is left to sync:
    /// gpui-fast places, clips and orders the page (docs/composition.md).
    pub fn sync(&mut self) {
        // WebKit may fill newly exposed tiles a frame after a viewport change.
        // Match its page background beneath those tiles instead of exposing
        // the application's dark window background at the resize edge.
        // SAFETY: WKWebView's getter, on the main thread the page lives on.
        let color = unsafe { self.view.underPageBackgroundColor() };
        let unchanged = self.background_color.as_ref().is_some_and(|previous| {
            // SAFETY: NSObject's `isEqual:` between two live colors, answering a BOOL.
            unsafe { msg_send![&**previous, isEqual: &*color] }
        });
        if !unchanged {
            // WebKit's native backing otherwise uses controlBackgroundColor,
            // independently of the CSS page background. Exposed resize tiles
            // must use the page color while the remote content catches up.
            // SAFETY: WKWebView's private `_setBackgroundColor:` takes an NSColor and returns
            // nothing, as Safari's own views call it; main thread.
            let _: () = unsafe { msg_send![&*self.view, _setBackgroundColor: &*color] };
            self.background_color = Some(color);
        }
        self.update_visibility();
    }

    /// The host the page is placed by.
    pub fn native_host(&self) -> gpui::composition::NativeHost {
        self.native.clone()
    }

    /// Return the keyboard to GPUI if the page holds it.
    pub fn focus_chrome(&self) {
        if has_focus(&self.view) {
            // A refusal leaves the keyboard where it is.
            let _refused = self.web.focus_parent();
        }
    }

    fn update_visibility(&self) {
        // Where it shows is gpui-fast's to say; here only whether it does.
        let visible = self.presentation != Presentation::Hidden
            && self.observer.ivars().error.borrow().is_none();
        if !visible || self.presentation == Presentation::Passthrough {
            self.focus_chrome();
        }
        if self.view.isHidden() == visible {
            // A refusal leaves the page as it was; the next sync asks again.
            let _refused = self.web.set_visible(visible);
        }
    }

    fn present(&mut self, presentation: Presentation) {
        self.presentation = presentation;
        // Drags pass to GPUI through the native host's hit routing.
        self.update_visibility();
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.focus_chrome();
        for monitor in [self.monitor.take(), self.wheel_monitor.take()].into_iter().flatten() {
            // SAFETY: a monitor `NativePage::new` added, removed once, on the main thread.
            unsafe {
                NSEvent::removeMonitor(&monitor);
            }
        }
        for key in OBSERVED {
            // SAFETY: the observations `NativePage::new` added, removed while the observer
            // still lives. Main thread.
            unsafe {
                self.view.removeObserver_forKeyPath(&self.observer, &NSString::from_str(key));
            }
        }
        // SAFETY: clearing the delegate before the observer goes. Main thread.
        unsafe {
            self.view.setNavigationDelegate(None);
        }
        // SAFETY: the page's own web view, told on the main thread it lives on.
        unsafe {
            self.view.stopLoading();
        }
        self.wheel.stop();
        self.view.removeFromSuperview();
        // The native host takes its container away with its last clone.
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn a_tick_glides_in_whole_pixels_to_exactly_its_lines() {
        let mut glide = crate::smooth_scroll::Glide::default();
        let mut now = Instant::now();
        glide.push(gpui::point(-1.0, -3.0), now);
        let (mut carry, mut total, mut slices) = ((0.0, 0.0), (0, 0), 0);
        while glide.active() {
            now += Duration::from_millis(8);
            let (dx, dy, rest) = whole_pixels(carry, glide.advance(now));
            carry = rest;
            total = (total.0 + dx, total.1 + dy);
            slices += 1;
        }
        // WebKit's step for a line, spread over Chrome's 200ms.
        assert_eq!(total, (-40, -120));
        assert_eq!(slices, 25);
    }

    #[test]
    fn a_replay_is_precise_and_hit_tests_where_the_tick_was() {
        let height = objc2_core_graphics::CGDisplayBounds(objc2_core_graphics::CGMainDisplayID())
            .size
            .height;
        let at = objc2_foundation::NSPoint::new(212.0, 340.0);
        let event = precise_wheel(3, -14, at, height).unwrap();
        assert!(event.hasPreciseScrollingDeltas());
        assert_eq!((event.scrollingDeltaX(), event.scrollingDeltaY()), (3.0, -14.0));
        assert_eq!(event.locationInWindow(), at);
    }
}

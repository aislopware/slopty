// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! AppKit boundary for the browser. Wry owns the native child and its UI
//! delegate; our navigation delegate supplies browser policy and state. All
//! callbacks enqueue events, never re-enter GPUI. No page-to-engine IPC.
// slopty: one page-to-shell message only, the page's own favicon (`FAVICON_SCRIPT`).
use super::model::{PageState, Presentation, allowed_navigation};
use gpui::{App, Window}; // slopty: no `Bounds`/`Pixels`; gpui-fast places the page
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{
    DefinedClass, MainThreadMarker, MainThreadOnly, class, define_class, msg_send, sel,
};
use objc2_app_kit::{NSColor, NSEvent, NSEventMask, NSEventModifierFlags, NSView}; // slopty
use objc2_foundation::{
    NSDictionary, NSError, NSKeyValueChangeKey, NSKeyValueObservingOptions, NSObject,
    NSObjectNSKeyValueObserverRegistration, NSObjectProtocol, NSString,
};
use objc2_web_kit::{
    WKNavigation, WKNavigationAction, WKNavigationActionPolicy, WKNavigationDelegate,
    WKNavigationResponse, WKNavigationResponsePolicy, WKWebView,
};
use std::{
    cell::{Cell, RefCell},
    mem::ManuallyDrop,
    ptr::NonNull, // slopty: the page goes to gpui-fast's native host
    rc::Rc,
};
use wry::{WebView, WebViewBuilderExtMacos, WebViewExtMacOS};

#[derive(Default)]
struct BrowserStore {
    profile: Option<super::profile::BrowserProfile>,
    store: Option<Retained<objc2_web_kit::WKWebsiteDataStore>>,
    preview_hosts: std::collections::BTreeSet<String>,
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
                return Self(store.clone());
            }
            let root = profile.root.clone();
            let store = Rc::new(RefCell::new(BrowserStore {
                profile: Some(profile),
                ..Default::default()
            }));
            profiles.insert(root, store.clone());
            Self(store)
        })
    }
    fn configuration(
        &self,
        mtm: MainThreadMarker,
    ) -> Retained<objc2_web_kit::WKWebViewConfiguration> {
        let mut data = self.0.borrow_mut();
        if data.store.is_none() {
            data.store = Some(unsafe {
                match &data.profile {
                    Some(profile) if persistent_profiles_supported() => {
                        let identifier =
                            objc2_foundation::NSUUID::from_bytes(profile.data_store_identifier());
                        objc2_web_kit::WKWebsiteDataStore::dataStoreForIdentifier(&identifier, mtm)
                    }
                    _ => objc2_web_kit::WKWebsiteDataStore::nonPersistentDataStore(mtm),
                }
            });
            if let Err(error) =
                configure_preview_proxy(data.store.as_ref().unwrap(), &data.preview_hosts)
            {
                tracing::warn!(%error, "preview hostname proxy unavailable");
            }
        }
        let configuration = unsafe { objc2_web_kit::WKWebViewConfiguration::new(mtm) };
        unsafe {
            configuration.setWebsiteDataStore(data.store.as_ref().unwrap());
            // slopty: WebKit's bare user agent names no browser, and sites (Google's among them)
            // serve it their pages for old browsers; it says what Safari says.
            configuration
                .setApplicationNameForUserAgent(Some(&NSString::from_str(&safari_user_agent())));
        }
        configuration
    }
    pub(super) fn register_preview(&self, address: &str) {
        let Ok(url) = url::Url::parse(address) else {
            return;
        };
        let Some(host) = url.host_str() else {
            return;
        };
        if url.scheme() != "http"
            || url.port() != Some(slopty_view_proto::PREVIEW_PROXY_PORT)
            || !host.ends_with(".localhost")
        {
            return;
        }
        let mut data = self.0.borrow_mut();
        if !data.preview_hosts.insert(host.into()) {
            return;
        }
        if let Some(store) = &data.store {
            if let Err(error) = configure_preview_proxy(store, &data.preview_hosts) {
                tracing::warn!(%error, "preview hostname proxy unavailable");
            }
        }
    }
}

/// What Safari adds to WebKit's user agent (slopty): its version, read from the Safari on this
/// Mac, and the WebKit build Safari has said since it froze it.
fn safari_user_agent() -> String {
    const PLIST: &str = "/Applications/Safari.app/Contents/Info.plist";
    const KEY: &str = "<key>CFBundleShortVersionString</key>";
    let version = std::fs::read_to_string(PLIST).ok().and_then(|plist| {
        let after = &plist[plist.find(KEY)? + KEY.len()..];
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

/// Network.framework objects are Objective-C OS objects. Resolve the newer API
/// dynamically so older systems can still open ordinary browser tabs. Match
/// only discovered/opened preview hostnames; other websites retain normal routing.
fn configure_preview_proxy(
    store: &objc2_web_kit::WKWebsiteDataStore,
    hosts: &std::collections::BTreeSet<String>,
) -> Result<(), String> {
    if hosts.is_empty() {
        return Ok(());
    }
    use objc2_foundation::{NSArray, NSObject};
    use std::ffi::{CStr, CString, c_char};
    unsafe {
        let supported: bool = msg_send![store, respondsToSelector: sel!(setProxyConfigurations:)];
        if !supported {
            return Err("Automatic preview hostnames require macOS 14 or later".into());
        }
        static NETWORK: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
        let handle = *NETWORK.get_or_init(|| {
            libc::dlopen(
                c"/System/Library/Frameworks/Network.framework/Network".as_ptr(),
                libc::RTLD_LAZY,
            ) as usize
        });
        let symbol = |name: &CStr| -> Result<*mut std::ffi::c_void, String> {
            let value = libc::dlsym(handle as *mut _, name.as_ptr());
            if handle == 0 || value.is_null() {
                Err("Preview proxy API unavailable".into())
            } else {
                Ok(value)
            }
        };
        let endpoint: unsafe extern "C" fn(*const c_char, *const c_char) -> *mut NSObject =
            std::mem::transmute(symbol(c"nw_endpoint_create_host")?);
        let proxy: unsafe extern "C" fn(*mut NSObject, *mut NSObject) -> *mut NSObject =
            std::mem::transmute(symbol(c"nw_proxy_config_create_http_connect")?);
        let match_domain: unsafe extern "C" fn(*mut NSObject, *const c_char) =
            std::mem::transmute(symbol(c"nw_proxy_config_add_match_domain")?);
        let endpoint = Retained::from_raw(endpoint(c"127.0.0.1".as_ptr(), c"7331".as_ptr()))
            .ok_or("Could not create preview endpoint")?;
        let config = Retained::from_raw(proxy(
            Retained::as_ptr(&endpoint) as *mut _,
            std::ptr::null_mut(),
        ))
        .ok_or("Could not create preview proxy")?;
        for host in hosts {
            let host = CString::new(host.as_str()).map_err(|_| "Invalid preview hostname")?;
            match_domain(Retained::as_ptr(&config) as *mut _, host.as_ptr());
        }
        let proxies = NSArray::arrayWithObject(&*config);
        let _: () = msg_send![store, setProxyConfigurations: &*proxies];
    }
    Ok(())
}

type Sender = tokio::sync::mpsc::Sender<NativeEvent>;
const OBSERVED: [&str; 6] = [
    "URL",
    "title",
    "loading",
    "canGoBack",
    "canGoForward",
    "underPageBackgroundColor",
];

pub(super) enum NativeEvent {
    Changed,
    Finished,
    NewTab(String),
    Key(gpui::Keystroke),
    // slopty: the icon itself, read inside the page as a PNG `data:` URL (no HTTP client of
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
            let url = unsafe { action.request().URL() }.and_then(|u| u.absoluteString()).map(|u| u.to_string()).unwrap_or_default();
            if allowed_navigation(&url) && unsafe { action.targetFrame() }.is_some_and(|frame| unsafe { frame.isMainFrame() }) {
                *self.ivars().requested_url.borrow_mut() = Some(url.clone());
            }
            decision.call((if allowed_navigation(&url) { WKNavigationActionPolicy::Allow } else { WKNavigationActionPolicy::Cancel },));
        }
        #[unsafe(method(webView:decidePolicyForNavigationResponse:decisionHandler:))]
        fn response(&self, _view: &WKWebView, response: &WKNavigationResponse, decision: &block2::Block<dyn Fn(WKNavigationResponsePolicy)>) {
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
            self.changed(); let _ = self.ivars().tx.try_send(NativeEvent::Finished);
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
        unsafe { msg_send![super(object), init] }
    }
    fn changed(&self) {
        if !self.ivars().pending.replace(true) {
            if self.ivars().tx.try_send(NativeEvent::Changed).is_err() {
                self.ivars().pending.set(false);
            }
        }
    }
    fn fail(&self, message: &str) {
        *self.ivars().error.borrow_mut() = Some(message.into());
        self.changed();
    }
}

// slopty: gpui-fast natives under one layer (docs/composition.md). Upstream's clip view, which
// cut the page to GPUI's paint mask and passed drags back to GPUI, is cut: the native host
// cuts the hole, clips and routes hits and focus.

pub(super) struct NativePage(Rc<RefCell<Host>>);

pub(super) struct Host {
    web: WebView,
    data: BrowserData,
    view: Retained<WKWebView>,
    observer: Retained<Observer>,
    // slopty: gpui-fast's native host replaces the clip view, its mask and its parent.
    native: gpui::composition::NativeHost,
    background_color: Option<Retained<NSColor>>,
    monitor: Option<Retained<AnyObject>>,
    shortcuts: Rc<RefCell<Vec<String>>>,
    presentation: Presentation,
    tx: Sender,
    #[cfg(feature = "browser-fixture")]
    visibility_changes: Cell<u64>,
}

impl NativePage {
    // slopty: gpui-fast natives under one layer (docs/composition.md): the host needs the
    // window mutably and the app.
    pub fn new(
        window: &mut Window,
        data: &BrowserData,
        tx: Sender,
        cx: &mut App,
    ) -> Result<Self, String> {
        let mtm = MainThreadMarker::new().ok_or("Browser must be created on the main thread")?;
        let new_tab = tx.clone();
        // slopty: the page's favicon, posted by `FAVICON_SCRIPT`; anything else is dropped.
        let favicon = tx.clone();
        let web = wry::WebViewBuilder::new()
            .with_webview_configuration(data.configuration(mtm))
            .with_visible(false)
            .with_focused(false)
            .with_new_window_req_handler(move |url, _| {
                if allowed_navigation(&url) {
                    let _ = new_tab.try_send(NativeEvent::NewTab(url));
                }
                wry::NewWindowResponse::Deny
            })
            .with_download_started_handler(|_, _| false)
            .with_ipc_handler(move |request| {
                if let Some((page, data)) = super::favicon_message(request.body()) {
                    let _ = favicon.try_send(NativeEvent::Favicon { page, data });
                }
            })
            .build_as_child(window)
            .map_err(|e| e.to_string())?;
        let view = Retained::into_super(web.webview());
        // slopty: gpui-fast natives under one layer (docs/composition.md).
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
        unsafe {
            view.setNavigationDelegate(Some(ProtocolObject::from_ref(&*observer)));
            for key in OBSERVED {
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
        let monitor_tx = tx.clone();
        let monitor_shortcuts = shortcuts.clone();
        let callback = block2::RcBlock::new(move |event: std::ptr::NonNull<NSEvent>| {
            let e = unsafe { event.as_ref() };
            if monitor_view.isHidden()
                || !has_focus(&monitor_view)
                || e.window(mtm) != monitor_view.window()
            {
                return event.as_ptr();
            }
            let mods = e.modifierFlags();
            let key = e
                .charactersIgnoringModifiers()
                .map(|s| s.to_string().to_lowercase())
                .unwrap_or_default();
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
            let Ok(keystroke) = gpui::Keystroke::parse(&combo) else {
                return event.as_ptr();
            };
            let browser_key = matches!(
                combo.as_str(),
                "cmd-l" | "cmd-t" | "cmd-w" | "cmd-[" | "cmd-]" | "cmd-shift-r" | "cmd-k" | "cmd-,"
            );
            let app_key = monitor_shortcuts
                .borrow()
                .iter()
                .any(|s| gpui::Keystroke::parse(s).is_ok_and(|s| s == keystroke));
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
        let monitor = unsafe {
            NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &callback)
        };
        Ok(Self(Rc::new(RefCell::new(Host {
            data: data.clone(),
            web,
            view,
            observer,
            native,
            background_color: None,
            monitor,
            shortcuts,
            presentation: Presentation::Hidden,
            #[cfg(feature = "browser-fixture")]
            visibility_changes: Cell::new(0),
            tx,
        }))))
    }
    pub fn handle(&self) -> Rc<RefCell<Host>> {
        self.0.clone()
    }
    pub fn focus_chrome(&self) {
        let _ = self.0.borrow().web.focus_parent();
    }
    pub fn set_shortcuts(&self, shortcuts: Vec<String>) {
        *self.0.borrow().shortcuts.borrow_mut() = shortcuts;
    }
    pub fn present(&mut self, presentation: Presentation) {
        self.0.borrow_mut().present(presentation);
    }
    pub fn load(&self, url: &str) -> Result<(), String> {
        let host = self.0.borrow();
        host.data.register_preview(url);
        host.observer.ivars().error.borrow_mut().take();
        *host.observer.ivars().requested_url.borrow_mut() = Some(url.into());
        host.web.load_url(url).map_err(|e| e.to_string())
    }
    pub fn reload(&self) {
        let host = self.0.borrow();
        host.observer.ivars().error.borrow_mut().take();
        // Retrying the requested URL also works after a failed provisional load.
        unsafe {
            host.view.reload();
        }
        host.observer.changed();
    }
    pub fn history(&self, forward: bool) {
        let host = self.0.borrow();
        unsafe {
            if forward {
                host.view.goForward();
            } else {
                host.view.goBack();
            }
        }
    }
    pub fn state(&self) -> PageState {
        let host = self.0.borrow();
        host.observer.ivars().pending.set(false);
        unsafe {
            PageState {
                url: host
                    .observer
                    .ivars()
                    .requested_url
                    .borrow()
                    .clone()
                    .or_else(|| {
                        host.view
                            .URL()
                            .and_then(|u| u.absoluteString())
                            .map(|u| u.to_string())
                    }),
                title: host.view.title().map(|s| s.to_string()).unwrap_or_default(),
                loading: host.view.isLoading(),
                can_back: host.view.canGoBack(),
                can_forward: host.view.canGoForward(),
                error: host.observer.ivars().error.borrow().clone(),
            }
        }
    }
    // slopty: the page reads its own icon in its own session and cookies, draws it to a
    // 64 px canvas and posts it as a PNG `data:` URL (zeron fetched the link with reqwest).
    pub fn discover_favicon(&self) {
        let host = self.0.borrow();
        // Our navigation delegate owns readiness, so Wry's private pending-
        // script queue is intentionally bypassed. This runs only after finish.
        unsafe {
            host.view.evaluateJavaScript_completionHandler(
                &NSString::from_str(FAVICON_SCRIPT),
                None,
            );
        }
    }
}

// slopty: finds the icon link (else `/favicon.ico`), fetches it as the page would, decodes it
// in WebKit (ICO and SVG included), and posts it once; then again whenever the page changes its
// icon link. Failures (no icon, CORS, CSP) post nothing and the tab keeps the globe.
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
        ipc.postMessage(JSON.stringify({ sloptyFavicon: page, data: canvas.toDataURL('image/png') }));
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

impl Host {
    /// slopty: the page's own background under freshly exposed tiles, all that is left to sync:
    /// gpui-fast places, clips and orders the page (docs/composition.md).
    pub fn sync(&mut self) {
        // WebKit may fill newly exposed tiles a frame after a viewport change.
        // Match its page background beneath those tiles instead of exposing
        // the application's dark window background at the resize edge.
        unsafe {
            let color = self.view.underPageBackgroundColor();
            let unchanged = self
                .background_color
                .as_ref()
                .is_some_and(|previous| msg_send![&**previous, isEqual: &*color]);
            if !unchanged {
                // WebKit's native backing otherwise uses controlBackgroundColor,
                // independently of the CSS page background. Exposed resize tiles
                // must use the page color while the remote content catches up.
                let _: () = msg_send![&*self.view, _setBackgroundColor: &*color];
                self.background_color = Some(color);
            }
        }
        self.update_visibility();
    }
    /// slopty: the host the page is placed by.
    pub fn native_host(&self) -> gpui::composition::NativeHost {
        self.native.clone()
    }
    fn update_visibility(&self) {
        // slopty: where it shows is gpui-fast's to say; here only whether it does.
        let visible = self.presentation != Presentation::Hidden
            && self.observer.ivars().error.borrow().is_none();
        if (!visible || self.presentation == Presentation::Passthrough) && has_focus(&self.view) {
            let _ = self.web.focus_parent();
        }
        if self.view.isHidden() == visible {
            #[cfg(feature = "browser-fixture")]
            self.visibility_changes
                .set(self.visibility_changes.get() + 1);
            let _ = self.web.set_visible(visible);
        }
    }
    fn present(&mut self, presentation: Presentation) {
        self.presentation = presentation;
        // slopty: drags pass to GPUI through the native host's hit routing.
        self.update_visibility();
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        if has_focus(&self.view) {
            let _ = self.web.focus_parent();
        }
        unsafe {
            if let Some(monitor) = self.monitor.take() {
                NSEvent::removeMonitor(&monitor);
            }
            for key in OBSERVED {
                self.view
                    .removeObserver_forKeyPath(&self.observer, &NSString::from_str(key));
            }
            self.view.setNavigationDelegate(None);
            self.view.stopLoading();
        }
        self.view.removeFromSuperview();
        // slopty: the native host takes its container away with its last clone.
    }
}

#[cfg(feature = "browser-fixture")]
impl NativePage {
    pub fn fixture_move_cursor(&self, x: f64, y: f64) {
        #[link(name = "CoreGraphics", kind = "framework")]
        unsafe extern "C" {
            fn CGWarpMouseCursorPosition(point: objc2_foundation::NSPoint) -> i32;
        }
        let host = self.0.borrow();
        let window = host.view.window().unwrap();
        let height = host.parent.clone().bounds().size.height;
        let p = window.convertPointToScreen(objc2_foundation::NSPoint::new(x, height - y));
        let screen = objc2_app_kit::NSScreen::mainScreen(host.view.mtm()).unwrap();
        unsafe {
            CGWarpMouseCursorPosition(objc2_foundation::NSPoint::new(
                p.x,
                screen.frame().size.height - p.y,
            ));
        }
    }
    pub fn fixture_backdrop_layers(&self) -> String {
        unsafe fn describe(layer: *mut AnyObject, depth: usize, out: &mut String) {
            if layer.is_null() || depth > 8 {
                return;
            }
            unsafe {
                let desc: Retained<NSString> = msg_send![layer, description];
                out.push_str(&format!("{}{}\n", " ".repeat(depth), desc));
                let layers: *mut AnyObject = msg_send![layer, sublayers];
                if !layers.is_null() {
                    let count: usize = msg_send![layers, count];
                    for i in 0..count.min(30) {
                        let child: *mut AnyObject = msg_send![layers, objectAtIndex:i];
                        describe(child, depth + 1, out);
                    }
                }
            }
        }
        let host = self.0.borrow();
        let mut out = String::new();
        for view in host.parent.subviews() {
            if view.class().name() == c"GPUIBackdropView" {
                unsafe {
                    let layer: *mut AnyObject = msg_send![&*view, layer];
                    describe(layer, 0, &mut out);
                }
            }
        }
        out
    }
    pub fn fixture_page_hit(&self, x: f64, y: f64) -> bool {
        let host = self.0.borrow();
        let point = objc2_foundation::NSPoint::new(x, host.parent.bounds().size.height - y);
        host.parent
            .hitTest(point)
            .is_some_and(|hit| hit.isDescendantOf(&host.clip))
    }
    pub fn fixture_backdrops(&self) -> Vec<(f64, f64, f64, f64)> {
        let host = self.0.borrow();
        host.parent
            .subviews()
            .iter()
            .filter(|view| view.class().name() == c"GPUIBackdropView" && !view.isHidden())
            .map(|view| {
                let r = view.frame();
                (
                    r.origin.x,
                    host.parent.bounds().size.height - r.origin.y - r.size.height,
                    r.size.width,
                    r.size.height,
                )
            })
            .collect()
    }
    pub fn fixture_geometry(&self) -> (f32, f32, f32) {
        let host = self.0.borrow();
        (
            host.bounds.unwrap().size.width.into(),
            host.view.frame().size.width as f32,
            host.visible_bounds.unwrap().size.width.into(),
        )
    }
    pub fn fixture_origin(&self) -> (f32, f32) {
        let b = self.0.borrow().bounds.unwrap();
        (b.origin.x.into(), b.origin.y.into())
    }
    pub fn fixture_overlay_visible(&self) -> bool {
        let host = self.0.borrow();
        host.parent
            .clone()
            .subviews()
            .iter()
            .any(|v| v.class().name() == c"GPUIOverlayView" && !v.isHidden())
    }
    pub fn fixture_visibility_changes(&self) -> u64 {
        let host = self.0.borrow();
        host.visibility_changes.get()
    }
    pub fn fixture_focus(&self) {
        let _ = self.0.borrow().web.focus();
    }
    pub fn fixture_focused(&self) -> bool {
        has_focus(&self.0.borrow().view)
    }
    pub fn fixture_overlay_at(&self, x: f64, y: f64) -> bool {
        let host = self.0.borrow();
        let parent = host.parent.clone();
        let point = objc2_foundation::NSPoint::new(
            x,
            if parent.isFlipped() {
                y
            } else {
                parent.bounds().size.height - y
            },
        );
        parent
            .hitTest(point)
            .is_some_and(|hit| hit.class().name() == c"GPUIOverlayView")
    }
    pub fn fixture_click(&self, x: f64, y: f64) {
        self.fixture_move_cursor(x, y);
        let host = self.0.borrow();
        let parent = host.parent.clone();
        let window = host.view.window().unwrap();
        let point = objc2_foundation::NSPoint::new(x, parent.bounds().size.height - y);
        let app = objc2_app_kit::NSApplication::sharedApplication(host.view.mtm());
        for kind in [
            objc2_app_kit::NSEventType::LeftMouseDown,
            objc2_app_kit::NSEventType::LeftMouseUp,
        ] {
            let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(kind, point, NSEventModifierFlags::empty(), 0., window.windowNumber(), None, 0, 1, 1.).unwrap();
            app.postEvent_atStart(&event, false);
        }
    }
    pub fn fixture_visible(&self) -> bool {
        !self.0.borrow().view.isHidden()
    }
    pub fn fixture_cookies(&self, completion: impl FnOnce(Vec<String>) + 'static) {
        use objc2_foundation::{NSArray, NSHTTPCookie};
        let completion = RefCell::new(Some(completion));
        let block = block2::RcBlock::new(move |cookies: std::ptr::NonNull<NSArray<NSHTTPCookie>>| {
            let cookies = unsafe { cookies.as_ref() };
            let values = cookies.iter().map(|cookie| format!(
                "{}={}; domain={}; path={}; httpOnly={}; sessionOnly={}",
                cookie.name(), cookie.value(), cookie.domain(), cookie.path(),
                cookie.isHTTPOnly(), cookie.isSessionOnly()
            )).collect();
            if let Some(completion) = completion.borrow_mut().take() {
                completion(values);
            }
        });
        unsafe {
            self.0.borrow().view.configuration().websiteDataStore().httpCookieStore().getAllCookies(&block);
        }
    }

    pub fn fixture_eval(&self, script: &str) {
        unsafe {
            self.0
                .borrow()
                .view
                .evaluateJavaScript_completionHandler(&NSString::from_str(script), None);
        }
    }
}

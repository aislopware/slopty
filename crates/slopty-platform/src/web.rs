//! The browser tile's page: a `WKWebView` over the GPUI window (macOS and iOS).
//!
//! GPUI draws every tile into one Metal layer, so a web page cannot be one of its elements.
//! It is a native view instead: a clipping view (the strip's area) inside the GPUI view, and
//! the web view inside that, placed over the tile's body every frame the strip draws. A
//! native view always draws above GPUI, so the caller hides it whenever something GPUI
//! draws should be on top (an overlay, the overview, the tile scrolled away).
//!
//! One delegate object answers `WebKit` for a page: its navigations, its UI (pop-ups and a
//! script's dialogs) and its downloads. The UI and download protocols are spoken by selector:
//! the bindings type much of `WKUIDelegate` for macOS only, and `WebKit` asks a delegate
//! `respondsToSelector:` for each method rather than its conformance.
//!
//! The keyboard differs by platform; each module says how.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::rc::Rc;

use block2::{DynBlock, RcBlock};
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, NSObjectProtocol};
use objc2::{
    DefinedClass as _, MainThreadMarker, MainThreadOnly, Message as _, define_class, msg_send,
};
use objc2_foundation::{
    NSDictionary, NSError, NSNumber, NSObject, NSProgress, NSString, NSURL, NSURLRequest,
};
use objc2_web_kit::{
    WKNavigation, WKNavigationActionPolicy, WKNavigationDelegate, WKNavigationResponsePolicy,
};

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
#[derive(Debug)]
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
    /// A pop-up (`window.open`) or a `target=_blank` link asked for a new window on this
    /// address, as the page names it. No window is made here; the tile opens another.
    Open(String),
    /// A script's `alert`, `confirm` or `prompt`. The page waits until it is answered.
    Dialog(Dialog),
    /// A download began, finished or failed.
    Download(Download),
    /// A find in the page answered: whether the text is on it.
    Found(bool),
    /// How many times `needle` is in the page's text, as a count asked for it answered.
    Counted {
        /// What was counted.
        needle: String,
        /// How many times it is there.
        count: usize,
    },
    /// The page closed its window (`window.close()`): its tile goes.
    Closed,
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
    /// There is a page to go forward to.
    pub can_go_forward: bool,
}

type Sink = Rc<dyn Fn(WebEvent)>;

/// Which of a script's dialogs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DialogKind {
    /// `alert`: a message and OK.
    Alert,
    /// `confirm`: a message, OK and Cancel.
    Confirm,
    /// `prompt`: a message, a field that starts at `default`, OK and Cancel.
    Prompt {
        /// What the field holds at first.
        default: String,
    },
}

/// A script's dialog, waiting for its answer.
///
/// Answered once: [`Dialog::accept`], [`Dialog::dismiss`], or dropped, which dismisses it,
/// since `WebKit` holds the page until its handler runs and raises if it never does.
pub struct Dialog {
    kind: DialogKind,
    message: String,
    reply: Option<Box<dyn FnOnce(Option<String>)>>,
}

impl std::fmt::Debug for Dialog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dialog")
            .field("kind", &self.kind)
            .field("message", &self.message)
            .finish_non_exhaustive()
    }
}

impl Dialog {
    /// A dialog whose answer goes to `reply`: the text for OK (a prompt's field; empty for the
    /// others), `None` for Cancel.
    pub fn new(
        kind: DialogKind,
        message: String,
        reply: impl FnOnce(Option<String>) + 'static,
    ) -> Self {
        Self { kind, message, reply: Some(Box::new(reply)) }
    }

    /// Which dialog.
    #[must_use]
    pub const fn kind(&self) -> &DialogKind {
        &self.kind
    }

    /// What the script says.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// OK, with `text` as a prompt's answer.
    pub fn accept(mut self, text: &str) {
        if let Some(reply) = self.reply.take() {
            reply(Some(text.to_owned()));
        }
    }

    /// Cancel (an alert's only way out is OK, which this is too).
    pub fn dismiss(mut self) {
        if let Some(reply) = self.reply.take() {
            reply(None);
        }
    }
}

impl Drop for Dialog {
    fn drop(&mut self) {
        if let Some(reply) = self.reply.take() {
            reply(None);
        }
    }
}

/// A download's news. `id` names it here and to [`WebView::received`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Download {
    /// It is being saved at `path`.
    Started {
        /// This download, among the page's.
        id: u64,
        /// Where it lands: a name of its own in [`downloads_dir`].
        path: PathBuf,
    },
    /// All of it is at its path.
    Finished {
        /// This download.
        id: u64,
    },
    /// It stopped short; the system's word for why.
    Failed {
        /// This download.
        id: u64,
        /// Why.
        why: String,
    },
}

/// Where a page's downloads land on this device, under `home`: `~/Downloads` on a Mac; on
/// iOS the app's own `Documents`, the one folder of its sandbox the Files app can be shown.
#[must_use]
pub fn downloads_dir(home: &Path) -> PathBuf {
    if cfg!(target_os = "ios") { home.join("Documents") } else { home.join("Downloads") }
}

/// A path in `dir` for a download the server calls `suggested`, which no file `taken` has.
///
/// The name's last component with the separators a file system refuses made dashes, then
/// `name (2).ext`, `name (3).ext`… as Safari numbers them.
#[must_use]
pub fn unique_path(dir: &Path, suggested: &str, taken: impl Fn(&Path) -> bool) -> PathBuf {
    let base = suggested.rsplit(['/', '\\']).next().unwrap_or_default();
    let base: String =
        base.chars().map(|c| if c == ':' || c.is_control() { '-' } else { c }).collect();
    let base = base.trim().trim_start_matches('.');
    let base = if base.is_empty() { "download" } else { base };
    let first = dir.join(base);
    if !taken(&first) {
        return first;
    }
    let (stem, ext) = match base.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, Some(ext)),
        _ => (base, None),
    };
    (2_u32..10_000)
        .map(|n| match ext {
            Some(ext) => dir.join(format!("{stem} ({n}).{ext}")),
            None => dir.join(format!("{stem} ({n})")),
        })
        .find(|p| !taken(p))
        .unwrap_or(first)
}

/// Show `path` selected in a Finder window.
#[cfg(target_os = "macos")]
pub fn reveal(path: &Path) {
    let path = NSString::from_str(&path.to_string_lossy());
    let workspace = objc2_app_kit::NSWorkspace::sharedWorkspace();
    let shown = workspace.selectFile_inFileViewerRootedAtPath(Some(&path), &NSString::new());
    tracing::debug!(shown, "reveal a download");
}

/// Whether a `Content-Disposition` header asks for the response to be saved, not shown.
#[must_use]
pub fn is_attachment(disposition: Option<&str>) -> bool {
    disposition.is_some_and(|d| {
        d.split(';').next().is_some_and(|kind| kind.trim().eq_ignore_ascii_case("attachment"))
    })
}

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
    /// Where downloads land.
    dir: PathBuf,
    /// The downloads under way, each with its id.
    downloads: RefCell<Vec<(u64, Retained<AnyObject>)>>,
    next_download: Cell<u64>,
    /// A navigation just turned into a download: the failure `WebKit` reports for the page's
    /// load that ends there is not the page's.
    became_download: Cell<bool>,
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

    // The web view and WebKit's other objects come in as plain objects: the bindings type
    // `WKWebView` for macOS only, and the delegate asks them for what it needs by selector.
    unsafe impl WKNavigationDelegate for Delegate {
        #[unsafe(method(webView:didStartProvisionalNavigation:))]
        fn started(&self, _web: &AnyObject, _navigation: Option<&WKNavigation>) {
            self.ivars().became_download.set(false);
        }

        #[unsafe(method(webView:didFinishNavigation:))]
        fn finished(&self, _web: &AnyObject, _navigation: Option<&WKNavigation>) {
            (self.ivars().sink)(WebEvent::Loaded);
        }

        #[unsafe(method(webView:didFailProvisionalNavigation:withError:))]
        fn failed_early(&self, _web: &AnyObject, _nav: Option<&WKNavigation>, error: &NSError) {
            self.failed_load(error);
        }

        #[unsafe(method(webView:didFailNavigation:withError:))]
        fn failed(&self, _web: &AnyObject, _nav: Option<&WKNavigation>, error: &NSError) {
            self.failed_load(error);
        }

        #[unsafe(method(webView:decidePolicyForNavigationAction:decisionHandler:))]
        fn decide_action(
            &self,
            _web: &AnyObject,
            action: &AnyObject,
            decide: &DynBlock<dyn Fn(WKNavigationActionPolicy)>,
        ) {
            // SAFETY: WebKit rule: `shouldPerformDownload` is a BOOL property of every
            // `WKNavigationAction` (a link with a `download` attribute sets it).
            let download: bool = unsafe { msg_send![action, shouldPerformDownload] };
            self.ivars().became_download.set(download);
            decide.call((if download {
                WKNavigationActionPolicy::Download
            } else {
                WKNavigationActionPolicy::Allow
            },));
        }

        #[unsafe(method(webView:decidePolicyForNavigationResponse:decisionHandler:))]
        fn decide_response(
            &self,
            _web: &AnyObject,
            response: &AnyObject,
            decide: &DynBlock<dyn Fn(WKNavigationResponsePolicy)>,
        ) {
            // SAFETY: WebKit rule: `canShowMIMEType` is a BOOL property of every
            // `WKNavigationResponse`.
            let shows: bool = unsafe { msg_send![response, canShowMIMEType] };
            let download = !shows || is_attachment(disposition(response).as_deref());
            self.ivars().became_download.set(download);
            decide.call((if download {
                WKNavigationResponsePolicy::Download
            } else {
                WKNavigationResponsePolicy::Allow
            },));
        }

        #[unsafe(method(webView:navigationAction:didBecomeDownload:))]
        fn action_became_download(&self, _web: &AnyObject, _action: &AnyObject, download: &AnyObject) {
            self.adopt(download);
        }

        #[unsafe(method(webView:navigationResponse:didBecomeDownload:))]
        fn response_became_download(
            &self,
            _web: &AnyObject,
            _response: &AnyObject,
            download: &AnyObject,
        ) {
            self.adopt(download);
        }
    }

    // `WKUIDelegate` and `WKDownloadDelegate`, by selector (the module's note says why).
    impl Delegate {
        /// A pop-up or a `_blank` link. No web view is made (nil): the tile opens its own
        /// with the address.
        #[unsafe(method(webView:createWebViewWithConfiguration:forNavigationAction:windowFeatures:))]
        fn create_web_view(
            &self,
            _web: &AnyObject,
            _config: &AnyObject,
            action: &AnyObject,
            _features: &AnyObject,
        ) -> *mut AnyObject {
            // SAFETY: WebKit rule: `request` is an `NSURLRequest` property of every
            // `WKNavigationAction`.
            let request: Option<Retained<NSURLRequest>> = unsafe { msg_send![action, request] };
            let url = request.and_then(|r| r.URL()).and_then(|u| u.absoluteString());
            if let Some(url) = url {
                (self.ivars().sink)(WebEvent::Open(url.to_string()));
            }
            std::ptr::null_mut()
        }

        /// The page closed its own window: a pop-up done with, or a page that had only one.
        #[unsafe(method(webViewDidClose:))]
        fn closed(&self, _web: &AnyObject) {
            (self.ivars().sink)(WebEvent::Closed);
        }

        #[unsafe(method(webView:runJavaScriptAlertPanelWithMessage:initiatedByFrame:completionHandler:))]
        fn alert(&self, _web: &AnyObject, message: &NSString, _frame: &AnyObject, done: &DynBlock<dyn Fn()>) {
            let done = done.copy();
            self.dialog(DialogKind::Alert, message, move |_| done.call(()));
        }

        #[unsafe(method(webView:runJavaScriptConfirmPanelWithMessage:initiatedByFrame:completionHandler:))]
        fn confirm(
            &self,
            _web: &AnyObject,
            message: &NSString,
            _frame: &AnyObject,
            done: &DynBlock<dyn Fn(Bool)>,
        ) {
            let done = done.copy();
            self.dialog(DialogKind::Confirm, message, move |answer| {
                done.call((Bool::new(answer.is_some()),));
            });
        }

        #[unsafe(method(webView:runJavaScriptTextInputPanelWithPrompt:defaultText:initiatedByFrame:completionHandler:))]
        fn prompt(
            &self,
            _web: &AnyObject,
            message: &NSString,
            default: Option<&NSString>,
            _frame: &AnyObject,
            done: &DynBlock<dyn Fn(*mut NSString)>,
        ) {
            let done = done.copy();
            let default = default.map(ToString::to_string).unwrap_or_default();
            self.dialog(DialogKind::Prompt { default }, message, move |answer| match answer {
                Some(text) => {
                    let text = NSString::from_str(&text);
                    done.call((Retained::as_ptr(&text).cast_mut(),));
                }
                None => done.call((std::ptr::null_mut(),)),
            });
        }

        #[unsafe(method(download:decideDestinationUsingResponse:suggestedFilename:completionHandler:))]
        fn destination(
            &self,
            download: &AnyObject,
            _response: &AnyObject,
            suggested: &NSString,
            done: &DynBlock<dyn Fn(*mut NSURL)>,
        ) {
            let Some(id) = self.download_id(download) else {
                // A nil destination cancels the download.
                done.call((std::ptr::null_mut(),));
                return;
            };
            let dir = &self.ivars().dir;
            if let Err(error) = std::fs::create_dir_all(dir) {
                self.forget(id);
                done.call((std::ptr::null_mut(),));
                (self.ivars().sink)(WebEvent::Download(Download::Failed { id, why: error.to_string() }));
                return;
            }
            let path = unique_path(dir, &suggested.to_string(), Path::exists);
            let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
            done.call((Retained::as_ptr(&url).cast_mut(),));
            (self.ivars().sink)(WebEvent::Download(Download::Started { id, path }));
        }

        #[unsafe(method(downloadDidFinish:))]
        fn download_finished(&self, download: &AnyObject) {
            if let Some(id) = self.download_id(download) {
                self.forget(id);
                (self.ivars().sink)(WebEvent::Download(Download::Finished { id }));
            }
        }

        #[unsafe(method(download:didFailWithError:resumeData:))]
        fn download_failed(&self, download: &AnyObject, error: &NSError, _resume: Option<&AnyObject>) {
            if let Some(id) = self.download_id(download) {
                self.forget(id);
                let why = error.localizedDescription().to_string();
                (self.ivars().sink)(WebEvent::Download(Download::Failed { id, why }));
            }
        }
    }
);

impl Delegate {
    fn new(sink: Sink, mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(DelegateIvars {
            sink,
            dir: downloads_dir(&crate::dirs::home()),
            downloads: RefCell::default(),
            next_download: Cell::new(1),
            became_download: Cell::new(false),
        });
        // SAFETY: `NSObject`'s `init` on a freshly allocated instance with ivars set.
        unsafe { msg_send![super(this), init] }
    }

    fn failed_load(&self, error: &NSError) {
        if self.ivars().became_download.replace(false) {
            return;
        }
        (self.ivars().sink)(WebEvent::Failed(error.localizedDescription().to_string()));
    }

    fn dialog(
        &self,
        kind: DialogKind,
        message: &NSString,
        reply: impl FnOnce(Option<String>) + 'static,
    ) {
        (self.ivars().sink)(WebEvent::Dialog(Dialog::new(kind, message.to_string(), reply)));
    }

    /// Take a new download on: this delegate hears how it goes, and it gets an id.
    fn adopt(&self, download: &AnyObject) {
        let ivars = self.ivars();
        let id = ivars.next_download.get();
        ivars.next_download.set(id.wrapping_add(1));
        // SAFETY: WebKit rule: a `WKDownload`'s delegate is any object answering the
        // `WKDownloadDelegate` selectors, held weakly; the page's view keeps this one, and
        // cancels its downloads before it goes (`cancel_downloads`).
        let () = unsafe { msg_send![download, setDelegate: self] };
        ivars.downloads.borrow_mut().push((id, download.retain()));
    }

    fn download_id(&self, download: &AnyObject) -> Option<u64> {
        let downloads = self.ivars().downloads.borrow();
        downloads.iter().find(|(_, d)| std::ptr::eq(&raw const **d, download)).map(|(id, _)| *id)
    }

    fn forget(&self, id: u64) {
        self.ivars().downloads.borrow_mut().retain(|(i, _)| *i != id);
    }

    /// How much of download `id` has come, and of how much (0 while that is unknown).
    fn received(&self, id: u64) -> Option<(u64, u64)> {
        let downloads = self.ivars().downloads.borrow();
        let (_, download) = downloads.iter().find(|(i, _)| *i == id)?;
        // SAFETY: WebKit rule: `progress` is an `NSProgress` property of every `WKDownload`.
        let progress: Option<Retained<NSProgress>> = unsafe { msg_send![&**download, progress] };
        let progress = progress?;
        let count = |n: i64| u64::try_from(n).unwrap_or(0);
        Some((count(progress.completedUnitCount()), count(progress.totalUnitCount())))
    }

    /// Stop download `id`; what it saved so far stays where it was going.
    fn cancel(&self, id: u64) {
        let download = {
            let downloads = self.ivars().downloads.borrow();
            downloads.iter().find(|(i, _)| *i == id).map(|(_, d)| Retained::clone(d))
        };
        if let Some(download) = download {
            self.forget(id);
            // SAFETY: WebKit rule: `cancel:` takes a nullable block of `(NSData *)` for the
            // resume data; nil asks for none.
            let () =
                unsafe { msg_send![&*download, cancel: None::<&DynBlock<dyn Fn(*mut AnyObject)>>] };
        }
    }

    /// Stop every download: the page is going.
    fn cancel_downloads(&self) {
        let ids: Vec<u64> = self.ivars().downloads.borrow().iter().map(|(id, _)| *id).collect();
        for id in ids {
            self.cancel(id);
        }
    }
}

/// The `Content-Disposition` header of a navigation response, when it is HTTP.
fn disposition(response: &AnyObject) -> Option<String> {
    // SAFETY: WebKit rule: `response` is an `NSURLResponse` property of every
    // `WKNavigationResponse`.
    let response: Option<Retained<AnyObject>> = unsafe { msg_send![response, response] };
    let response = response?;
    let http = AnyClass::get(c"NSHTTPURLResponse")?;
    // SAFETY: `NSObject` rule: `isKindOfClass:` may be asked of any object.
    let is_http: bool = unsafe { msg_send![&*response, isKindOfClass: http] };
    if !is_http {
        return None;
    }
    let name = NSString::from_str("Content-Disposition");
    // SAFETY: Foundation rule: `valueForHTTPHeaderField:` on an `NSHTTPURLResponse` returns
    // the field's value, or nil.
    let value: Option<Retained<NSString>> =
        unsafe { msg_send![&*response, valueForHTTPHeaderField: &*name] };
    value.map(|v| v.to_string())
}

/// Hand `web`'s UI to `delegate` (pop-ups, a script's dialogs) and open it to Web Inspector
/// in a debug build (Safari's Develop menu, for a Mac or a simulator).
fn adopt_view(web: &AnyObject, delegate: &Delegate) {
    // SAFETY: WebKit rule: the UI delegate is any object answering `WKUIDelegate`'s
    // selectors, held weakly; the view's owner keeps `delegate` as long as the view.
    let () = unsafe { msg_send![web, setUIDelegate: delegate] };
    // SAFETY: WebKit rule: `inspectable` is a BOOL property of `WKWebView`, settable any time.
    let () = unsafe { msg_send![web, setInspectable: cfg!(debug_assertions)] };
}

/// Take `web`'s UI back from its delegate, which is going, and stop its downloads.
fn forget_view(web: &AnyObject, delegate: &Delegate) {
    // SAFETY: WebKit rule: a delegate is cleared before it goes.
    let () = unsafe { msg_send![web, setUIDelegate: None::<&AnyObject>] };
    delegate.cancel_downloads();
}

/// Find `text` in `web`'s page, the next match after the selection (or before it,
/// `backwards`), wrapping round; the answer comes back as [`WebEvent::Found`].
fn find_in(web: &AnyObject, text: &str, backwards: bool, sink: Sink) {
    let Some(class) = AnyClass::get(c"WKFindConfiguration") else { return };
    // SAFETY: `NSObject` rule: `new` on a class returns a fresh, initialised instance.
    let config: Option<Retained<AnyObject>> = unsafe { msg_send![class, new] };
    let Some(config) = config else { return };
    // SAFETY: WebKit rule: `backwards` is a BOOL property of `WKFindConfiguration`; it wraps
    // by default.
    let () = unsafe { msg_send![&*config, setBackwards: backwards] };
    let done = RcBlock::new(move |result: NonNull<AnyObject>| {
        // SAFETY: WebKit rule: the result is a valid `WKFindResult` for the handler's call.
        let result = unsafe { result.as_ref() };
        // SAFETY: WebKit rule: `matchFound` is a BOOL property of `WKFindResult`.
        let found: bool = unsafe { msg_send![result, matchFound] };
        sink(WebEvent::Found(found));
    });
    let needle = NSString::from_str(text);
    // SAFETY: WebKit rule: `findString:withConfiguration:completionHandler:` takes a block of
    // `(WKFindResult *)` and runs it once, on the main thread.
    unsafe {
        let (): () = msg_send![
            web,
            findString: &*needle,
            withConfiguration: &*config,
            completionHandler: &*done
        ];
    }
}

/// What counts a needle in the page's text, as `findString:` matches it: case folded, the
/// matches apart. `WebKit`'s find answers only found or not, so the count is the page's.
const COUNT_SCRIPT: &str = "const text = (document.body ? document.body.innerText : '')\
.toLocaleLowerCase(); const n = needle.toLocaleLowerCase(); let count = 0; \
for (let at = n ? text.indexOf(n) : -1; at !== -1; at = text.indexOf(n, at + n.length)) \
{ count += 1; } return count;";

/// Count `text` in `web`'s page, in a script world of this app's that the page's own scripts
/// cannot reach; the answer comes back as [`WebEvent::Counted`].
fn count_in(web: &AnyObject, text: &str, sink: Sink) {
    let Some(worlds) = AnyClass::get(c"WKContentWorld") else { return };
    // SAFETY: WebKit rule: `defaultClientWorld` is a class property of `WKContentWorld`, a
    // world apart from the page's, shared by the app's scripts.
    let world: Option<Retained<AnyObject>> = unsafe { msg_send![worlds, defaultClientWorld] };
    let Some(world) = world else { return };
    let key = NSString::from_str("needle");
    let value = NSString::from_str(text);
    let value: &AnyObject = &value;
    let arguments = NSDictionary::<NSString, AnyObject>::from_slices(&[&*key], &[value]);
    let needle = text.to_owned();
    let done = RcBlock::new(move |result: *mut AnyObject, _error: *mut NSError| {
        // SAFETY: WebKit rule: the result is nil or a valid object for the handler's call.
        let result = unsafe { result.as_ref() };
        let count = result.and_then(AnyObject::downcast_ref::<NSNumber>).map(NSNumber::as_isize);
        let count = count.and_then(|n| usize::try_from(n).ok()).unwrap_or(0);
        sink(WebEvent::Counted { needle: needle.clone(), count });
    });
    let body = NSString::from_str(COUNT_SCRIPT);
    // SAFETY: WebKit rule: `callAsyncJavaScript:arguments:inFrame:inContentWorld:
    // completionHandler:` takes a function body, its arguments as a dictionary of names to
    // property-list values, nil for the main frame, a content world, and a nullable block of
    // `(id, NSError *)` it runs once, on the main thread.
    unsafe {
        let (): () = msg_send![
            web,
            callAsyncJavaScript: &*body,
            arguments: &*arguments,
            inFrame: None::<&AnyObject>,
            inContentWorld: &*world,
            completionHandler: &*done
        ];
    }
}

/// Show `web`'s page at `zoom` (1 is 100 %), as Safari's ⌘+ does: text, pictures and layout
/// together.
fn zoom_in(web: &AnyObject, zoom: f64) {
    // SAFETY: WebKit rule: `pageZoom` is a `CGFloat` property of `WKWebView` (a double on
    // Apple silicon), any positive value.
    let () = unsafe { msg_send![web, setPageZoom: zoom] };
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

    /// A script's dialog is answered exactly once: OK with the text, Cancel with none, and a
    /// dialog nobody answered (its tile closed) as Cancel when it goes.
    #[test]
    fn a_dialog_answers_once_and_a_dropped_one_cancels() {
        let answers: Rc<RefCell<Vec<Option<String>>>> = Rc::default();
        let dialog = |kind| {
            let answers = Rc::clone(&answers);
            Dialog::new(kind, "Sure?".to_owned(), move |a| answers.borrow_mut().push(a))
        };
        dialog(DialogKind::Prompt { default: "x".into() }).accept("typed");
        dialog(DialogKind::Confirm).dismiss();
        drop(dialog(DialogKind::Alert));
        assert_eq!(*answers.borrow(), [Some("typed".to_owned()), None, None]);
    }

    /// A download lands in this device's Downloads under the server's name, numbered past a
    /// file already there, never outside the folder.
    #[test]
    fn a_download_gets_a_name_of_its_own_in_downloads() {
        let dir = downloads_dir(Path::new("/Users/me"));
        assert_eq!(dir, Path::new("/Users/me/Downloads"));
        let none = |_: &Path| false;
        assert_eq!(unique_path(&dir, "report.pdf", none), dir.join("report.pdf"));
        let taken = |p: &Path| {
            p.ends_with("report.pdf") || p.ends_with("report (2).pdf") || p.ends_with("notes")
        };
        assert_eq!(unique_path(&dir, "report.pdf", taken), dir.join("report (3).pdf"));
        assert_eq!(unique_path(&dir, "notes", taken), dir.join("notes (2)"));
        assert_eq!(unique_path(&dir, "../../etc/passwd", none), dir.join("passwd"));
        assert_eq!(unique_path(&dir, ".hidden", none), dir.join("hidden"));
        assert_eq!(unique_path(&dir, "a:b", none), dir.join("a-b"));
        assert_eq!(unique_path(&dir, "", none), dir.join("download"));
    }

    /// `WebKit` asks the delegate `respondsToSelector:` for each thing it may hand over: the
    /// class registers (which checks the navigation methods against the protocol's
    /// signatures) and answers every selector of pop-ups, a page closing itself,
    /// dialogs and downloads.
    #[test]
    fn the_delegate_answers_what_webkit_asks_it() {
        let class = <Delegate as objc2::ClassType>::class();
        let wanted = [
            objc2::sel!(webView:decidePolicyForNavigationResponse:decisionHandler:),
            objc2::sel!(webView:navigationResponse:didBecomeDownload:),
            objc2::sel!(webView:createWebViewWithConfiguration:forNavigationAction:windowFeatures:),
            objc2::sel!(webViewDidClose:),
            objc2::sel!(webView:runJavaScriptAlertPanelWithMessage:initiatedByFrame:completionHandler:),
            objc2::sel!(webView:runJavaScriptConfirmPanelWithMessage:initiatedByFrame:completionHandler:),
            objc2::sel!(webView:runJavaScriptTextInputPanelWithPrompt:defaultText:initiatedByFrame:completionHandler:),
            objc2::sel!(download:decideDestinationUsingResponse:suggestedFilename:completionHandler:),
            objc2::sel!(downloadDidFinish:),
            objc2::sel!(download:didFailWithError:resumeData:),
        ];
        for sel in wanted {
            assert!(class.instance_method(sel).is_some(), "{sel:?}");
        }
    }

    #[test]
    fn an_attachment_is_saved_and_inline_is_shown() {
        assert!(is_attachment(Some("attachment; filename=\"a.csv\"")));
        assert!(is_attachment(Some("Attachment")));
        assert!(!is_attachment(Some("inline; filename=a.pdf")));
        assert!(!is_attachment(None));
    }
}

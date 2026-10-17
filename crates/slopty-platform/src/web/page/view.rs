//! A page's `WKWebView` and the objects `WebKit` calls back for it: the scheme handler that
//! serves its origin, the relay that hears its bridge, and the delegate that refuses its
//! navigations. The same on macOS and iOS: the bindings type `WKWebView` for macOS only, so the
//! view is spoken to by selector.

use std::cell::Cell;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::rc::{Rc, Weak};
use std::time::Instant;

use block2::{DynBlock, RcBlock};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject, Sel};
use objc2::{
    AnyThread as _, DefinedClass as _, MainThreadMarker, MainThreadOnly, Message as _,
    define_class, msg_send, sel,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGColorSpace, CGContext, CGImage, CGImageAlphaInfo,
    CGImageByteOrderInfo, kCGColorSpaceSRGB,
};
use objc2_foundation::{
    NSData, NSDictionary, NSError, NSHTTPURLResponse, NSObject, NSString, NSURL, NSURLRequest,
};
use objc2_web_kit::{
    WKAudiovisualMediaTypes, WKContentRuleList, WKContentWorld, WKNavigation,
    WKNavigationActionPolicy, WKNavigationDelegate, WKScriptMessage,
    WKScriptMessageHandlerWithReply, WKSnapshotConfiguration, WKURLSchemeHandler,
    WKUserContentController, WKUserScript, WKUserScriptInjectionTime, WKWebViewConfiguration,
    WKWebsiteDataStore,
};
use serde_json::Value;

use super::{Refusal, Resource, Responder, Shared, Snapshot, bridge, sandbox};

/// The content world the host document's script runs in, and its message handler's name.
pub(super) const WORLD: &str = "slopty-page";
const HANDLER: &str = "slopty";

/// The host document's script, in its own world: it relays what the view posts to its parent,
/// stamped with whether the person had just acted in the page, and posts the host's answer
/// back; it tells the host when the view's document has loaded. The view cannot reach it: the
/// frame's origin is opaque, and the script's world is not the page's.
fn relay_script() -> String {
    format!(
        "(() => {{\
         const handler = window.webkit && window.webkit.messageHandlers && \
         window.webkit.messageHandlers.{HANDLER};\
         if (!handler) return;\
         const view = () => document.getElementById('view');\
         const send = (envelope) => handler.postMessage(JSON.stringify(envelope));\
         document.addEventListener('load', (event) => {{\
           if (event.target === view()) send({{ k: 'load' }}).catch(() => {{}});\
         }}, true);\
         window.addEventListener('message', (event) => {{\
           const frame = view();\
           if (!frame || event.source !== frame.contentWindow) return;\
           let text;\
           try {{ text = JSON.stringify({{ k: 'm', m: event.data, \
             a: !!(navigator.userActivation && navigator.userActivation.isActive) }}); }} \
           catch (_) {{ return; }}\
           if (typeof text !== 'string' || text.length > {limit}) return;\
           handler.postMessage(text).then((reply) => {{\
             if (typeof reply === 'string' && frame.contentWindow) \
               frame.contentWindow.postMessage(JSON.parse(reply), '*');\
           }}, () => {{}});\
         }});\
         }})();",
        limit = bridge::MAX_MESSAGE,
    )
}

/// What posts the host's message `m` (JSON) to the view.
const POST_SCRIPT: &str = "const frame = document.getElementById('view'); \
if (!frame || !frame.contentWindow) return false; \
frame.contentWindow.postMessage(JSON.parse(m), '*'); return true;";

/// What gives the host document the theme's scheme and background.
const THEME_SCRIPT: &str = "document.documentElement.style.colorScheme = scheme; \
document.body.style.background = background; \
const frame = document.getElementById('view'); if (frame) frame.style.background = background; \
const meta = document.querySelector('meta[name=\"color-scheme\"]'); if (meta) meta.content = scheme; \
return true;";

/// `-[WKWebView _webProcessIdentifier]` (`WKWebViewPrivate.h`, `WebKit`'s `WKWebView.mm`): the
/// process id of the page's `WebContent` process, 0 while it has none.
pub(super) fn process_selector() -> Sel {
    sel!(_webProcessIdentifier)
}

/// A page's web view while it is mounted. Dropping it takes the view away and ends its page.
pub(super) struct Mounted {
    web: Retained<AnyObject>,
    controller: Retained<WKUserContentController>,
    /// Held as long as the view, which holds it weakly.
    _delegate: Retained<PageDelegate>,
}

impl Mounted {
    pub(super) fn view(&self) -> NonNull<c_void> {
        NonNull::from(&*self.web).cast()
    }
}

impl Drop for Mounted {
    fn drop(&mut self) {
        // SAFETY: WebKit rule (`WKUserContentController.h`): handlers and scripts may be removed
        // at any time on the main thread; the controller holds its handlers strongly until then.
        unsafe {
            self.controller.removeAllScriptMessageHandlers();
        }
        // SAFETY: as above.
        unsafe {
            self.controller.removeAllUserScripts();
        }
        #[cfg(target_os = "ios")]
        {
            // SAFETY: UIKit rule: `endEditing:` (the `UITextField` category on `UIView`) resigns
            // whichever view below the receiver is first responder.
            let _ended: bool = unsafe { msg_send![&*self.web, endEditing: true] };
        }
        // SAFETY: WebKit rule: a delegate is cleared before it goes; both are weak.
        let () = unsafe { msg_send![&*self.web, setNavigationDelegate: None::<&AnyObject>] };
        // SAFETY: as above.
        let () = unsafe { msg_send![&*self.web, setUIDelegate: None::<&AnyObject>] };
        // SAFETY: AppKit and UIKit rule: `removeFromSuperview` on a view in no superview does
        // nothing.
        let () = unsafe { msg_send![&*self.web, removeFromSuperview] };
        // Its delegates are gone, so the end is not heard as a crash.
        crate::web::end_page(&self.web);
    }
}

/// A `WKWebView` for `shared`'s page: its own data store that keeps nothing, its own scheme
/// handler, the relay in `world`, and the delegate. It loads once its rule list is in place
/// ([`load`]). `None` without `WebKit`, which only iOS asks for by name.
#[cfg_attr(
    target_os = "macos",
    expect(
        clippy::unnecessary_wraps,
        reason = "the Mac's view is a subclass, linked; iOS may miss it"
    )
)]
pub(super) fn mount(
    shared: &Rc<Shared>,
    world: &WKContentWorld,
    mtm: MainThreadMarker,
) -> Option<Mounted> {
    // SAFETY: WebKit rule: a configuration made by `new` is complete.
    let config = unsafe { WKWebViewConfiguration::new(mtm) };
    // SAFETY: WebKit rule: a non-persistent store may be made at any time on the main thread;
    // each is new, so pages share no cookies, caches or storage.
    let store = unsafe { WKWebsiteDataStore::nonPersistentDataStore(mtm) };
    // SAFETY: WebKit rule: a configuration takes any store before the view is made.
    unsafe {
        config.setWebsiteDataStore(&store);
    }
    // SAFETY: WebKit rule: media types are a plain property set before the view is made.
    unsafe {
        config.setMediaTypesRequiringUserActionForPlayback(WKAudiovisualMediaTypes::All);
    }
    let scheme = SchemeHandler::new(Rc::clone(shared), mtm);
    let name = NSString::from_str(sandbox::SCHEME);
    // SAFETY: WebKit rule (`WKWebViewConfiguration.h`): a handler for a scheme `WebKit` does not
    // handle itself, set before the view is made; the configuration keeps it.
    unsafe {
        config.setURLSchemeHandler_forURLScheme(Some(ProtocolObject::from_ref(&*scheme)), &name);
    }
    // SAFETY: WebKit rule: a configuration made by `new` has a user content controller.
    let controller = unsafe { config.userContentController() };
    let source = NSString::from_str(&relay_script());
    // SAFETY: WebKit rule (`WKUserScript.h`): a script, when it runs, whether in the main frame
    // only, and the world it runs in.
    let script = unsafe {
        WKUserScript::initWithSource_injectionTime_forMainFrameOnly_inContentWorld(
            WKUserScript::alloc(mtm),
            &source,
            WKUserScriptInjectionTime::AtDocumentStart,
            true,
            world,
        )
    };
    // SAFETY: WebKit rule: user scripts are added on the main thread.
    unsafe {
        controller.addUserScript(&script);
    }
    let relay = Relay::new(Rc::clone(shared), mtm);
    let handler = NSString::from_str(HANDLER);
    // SAFETY: WebKit rule (`WKUserContentController.h`): one handler per name and world; this
    // controller is new.
    unsafe {
        controller.addScriptMessageHandlerWithReply_contentWorld_name(
            ProtocolObject::from_ref(&*relay),
            world,
            &handler,
        );
    }
    let frame = rect(shared.width.get(), shared.height.get());
    // On the Mac the view hands on the wheel it cannot take (`super::wheel`).
    #[cfg(target_os = "macos")]
    let web: Retained<AnyObject> = super::wheel::PageWebView::new(frame, &config, mtm).into();
    #[cfg(not(target_os = "macos"))]
    let web = {
        let class = objc2::runtime::AnyClass::get(c"WKWebView")?;
        // SAFETY: `NSObject` rule: `alloc` on a class returns a fresh instance to initialise.
        let allocated: objc2::rc::Allocated<AnyObject> = unsafe { msg_send![class, alloc] };
        // SAFETY: WebKit rule: `initWithFrame:configuration:` is `WKWebView`'s designated
        // initialiser, which copies the configuration.
        let web: Option<Retained<AnyObject>> =
            unsafe { msg_send![allocated, initWithFrame: frame, configuration: &*config] };
        web?
    };
    let delegate = PageDelegate::new(Rc::clone(shared), mtm);
    // SAFETY: WebKit rule: the navigation delegate is held weakly; `Mounted` keeps it as long as
    // the view.
    let () = unsafe { msg_send![&*web, setNavigationDelegate: &*delegate] };
    // SAFETY: WebKit rule: the UI delegate is any object answering `WKUIDelegate`'s selectors,
    // held weakly, as above.
    let () = unsafe { msg_send![&*web, setUIDelegate: &*delegate] };
    // SAFETY: WebKit rule: `inspectable` is a BOOL property of `WKWebView`, settable any time.
    let () = unsafe { msg_send![&*web, setInspectable: cfg!(debug_assertions)] };
    *shared.web.borrow_mut() = Some(objc2::rc::Weak::new(&web));
    Some(Mounted { web, controller, _delegate: delegate })
}

/// Put `rules` in `web`'s place of every other list, and load `url`.
pub(super) fn load(web: &AnyObject, rules: &WKContentRuleList, url: &str) {
    // SAFETY: WebKit rule: `configuration` is a copy whose `userContentController` is the view's
    // own, which rule lists may be added to and removed from at any time.
    let config: Retained<WKWebViewConfiguration> = unsafe { msg_send![web, configuration] };
    // SAFETY: as above.
    let controller = unsafe { config.userContentController() };
    // SAFETY: as above.
    unsafe {
        controller.removeAllContentRuleLists();
    }
    // SAFETY: as above.
    unsafe {
        controller.addContentRuleList(rules);
    }
    let Some(address) = NSURL::URLWithString(&NSString::from_str(url)) else { return };
    let request = NSURLRequest::requestWithURL(&address);
    // SAFETY: WebKit rule: any `NSURLRequest` may be loaded; the navigation it returns may be
    // ignored.
    let _navigation: Option<Retained<AnyObject>> =
        unsafe { msg_send![web, loadRequest: &*request] };
}

/// A rectangle at the origin, `width` by `height` points.
const fn rect(width: f64, height: f64) -> CGRect {
    CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(width, height))
}

/// Lay the page's view out at its width and height.
pub(super) fn frame(shared: &Shared) {
    let Some(web) = shared.web() else { return };
    let frame = rect(shared.width.get(), shared.height.get());
    // SAFETY: AppKit and UIKit rule: `setFrame:` takes a `CGRect` (`NSRect` is one).
    let () = unsafe { msg_send![&*web, setFrame: frame] };
}

/// Run `body` in the host document, in the relay's world, with `arguments` (names to strings).
fn run(shared: &Shared, body: &str, arguments: &[(&str, &str)]) {
    let Some(web) = shared.web() else { return };
    let keys: Vec<Retained<NSString>> =
        arguments.iter().map(|(k, _)| NSString::from_str(k)).collect();
    let values: Vec<Retained<NSString>> =
        arguments.iter().map(|(_, v)| NSString::from_str(v)).collect();
    let keys: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
    let values: Vec<&AnyObject> = values.iter().map(|v| -> &AnyObject { v }).collect();
    let arguments = NSDictionary::<NSString, AnyObject>::from_slices(&keys, &values);
    let body = NSString::from_str(body);
    let world = &shared.cx.world;
    // SAFETY: WebKit rule: `callAsyncJavaScript:arguments:inFrame:inContentWorld:
    // completionHandler:` takes a function body, its arguments as property-list values, nil for
    // the main frame, a content world, and a nullable handler.
    unsafe {
        let (): () = msg_send![
            &*web,
            callAsyncJavaScript: &*body,
            arguments: &*arguments,
            inFrame: None::<&AnyObject>,
            inContentWorld: &**world,
            completionHandler: None::<&DynBlock<dyn Fn(*mut AnyObject, *mut NSError)>>
        ];
    }
}

/// Post `message` to the view.
pub(super) fn post(shared: &Shared, message: &Value) {
    run(shared, POST_SCRIPT, &[("m", &message.to_string())]);
}

/// Give the host document the theme's scheme and background.
pub(super) fn retheme(shared: &Shared) {
    let theme = shared.cx.theme.borrow();
    let scheme = if theme.dark { "dark" } else { "light" };
    let background = sandbox::background(&theme).to_owned();
    drop(theme);
    run(shared, THEME_SCRIPT, &[("scheme", scheme), ("background", &background)]);
}

/// Take a picture of the page, as BGRA: it comes as [`super::PageEvent::Snapshot`], and is kept
/// as the page's last.
pub(super) fn snapshot(shared: &Rc<Shared>) {
    let Some(web) = shared.web() else { return };
    let started = Instant::now();
    let weak = Rc::downgrade(shared);
    let done = RcBlock::new(move |image: *mut AnyObject, _error: *mut NSError| {
        let Some(shared) = weak.upgrade() else { return };
        // SAFETY: WebKit rule: the image is nil or a valid `NSImage` (`UIImage` on iOS) for the
        // handler's call.
        let Some(image) = (unsafe { image.as_ref() }) else { return };
        let Some((width, height, bgra)) = pixels(image) else { return };
        let snapshot = Rc::new(Snapshot { width, height, bgra, took: started.elapsed() });
        *shared.snapshot.borrow_mut() = Some(Rc::clone(&snapshot));
        shared.emit(super::PageEvent::Snapshot(snapshot));
    });
    // SAFETY: WebKit rule: a default snapshot configuration captures the visible page after its
    // next screen update.
    let config = unsafe { WKSnapshotConfiguration::new(shared.cx.mtm) };
    // SAFETY: WebKit rule: `takeSnapshotWithConfiguration:completionHandler:` takes a block of
    // `(NSImage *, NSError *)` (`UIImage *` on iOS) and runs it once, on the main thread.
    unsafe {
        let (): () =
            msg_send![&*web, takeSnapshotWithConfiguration: &*config, completionHandler: &*done];
    }
}

/// The image's pixels, drawn into BGRA (premultiplied, the top row first): its width, height
/// and bytes.
fn pixels(image: &AnyObject) -> Option<(u32, u32, Vec<u8>)> {
    #[cfg(target_os = "macos")]
    // SAFETY: AppKit rule (`NSImage.h`): `CGImageForProposedRect:context:hints:` with no rect,
    // context or hints returns the image's best `CGImage`, owned by the image (not retained),
    // valid while the image is.
    let cg: *mut CGImage = unsafe {
        msg_send![
            image,
            CGImageForProposedRect: std::ptr::null_mut::<CGRect>(),
            context: None::<&AnyObject>,
            hints: None::<&AnyObject>
        ]
    };
    #[cfg(target_os = "ios")]
    // SAFETY: UIKit rule (`UIImage.h`): `CGImage` is the image's own, not retained, valid while
    // the image is.
    let cg: *mut CGImage = unsafe { msg_send![image, CGImage] };
    // SAFETY: as above: nil or a live image for this call.
    let cg = unsafe { cg.as_ref() }?;
    let width = CGImage::width(Some(cg));
    let height = CGImage::height(Some(cg));
    let row = width.checked_mul(4)?;
    let mut bgra = vec![0_u8; row.checked_mul(height)?];
    if bgra.is_empty() {
        return None;
    }
    // SAFETY: a `CFString` static Core Graphics defines (`CGColorSpace.h`), read-only.
    let srgb = unsafe { kCGColorSpaceSRGB };
    let space = CGColorSpace::with_name(Some(srgb))?;
    let info = CGImageByteOrderInfo::Order32Little.0 | CGImageAlphaInfo::PremultipliedFirst.0;
    // SAFETY: `CGBitmapContextCreate` (`CGBitmapContext.h`) draws into `bgra` for the context's
    // life: it holds `row × height` bytes and outlives the context, dropped before it is read.
    // 8 bits a component, four components, little-endian with alpha first is BGRA, one of its
    // supported formats.
    let context = unsafe {
        CGBitmapContextCreate(
            bgra.as_mut_ptr().cast::<c_void>(),
            width,
            height,
            8,
            row,
            Some(&space),
            info,
        )
    }?;
    #[expect(clippy::cast_precision_loss, reason = "a page is a few thousand pixels at most")]
    let whole = rect(width as f64, height as f64);
    CGContext::draw_image(Some(&context), whole, Some(cg));
    drop(context);
    Some((u32::try_from(width).ok()?, u32::try_from(height).ok()?, bgra))
}

/// The process id of `web`'s `WebContent` process, while it has one.
pub(super) fn process(web: &AnyObject) -> Option<i32> {
    let selector = process_selector();
    // SAFETY: `NSObject` rule: `respondsToSelector:` may be asked of any object.
    let private: bool = unsafe { msg_send![web, respondsToSelector: selector] };
    if !private {
        return None;
    }
    // SAFETY: WebKit rule (`WKWebViewPrivate.h`): `_webProcessIdentifier` takes no argument and
    // returns a `pid_t`; present as asked above.
    let pid: i32 = unsafe { msg_send![web, _webProcessIdentifier] };
    (pid > 0).then_some(pid)
}

/// One request of the scheme's, which `WebKit` waits on until it is answered or stopped.
pub(super) struct Task {
    inner: Retained<AnyObject>,
    url: Retained<NSURL>,
    /// `WebKit` still waits for it: no callback may reach a task it stopped.
    live: Rc<Cell<bool>>,
    shared: Weak<Shared>,
}

impl Task {
    pub(super) fn answer(self, resource: Option<Resource>) {
        if MainThreadMarker::new().is_none() || !self.live.get() {
            return;
        }
        self.live.set(false);
        if let Some(shared) = self.shared.upgrade() {
            let at = Retained::as_ptr(&self.inner).addr();
            shared.tasks.borrow_mut().retain(|(task, _)| *task != at);
        }
        match resource {
            Some(resource) => {
                respond(&self.inner, &self.url, 200, &resource.mime, resource.bytes, None);
            }
            None => respond(&self.inner, &self.url, 404, "text/plain", Vec::new(), None),
        }
    }
}

/// Answer `task` with a response of `status`, `mime`, `body` and, for the host document, its
/// policy.
fn respond(
    task: &AnyObject,
    url: &NSURL,
    status: isize,
    mime: &str,
    body: Vec<u8>,
    policy: Option<&str>,
) {
    let mut headers = vec![
        ("Content-Type", mime.to_owned()),
        ("Cache-Control", "no-store".to_owned()),
        ("X-Content-Type-Options", "nosniff".to_owned()),
        // The view's frame is an opaque origin; its own origin's files are its to fetch.
        ("Access-Control-Allow-Origin", "*".to_owned()),
    ];
    if let Some(policy) = policy {
        headers.push(("Content-Security-Policy", policy.to_owned()));
    }
    let keys: Vec<Retained<NSString>> =
        headers.iter().map(|(k, _)| NSString::from_str(k)).collect();
    let values: Vec<Retained<NSString>> =
        headers.iter().map(|(_, v)| NSString::from_str(v)).collect();
    let keys: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
    let values: Vec<&NSString> = values.iter().map(|v| &**v).collect();
    let fields = NSDictionary::from_slices(&keys, &values);
    let version = NSString::from_str("HTTP/1.1");
    let response = NSHTTPURLResponse::initWithURL_statusCode_HTTPVersion_headerFields(
        NSHTTPURLResponse::alloc(),
        url,
        status,
        Some(&version),
        Some(&fields),
    );
    let Some(response) = response else { return fail(task) };
    // SAFETY: WebKit rule (`WKURLSchemeTask.h`): a live task takes one response, its data, then
    // its end, on the main thread.
    let () = unsafe { msg_send![task, didReceiveResponse: &*response] };
    let data = NSData::from_vec(body);
    // SAFETY: as above.
    let () = unsafe { msg_send![task, didReceiveData: &*data] };
    // SAFETY: as above.
    let () = unsafe { msg_send![task, didFinish] };
}

/// End `task` with an error.
fn fail(task: &AnyObject) {
    let domain = NSString::from_str("SloptyPage");
    let error = NSError::new(1, &domain);
    // SAFETY: WebKit rule (`WKURLSchemeTask.h`): a live task may end with an error instead of
    // a response.
    let () = unsafe { msg_send![task, didFailWithError: &*error] };
}

struct SchemeIvars {
    shared: Rc<Shared>,
}

define_class!(
    // SAFETY:
    // - `NSObject` has no subclassing requirements.
    // - `SchemeHandler` does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SloptyPageScheme"]
    #[ivars = SchemeIvars]
    struct SchemeHandler;

    unsafe impl NSObjectProtocol for SchemeHandler {}

    // The view and the task come in as plain objects: the bindings type `WKWebView` for macOS
    // only.
    unsafe impl WKURLSchemeHandler for SchemeHandler {
        #[unsafe(method(webView:startURLSchemeTask:))]
        fn start(&self, _web: &AnyObject, task: &AnyObject) {
            self.serve(task);
        }

        #[unsafe(method(webView:stopURLSchemeTask:))]
        fn stop(&self, _web: &AnyObject, task: &AnyObject) {
            let at = std::ptr::from_ref(task).addr();
            self.ivars().shared.tasks.borrow_mut().retain(|(t, live)| {
                let stopped = *t == at;
                if stopped {
                    live.set(false);
                }
                !stopped
            });
        }
    }
);

impl SchemeHandler {
    fn new(shared: Rc<Shared>, mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(SchemeIvars { shared });
        // SAFETY: `NSObject`'s `init` on a freshly allocated instance with ivars set.
        unsafe { msg_send![super(this), init] }
    }

    /// Serve the page's origin: its host document, its policy's reports, and its files through
    /// the app; nothing of another page's.
    fn serve(&self, task: &AnyObject) {
        let shared = &self.ivars().shared;
        // SAFETY: WebKit rule (`WKURLSchemeTask.h`): `request` is the task's `NSURLRequest`.
        let request: Retained<NSURLRequest> = unsafe { msg_send![task, request] };
        let Some(url) = request.URL() else { return fail(task) };
        let host = url.host().map(|h| h.to_string()).unwrap_or_default();
        let address = url.absoluteString().map(|a| a.to_string()).unwrap_or_default();
        if host != shared.id.host() {
            shared.refuse(Refusal::Resource { url: address });
            return fail(task);
        }
        let path = url
            .path()
            .map(|p| p.to_string())
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| "/".to_owned());
        let method = request.HTTPMethod().map(|m| m.to_string()).unwrap_or_default();
        match (method.as_str(), path.as_str()) {
            ("POST", sandbox::REPORTS) => {
                let body = request.HTTPBody().map(|b| b.to_vec()).unwrap_or_default();
                if let Some(refusal) = sandbox::report(&body) {
                    shared.refuse(refusal);
                }
                respond(task, &url, 204, "text/plain", Vec::new(), None);
            }
            ("GET", "/") => {
                let document = shared.document().into_bytes();
                let policy = shared.policy();
                respond(task, &url, 200, "text/html; charset=utf-8", document, Some(&policy));
            }
            ("GET", _) => {
                match shared.resources.clone().filter(|_| sandbox::relative(&path).is_some()) {
                    Some(resources) => {
                        let live = Rc::new(Cell::new(true));
                        let at = std::ptr::from_ref(task).addr();
                        shared.tasks.borrow_mut().push((at, Rc::clone(&live)));
                        let task =
                            Task { inner: task.retain(), url, live, shared: Rc::downgrade(shared) };
                        resources(
                            path.strip_prefix('/').unwrap_or(&path),
                            Responder { task: Some(task) },
                        );
                    }
                    None => respond(task, &url, 404, "text/plain", Vec::new(), None),
                }
            }
            _ => respond(task, &url, 405, "text/plain", Vec::new(), None),
        }
    }
}

struct RelayIvars {
    shared: Rc<Shared>,
}

define_class!(
    // SAFETY:
    // - `NSObject` has no subclassing requirements.
    // - `Relay` does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SloptyPageRelay"]
    #[ivars = RelayIvars]
    struct Relay;

    unsafe impl NSObjectProtocol for Relay {}

    unsafe impl WKScriptMessageHandlerWithReply for Relay {
        #[unsafe(method(userContentController:didReceiveScriptMessage:replyHandler:))]
        fn received(
            &self,
            _controller: &WKUserContentController,
            message: &WKScriptMessage,
            reply: &DynBlock<dyn Fn(*mut AnyObject, *mut NSString)>,
        ) {
            let answer = self.heard(message);
            match answer {
                Some(answer) => {
                    let text = NSString::from_str(&answer.to_string());
                    reply.call((
                        Retained::as_ptr(&text).cast_mut().cast::<AnyObject>(),
                        std::ptr::null_mut(),
                    ));
                }
                None => reply.call((std::ptr::null_mut(), std::ptr::null_mut())),
            }
        }
    }
);

impl Relay {
    fn new(shared: Rc<Shared>, mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(RelayIvars { shared });
        // SAFETY: `NSObject`'s `init` on a freshly allocated instance with ivars set.
        unsafe { msg_send![super(this), init] }
    }

    /// What the relay heard, from the host document alone; the host's answer when it has one.
    fn heard(&self, message: &WKScriptMessage) -> Option<Value> {
        // SAFETY: WebKit rule: `frameInfo` is the `WKFrameInfo` of the frame that posted.
        let frame = unsafe { message.frameInfo() };
        // SAFETY: WebKit rule: `isMainFrame` is a BOOL property of `WKFrameInfo`.
        if !unsafe { frame.isMainFrame() } {
            return None;
        }
        // SAFETY: WebKit rule: `body` is the posted value as an Objective-C object.
        let body = unsafe { message.body() };
        let text = body.downcast_ref::<NSString>()?.to_string();
        let relayed = bridge::relayed(&text)?;
        self.ivars().shared.heard(relayed)
    }
}

struct DelegateIvars {
    shared: Rc<Shared>,
}

define_class!(
    // SAFETY:
    // - `NSObject` has no subclassing requirements.
    // - `PageDelegate` does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SloptyPageDelegate"]
    #[ivars = DelegateIvars]
    struct PageDelegate;

    unsafe impl NSObjectProtocol for PageDelegate {}

    unsafe impl WKNavigationDelegate for PageDelegate {
        /// The host's own load of the page's address, once, and the view's frame are let
        /// through; every other navigation is refused and counted.
        #[unsafe(method(webView:decidePolicyForNavigationAction:decisionHandler:))]
        fn decide_action(
            &self,
            _web: &AnyObject,
            action: &AnyObject,
            decide: &DynBlock<dyn Fn(WKNavigationActionPolicy)>,
        ) {
            let allowed = self.allowed(action);
            decide.call((if allowed { WKNavigationActionPolicy::Allow } else { WKNavigationActionPolicy::Cancel },));
        }

        #[unsafe(method(webView:didFinishNavigation:))]
        fn finished(&self, _web: &AnyObject, _navigation: Option<&WKNavigation>) {
            self.ivars().shared.emit(super::PageEvent::Loaded);
        }

        #[unsafe(method(webViewWebContentProcessDidTerminate:))]
        fn terminated(&self, _web: &AnyObject) {
            self.ivars().shared.emit(super::PageEvent::Crashed);
        }
    }

    // `WKUIDelegate`, by selector, as the browser's delegate speaks it (`super::super`).
    impl PageDelegate {
        /// A pop-up: never.
        #[unsafe(method(webView:createWebViewWithConfiguration:forNavigationAction:windowFeatures:))]
        fn create_web_view(
            &self,
            _web: &AnyObject,
            _config: &AnyObject,
            action: &AnyObject,
            _features: &AnyObject,
        ) -> *mut AnyObject {
            let url = target(action);
            self.ivars().shared.refuse(Refusal::Navigation { url });
            std::ptr::null_mut()
        }

        /// The camera and the microphone: never.
        #[unsafe(method(webView:requestMediaCapturePermissionForOrigin:initiatedByFrame:type:decisionHandler:))]
        fn media(
            &self,
            _web: &AnyObject,
            _origin: &AnyObject,
            _frame: &AnyObject,
            _kind: isize,
            decide: &DynBlock<dyn Fn(isize)>,
        ) {
            decide.call((DENY,));
        }

        /// A wheel event neither the page's scrolling nor its process took (`WKUIDelegatePrivate.h`,
        /// private, macOS): the page has nothing to scroll that way, so it goes on to the chat.
        #[cfg(target_os = "macos")]
        #[unsafe(method(_webView:didNotHandleWheelEvent:))]
        fn unhandled_wheel(&self, web: &AnyObject, event: &objc2_app_kit::NSEvent) {
            super::wheel::pass_on(web, event);
        }

        /// The device's motion and orientation: never.
        #[unsafe(method(webView:requestDeviceOrientationAndMotionPermissionForOrigin:initiatedByFrame:decisionHandler:))]
        fn motion(
            &self,
            _web: &AnyObject,
            _origin: &AnyObject,
            _frame: &AnyObject,
            decide: &DynBlock<dyn Fn(isize)>,
        ) {
            decide.call((DENY,));
        }
    }
);

/// `WKPermissionDecisionDeny` (`WKUIDelegate.h`).
const DENY: isize = 2;

/// The address a navigation action goes to.
fn target(action: &AnyObject) -> String {
    // SAFETY: WebKit rule: `request` is an `NSURLRequest` property of every
    // `WKNavigationAction`.
    let request: Option<Retained<NSURLRequest>> = unsafe { msg_send![action, request] };
    request
        .and_then(|r| r.URL())
        .and_then(|u| u.absoluteString())
        .map(|u| u.to_string())
        .unwrap_or_default()
}

impl PageDelegate {
    fn new(shared: Rc<Shared>, mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(DelegateIvars { shared });
        // SAFETY: `NSObject`'s `init` on a freshly allocated instance with ivars set.
        unsafe { msg_send![super(this), init] }
    }

    fn allowed(&self, action: &AnyObject) -> bool {
        let shared = &self.ivars().shared;
        let url = target(action);
        // SAFETY: WebKit rule: `targetFrame` is the `WKFrameInfo` the navigation is in, nil for
        // a new window.
        let frame: Option<Retained<AnyObject>> = unsafe { msg_send![action, targetFrame] };
        let allowed = frame.is_some_and(|frame| {
            // SAFETY: WebKit rule: `isMainFrame` is a BOOL property of `WKFrameInfo`.
            let main: bool = unsafe { msg_send![&*frame, isMainFrame] };
            if main {
                shared.admit.replace(false) && url == shared.url()
            } else {
                url == "about:srcdoc"
            }
        });
        if !allowed {
            shared.refuse(Refusal::Navigation { url });
        }
        allowed
    }
}

#[cfg(test)]
mod tests {
    use objc2::runtime::AnyClass;

    use super::*;

    /// The private selector a page's process is named by is still `WebKit`'s.
    #[test]
    fn webkit_still_answers_the_process_selector() {
        let view = AnyClass::get(c"WKWebView").expect("WebKit is linked");
        assert!(view.responds_to(process_selector()), "-[WKWebView _webProcessIdentifier]");
    }

    /// `WebKit` asks each object for what it may hand it: the classes register (checking each
    /// protocol method's signature) and answer every selector of the scheme, the relay, the
    /// navigations and the permissions.
    #[test]
    fn the_page_objects_answer_what_webkit_asks() {
        let scheme = <SchemeHandler as objc2::ClassType>::class();
        assert!(scheme.instance_method(sel!(webView:startURLSchemeTask:)).is_some());
        assert!(scheme.instance_method(sel!(webView:stopURLSchemeTask:)).is_some());
        let relay = <Relay as objc2::ClassType>::class();
        assert!(
            relay
                .instance_method(sel!(userContentController:didReceiveScriptMessage:replyHandler:))
                .is_some()
        );
        let delegate = <PageDelegate as objc2::ClassType>::class();
        for wanted in [
            sel!(webView:decidePolicyForNavigationAction:decisionHandler:),
            sel!(webView:createWebViewWithConfiguration:forNavigationAction:windowFeatures:),
            sel!(webView:requestMediaCapturePermissionForOrigin:initiatedByFrame:type:decisionHandler:),
            sel!(webViewWebContentProcessDidTerminate:),
            #[cfg(target_os = "macos")]
            sel!(_webView:didNotHandleWheelEvent:),
        ] {
            assert!(delegate.instance_method(wanted).is_some(), "{wanted:?}");
        }
    }
}

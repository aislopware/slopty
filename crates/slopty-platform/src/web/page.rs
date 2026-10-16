//! Pages inline in a transcript: HTML an agent made (an MCP App's view, a `show_page`, an
//! `.html` or `.svg` file) in a `WKWebView` of its own, sandboxed, speaking MCP Apps' bridge
//! (macOS and iOS).
//!
//! A [`PageHost`] keeps one window's pages. Each page:
//! - is served from an origin of its own (`slopty-page://<id>`) by a scheme handler that answers
//!   only that host, in a data store that keeps nothing;
//! - is a trusted host document holding the view in a `sandbox="allow-scripts"` frame, under a
//!   policy and a content rule list that let it reach no network until the app grants it origins
//!   ([`PageHost::set_network`]);
//! - speaks MCP Apps' JSON-RPC with the host through the host document's script, which runs in a
//!   content world of its own: the handshake is answered here, and whatever would act for the
//!   person comes to the app as a [`PageEvent::Asked`], for the app to answer
//!   ([`PageHost::answer`]) once the person has decided, never before;
//! - is cold (no web view), warm (a web view GPUI does not place; its BGRA [`Snapshot`] stands in)
//!   or live (placed), under the caps of [`stage`].
//!
//! The app owns the host and hands [`PageHost::view`] to a GPUI native host while a page is live,
//! as the browser tile does with its page (`docs/decisions/platform.md`, "Pages run in a sandbox
//! of their own").

mod bridge;
pub mod libs;
mod sandbox;
pub mod stage;
mod view;

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::ptr::NonNull;
use std::rc::{Rc, Weak};
use std::time::Duration;

use block2::RcBlock;
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_foundation::{NSError, NSString};
use objc2_web_kit::{WKContentRuleList, WKContentRuleListStore, WKContentWorld};
use serde_json::{Value, json};
pub use stage::{Change, Device, MOUNTED_CAP, Presence};

/// A page of a [`PageHost`]: its origin's host is its number in hex.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PageId(pub u64);

impl PageId {
    /// The host of the page's origin.
    #[must_use]
    pub fn host(self) -> String {
        format!("{:x}", self.0)
    }

    /// The page's origin, `slopty-page://<host>`: what no other page may reach.
    #[must_use]
    pub fn origin(self) -> String {
        sandbox::origin(self)
    }
}

/// The app's theme as a page sees it.
///
/// Dark or light, and MCP Apps' CSS variables (`--color-background-primary`, `--font-sans`, …)
/// as the app maps its tokens to them. The background variable is the page's background, which a
/// native shows opaque.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Theme {
    /// The dark appearance.
    pub dark: bool,
    /// The variables, by name with their `--`; a name or value that could break out of its
    /// declaration is left out.
    pub variables: Vec<(String, String)>,
}

/// The origins a page may reach once the app grants them, as MCP Apps' `_meta.ui.csp` names
/// them: `scheme://host[:port]`, the host possibly `*.`-prefixed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Network {
    /// What it may fetch and open sockets to (`connectDomains`).
    pub connect: Vec<String>,
    /// What it may load scripts, styles, images, fonts and media from (`resourceDomains`).
    pub resources: Vec<String>,
}

/// Why a grant was refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum NetworkError {
    /// Not an origin of the web or its sockets.
    #[error("not an origin a page may reach: {0}")]
    Origin(String),
    /// No such page.
    #[error("no such page")]
    NoPage,
}

/// What serves a page's files.
///
/// The view asks its own origin for one (`./data.csv`, a model, an image beside the page's
/// file) and this gets its path under the page's folder (`data.csv`,
/// `models/a.glb`; never `..`, `.` or an empty part, which are answered 404 here), and answers
/// through the [`Responder`] with the bytes or not found. The app backs it with the worker's read
/// of the page's folder (`ContentRef::item_asset`), so the host knows nothing of agents or
/// workers.
pub type Resources = Rc<dyn Fn(&str, Responder)>;

/// What a page is.
#[derive(Clone)]
pub struct PageSpec {
    /// Its name, which assistive technologies read and the host document is titled.
    pub title: String,
    /// The view's own document.
    pub html: String,
    /// The view is an MCP App's, which may call its server's tools and read its resources
    /// (through the person and the agent's door); `false` for a page of no server.
    pub server: bool,
    /// What serves its origin's other paths; none answers each with 404.
    pub resources: Option<Resources>,
}

impl std::fmt::Debug for PageSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PageSpec")
            .field("title", &self.title)
            .field("html", &self.html.len())
            .field("server", &self.server)
            .field("resources", &self.resources.is_some())
            .finish()
    }
}

/// A file the app serves a page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resource {
    /// Its MIME type.
    pub mime: String,
    /// Its bytes.
    pub bytes: Vec<u8>,
}

/// The answer to one request of a view's for a file. Answered once; dropped unanswered, it
/// answers 404.
pub struct Responder {
    task: Option<view::Task>,
}

impl std::fmt::Debug for Responder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Responder").field("open", &self.task.is_some()).finish()
    }
}

impl Responder {
    /// Answer with `resource`, or 404 when `None`. Main thread only; a request the view has
    /// given up on is not answered.
    pub fn answer(mut self, resource: Option<Resource>) {
        if let Some(task) = self.task.take() {
            task.answer(resource);
        }
    }
}

impl Drop for Responder {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.answer(None);
        }
    }
}

/// A request of the view's the app answers for the person: by its id, through
/// [`PageHost::answer`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestId(Value);

/// What a view asks the host to do for the person.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ask {
    /// `tools/call`: call a tool of the page's server (through the agent's door).
    CallTool {
        /// The tool.
        name: String,
        /// Its arguments.
        arguments: Value,
    },
    /// `resources/read`: read a resource of the page's server.
    ReadResource {
        /// The resource.
        uri: String,
    },
    /// `ui/message`: words for the chat, which become the person's draft, never sent alone.
    Message {
        /// Whose words they would be.
        role: String,
        /// MCP content blocks.
        content: Value,
    },
    /// `ui/update-model-context`: context for the model's next turn, shown before it rides.
    UpdateContext {
        /// The request's parameters (`content`, `structuredContent`).
        params: Value,
    },
    /// `ui/open-link`: an address to open.
    OpenLink {
        /// The address.
        url: String,
    },
    /// `ui/request-display-mode`: inline, fullscreen or pip.
    DisplayMode {
        /// The mode asked for.
        mode: String,
    },
}

/// One request of a view's, for the app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// Its id, to answer it by.
    pub id: RequestId,
    /// What it asks.
    pub ask: Ask,
    /// The person had just acted in the page (a click, a key) when it asked: a tool call needs
    /// it.
    pub activated: bool,
}

/// How the app declines a request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Declined {
    /// The JSON-RPC error code.
    pub code: i64,
    /// Why, in words for the view.
    pub message: String,
}

impl Declined {
    /// The person said no, or did not say yes.
    #[must_use]
    pub fn by_the_person() -> Self {
        Self { code: -32000, message: "Declined by the person".to_owned() }
    }
}

/// What the host tells a view of the tool call it shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    /// `ui/notifications/tool-input`: the call's whole arguments (`{"arguments": …}`).
    Input(Value),
    /// `ui/notifications/tool-input-partial`: the arguments so far, while they stream.
    PartialInput(Value),
    /// `ui/notifications/tool-result`: the call's result.
    Output(Value),
    /// `ui/notifications/tool-cancelled`: the call was stopped (`{"reason": …}`).
    Cancelled(Value),
}

impl Notice {
    const fn method(&self) -> &'static str {
        match self {
            Self::Input(_) => "ui/notifications/tool-input",
            Self::PartialInput(_) => "ui/notifications/tool-input-partial",
            Self::Output(_) => "ui/notifications/tool-result",
            Self::Cancelled(_) => "ui/notifications/tool-cancelled",
        }
    }

    const fn params(&self) -> &Value {
        match self {
            Self::Input(p) | Self::PartialInput(p) | Self::Output(p) | Self::Cancelled(p) => p,
        }
    }
}

/// What a page tried that its sandbox refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// A navigation away from the view: its own frame, the host document, a new window.
    Navigation {
        /// Where to.
        url: String,
    },
    /// A load the host document's policy blocked, as it reported it.
    Blocked {
        /// The directive it broke.
        directive: String,
        /// What it tried to load.
        url: String,
    },
    /// A method off the bridge's allowlist.
    Method {
        /// The method.
        method: String,
    },
    /// A request of the scheme's for another page's origin.
    Resource {
        /// The address.
        url: String,
    },
}

/// A picture of a page: its pixels as BGRA, premultiplied, the top row first, as GPUI's
/// images take them.
#[derive(Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// Width, in device pixels.
    pub width: u32,
    /// Height, in device pixels.
    pub height: u32,
    /// The pixels, four bytes each.
    pub bgra: Vec<u8>,
    /// From the ask to the pixels.
    pub took: Duration,
}

impl std::fmt::Debug for Snapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Snapshot")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("took", &self.took)
            .finish_non_exhaustive()
    }
}

/// What a page's sandbox has refused and seen, counted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    /// Navigations refused.
    pub navigations_refused: u32,
    /// Loads the policy blocked, as the host document reported them.
    pub blocked: u32,
    /// Methods off the allowlist.
    pub methods_refused: u32,
    /// Requests for another page's origin.
    pub resources_refused: u32,
    /// Times the view's document loaded.
    pub view_loads: u32,
}

/// What a page tells the app. Delivered on the main thread, from inside a framework callback:
/// the receiver must only queue it.
#[derive(Debug)]
pub enum PageEvent {
    /// The host document loaded.
    Loaded,
    /// The view's document loaded.
    ViewLoaded,
    /// The view finished MCP Apps' handshake: notices reach it from now on.
    Initialized,
    /// The view asked for a height, which the host clamped to its cap.
    Resized {
        /// What it asked for, in points.
        asked: f64,
        /// What it got.
        height: f64,
    },
    /// The view asks something of the person.
    Asked(Request),
    /// The view logged (`notifications/message`).
    Log {
        /// Its level.
        level: String,
        /// Its logger, when it named one.
        logger: Option<String>,
        /// What it logged.
        data: Value,
    },
    /// The sandbox refused something.
    Refused(Refusal),
    /// A new picture of the page.
    Snapshot(Rc<Snapshot>),
    /// The page's network grant took effect: it reaches this many origins now.
    NetworkSet(usize),
    /// Something the page needs failed (its rule list did not compile): it stays unloaded.
    Failed(String),
    /// The view answered the host's teardown.
    TornDown,
    /// The page's process ended.
    Crashed,
}

/// The least height a page takes, in points.
pub const MIN_HEIGHT: f64 = 80.0;
/// The most height a page takes inline, in points, on a viewport tall enough.
pub const MAX_HEIGHT: f64 = 720.0;
/// The share of the viewport's height a page takes at most inline.
pub const VIEWPORT_SHARE: f64 = 0.8;

/// The tallest a page may be inline in a viewport `viewport` points high.
#[must_use]
pub const fn cap(viewport: f64) -> f64 {
    let share = if viewport.is_finite() { viewport * VIEWPORT_SHARE } else { MAX_HEIGHT };
    share.clamp(MIN_HEIGHT, MAX_HEIGHT)
}

/// `asked` within `[MIN_HEIGHT, cap]`; anything not a number is the least.
#[must_use]
pub const fn clamp_height(asked: f64, cap: f64) -> f64 {
    if asked.is_finite() { asked.clamp(MIN_HEIGHT, cap.max(MIN_HEIGHT)) } else { MIN_HEIGHT }
}

/// What a window's pages have in common.
#[derive(Debug)]
struct Context {
    mtm: MainThreadMarker,
    device: Device,
    sink: Sink,
    theme: RefCell<Theme>,
    viewport: Cell<f64>,
    locale: RefCell<String>,
    /// The world the host document's script runs in, apart from the page's.
    world: Retained<WKContentWorld>,
}

impl Context {
    const fn cap(&self) -> f64 {
        cap(self.viewport.get())
    }
}

/// Where a host's events go.
#[derive(Clone)]
struct Sink(Rc<dyn Fn(PageId, PageEvent)>);

impl std::fmt::Debug for Sink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Sink")
    }
}

/// One page's state, shared with the objects `WebKit` calls back.
struct Shared {
    id: PageId,
    cx: Rc<Context>,
    title: String,
    html: String,
    server: bool,
    resources: Option<Resources>,
    grants: RefCell<sandbox::Grants>,
    /// The page's own rule list, once its grant compiled; the host's closed list otherwise.
    rules: RefCell<Option<Retained<WKContentRuleList>>>,
    web: RefCell<Option<objc2::rc::Weak<AnyObject>>>,
    width: Cell<f64>,
    height: Cell<f64>,
    /// The host's own next load of the page's address is expected: the one navigation of the
    /// main frame let through.
    admit: Cell<bool>,
    initialized: Cell<bool>,
    /// The tool call's notices so far, the last of each kind in the order they first came: sent
    /// to the view once it finishes its handshake, and again to each view a reload brings.
    told: RefCell<Vec<Value>>,
    /// The view's requests the app has not answered.
    pending: RefCell<Vec<Value>>,
    teardown: RefCell<Option<Value>>,
    asked: Cell<u64>,
    facts: Cell<Facts>,
    snapshot: RefCell<Option<Rc<Snapshot>>>,
    display_mode: RefCell<String>,
    /// The scheme's requests under way, each with whether `WebKit` still waits for it.
    tasks: RefCell<Vec<(usize, Rc<Cell<bool>>)>>,
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shared").field("id", &self.id).finish_non_exhaustive()
    }
}

impl Shared {
    fn emit(&self, event: PageEvent) {
        (self.cx.sink.0)(self.id, event);
    }

    fn web(&self) -> Option<Retained<AnyObject>> {
        self.web.borrow().as_ref().and_then(objc2::rc::Weak::load)
    }

    fn url(&self) -> String {
        format!("{}/", self.id.origin())
    }

    fn count(&self, bump: impl FnOnce(&mut Facts)) {
        let mut facts = self.facts.get();
        bump(&mut facts);
        self.facts.set(facts);
    }

    fn refuse(&self, refusal: Refusal) {
        self.count(|f| match &refusal {
            Refusal::Navigation { .. } => {
                f.navigations_refused = f.navigations_refused.saturating_add(1);
            }
            Refusal::Blocked { .. } => f.blocked = f.blocked.saturating_add(1),
            Refusal::Method { .. } => f.methods_refused = f.methods_refused.saturating_add(1),
            Refusal::Resource { .. } => f.resources_refused = f.resources_refused.saturating_add(1),
        });
        self.emit(PageEvent::Refused(refusal));
    }

    /// Run `f` with the host's context for this page, as the bridge tells it.
    fn with_context<R>(&self, f: impl FnOnce(&bridge::Context<'_>) -> R) -> R {
        let theme = self.cx.theme.borrow();
        let locale = self.cx.locale.borrow();
        let display_mode = self.display_mode.borrow();
        let context = bridge::Context::new(
            &theme,
            self.cx.device,
            &display_mode,
            self.width.get(),
            self.cx.cap(),
            &locale,
        );
        f(&context)
    }

    #[expect(
        clippy::redundant_closure_for_method_calls,
        reason = "the method's path names one lifetime, where the bound asks for any"
    )]
    fn context(&self) -> Value {
        self.with_context(|context| context.json())
    }

    /// The host document, as served now.
    fn document(&self) -> String {
        sandbox::host_document(&self.title, &self.html, &self.cx.theme.borrow())
    }

    fn policy(&self) -> String {
        sandbox::policy(self.id, &self.grants.borrow())
    }

    /// Tell the view of its tool call now, if it has finished its handshake, and keep it for
    /// the next view the page loads.
    fn tell(&self, method: &str, params: &Value) {
        let message = bridge::notification(method, params);
        {
            let mut told = self.told.borrow_mut();
            let same = |m: &Value| m.get("method").and_then(Value::as_str) == Some(method);
            match told.iter_mut().find(|m| same(m)) {
                Some(kept) => kept.clone_from(&message),
                None => told.push(message.clone()),
            }
        }
        if self.initialized.get() {
            view::post(self, &message);
        }
    }

    /// Tell a view that has finished its handshake that the host's context changed; one that has
    /// not gets the whole context with its handshake.
    fn changed(&self, params: &Value) {
        if self.initialized.get() {
            view::post(
                self,
                &bridge::notification("ui/notifications/host-context-changed", params),
            );
        }
    }

    /// What the view sent, through the relay.
    fn heard(self: &Rc<Self>, relayed: bridge::Relayed) -> Option<Value> {
        let (message, activated) = match relayed {
            bridge::Relayed::ViewLoaded => {
                self.count(|f| f.view_loads = f.view_loads.saturating_add(1));
                self.emit(PageEvent::ViewLoaded);
                view::snapshot(self);
                return None;
            }
            bridge::Relayed::Message { message, activated } => (message, activated),
        };
        match bridge::route(&message) {
            bridge::Route::Initialize { id } => Some(self.with_context(|context| {
                context.initialized(&id, self.server, &self.grants.borrow())
            })),
            bridge::Route::Ping { id } => Some(bridge::result(&id, &json!({}))),
            bridge::Route::Initialized => {
                self.initialized.set(true);
                let told = self.told.borrow().clone();
                for message in &told {
                    view::post(self, message);
                }
                self.emit(PageEvent::Initialized);
                None
            }
            bridge::Route::Size { height: asked } => {
                let height = clamp_height(asked, self.cx.cap());
                self.height.set(height);
                view::frame(self);
                self.emit(PageEvent::Resized { asked, height });
                view::snapshot(self);
                None
            }
            bridge::Route::Log { level, logger, data } => {
                self.emit(PageEvent::Log { level, logger, data });
                None
            }
            bridge::Route::Ask { id, ask } => {
                let mut pending = self.pending.borrow_mut();
                if pending.contains(&id) {
                    return Some(bridge::error(&id, bridge::INVALID_PARAMS, "request id in use"));
                }
                pending.push(id.clone());
                drop(pending);
                self.emit(PageEvent::Asked(Request { id: RequestId(id), ask, activated }));
                None
            }
            bridge::Route::Answered { id } => {
                let ours = self.teardown.borrow().as_ref() == Some(&id);
                if ours {
                    *self.teardown.borrow_mut() = None;
                    self.emit(PageEvent::TornDown);
                }
                None
            }
            bridge::Route::Invalid { id, method } => Some(bridge::error(
                &id,
                bridge::INVALID_PARAMS,
                &format!("invalid params for {method}"),
            )),
            bridge::Route::Refused { id, method } => {
                self.refuse(Refusal::Method { method: method.clone() });
                id.map(|id| {
                    bridge::error(
                        &id,
                        bridge::METHOD_NOT_FOUND,
                        &format!("{method} is not offered"),
                    )
                })
            }
            bridge::Route::Malformed => None,
        }
    }

    /// Load the page's host document, which its rule list is in place for.
    fn start(&self, rules: &WKContentRuleList) {
        let Some(web) = self.web() else { return };
        self.initialized.set(false);
        self.pending.borrow_mut().clear();
        self.admit.set(true);
        view::load(&web, rules, &self.url());
    }
}

/// The host's own rule list, the one every page starts with.
enum Closed {
    /// Being compiled; these pages load once it is.
    Compiling(Vec<Weak<Shared>>),
    Ready(Retained<WKContentRuleList>),
    Failed(String),
}

struct Entry {
    shared: Rc<Shared>,
    mounted: Option<view::Mounted>,
}

/// What a [`PageHost`] is made with.
#[derive(Clone, Debug, PartialEq)]
pub struct HostConfig {
    /// The device, which caps the live pages.
    pub device: Device,
    /// The theme pages take.
    pub theme: Theme,
    /// The height of the viewport pages show in, in points, which caps their height.
    pub viewport: f64,
    /// The person's language and region (BCP 47).
    pub locale: String,
}

/// One window's pages. Main thread only.
pub struct PageHost {
    cx: Rc<Context>,
    stage: RefCell<stage::Stage>,
    pages: RefCell<Vec<Entry>>,
    next: Cell<u64>,
    closed: Rc<RefCell<Closed>>,
}

impl std::fmt::Debug for PageHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PageHost")
            .field("pages", &self.pages.borrow().len())
            .finish_non_exhaustive()
    }
}

/// The width a page is laid out at before the app sizes it, in points.
const DEFAULT_WIDTH: f64 = 600.0;

impl PageHost {
    /// A host for one window's pages, telling `sink` what they do. It compiles the content rule
    /// list every page loads under at once; pages mounted before it is ready load when it is.
    /// `None` off the main thread.
    pub fn new(config: HostConfig, sink: impl Fn(PageId, PageEvent) + 'static) -> Option<Self> {
        let mtm = MainThreadMarker::new()?;
        let name = NSString::from_str(view::WORLD);
        // SAFETY: WebKit rule (`WKContentWorld.h`): a named world is made or found on the main
        // thread, and lives as long as it is held.
        let world = unsafe { WKContentWorld::worldWithName(&name, mtm) };
        let cx = Rc::new(Context {
            mtm,
            device: config.device,
            sink: Sink(Rc::new(sink)),
            theme: RefCell::new(config.theme),
            viewport: Cell::new(config.viewport),
            locale: RefCell::new(config.locale),
            world,
        });
        let closed = Rc::new(RefCell::new(Closed::Compiling(Vec::new())));
        let ready = Rc::clone(&closed);
        compile(&sandbox::rules(&sandbox::Grants::default()), mtm, move |outcome| {
            let was = std::mem::replace(
                &mut *ready.borrow_mut(),
                match &outcome {
                    Ok(list) => Closed::Ready(Retained::clone(list)),
                    Err(why) => Closed::Failed(why.clone()),
                },
            );
            let Closed::Compiling(waiting) = was else { return };
            for shared in waiting.iter().filter_map(Weak::upgrade) {
                match &outcome {
                    Ok(list) => {
                        let own = shared.rules.borrow().clone();
                        shared.start(own.as_deref().unwrap_or(list));
                    }
                    Err(why) => shared.emit(PageEvent::Failed(why.clone())),
                }
            }
        });
        Some(Self {
            cx,
            stage: RefCell::new(stage::Stage::new(config.device)),
            pages: RefCell::new(Vec::new()),
            next: Cell::new(1),
            closed,
        })
    }

    /// A new page, cold: [`Self::mount`] or [`Self::engage`] gives it a web view.
    #[must_use]
    pub fn open(&self, spec: PageSpec) -> PageId {
        let id = PageId(self.next.get());
        self.next.set(id.0.saturating_add(1));
        let shared = Rc::new(Shared {
            id,
            cx: Rc::clone(&self.cx),
            title: spec.title,
            html: spec.html,
            server: spec.server,
            resources: spec.resources,
            grants: RefCell::default(),
            rules: RefCell::new(None),
            web: RefCell::new(None),
            width: Cell::new(DEFAULT_WIDTH),
            height: Cell::new(MIN_HEIGHT),
            admit: Cell::new(false),
            initialized: Cell::new(false),
            told: RefCell::default(),
            pending: RefCell::default(),
            teardown: RefCell::new(None),
            asked: Cell::new(0),
            facts: Cell::default(),
            snapshot: RefCell::new(None),
            display_mode: RefCell::new("inline".to_owned()),
            tasks: RefCell::default(),
        });
        self.pages.borrow_mut().push(Entry { shared, mounted: None });
        self.stage.borrow_mut().add(id);
        id
    }

    /// Forget `page`: its web view goes, and its snapshot.
    pub fn close(&self, page: PageId) {
        let _was = self.stage.borrow_mut().remove(page);
        let entry = {
            let mut pages = self.pages.borrow_mut();
            pages.iter().position(|e| e.shared.id == page).map(|at| pages.remove(at))
        };
        drop(entry);
    }

    /// Give `page` a web view (warm), cooling the page mounted least lately past
    /// [`MOUNTED_CAP`]. What moved.
    #[must_use]
    pub fn mount(&self, page: PageId) -> Vec<Change> {
        let changes = self.stage.borrow_mut().mount(page);
        self.apply(&changes);
        changes
    }

    /// Make `page` live, mounting it when cold, resting the live page used least lately past
    /// the device's cap. What moved: a page gone warm has its snapshot taken, for GPUI to draw
    /// in its place.
    #[must_use]
    pub fn engage(&self, page: PageId) -> Vec<Change> {
        let changes = self.stage.borrow_mut().engage(page);
        self.apply(&changes);
        changes
    }

    /// `page` goes warm: its snapshot is taken.
    #[must_use]
    pub fn rest(&self, page: PageId) -> Vec<Change> {
        let changes = self.stage.borrow_mut().rest(page);
        self.apply(&changes);
        changes
    }

    /// Every live page goes warm: the app went to the background.
    #[must_use]
    pub fn rest_all(&self) -> Vec<Change> {
        let changes = self.stage.borrow_mut().rest_all();
        self.apply(&changes);
        changes
    }

    /// `page` loses its web view and its process; its last snapshot stays.
    #[must_use]
    pub fn unmount(&self, page: PageId) -> Vec<Change> {
        let changes = self.stage.borrow_mut().unmount(page);
        self.apply(&changes);
        changes
    }

    fn apply(&self, changes: &[Change]) {
        for change in changes {
            match (change.from, change.to) {
                (Presence::Cold, _) => self.mount_view(change.page),
                (_, Presence::Cold) => self.unmount_view(change.page),
                (Presence::Live, Presence::Warm) => {
                    if let Some(shared) = self.shared(change.page) {
                        view::snapshot(&shared);
                    }
                }
                _ => {}
            }
        }
    }

    fn mount_view(&self, page: PageId) {
        let mut pages = self.pages.borrow_mut();
        let Some(entry) = pages.iter_mut().find(|e| e.shared.id == page) else { return };
        if entry.mounted.is_some() {
            return;
        }
        let shared = Rc::clone(&entry.shared);
        let Some(mounted) = view::mount(&shared, &self.cx.world, self.cx.mtm) else {
            shared.emit(PageEvent::Failed("no web view".to_owned()));
            return;
        };
        entry.mounted = Some(mounted);
        drop(pages);
        let own = shared.rules.borrow().clone();
        if let Some(rules) = own {
            shared.start(&rules);
            return;
        }
        let ready = match &mut *self.closed.borrow_mut() {
            Closed::Ready(list) => Some(Retained::clone(list)),
            Closed::Compiling(waiting) => {
                waiting.push(Rc::downgrade(&shared));
                None
            }
            Closed::Failed(why) => {
                shared.emit(PageEvent::Failed(why.clone()));
                None
            }
        };
        if let Some(list) = ready {
            shared.start(&list);
        }
    }

    fn unmount_view(&self, page: PageId) {
        let mounted = {
            let mut pages = self.pages.borrow_mut();
            let Some(entry) = pages.iter_mut().find(|e| e.shared.id == page) else { return };
            // The view may outlive its owner in an autorelease pool: the page lets go of it now.
            *entry.shared.web.borrow_mut() = None;
            entry.shared.initialized.set(false);
            entry.mounted.take()
        };
        drop(mounted);
    }

    fn shared(&self, page: PageId) -> Option<Rc<Shared>> {
        self.pages.borrow().iter().find(|e| e.shared.id == page).map(|e| Rc::clone(&e.shared))
    }

    fn mounted(&self) -> Vec<Rc<Shared>> {
        self.pages
            .borrow()
            .iter()
            .filter(|e| e.mounted.is_some())
            .map(|e| Rc::clone(&e.shared))
            .collect()
    }

    /// Where `page` is now; `None` for no such page.
    #[must_use]
    pub fn presence(&self, page: PageId) -> Option<Presence> {
        self.stage.borrow().presence(page)
    }

    /// How many pages are `presence` now.
    #[must_use]
    pub fn count(&self, presence: Presence) -> usize {
        self.stage.borrow().count(presence)
    }

    /// `page`'s `WKWebView` while it is mounted: an `NSView *` or `UIView *` for a GPUI native
    /// host to adopt. Valid while the page stays mounted.
    #[must_use]
    pub fn view(&self, page: PageId) -> Option<NonNull<c_void>> {
        let pages = self.pages.borrow();
        pages.iter().find(|e| e.shared.id == page)?.mounted.as_ref().map(view::Mounted::view)
    }

    /// Lay `page` out `width` points wide; its height is its own, within the cap, as it last
    /// asked. The view hears its new width.
    pub fn resize(&self, page: PageId, width: f64) {
        let Some(shared) = self.shared(page) else { return };
        if !width.is_finite() || width <= 0.0 {
            return;
        }
        shared.width.set(width);
        view::frame(&shared);
        let dimensions =
            json!({ "containerDimensions": { "width": width, "maxHeight": self.cx.cap() } });
        if shared.web().is_some() {
            shared.changed(&dimensions);
        }
    }

    /// The viewport is `height` points high now: the cap on every page's height follows.
    pub fn set_viewport(&self, height: f64) {
        self.cx.viewport.set(height);
        let cap = self.cx.cap();
        for shared in self.mounted() {
            let dimensions =
                json!({ "containerDimensions": { "width": shared.width.get(), "maxHeight": cap } });
            shared.changed(&dimensions);
            let height = clamp_height(shared.height.get(), cap);
            if (height - shared.height.get()).abs() > f64::EPSILON {
                shared.height.set(height);
                view::frame(&shared);
            }
        }
    }

    /// The tallest a page may be now, in points.
    #[must_use]
    pub fn cap(&self) -> f64 {
        self.cx.cap()
    }

    /// The app's theme changed: every mounted page hears it and takes a new snapshot.
    pub fn set_theme(&self, theme: Theme) {
        *self.cx.theme.borrow_mut() = theme;
        for shared in self.mounted() {
            view::retheme(&shared);
            let context = shared.context();
            let params = json!({ "theme": context.get("theme"), "styles": context.get("styles") });
            shared.changed(&params);
            view::snapshot(&shared);
        }
    }

    /// The person's language and region changed.
    pub fn set_locale(&self, locale: &str) {
        locale.clone_into(&mut self.cx.locale.borrow_mut());
        for shared in self.mounted() {
            shared.changed(&json!({ "locale": locale }));
        }
    }

    /// `page` is shown as `mode` now (inline, fullscreen, pip).
    pub fn set_display_mode(&self, page: PageId, mode: &str) {
        let Some(shared) = self.shared(page) else { return };
        mode.clone_into(&mut shared.display_mode.borrow_mut());
        if shared.web().is_some() {
            shared.changed(&json!({ "displayMode": mode }));
        }
    }

    /// Tell `page`'s view of its tool call; held until the view has finished its handshake.
    /// Whether the page is mounted to hear it.
    #[must_use]
    pub fn notify(&self, page: PageId, notice: &Notice) -> bool {
        let Some(shared) = self.shared(page).filter(|s| s.web().is_some()) else { return false };
        shared.tell(notice.method(), notice.params());
        true
    }

    /// Answer the view's request `id`, as the person decided. Whether it was waiting.
    #[must_use]
    pub fn answer(&self, page: PageId, id: &RequestId, outcome: Result<Value, Declined>) -> bool {
        let Some(shared) = self.shared(page) else { return false };
        let waiting = {
            let mut pending = shared.pending.borrow_mut();
            pending.iter().position(|p| *p == id.0).map(|at| pending.remove(at)).is_some()
        };
        if !waiting {
            return false;
        }
        let message = match outcome {
            Ok(result) => bridge::result(&id.0, &result),
            Err(declined) => bridge::error(&id.0, declined.code, &declined.message),
        };
        view::post(&shared, &message);
        true
    }

    /// Ask `page`'s view to save its state before its page goes (`ui/resource-teardown`); its
    /// answer comes as [`PageEvent::TornDown`]. Whether it was asked.
    #[must_use]
    pub fn teardown(&self, page: PageId) -> bool {
        let Some(shared) = self.shared(page) else { return false };
        if !shared.initialized.get() || shared.web().is_none() {
            return false;
        }
        let n = shared.asked.get().saturating_add(1);
        shared.asked.set(n);
        let id = json!(format!("slopty-{n}"));
        *shared.teardown.borrow_mut() = Some(id.clone());
        view::post(&shared, &bridge::request(&id, "ui/resource-teardown", &json!({})));
        true
    }

    /// Let `page` reach `network`'s origins, and no others: the person's explicit grant. An
    /// empty one closes it again. A mounted page reloads under its new policy and rule list once
    /// the list compiles ([`PageEvent::NetworkSet`]).
    ///
    /// # Errors
    ///
    /// When an origin is not one of the web or its sockets, or there is no such page.
    pub fn set_network(&self, page: PageId, network: &Network) -> Result<(), NetworkError> {
        let shared = self.shared(page).ok_or(NetworkError::NoPage)?;
        let grants = sandbox::Grants::read(network)?;
        let reach = grants.reach();
        let empty = grants.is_empty();
        let rules = sandbox::rules(&grants);
        *shared.grants.borrow_mut() = grants;
        if empty {
            *shared.rules.borrow_mut() = None;
            let closed = match &mut *self.closed.borrow_mut() {
                Closed::Ready(list) => Some(Retained::clone(list)),
                Closed::Compiling(waiting) => {
                    waiting.push(Rc::downgrade(&shared));
                    None
                }
                Closed::Failed(_) => None,
            };
            if let Some(list) = closed.filter(|_| shared.web().is_some()) {
                shared.start(&list);
            }
            shared.emit(PageEvent::NetworkSet(0));
            return Ok(());
        }
        let weak = Rc::downgrade(&shared);
        compile(&rules, self.cx.mtm, move |outcome| {
            let Some(shared) = weak.upgrade() else { return };
            match outcome {
                Ok(list) => {
                    *shared.rules.borrow_mut() = Some(Retained::clone(&list));
                    if shared.web().is_some() {
                        shared.start(&list);
                    }
                    shared.emit(PageEvent::NetworkSet(reach));
                }
                Err(why) => shared.emit(PageEvent::Failed(why)),
            }
        });
        Ok(())
    }

    /// Take a new picture of `page`; it comes as [`PageEvent::Snapshot`]. Whether it is mounted
    /// to take one of.
    #[must_use]
    pub fn snapshot(&self, page: PageId) -> bool {
        let Some(shared) = self.shared(page).filter(|s| s.web().is_some()) else { return false };
        view::snapshot(&shared);
        true
    }

    /// `page`'s last picture.
    #[must_use]
    pub fn last_snapshot(&self, page: PageId) -> Option<Rc<Snapshot>> {
        self.shared(page)?.snapshot.borrow().clone()
    }

    /// The height `page` takes now, in points.
    #[must_use]
    pub fn height(&self, page: PageId) -> Option<f64> {
        Some(self.shared(page)?.height.get())
    }

    /// What `page`'s sandbox has refused and seen.
    #[must_use]
    pub fn facts(&self, page: PageId) -> Option<Facts> {
        Some(self.shared(page)?.facts.get())
    }

    /// The process id of `page`'s `WebContent` process while it is mounted.
    #[must_use]
    pub fn process(&self, page: PageId) -> Option<i32> {
        let web = self.shared(page)?.web()?;
        view::process(&web)
    }
}

/// Compile `rules` into a content rule list kept under a name of its own, for `done` on the main
/// thread.
fn compile(
    rules: &str,
    mtm: MainThreadMarker,
    done: impl FnOnce(Result<Retained<WKContentRuleList>, String>) + 'static,
) {
    // SAFETY: WebKit rule (`WKContentRuleListStore.h`): the default store is shared and made on
    // the main thread.
    let Some(store) = (unsafe { WKContentRuleListStore::defaultStore(mtm) }) else {
        return done(Err("no content rule list store".to_owned()));
    };
    let done = Cell::new(Some(done));
    let handler = RcBlock::new(move |list: *mut WKContentRuleList, error: *mut NSError| {
        let Some(done) = done.take() else { return };
        // SAFETY: WebKit rule: the handler gets a list or an error, each nil or valid for the
        // call; the list is retained to be kept.
        let list = unsafe { Retained::retain(list) };
        // SAFETY: as above, for the error.
        let why = unsafe { error.as_ref() }.map(|e| e.localizedDescription().to_string());
        done(list.ok_or_else(|| why.unwrap_or_else(|| "the rule list did not compile".to_owned())));
    });
    let identifier = NSString::from_str(&sandbox::rules_identifier(rules));
    let encoded = NSString::from_str(rules);
    // SAFETY: WebKit rule (`WKContentRuleListStore.h`): an identifier, a JSON rule list and a
    // handler it runs once, on the main thread.
    unsafe {
        store.compileContentRuleListForIdentifier_encodedContentRuleList_completionHandler(
            Some(&identifier),
            Some(&encoded),
            Some(&handler),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A page is at least 80 points and at most 720, or 0.8 of a shorter viewport.
    #[test]
    fn a_pages_height_is_clamped_to_the_cap() {
        let tall = cap(1200.0);
        assert!((tall - MAX_HEIGHT).abs() < f64::EPSILON);
        assert!((clamp_height(5000.0, tall) - MAX_HEIGHT).abs() < f64::EPSILON);
        assert!((clamp_height(300.0, tall) - 300.0).abs() < f64::EPSILON);
        assert!((clamp_height(10.0, tall) - MIN_HEIGHT).abs() < f64::EPSILON);
        assert!((clamp_height(f64::NAN, tall) - MIN_HEIGHT).abs() < f64::EPSILON);
        assert!((cap(500.0) - 400.0).abs() < f64::EPSILON, "0.8 of a short viewport");
        assert!((cap(50.0) - MIN_HEIGHT).abs() < f64::EPSILON);
    }

    /// A page's origin is its own: two pages, two hosts.
    #[test]
    fn each_page_has_an_origin_of_its_own() {
        assert_eq!(PageId(255).origin(), "slopty-page://ff");
        assert_ne!(PageId(1).origin(), PageId(2).origin());
    }
}

//! A browser tile: a web page the worker reaches, in a native web view laid over the tile.
//!
//! The item's address is the worker's (`http://localhost:5173/` means port 5173 on the
//! worker, `http://db-admin:8080/` the host the worker's resolver names), so every client of
//! the worker opens the same page. A page's network never proxies the loopback, so each client
//! serves a loopback port on its own loopback, at whatever local port it could get, and
//! rewrites the address to it before the page loads ([`local_url`]). Every other host goes
//! through the worker's proxy (`slopty_client::tunnel::Proxy`) as it is, in the worker's own
//! data store (`slopty_platform::web::route`).
//!
//! GPUI cannot draw a page, so the page is the platform's web view (`slopty_platform::web`),
//! which the window composes with GPUI's content through a native host: the tile's body is a
//! `native_view` element, and the page shows wherever that element is drawn, cut to what clips
//! it (the strip) and under whatever GPUI draws after it (a menu, the palette, a toast, a
//! script's dialog). A tile drawn scaled (the overview) shows the page's last snapshot, as a
//! page laid out at that size would reflow. GPUI's focus on the element is the page's
//! keyboard: a click in the page focuses it, and focus leaving it gives the keyboard back.
//!
//! What a browser brings of its own sits in the tile: the find bar above the page, a
//! download's row below it, and a script's dialog over it while the page waits for the answer.
//! A pop-up opens a tile of its own (`docs/decisions/ui.md`, "The browser tile's own chrome").

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::accesskit::Role;
use gpui::composition::{NativeHost, NativeHostOptions, native_view};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AnyWindowHandle, AppContext as _, Context, Entity, EventEmitter, FocusHandle,
    InteractiveElement as _, IntoElement, KeyDownEvent, ObjectFit, ParentElement as _, Render,
    RenderImage, SharedString, StatefulInteractiveElement as _, Styled as _, StyledImage as _,
    Subscription, Task, Window, div, img, px,
};
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState};
pub use native::{Dialog, DialogKind, Edit};
use slopty_client::layout::WorkerKey;
use slopty_core::ItemId;
use slopty_proto::transfer::TunnelRefusal;
use slopty_theme::Theme;

use crate::colors::hsla;
use crate::icons::{IconName, IconSize};
use crate::kit::find::{PLACEHOLDER as FIND_PLACEHOLDER, Tally};
use crate::kit::{self, ButtonKind, FindBar};
use crate::terminal::{CloseFind, FindNext, FindPrev};
use crate::workspace::actions::{PageCut, PageRedo, PageSelectAll, PageUndo, UndoClose};

/// The key context of a page's area; the keymap binds the page's own edits under it, where
/// the page holds the keyboard (`keymap::PAGE_HELD`).
pub const CTX: &str = "PageBody";

/// Two presses of Esc on a page closer than this give the keyboard back; one alone reaches
/// the page.
const DOUBLE_ESCAPE: Duration = Duration::from_millis(400);

/// How often a shown page is asked for its title and address, which scripts change without
/// a navigation.
const POLL: Duration = Duration::from_secs(1);

/// How often a download under way is asked how far it has come.
const DOWNLOAD_POLL: Duration = Duration::from_millis(250);

/// A script's dialog at its widest, as Safari's sheet.
const DIALOG_WIDTH: f32 = 360.0;

/// The page's zoom steps, Safari's: ⌘+ and ⌘− walk them, ⌘0 goes back to 1.
pub const ZOOMS: [f64; 13] = [0.5, 0.67, 0.75, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0];

/// A step of the page's zoom.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Zoom {
    /// ⌘+: the next step up.
    In,
    /// ⌘−: the next step down.
    Out,
    /// ⌘0: the page's own size.
    Reset,
}

/// The zoom after `step` from `current`; past either end of [`ZOOMS`] it stays.
#[must_use]
pub fn next_zoom(current: f64, step: Zoom) -> f64 {
    const NUDGE: f64 = 0.001;
    match step {
        Zoom::Reset => 1.0,
        Zoom::In => ZOOMS.iter().copied().find(|z| *z > current + NUDGE).unwrap_or(current),
        Zoom::Out => ZOOMS.iter().rev().copied().find(|z| *z < current - NUDGE).unwrap_or(current),
    }
}

/// What a browser tile tells the workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BrowserEvent {
    /// The page took the keyboard (a click in it): its tile takes the focus.
    Focused,
    /// The keyboard is the workspace's again: the page gave it back with nothing else taking
    /// it (Esc twice, a click where nothing takes the keyboard), or the find bar or a dialog
    /// that had it closed.
    Released,
    /// A pop-up or a `_blank` link asked for this address, as the worker names it: a tile of
    /// its own beside this one.
    Open(String),
    /// The page failed to load: the workspace may know why better than the page does (the
    /// worker could not reach its host), and say so ([`BrowserView::unreachable`]).
    Failed,
    /// The page closed its own window (`window.close()`): its tile goes.
    Closed,
}

/// A download of the page's, as its row shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadRow {
    /// The platform's id for it.
    pub id: u64,
    /// Where it lands.
    pub path: PathBuf,
    /// How it goes.
    pub state: DownloadState,
}

/// How a download goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DownloadState {
    /// Bytes are coming: `done` of `total` (0 while the server has not said).
    Receiving {
        /// Bytes saved so far.
        done: u64,
        /// Bytes in all, or 0.
        total: u64,
    },
    /// All of it is saved.
    Saved,
    /// It stopped; why.
    Failed(String),
}

impl DownloadRow {
    /// The file's name.
    #[must_use]
    pub fn name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    /// What the row says of how it goes: `42% of 3.1 MB`, `3.1 MB`, `Saved`, the failure.
    #[must_use]
    pub fn status(&self) -> String {
        match &self.state {
            DownloadState::Receiving { done, total: 0 } => kit::size_label(*done),
            DownloadState::Receiving { done, total } => {
                let percent = done.saturating_mul(100).checked_div(*total).unwrap_or(0).min(100);
                format!("{percent}% of {}", kit::size_label(*total))
            }
            DownloadState::Saved => "Saved".to_owned(),
            DownloadState::Failed(why) => why.clone(),
        }
    }
}

/// The find bar over the page.
struct PageFind {
    input: Entity<InputState>,
    needle: String,
    /// The page's answer for the needle, once it has one.
    found: Option<bool>,
    /// How many times the needle is in the page's text, once the page has counted.
    count: Option<usize>,
    _subscription: Subscription,
}

/// A script's dialog, waiting in the tile.
struct PageDialog {
    dialog: Dialog,
    /// Who asks: the page's host.
    host: String,
    /// A prompt's field.
    input: Option<Entity<InputState>>,
    focus: FocusHandle,
    _subscription: Option<Subscription>,
}

/// What the page shows, as last read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PageState {
    /// The document's title, empty before one loads.
    pub title: String,
    /// The address shown now.
    pub url: String,
    /// A navigation is under way.
    pub loading: bool,
    /// There is a page to go back to.
    pub can_go_back: bool,
    /// There is a page to go forward to.
    pub can_go_forward: bool,
    /// Why the last navigation failed, until the next one starts.
    pub failed: Option<String>,
}

/// The view of one browser item.
pub struct BrowserView {
    id: ItemId,
    worker: WorkerKey,
    /// The item's address, as the worker sees it.
    url: String,
    /// The address this client loads: `url` with a loopback port moved to where this client
    /// serves it. `None` until the worker's port is served here.
    local: Option<String>,
    /// The address the open page was last given.
    loaded: Option<String>,
    page: PageState,
    /// The page's last picture, shown where the tile is drawn scaled and in a render.
    snapshot: Option<Arc<RenderImage>>,
    theme: Theme,
    native: native::Native,
    /// Where the page is composed into its window, once it is open there.
    host: Option<PageHost>,
    /// GPUI's focus standing for the page's keyboard: while it is focused the page holds it.
    page_focus: FocusHandle,
    /// Hears the page take the keyboard and give it back, once the view is in a window.
    focus_watch: Vec<Subscription>,
    /// The tile is drawn at its own size, so the page itself shows. Drawn scaled (the
    /// overview, the strip zoomed out) it shows its snapshot.
    live: bool,
    /// When Esc last went to the page, for a second one soon after to give the keyboard back.
    last_escape: Option<Instant>,
    /// The page's messages, and the poll for its title while it is open.
    tasks: Vec<Task<()>>,
    /// The page's zoom, 1 at its own size.
    zoom: f64,
    search: Option<PageFind>,
    dialog: Option<PageDialog>,
    downloads: Vec<DownloadRow>,
    /// A poll of the downloads under way is running.
    polling_downloads: bool,
}

/// The page's native host in the window it is composed into.
struct PageHost {
    window: AnyWindowHandle,
    host: NativeHost,
}

impl std::fmt::Debug for BrowserView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserView")
            .field("id", &self.id)
            .field("url", &self.url)
            .field("page", &self.page)
            .finish_non_exhaustive()
    }
}

impl EventEmitter<BrowserEvent> for BrowserView {}

impl BrowserView {
    /// A tile for `url` on `worker`; the page itself opens the first time the tile is shown
    /// once the address is served here ([`Self::set_local`]).
    #[must_use]
    pub fn new(id: ItemId, worker: WorkerKey, url: &str, theme: Theme, cx: &Context<Self>) -> Self {
        Self {
            id,
            worker,
            url: url.to_owned(),
            local: None,
            loaded: None,
            page: PageState { url: url.to_owned(), loading: true, ..PageState::default() },
            snapshot: None,
            theme,
            native: native::Native::default(),
            host: None,
            page_focus: cx.focus_handle(),
            focus_watch: Vec::new(),
            live: true,
            last_escape: None,
            tasks: Vec::new(),
            zoom: 1.0,
            search: None,
            dialog: None,
            downloads: Vec::new(),
            polling_downloads: false,
        }
    }

    /// Item this tile belongs to.
    #[must_use]
    pub const fn id(&self) -> ItemId {
        self.id
    }

    /// The address the item names, as the worker sees it.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The worker the item is on.
    #[must_use]
    pub const fn worker(&self) -> WorkerKey {
        self.worker
    }

    /// The address this client loads, once it has one.
    #[must_use]
    pub fn local_url(&self) -> Option<&str> {
        self.local.as_deref()
    }

    /// Where this client reaches the page. `None` forgets it (the worker's link came back,
    /// so the port is served anew); an address the open page is not on loads in it.
    pub fn set_local(&mut self, local: Option<String>, cx: &mut Context<Self>) {
        if self.local == local {
            return;
        }
        if let Some(address) = &local
            && self.native.open()
            && self.loaded.as_ref() != Some(address)
        {
            tracing::info!(item = %self.id, %address, "the page moves to a new local port");
            self.native.load(address);
            self.loaded = Some(address.clone());
        }
        self.local = local;
        cx.notify();
    }

    /// The page cannot be reached from here; `why` shows in the body, and "↻" tries again.
    pub fn unreachable(&mut self, why: String, cx: &mut Context<Self>) {
        self.page.failed = Some(why);
        self.page.loading = false;
        self.local = None;
        cx.notify();
    }

    /// The address this client loads has to be worked out (again).
    #[must_use]
    pub const fn needs_local(&self) -> bool {
        self.local.is_none() && self.page.failed.is_none()
    }

    /// What the page shows, as last read.
    #[must_use]
    pub const fn page(&self) -> &PageState {
        &self.page
    }

    /// What the page shows this moment, read from the view itself (the self-test's
    /// readback); the last read when there is no view.
    #[must_use]
    pub fn live_page(&self) -> PageState {
        self.native.page().map_or_else(|| self.page.clone(), |page| self.as_the_worker_sees(page))
    }

    /// `page` read from the view, its address put back on the worker's port and the last
    /// failure kept.
    fn as_the_worker_sees(&self, page: PageState) -> PageState {
        let url = match &self.loaded {
            Some(loaded) => worker_url(&page.url, loaded, &self.url),
            None => page.url,
        };
        PageState { url, failed: self.page.failed.clone(), ..page }
    }

    /// Whether the page is on screen now, as the platform shows it.
    #[must_use]
    pub fn shown(&self) -> bool {
        self.native.shown()
    }

    /// Whether the page has the keyboard, as the platform's first responder says.
    #[must_use]
    pub fn focused(&self) -> bool {
        self.native.focused()
    }

    /// Whether GPUI's focus is on the page, which gives it the platform's keyboard.
    #[must_use]
    pub fn holds_keyboard(&self, window: &Window) -> bool {
        self.page_focus.is_focused(window)
    }

    /// Whether a picture of the page is ready for when the tile is drawn without it.
    #[must_use]
    pub const fn has_snapshot(&self) -> bool {
        self.snapshot.is_some()
    }

    /// The number of the window the page is in, for a key the self-test delivers there.
    #[cfg(target_os = "macos")]
    #[must_use]
    pub fn window_number(&self) -> Option<isize> {
        self.native.window_number()
    }

    /// A test's page with history `back` and `forward` of it, as a load would read back.
    #[cfg(test)]
    pub(crate) fn set_history(&mut self, back: bool, forward: bool, cx: &mut Context<Self>) {
        self.page.can_go_back = back;
        self.page.can_go_forward = forward;
        cx.notify();
    }

    /// The edits done in a test's page, oldest first.
    #[cfg(test)]
    pub(crate) fn performed(&self) -> Vec<Edit> {
        self.native.performed()
    }

    /// The page's native host, once it is open in a window.
    #[must_use]
    pub fn native_host(&self) -> Option<&NativeHost> {
        self.host.as_ref().map(|h| &h.host)
    }

    /// Whether the tile is drawn at its own size, which shows the page itself; scaled, it
    /// shows its snapshot, taken again as it goes.
    pub fn set_live(&mut self, live: bool, cx: &mut Context<Self>) {
        if self.live != live {
            self.live = live;
            if !live {
                self.native.snapshot();
            }
            cx.notify();
        }
    }

    /// Draw by another theme.
    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        if self.theme != theme {
            self.theme = theme;
            cx.notify();
        }
    }

    /// Open the page at `address` in `window`: the web view, and the host that composes it
    /// there. Opened as the window draws, the first time the tile is drawn with an address.
    fn open(&mut self, address: &str, window: &mut Window, cx: &mut Context<Self>) {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<native::Event>();
        let sink: Rc<dyn Fn(native::Event)> = Rc::new(move |event| {
            let _sent = tx.send(event);
        });
        let opened =
            self.native.create(window, self.worker, address, sink) && self.compose_in(window, cx);
        if !opened {
            self.native = native::Native::default();
            self.page.failed = Some("This device has no web view".to_owned());
            // Opened as the window draws, where a notify would only reach the next frame the
            // window happens to draw: this one asks for it.
            let view = cx.entity_id();
            cx.defer(move |cx| cx.notify(view));
            return;
        }
        self.loaded = Some(address.to_owned());
        if (self.zoom - 1.0).abs() > f64::EPSILON {
            self.native.set_zoom(self.zoom);
        }
        tracing::info!(item = %self.id, url = %self.url, %address, "browser tile opened");
        let events = cx.spawn_in(window, async move |this, cx| {
            while let Some(event) = rx.recv().await {
                let sent =
                    this.update_in(cx, |view, window, cx| view.native_event(event, window, cx));
                if sent.is_err() {
                    break;
                }
            }
        });
        let poll = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL).await;
                if this.update(cx, Self::refresh).is_err() {
                    break;
                }
            }
        });
        self.tasks = vec![events, poll];
    }

    /// Compose the open page into `window`, whose GPUI view the page's host is made in.
    /// Whether it could: a window of a platform that composes natives.
    fn compose_in(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let here = window.window_handle();
        if self.host.as_ref().is_some_and(|h| h.window == here) {
            return true;
        }
        let options = NativeHostOptions {
            opaque: true,
            interactive: true,
            label: Some(SharedString::from(format!("Page {}", self.url))),
        };
        let host = match window.create_native_host(options, cx) {
            Ok(host) => host,
            Err(e) => {
                tracing::warn!(item = %self.id, "no native host for the page: {e:#}");
                return false;
            }
        };
        if !self.native.attach(&host) {
            return false;
        }
        self.host = Some(PageHost { window: here, host });
        self.watch_focus(window, cx);
        true
    }

    /// Hear the page take the keyboard (GPUI focusing its element: a click in it, or the
    /// platform's first responder moving into it) and give it back.
    fn watch_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let took = cx.on_focus(&self.page_focus, window, |this, _window, cx| {
            this.last_escape = None;
            cx.emit(BrowserEvent::Focused);
        });
        // Focus that went somewhere (a field, another tile) is where it belongs; focus that
        // went nowhere is the workspace's again.
        let gave = cx.on_blur(&self.page_focus, window, |_this, window, cx| {
            if window.focused(cx).is_none() {
                cx.emit(BrowserEvent::Released);
            }
        });
        self.focus_watch = vec![took, gave];
    }

    /// A key while the page holds the keyboard, before any binding and the page see it: a
    /// second Esc soon after the first gives the keyboard back, and is the page's no more.
    fn page_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let m = keystroke.modifiers;
        if keystroke.key != "escape" || m.control || m.alt || m.shift || m.platform || m.function {
            return;
        }
        let now = Instant::now();
        let twice =
            self.last_escape.is_some_and(|at| now.saturating_duration_since(at) < DOUBLE_ESCAPE);
        self.last_escape = if twice { None } else { Some(now) };
        if twice {
            cx.stop_propagation();
            window.blur(cx);
        }
    }

    /// Open as a test's page: composed into `window` through a native host of the test
    /// platform's, with no web view behind it.
    #[cfg(test)]
    pub(crate) fn open_stand_in(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.native = native::Native::stand_in();
        self.loaded = Some(self.url.clone());
        let composed = self.compose_in(window, cx);
        debug_assert!(composed, "the test platform composes natives");
        cx.notify();
    }

    /// A message from the page (or a test's, standing in for it).
    pub(crate) fn native_event(
        &mut self,
        event: native::Event,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(event, native::Event::Snapshot(_)) {
            tracing::debug!(item = %self.id, ?event, "page event");
        }
        match event {
            native::Event::Loaded => {
                self.page.failed = None;
                self.refresh(cx);
                self.native.snapshot();
            }
            native::Event::Failed(why) => {
                tracing::info!(item = %self.id, %why, "page failed");
                self.page.failed = Some(why);
                self.page.loading = false;
                cx.emit(BrowserEvent::Failed);
                cx.notify();
            }
            native::Event::Snapshot(png) => {
                let decoding = cx.background_spawn(async move { texture(&png) });
                cx.spawn(async move |this, cx| {
                    if let Some(image) = decoding.await {
                        let _gone = this.update(cx, |view, cx| {
                            view.snapshot = Some(image);
                            cx.notify();
                        });
                    }
                })
                .detach();
            }
            native::Event::Open(url) => {
                let url = match &self.loaded {
                    Some(loaded) => worker_url(&url, loaded, &self.url),
                    None => url,
                };
                if is_web_url(&url) {
                    tracing::info!(item = %self.id, %url, "the page asks for a new window");
                    cx.emit(BrowserEvent::Open(url));
                } else {
                    tracing::info!(item = %self.id, %url, "a new window with no web address");
                }
            }
            native::Event::Dialog(dialog) => self.show_dialog(dialog, window, cx),
            native::Event::Download(download) => self.download_event(download, cx),
            native::Event::Found(found) => {
                if let Some(search) = &mut self.search {
                    search.found = Some(found);
                    cx.notify();
                }
            }
            native::Event::Counted { needle, count } => {
                // A count for text since typed over is not this one's.
                if let Some(search) = self.search.as_mut().filter(|s| s.needle == needle) {
                    search.count = Some(count);
                    cx.notify();
                }
            }
            native::Event::Closed => {
                tracing::info!(item = %self.id, "the page closed its window");
                cx.emit(BrowserEvent::Closed);
            }
        }
    }

    /// Read the page's title and address again; a change repaints the header. Between a new
    /// address and its load the view still names the old page, so it is not asked.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.loaded != self.local {
            return;
        }
        let Some(page) = self.native.page() else { return };
        let page = self.as_the_worker_sees(page);
        if page != self.page {
            self.page = page;
            cx.notify();
        }
    }

    /// Go to `url`, the item's address as the worker names it, which the item has just
    /// taken (`ItemOp::SetUrl`, from any client). The header names it at once. A new address
    /// is served here anew and loads once it is (`set_local`); the same one loads again.
    pub fn go_to(&mut self, url: &str, cx: &mut Context<Self>) {
        if url == self.url {
            if let Some(local) = self.local.clone() {
                self.native.load(&local);
                self.loaded = Some(local);
            }
        } else {
            tracing::info!(item = %self.id, %url, "the page goes to a new address");
            url.clone_into(&mut self.url);
            self.local = None;
        }
        self.page = PageState { url: url.to_owned(), loading: true, ..PageState::default() };
        cx.notify();
    }

    /// Back one page.
    pub fn back(&mut self, cx: &mut Context<Self>) {
        self.native.back();
        self.page.loading = true;
        cx.notify();
    }

    /// Forward one page.
    pub fn forward(&mut self, cx: &mut Context<Self>) {
        self.native.forward();
        self.page.loading = true;
        cx.notify();
    }

    /// Load the page again; a failed page gets another try.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        if self.page.failed.take().is_some() {
            // Never opened: the port is asked for again on the next frame.
            if let Some(address) = &self.loaded {
                self.native.load(address);
            }
        } else {
            self.native.reload();
        }
        self.page.loading = true;
        cx.notify();
    }

    /// ⌘F: the find bar above the page, or the caret back in it with its text selected. A
    /// page that had the keyboard gives it to the bar.
    pub fn find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search.is_none() {
            let input = cx.new(|cx| InputState::new(window, cx).placeholder(FIND_PLACEHOLDER));
            let subscription = cx.subscribe(&input, |this, _input, event, cx| match event {
                InputEvent::Change => this.search_changed(cx),
                InputEvent::PressEnter { shift, .. } => this.find_step(*shift),
                InputEvent::Focus | InputEvent::Blur => {}
            });
            self.search = Some(PageFind {
                input,
                needle: String::new(),
                found: None,
                count: None,
                _subscription: subscription,
            });
        }
        if let Some(search) = &self.search {
            search.input.update(cx, |input, cx| {
                input.focus(window, cx);
                input.select_all(window, cx);
            });
        }
        cx.notify();
    }

    fn search_changed(&mut self, cx: &mut Context<Self>) {
        let Some(search) = &mut self.search else { return };
        let needle = search.input.read(cx).value().to_string();
        if needle == search.needle {
            return;
        }
        search.needle = needle;
        search.found = None;
        search.count = None;
        if !search.needle.is_empty() {
            self.native.find(&search.needle, false);
            self.native.count(&search.needle);
        }
        cx.notify();
    }

    /// The next match (↩), or the one before (⇧↩).
    pub fn find_step(&self, backwards: bool) {
        if let Some(search) = self.search.as_ref().filter(|s| !s.needle.is_empty()) {
            self.native.find(&search.needle, backwards);
        }
    }

    /// Esc or ✕ in the find bar: it closes, and the keyboard goes back to the workspace.
    pub fn close_find(&mut self, cx: &mut Context<Self>) {
        if self.search.take().is_some() {
            cx.emit(BrowserEvent::Released);
            cx.notify();
        }
    }

    /// The find bar's text, while the bar is open.
    #[must_use]
    pub fn search_needle(&self) -> Option<&str> {
        self.search.as_ref().map(|s| s.needle.as_str())
    }

    /// Whether the page has the find bar's text, once it has answered.
    #[must_use]
    pub fn found(&self) -> Option<bool> {
        self.search.as_ref().and_then(|s| s.found)
    }

    /// Web Inspector on the page, in its own window. Whether it opened: a page that is open
    /// in the Mac app.
    pub fn inspect(&self) -> bool {
        self.native.inspect()
    }

    /// ⌘+, ⌘− or ⌘0 on the page.
    pub fn zoom_by(&mut self, step: Zoom, cx: &mut Context<Self>) {
        let zoom = next_zoom(self.zoom, step);
        if (zoom - self.zoom).abs() > f64::EPSILON {
            self.zoom = zoom;
            self.native.set_zoom(zoom);
            cx.notify();
        }
    }

    /// The page's zoom, 1 at its own size.
    #[must_use]
    pub const fn zoom(&self) -> f64 {
        self.zoom
    }

    /// A script's dialog: drawn in the tile on the page's picture, the keyboard in it. One
    /// still up is dismissed first, so the page never waits on two.
    fn show_dialog(&mut self, dialog: Dialog, window: &mut Window, cx: &mut Context<Self>) {
        let input = match dialog.kind() {
            DialogKind::Prompt { default } => {
                let default = default.clone();
                Some(cx.new(|cx| InputState::new(window, cx).default_value(default)))
            }
            DialogKind::Alert | DialogKind::Confirm => None,
        };
        let subscription = input.as_ref().map(|input| {
            cx.subscribe(input, |this, _input, event, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.answer_dialog(true, cx);
                }
            })
        });
        let focus = cx.focus_handle();
        match &input {
            Some(input) => input.update(cx, |input, cx| {
                input.focus(window, cx);
                input.select_all(window, cx);
            }),
            None => window.focus(&focus, cx),
        }
        let host =
            split_origin(&self.page.url).map(|(_, host, _)| host.to_owned()).unwrap_or_default();
        tracing::info!(item = %self.id, kind = ?dialog.kind(), "a script's dialog");
        self.dialog = Some(PageDialog { dialog, host, input, focus, _subscription: subscription });
        cx.notify();
    }

    /// The dialog up, if one is.
    #[must_use]
    pub fn dialog(&self) -> Option<&Dialog> {
        self.dialog.as_ref().map(|d| &d.dialog)
    }

    /// A prompt's field, while one is up.
    #[cfg(test)]
    pub(crate) fn dialog_field(&self) -> Option<Entity<InputState>> {
        self.dialog.as_ref().and_then(|d| d.input.clone())
    }

    /// Whether a script's dialog holds the keyboard: the sheet, or a prompt's field.
    #[must_use]
    pub fn dialog_has_keyboard(&self, window: &Window, cx: &gpui::App) -> bool {
        self.dialog.as_ref().is_some_and(|d| {
            d.focus.is_focused(window)
                || d.input.as_ref().is_some_and(|i| {
                    gpui::Focusable::focus_handle(i.read(cx), cx).is_focused(window)
                })
        })
    }

    /// OK (with a prompt's text) or Cancel: the page goes on, and the keyboard goes back to
    /// the workspace.
    pub fn answer_dialog(&mut self, ok: bool, cx: &mut Context<Self>) {
        let Some(pending) = self.dialog.take() else { return };
        if ok {
            let text = pending.input.map(|i| i.read(cx).value().to_string()).unwrap_or_default();
            pending.dialog.accept(&text);
        } else {
            pending.dialog.dismiss();
        }
        cx.emit(BrowserEvent::Released);
        cx.notify();
    }

    fn download_event(&mut self, event: native::Download, cx: &mut Context<Self>) {
        match event {
            native::Download::Started { id, path } => {
                tracing::info!(item = %self.id, path = %path.display(), "a download starts");
                self.downloads.push(DownloadRow {
                    id,
                    path,
                    state: DownloadState::Receiving { done: 0, total: 0 },
                });
                self.poll_downloads(cx);
            }
            native::Download::Finished { id } => {
                if let Some(row) = self.downloads.iter_mut().find(|r| r.id == id) {
                    row.state = DownloadState::Saved;
                }
            }
            native::Download::Failed { id, why } => {
                tracing::info!(item = %self.id, %why, "a download failed");
                if let Some(row) = self.downloads.iter_mut().find(|r| r.id == id) {
                    row.state = DownloadState::Failed(why);
                }
            }
        }
        cx.notify();
    }

    /// Ask how far each download under way has come, every [`DOWNLOAD_POLL`] while one is.
    fn poll_downloads(&mut self, cx: &Context<Self>) {
        if self.polling_downloads {
            return;
        }
        self.polling_downloads = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(DOWNLOAD_POLL).await;
                let going = this.update(cx, Self::read_downloads).unwrap_or(false);
                if !going {
                    break;
                }
            }
        })
        .detach();
    }

    /// Read each download's progress; whether any is still under way.
    fn read_downloads(&mut self, cx: &mut Context<Self>) -> bool {
        let mut going = false;
        let mut changed = false;
        for row in &mut self.downloads {
            let DownloadState::Receiving { done, total } = &mut row.state else { continue };
            going = true;
            if let Some(now) = self.native.received(row.id)
                && now != (*done, *total)
            {
                (*done, *total) = now;
                changed = true;
            }
        }
        if changed {
            cx.notify();
        }
        self.polling_downloads = going;
        going
    }

    /// The page's downloads, as their rows show them.
    #[must_use]
    pub fn downloads(&self) -> &[DownloadRow] {
        &self.downloads
    }

    /// ✕ on a download's row: one under way stops; the row goes.
    pub fn dismiss_download(&mut self, id: u64, cx: &mut Context<Self>) {
        let receiving = |r: &DownloadRow| matches!(r.state, DownloadState::Receiving { .. });
        if self.downloads.iter().any(|r| r.id == id && receiving(r)) {
            self.native.cancel_download(id);
        }
        self.downloads.retain(|r| r.id != id);
        cx.notify();
    }

    /// The header's words: the page's title, else its address without the scheme.
    #[must_use]
    pub fn title(&self) -> String {
        if self.page.title.trim().is_empty() {
            short_url(&self.page.url).to_owned()
        } else {
            self.page.title.trim().to_owned()
        }
    }

    /// The address as the header shows it beside a title: host, port and path.
    #[must_use]
    pub fn short_url(&self) -> &str {
        short_url(&self.page.url)
    }
}

/// What a page's find bar says of `needle`'s matches: how many, none, or nothing yet.
///
/// From the page's find (`found`) and its count. The page steps through its matches without
/// saying which one is on show.
#[must_use]
pub const fn match_tally(needle: &str, found: Option<bool>, count: Option<usize>) -> Tally {
    if needle.is_empty() {
        return Tally::Quiet;
    }
    match (found, count) {
        (Some(false), _) | (None, Some(0)) => Tally::Found { at: None, total: 0, more: false },
        (_, Some(n)) if n > 0 => Tally::Found { at: None, total: n, more: false },
        // Found where the count does not reach (a frame of the page's), or not answered yet.
        _ => Tally::Quiet,
    }
}

/// `url` without its scheme and a lone trailing slash: `localhost:5173/app`.
#[must_use]
pub fn short_url(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.strip_suffix('/').filter(|r| !r.contains('/')).unwrap_or(rest)
}

/// An address as a header shows it at rest: its scheme with `://`, its host (and port), and
/// the rest, where a lone `/` is none. `None` for text with no scheme.
#[must_use]
pub fn address_parts(url: &str) -> Option<(&str, &str, &str)> {
    let (_, after) = url.split_once("://")?;
    let scheme = url.strip_suffix(after)?;
    let end = after.find(['/', '?', '#']).unwrap_or(after.len());
    let (authority, rest) = after.split_at(end);
    Some((scheme, authority, if rest == "/" { "" } else { rest }))
}

/// Whether `text` is an address a browser tile opens: http or https, a host, at most 2 KiB
/// (the worker's own limit).
#[must_use]
pub fn is_web_url(text: &str) -> bool {
    let text = text.trim();
    let Some(rest) = text.strip_prefix("http://").or_else(|| text.strip_prefix("https://")) else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    // `[::1]` is a host whose colons are not a port.
    let port_ok = authority.ends_with(']')
        || authority
            .rsplit_once(':')
            .is_none_or(|(_, port)| !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()));
    text.len() <= 2048 && !authority.is_empty() && port_ok && !rest.contains(char::is_whitespace)
}

/// `text` as an address: as it is when it names a scheme, else `http://` in front
/// (`localhost:5173` is a dev server, not a search).
#[must_use]
pub fn web_url(text: &str) -> Option<String> {
    let text = text.trim();
    let url = if text.contains("://") { text.to_owned() } else { format!("http://{text}") };
    is_web_url(&url).then_some(url)
}

/// The scheme and authority of an address, and the rest (path, query, fragment).
fn split_origin(url: &str) -> Option<(&str, &str, &str)> {
    let (scheme, rest) = url.split_once("://")?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    Some((scheme, authority, tail))
}

/// The worker's port an address names, when its host is the worker's loopback.
///
/// Those hosts are `localhost`, `127.0.0.1` and `[::1]`; the port is the one this client has
/// to serve for the page to load. `None` for any other host, which reaches the worker through
/// its proxy: a page's network never proxies the loopback.
#[must_use]
pub fn worker_port(url: &str) -> Option<u16> {
    let (host, port) = host_port(url)?;
    let loopback = host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1";
    loopback.then_some(port)
}

/// The host an address names, an IPv6 one without its brackets, and its port, the scheme's
/// own when it names none.
#[must_use]
pub fn host_port(url: &str) -> Option<(&str, u16)> {
    let (scheme, authority, _) = split_origin(url)?;
    let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
        let (host, after) = v6.split_once(']')?;
        (host, after.strip_prefix(':'))
    } else {
        match authority.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        }
    };
    let port = match port {
        Some(port) => port.parse().ok()?,
        None if scheme.eq_ignore_ascii_case("https") => 443,
        None => 80,
    };
    Some((host, port))
}

/// Send `worker`'s pages, open and to come, through the worker's proxy at this client's
/// `127.0.0.1:port`.
pub fn route(worker: WorkerKey, port: u16) {
    slopty_platform::web::route(worker.value(), port);
}

/// Why a page could not load, when the worker could not reach its `host:port`.
#[must_use]
pub fn refusal_text(why: TunnelRefusal, host: &str, port: u16) -> String {
    match why {
        TunnelRefusal::Unresolved => format!("the worker finds no host named {host}"),
        TunnelRefusal::Refused => format!("nothing listens on port {port} of {host}"),
        TunnelRefusal::Unreachable => format!("the worker can't reach {host}"),
    }
}

/// `url` with its loopback port moved to `local`, where this client serves the worker's.
///
/// The host stays (it is the page's origin, for cookies and redirects), except `[::1]`: the
/// forward listens on IPv4 only, so it becomes `localhost`.
#[must_use]
pub fn local_url(url: &str, local: u16) -> String {
    let Some((scheme, authority, tail)) = split_origin(url) else { return url.to_owned() };
    let host = if authority.starts_with('[') {
        "localhost"
    } else {
        authority.rsplit_once(':').map_or(authority, |(host, _)| host)
    };
    format!("{scheme}://{host}:{local}{tail}")
}

/// `page`, an address the view reports, as the worker would name it: on the origin this
/// client `loaded` from, the item's own origin (`item`) goes back in; elsewhere, unchanged.
#[must_use]
pub fn worker_url(page: &str, loaded: &str, item: &str) -> String {
    let (Some((_, _, tail)), Some((ls, la, _)), Some((is, ia, _))) =
        (split_origin(page), split_origin(loaded), split_origin(item))
    else {
        return page.to_owned();
    };
    let on_loaded = split_origin(page).is_some_and(|(s, a, _)| s == ls && a == la);
    if on_loaded { format!("{is}://{ia}{tail}") } else { page.to_owned() }
}

/// A PNG as GPUI keeps pictures: premultiplied BGRA.
fn texture(png: &[u8]) -> Option<Arc<RenderImage>> {
    let mut rgba = image::load_from_memory(png).ok()?.into_rgba8();
    for px in rgba.pixels_mut() {
        px.0.swap(0, 2);
    }
    Some(Arc::new(RenderImage::new([image::Frame::new(rgba)])))
}

impl Render for BrowserView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(address) = self.local.clone()
            && !self.native.open()
            && self.page.failed.is_none()
        {
            self.open(&address, window, cx);
        }
        // A render cannot see a native view, so it draws the page's picture instead.
        let live = self.live && self.page.failed.is_none() && !crate::screen::capturing(cx);
        let host = if live && self.native.open() && self.compose_in(window, cx) {
            self.native_host().cloned()
        } else {
            None
        };
        // Blank while a page that loads in time would fill it; past the grace, a word.
        let waited = host.is_none()
            && self.page.failed.is_none()
            && self.snapshot.is_none()
            && crate::screen::past_grace("browser-opening", window, cx);
        let theme = &self.theme;
        let s = &theme.surfaces;
        let id = *self.id.as_uuid();
        let notice = |text: String| {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .px(px(theme.spacing.md))
                .text_size(px(theme.typography.small()))
                .font_family(theme.typography.ui_family.clone())
                .text_color(hsla(s.text_muted))
                .child(SharedString::from(text))
        };
        let body = match (host, &self.page.failed, &self.snapshot) {
            (Some(host), ..) => native_view(&host)
                .debug_selector(move || format!("page-{id}"))
                .size_full()
                .track_focus(&self.page_focus)
                .on_key_down(cx.listener(Self::page_key))
                .on_action(cx.listener(|this, _: &PageUndo, _, _| this.native.perform(Edit::Undo)))
                // ⌘Z is "Undo close" in the menu bar, which has it before any binding: while the
                // page holds the keyboard it is the page's undo, as Edit ▸ Undo is a field's.
                .on_action(cx.listener(|this, _: &UndoClose, _, _| this.native.perform(Edit::Undo)))
                .on_action(cx.listener(|this, _: &PageRedo, _, _| this.native.perform(Edit::Redo)))
                .on_action(cx.listener(|this, _: &PageCut, _, _| this.native.perform(Edit::Cut)))
                .on_action(cx.listener(|this, _: &PageSelectAll, _, _| {
                    this.native.perform(Edit::SelectAll);
                }))
                .into_any_element(),
            (None, Some(why), _) => {
                notice(format!("Can't open {}: {why}", short_url(&self.url))).into_any_element()
            }
            (None, None, Some(image)) => img(Arc::clone(image))
                .debug_selector(move || format!("page-picture-{id}"))
                .size_full()
                .object_fit(ObjectFit::Cover)
                .into_any_element(),
            (None, None, None) if waited => {
                notice(format!("Opening {}…", short_url(&self.url))).into_any_element()
            }
            (None, None, None) => div().size_full().into_any_element(),
        };
        // The page's own area, under the find bar and over the downloads; a script's dialog
        // is drawn after the page, so over it.
        let page = div()
            .key_context(CTX)
            .relative()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .child(body)
            .children(self.render_find(cx))
            .children(self.render_dialog(cx));
        div()
            .id(SharedString::from(format!("browser-{id}")))
            .debug_selector(move || format!("browser-{id}"))
            .role(Role::Document)
            .aria_label(SharedString::from(format!("Page {}", self.page.url)))
            .aria_value(SharedString::from(self.title()))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            // The tile's own body surface until the page draws over it.
            .bg(hsla(theme.content()))
            .child(page)
            .children(self.render_downloads(cx))
    }
}

impl BrowserView {
    /// The find bar over the page's top-right corner: drawn after the page, so over it.
    fn render_find(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let search = self.search.as_ref()?;
        let id = *self.id.as_uuid();
        let tally = match_tally(&search.needle, search.found, search.count);
        let this = cx.entity().downgrade();
        let closing = this.clone();
        let bar = FindBar::new("page-find", "Find in page", &search.input, tally, &self.theme)
            .on_step(move |delta, _window, cx| {
                let _gone = this.update(cx, |v, _cx| v.find_step(delta < 0));
            })
            .on_close(move |_window, cx| {
                let _gone = closing.update(cx, Self::close_find);
            });
        let spacing = self.theme.spacing;
        Some(
            div()
                .id("page-find-keys")
                .debug_selector(move || format!("page-find-{id}"))
                .key_context("PageSearch")
                .absolute()
                .top(px(spacing.sm))
                .right(px(spacing.sm))
                .on_action(cx.listener(|this, _: &CloseFind, _window, cx| this.close_find(cx)))
                .on_action(cx.listener(|this, _: &Escape, _window, cx| this.close_find(cx)))
                .on_action(cx.listener(|this, _: &FindNext, _window, _cx| this.find_step(false)))
                .on_action(cx.listener(|this, _: &FindPrev, _window, _cx| this.find_step(true)))
                .child(bar)
                .into_any_element(),
        )
    }

    /// A script's dialog: a sheet over the page, under the scrim, which takes the pointer
    /// from the page.
    fn render_dialog(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let pending = self.dialog.as_ref()?;
        let theme = &self.theme;
        let s = theme.surfaces;
        let id = *self.id.as_uuid();
        let asks = !matches!(pending.dialog.kind(), DialogKind::Alert);
        let who: SharedString = if pending.host.is_empty() {
            "The page says".into()
        } else {
            format!("{} says", pending.host).into()
        };
        let message = SharedString::from(pending.dialog.message().to_owned());
        let sheet = kit::elevate(div(), theme)
            .id("page-dialog")
            .debug_selector(move || format!("page-dialog-{id}"))
            .key_context("PageDialog")
            .track_focus(&pending.focus)
            .role(Role::AlertDialog)
            .aria_label(message.clone())
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                let ok = match event.keystroke.key.as_str() {
                    "enter" => true,
                    "escape" => false,
                    _ => return,
                };
                cx.stop_propagation();
                this.answer_dialog(ok, cx);
            }))
            .on_action(cx.listener(|this, _: &Escape, _window, cx| this.answer_dialog(false, cx)))
            .w_full()
            .min_w_0()
            .max_w(px(DIALOG_WIDTH))
            .flex()
            .flex_col()
            .gap(px(theme.spacing.sm))
            .p(px(theme.spacing.md))
            .rounded(px(theme.radii.lg))
            .text_size(px(theme.typography.ui_size))
            .font_family(theme.typography.ui_family.clone())
            .text_color(hsla(s.text))
            .child(kit::meta(div(), theme).child(who))
            .child(div().max_h(px(DIALOG_WIDTH)).overflow_hidden().child(message))
            .children(pending.input.as_ref().map(|i| Input::new(i).aria_label("Answer")))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(theme.spacing.sm))
                    .when(asks, |el| {
                        el.child(
                            kit::button(theme, "page-dialog-cancel", "Cancel", ButtonKind::Ghost)
                                .on_click(cx.listener(|this, _ev, _window, cx| {
                                    this.answer_dialog(false, cx);
                                })),
                        )
                    })
                    .child(
                        kit::button(theme, "page-dialog-ok", "OK", ButtonKind::Primary).on_click(
                            cx.listener(|this, _ev, _window, cx| this.answer_dialog(true, cx)),
                        ),
                    ),
            );
        let layer = div()
            .occlude()
            .absolute()
            .inset_0()
            .flex()
            .flex_col()
            .items_center()
            .px(px(theme.spacing.md))
            .pt(px(theme.spacing.xl))
            .bg(kit::scrim(theme))
            .child(sheet);
        Some(layer.into_any_element())
    }

    /// A row per download, under the page: what it is, how it goes, and the way to stop or
    /// dismiss it.
    fn render_downloads(&self, cx: &Context<Self>) -> Vec<AnyElement> {
        let theme = &self.theme;
        let s = theme.surfaces;
        self.downloads
            .iter()
            .map(|row| {
                let id = row.id;
                let (icon, tone) = match row.state {
                    DownloadState::Receiving { .. } => (IconName::Download, s.text_muted),
                    DownloadState::Saved => (IconName::Check, s.success),
                    DownloadState::Failed(_) => (IconName::CircleAlert, s.error),
                };
                let receiving = matches!(row.state, DownloadState::Receiving { .. });
                let name = SharedString::from(row.name());
                let status = SharedString::from(row.status());
                let close = if receiving { "Cancel download" } else { "Dismiss" };
                kit::row(theme, kit::Row::One)
                    .id(SharedString::from(format!("page-download-{id}")))
                    .debug_selector(move || format!("page-download-{id}"))
                    .role(Role::Group)
                    .aria_label(name.clone())
                    .aria_value(status.clone())
                    .border_t(kit::hair(theme))
                    .border_color(hsla(s.border))
                    .text_size(px(theme.typography.small()))
                    .font_family(theme.typography.ui_family.clone())
                    .text_color(hsla(s.text))
                    .child(crate::icons::icon(theme, icon, IconSize::Inline, hsla(tone)))
                    .child(div().flex_1().min_w_0().truncate().child(name))
                    .child(kit::meta(kit::tabular(div()), theme).flex_none().child(status))
                    .when(cfg!(target_os = "macos") && row.state == DownloadState::Saved, |el| {
                        let path = row.path.clone();
                        el.child(
                            kit::button(
                                theme,
                                "page-download-show",
                                "Show in Finder",
                                ButtonKind::Link,
                            )
                            .on_click(move |_ev, _window, _cx| native::reveal(&path)),
                        )
                    })
                    .child(
                        kit::icon_button(theme, "page-download-close", IconName::X, close)
                            .on_click(cx.listener(move |this, _ev, _window, cx| {
                                this.dismiss_download(id, cx);
                            })),
                    )
                    .into_any_element()
            })
            .collect()
    }
}

/// The platform's web view (`WKWebView` on macOS and iOS) behind the tile's API.
mod native {
    use std::rc::Rc;

    use gpui::Window;
    use gpui::composition::NativeHost;
    use slopty_client::layout::WorkerKey;
    pub use slopty_platform::web::{Dialog, DialogKind, Edit};
    pub(super) use slopty_platform::web::{Download, WebEvent as Event};

    use super::PageState;

    /// The page, once it is open.
    #[derive(Default)]
    pub(super) struct Native {
        view: Option<slopty_platform::web::WebView>,
        /// A test's page: open, with no web view behind it.
        stand_in: bool,
        /// The edits a test's page was asked to do.
        #[cfg(test)]
        performed: std::cell::RefCell<Vec<Edit>>,
    }

    impl Native {
        pub(super) const fn open(&self) -> bool {
            self.view.is_some() || self.stand_in
        }

        /// A test's page, open with no web view behind it.
        #[cfg(test)]
        pub(super) fn stand_in() -> Self {
            Self { stand_in: true, ..Self::default() }
        }

        #[cfg(test)]
        pub(super) fn performed(&self) -> Vec<Edit> {
            self.performed.borrow().clone()
        }

        /// Do `edit` in the page.
        #[cfg(target_os = "macos")]
        pub(super) fn perform(&self, edit: Edit) {
            #[cfg(test)]
            self.performed.borrow_mut().push(edit);
            if let Some(view) = &self.view {
                view.perform(edit);
            }
        }

        /// UIKit hands a hardware keyboard's keys to the page while it holds them, which does
        /// its own edits; GPUI's keymap never sees them there.
        #[cfg(target_os = "ios")]
        #[expect(
            clippy::unused_self,
            reason = "one call for both platforms; the Mac's reads its web view"
        )]
        pub(super) const fn perform(&self, _edit: Edit) {}

        #[cfg(target_os = "macos")]
        pub(super) fn window_number(&self) -> Option<isize> {
            self.view.as_ref().and_then(slopty_platform::web::WebView::window_number)
        }

        /// Open `worker`'s page for `window`; `false` when the platform has no web view to
        /// give.
        pub(super) fn create(
            &mut self,
            window: &Window,
            worker: WorkerKey,
            url: &str,
            sink: Rc<dyn Fn(Event)>,
        ) -> bool {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            let worker = worker.value();
            self.view = match HasWindowHandle::window_handle(window).map(|h| h.as_raw()) {
                #[cfg(target_os = "macos")]
                Ok(RawWindowHandle::AppKit(handle)) => {
                    slopty_platform::web::WebView::new(handle.ns_view, worker, url, sink)
                }
                #[cfg(target_os = "ios")]
                Ok(RawWindowHandle::UiKit(_)) => {
                    slopty_platform::web::WebView::new(worker, url, sink)
                }
                _ => None,
            };
            self.view.is_some()
        }

        /// Make the web view `host`'s content. Whether it is: a test's stand-in has none to
        /// give and needs none.
        pub(super) fn attach(&self, host: &NativeHost) -> bool {
            let Some(view) = &self.view else { return self.open() };
            // SAFETY: gpui's `attach_view` rule: `view` is the live `WKWebView` (an `NSView *`
            // on macOS, a `UIView *` on iOS) this struct keeps, and a GPUI window's render,
            // where this is called, runs on the main thread.
            match unsafe { host.attach_view(view.view()) } {
                Ok(()) => true,
                Err(e) => {
                    tracing::warn!("the page's host refused its view: {e:#}");
                    false
                }
            }
        }

        pub(super) fn shown(&self) -> bool {
            self.view.as_ref().is_some_and(slopty_platform::web::WebView::shown)
        }

        pub(super) fn focused(&self) -> bool {
            self.view.as_ref().is_some_and(slopty_platform::web::WebView::focused)
        }

        pub(super) fn page(&self) -> Option<PageState> {
            self.view.as_ref().map(|v| {
                let p = v.page();
                PageState {
                    title: p.title,
                    url: p.url,
                    loading: p.loading,
                    can_go_back: p.can_go_back,
                    can_go_forward: p.can_go_forward,
                    failed: None,
                }
            })
        }

        pub(super) fn snapshot(&self) {
            if let Some(view) = &self.view {
                view.snapshot();
            }
        }

        pub(super) fn load(&self, url: &str) {
            if let Some(view) = &self.view {
                view.load(url);
            }
        }

        pub(super) fn back(&self) {
            if let Some(view) = &self.view {
                view.back();
            }
        }

        pub(super) fn forward(&self) {
            if let Some(view) = &self.view {
                view.forward();
            }
        }

        pub(super) fn reload(&self) {
            if let Some(view) = &self.view {
                view.reload();
            }
        }

        pub(super) fn find(&self, text: &str, backwards: bool) {
            if let Some(view) = &self.view {
                view.find(text, backwards);
            }
        }

        pub(super) fn count(&self, text: &str) {
            if let Some(view) = &self.view {
                view.count(text);
            }
        }

        pub(super) fn inspect(&self) -> bool {
            self.view.as_ref().is_some_and(slopty_platform::web::WebView::inspect)
        }

        pub(super) fn set_zoom(&self, zoom: f64) {
            if let Some(view) = &self.view {
                view.set_zoom(zoom);
            }
        }

        pub(super) fn received(&self, id: u64) -> Option<(u64, u64)> {
            self.view.as_ref()?.received(id)
        }

        pub(super) fn cancel_download(&self, id: u64) {
            if let Some(view) = &self.view {
                view.cancel_download(id);
            }
        }
    }

    /// Show a saved download in the Finder.
    #[cfg(target_os = "macos")]
    pub(super) fn reveal(path: &std::path::Path) {
        slopty_platform::web::reveal(path);
    }

    /// The Files app is the way to a download on iOS, and no row offers this.
    #[cfg(target_os = "ios")]
    pub(super) const fn reveal(_path: &std::path::Path) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_are_http_or_https_with_a_host() {
        assert!(is_web_url("http://localhost:5173/"));
        assert!(is_web_url("https://example.test/a?b=c"));
        assert!(!is_web_url("file:///etc/passwd"));
        assert!(!is_web_url("javascript:alert(1)"));
        assert!(!is_web_url("http://"));
        assert!(!is_web_url("http://localhost:"));
        assert!(is_web_url("http://[::1]"));
        assert!(is_web_url("http://[::1]:5173/"));
        assert!(!is_web_url(&format!("http://h/{}", "a".repeat(2048))));
        assert_eq!(web_url("localhost:5173").as_deref(), Some("http://localhost:5173"));
        assert_eq!(web_url("https://a.test").as_deref(), Some("https://a.test"));
        assert_eq!(web_url("ftp://a.test"), None);
        assert_eq!(short_url("http://localhost:5173/"), "localhost:5173");
        assert_eq!(short_url("http://localhost:5173/app/"), "localhost:5173/app/");
        assert_eq!(
            address_parts("http://127.0.0.1:5173/"),
            Some(("http://", "127.0.0.1:5173", ""))
        );
        assert_eq!(
            address_parts("https://a.test/docs?q=1#top"),
            Some(("https://", "a.test", "/docs?q=1#top"))
        );
        assert_eq!(address_parts("a.test/docs"), None);
    }

    #[test]
    fn a_loopback_address_is_the_worker_s_and_moves_to_the_local_port() {
        assert_eq!(worker_port("http://localhost:5173/"), Some(5173));
        assert_eq!(worker_port("http://LOCALHOST:5173"), Some(5173));
        assert_eq!(worker_port("http://127.0.0.1:8080/a?b#c"), Some(8080));
        assert_eq!(worker_port("http://[::1]:3000/"), Some(3000));
        assert_eq!(worker_port("http://localhost/"), Some(80));
        assert_eq!(worker_port("https://localhost/"), Some(443));
        assert_eq!(worker_port("https://example.test:8443/"), None, "not the worker's");
        assert_eq!(worker_port("http://localhost.example.test/"), None);

        assert_eq!(
            local_url("http://localhost:5173/app?x=1", 5174),
            "http://localhost:5174/app?x=1"
        );
        assert_eq!(local_url("http://127.0.0.1:8080", 8081), "http://127.0.0.1:8081");
        assert_eq!(local_url("http://[::1]:3000/", 3001), "http://localhost:3001/");
        assert_eq!(local_url("http://localhost/", 8000), "http://localhost:8000/");

        let (loaded, item) = ("http://localhost:5174/", "http://localhost:5173/");
        assert_eq!(
            worker_url("http://localhost:5174/docs#a", loaded, item),
            "http://localhost:5173/docs#a"
        );
        assert_eq!(worker_url("https://example.test/", loaded, item), "https://example.test/");
    }

    #[test]
    fn zoom_walks_safari_s_steps_and_stops_at_the_ends() {
        assert!((next_zoom(1.0, Zoom::In) - 1.1).abs() < f64::EPSILON);
        assert!((next_zoom(1.0, Zoom::Out) - 0.9).abs() < f64::EPSILON);
        assert!((next_zoom(3.0, Zoom::In) - 3.0).abs() < f64::EPSILON, "the top stays");
        assert!((next_zoom(0.5, Zoom::Out) - 0.5).abs() < f64::EPSILON, "the bottom stays");
        assert!((next_zoom(1.2, Zoom::In) - 1.25).abs() < f64::EPSILON, "off a step, the next");
        assert!((next_zoom(2.5, Zoom::Reset) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn the_find_bar_counts_what_the_page_counted() {
        let said = |needle, found, count| match_tally(needle, found, count).words();
        assert_eq!(said("", Some(false), Some(0)), "", "nothing typed");
        assert_eq!(said("x", None, None), "", "no answer yet");
        assert_eq!(said("x", Some(false), Some(3)), "No matches", "find is the word");
        assert_eq!(said("x", None, Some(0)), "No matches");
        assert_eq!(said("x", Some(true), Some(1)), "1 match");
        assert_eq!(said("x", None, Some(12)), "12 matches");
        assert_eq!(said("x", Some(true), Some(0)), "", "found in a frame it cannot count");
    }

    #[test]
    fn a_download_row_says_how_far_it_has_come() {
        let row = |state| DownloadRow { id: 1, path: "/d/a.zip".into(), state };
        let receiving = |done, total| row(DownloadState::Receiving { done, total });
        assert_eq!(receiving(512, 0).status(), "512 B", "no size given: what has come");
        assert_eq!(receiving(1_048_576, 4_194_304).status(), "25% of 4.0 MB");
        assert_eq!(row(DownloadState::Saved).status(), "Saved");
        assert_eq!(row(DownloadState::Failed("No space".into())).status(), "No space");
        assert_eq!(row(DownloadState::Saved).name(), "a.zip");
    }

    #[test]
    fn an_address_typed_into_the_palette_opens_in_a_tile() {
        let items = crate::palette::path_items("http://localhost:5173/");
        let runs: Vec<String> = items.iter().map(|i| format!("{:?}", i.run)).collect();
        assert_eq!(runs, [r#"OpenInTile("http://localhost:5173/")"#]);
        assert_eq!(items.first().map(|i| i.label.as_str()), Some("Open localhost:5173 in a tile"));
        assert!(crate::palette::path_items("http://localhost:").is_empty(), "no host yet");
        assert!(crate::palette::path_items("javascript://x").is_empty());
    }
}

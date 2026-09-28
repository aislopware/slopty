//! A browser tile: a web page, usually a server on the worker reached through a forwarded
//! port, in a native web view laid over the tile.
//!
//! The item's address is the worker's (`http://localhost:5173/` means port 5173 on the
//! worker), so every client of the worker opens the same page. Each client serves that port
//! on its own loopback, at whatever local port it could get, and rewrites the address to it
//! before the page loads ([`local_url`]).
//!
//! GPUI cannot draw a page, so the page is the platform's web view (`slopty_platform::web`),
//! a native view that always draws above everything GPUI draws. The workspace measures the
//! tile's body every frame and asks [`placement`] where the page goes: over the body, cut to
//! the strip, or nowhere when GPUI has something to show on top (the palette, a menu, the
//! overview) or the tile is off the strip. While the page is hidden the body shows its last
//! snapshot, so covering it never leaves a hole.
//!
//! What a browser brings of its own sits in the tile beside the page, never over it, since
//! nothing GPUI draws can cover a native view: the find bar above the page, a download's row
//! below it, and a script's dialog on the page's snapshot while the page waits for the answer.
//! A pop-up opens a tile of its own (`docs/decisions/ui.md`, "The browser tile's own chrome").

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, Bounds, Context, Entity, EventEmitter, FocusHandle,
    InteractiveElement as _, IntoElement, KeyDownEvent, ObjectFit, ParentElement as _, Pixels,
    Render, RenderImage, SharedString, StatefulInteractiveElement as _, Styled as _,
    StyledImage as _, Subscription, Task, Window, canvas, div, img, px,
};
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState};
pub use native::{Dialog, DialogKind};
use slopty_client::layout::{Rect, WorkerKey};
use slopty_core::ItemId;
use slopty_theme::Theme;

use crate::colors::hsla;
use crate::icons::{IconName, IconSize};
use crate::kit::{self, ButtonKind, FIND_PLACEHOLDER};
use crate::terminal::{CloseFind, FindNext, FindPrev};

/// Below this the tile is fading out (or in) and the page stays hidden; a native view would
/// not fade with it.
pub const MIN_ALPHA: f32 = 0.05;

/// How often a shown page is asked for its title and address, which scripts change without
/// a navigation.
const POLL: Duration = Duration::from_secs(1);

/// How often a download under way is asked how far it has come.
const DOWNLOAD_POLL: Duration = Duration::from_millis(250);

/// The find field's width at most, as a file tile's.
const FIND_WIDTH: f32 = 240.0;

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

/// What GPUI is drawing over the strip this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Cover {
    /// An overlay: the palette, the picker, a titlebar menu, a dialog of the app's.
    pub overlay: bool,
    /// The overview, open or on its way.
    pub overview: bool,
    /// Where a toast was drawn, at the foot of the strip: pages end above it.
    pub toast: Option<Rect>,
}

/// Where a page goes this frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Placement {
    /// Out of sight: covered, off the strip, fading, or not drawn at all.
    Hidden,
    /// Over `frame`, cut to `clip`, at `alpha`; window coordinates in points.
    Shown {
        /// The strip's area: nothing of the page shows outside it.
        clip: Rect,
        /// The tile's body.
        frame: Rect,
        /// The tile's opacity.
        alpha: f32,
    },
}

/// Where the page of a tile goes: `body` is where its tile's body was drawn this frame (none
/// when the tile was not drawn), `strip` the strip's area, `alpha` the tile's opacity.
#[must_use]
pub fn placement(body: Option<Rect>, strip: Rect, alpha: f32, cover: Cover) -> Placement {
    let Some(frame) = body else { return Placement::Hidden };
    // A native page draws over everything, so the clip stops where a toast starts.
    let clip = match cover.toast {
        Some(toast) if toast.y < strip.y + strip.h => {
            Rect { h: (toast.y - strip.y).max(0.0), ..strip }
        }
        _ => strip,
    };
    let visible = frame.w >= 1.0 && frame.h >= 1.0 && clip.h >= 1.0 && frame.intersects(&clip);
    if cover.overlay || cover.overview || alpha < MIN_ALPHA || !visible {
        return Placement::Hidden;
    }
    Placement::Shown { clip, frame, alpha: alpha.min(1.0) }
}

/// Where a tile's body was drawn, and in which of the workspace's frames.
pub(crate) type Drawn = Rc<Cell<Option<(u64, Bounds<Pixels>)>>>;

/// What a browser tile tells the workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BrowserEvent {
    /// The page was clicked and has the keyboard: its tile takes the focus.
    Focused,
    /// The keyboard is the workspace's again: the page gave it back (a click elsewhere,
    /// ⌃Tab, Esc twice), or the find bar or a dialog that had it closed.
    Released,
    /// A pop-up or a `_blank` link asked for this address, as the worker names it: a tile of
    /// its own beside this one.
    Open(String),
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
            DownloadState::Receiving { done, total: 0 } => crate::file::size_label(*done),
            DownloadState::Receiving { done, total } => {
                let percent = done.saturating_mul(100).checked_div(*total).unwrap_or(0).min(100);
                format!("{percent}% of {}", crate::file::size_label(*total))
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
    /// Where the body was drawn, and in which frame of the workspace's.
    drawn: Drawn,
    /// The tile's opacity this frame.
    alpha: f32,
    /// The page's last picture, shown while the page is hidden.
    snapshot: Option<Arc<RenderImage>>,
    theme: Theme,
    native: native::Native,
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
    pub fn new(id: ItemId, worker: WorkerKey, url: &str, theme: Theme) -> Self {
        Self {
            id,
            worker,
            url: url.to_owned(),
            local: None,
            loaded: None,
            page: PageState { url: url.to_owned(), loading: true, ..PageState::default() },
            drawn: Rc::default(),
            alpha: 1.0,
            snapshot: None,
            theme,
            native: native::Native::default(),
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
        self.native.hide();
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

    /// Whether the page is on screen now.
    #[must_use]
    pub fn shown(&self) -> bool {
        self.native.shown()
    }

    /// Whether the page has the keyboard.
    #[must_use]
    pub fn focused(&self) -> bool {
        self.native.focused()
    }

    /// Whether a picture of the page is ready for when it is hidden.
    #[must_use]
    pub const fn has_snapshot(&self) -> bool {
        self.snapshot.is_some()
    }

    /// Where the body was drawn in `frame` of the workspace's, if it was.
    #[must_use]
    pub fn drawn_in(&self, frame: u64) -> Option<Bounds<Pixels>> {
        self.drawn.get().filter(|(f, _)| *f == frame).map(|(_, b)| b)
    }

    /// The tile's opacity this frame.
    pub const fn set_alpha(&mut self, alpha: f32) {
        self.alpha = alpha;
    }

    /// The tile's opacity as last set.
    #[must_use]
    pub const fn alpha(&self) -> f32 {
        self.alpha
    }

    /// Draw by another theme.
    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        if self.theme != theme {
            self.theme = theme;
            cx.notify();
        }
    }

    /// Put the page where [`placement`] says, opening it the first time it is shown. A
    /// failed page stays hidden, so its reason shows instead of a blank page.
    pub fn apply(&mut self, placement: Placement, window: &Window, cx: &mut Context<Self>) {
        match placement {
            // A dialog is drawn on the page's picture: the page waits under it.
            Placement::Shown { clip, frame, alpha }
                if self.page.failed.is_none() && self.dialog.is_none() =>
            {
                if !self.native.open() {
                    let Some(address) = self.local.clone() else { return };
                    self.open(&address, window, cx);
                }
                self.native.place(clip, frame, alpha);
            }
            Placement::Shown { .. } | Placement::Hidden => {
                if self.native.shown() {
                    // The picture it leaves behind is the page as it was a moment ago.
                    self.native.snapshot();
                    self.native.hide();
                }
            }
        }
    }

    fn open(&mut self, address: &str, window: &Window, cx: &mut Context<Self>) {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<native::Event>();
        let sink: Rc<dyn Fn(native::Event)> = Rc::new(move |event| {
            let _sent = tx.send(event);
        });
        if !self.native.create(window, address, sink) {
            self.page.failed = Some("This device has no web view".to_owned());
            cx.notify();
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
                self.native.hide();
                cx.notify();
            }
            native::Event::Clicked => cx.emit(BrowserEvent::Focused),
            native::Event::Released => {
                self.native.snapshot();
                cx.emit(BrowserEvent::Released);
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
        if self.native.focused() {
            self.native.release();
        }
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

    /// What the find bar says of its matches: `3 matches`, `No matches`, or nothing yet.
    #[must_use]
    pub fn matches(&self) -> String {
        self.search.as_ref().map(|s| match_status(&s.needle, s.found, s.count)).unwrap_or_default()
    }

    /// Web Inspector on the page, in its own window. Whether it opened: a page that is open,
    /// in a debug build of the Mac app.
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
        if self.native.focused() {
            self.native.release();
        }
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

    /// Give the page the keyboard (a click on the tile's body when the page is hidden does
    /// nothing; the page takes its own clicks).
    pub fn focus(&self) {
        self.native.focus();
    }

    /// Take the keyboard back from the page.
    pub fn release(&self) {
        self.native.release();
    }

    /// Give the page the keyboard and show the platform's key handling `key` (`cmd-a`,
    /// `cmd-shift-z`), as a key down would: the self-test's way in. Whether it was taken for
    /// the page.
    #[must_use]
    pub fn press(&self, key: &str) -> bool {
        self.native.focus();
        let mut command = false;
        let mut shift = false;
        let mut last = "";
        for part in key.split('-') {
            match part {
                "cmd" => command = true,
                "shift" => shift = true,
                other => last = other,
            }
        }
        self.native.press(last, command, shift)
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

/// What a page's find bar says of `needle`'s matches, from the page's find (`found`) and its
/// count: `3 matches`, `1 match`, `No matches`, or nothing while neither has answered.
#[must_use]
pub fn match_status(needle: &str, found: Option<bool>, count: Option<usize>) -> String {
    if needle.is_empty() {
        return String::new();
    }
    match (found, count) {
        (Some(false), _) | (None, Some(0)) => "No matches".to_owned(),
        (_, Some(1)) => "1 match".to_owned(),
        (_, Some(n)) if n > 0 => format!("{n} matches"),
        // Found where the count does not reach (a frame of the page's), or not answered yet.
        _ => String::new(),
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
/// to serve for the page to load. `None` for any other host, which every client reaches as it
/// is.
#[must_use]
pub fn worker_port(url: &str) -> Option<u16> {
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
    let loopback = host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1";
    if !loopback {
        return None;
    }
    match port {
        Some(port) => port.parse().ok(),
        None if scheme.eq_ignore_ascii_case("https") => Some(443),
        None => Some(80),
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
        // Blank while a page that loads in time would fill it; past the grace, a word.
        let waited = self.page.failed.is_none()
            && self.snapshot.is_none()
            && crate::screen::past_grace("browser-opening", window, cx);
        let theme = &self.theme;
        let s = &theme.surfaces;
        let id = *self.id.as_uuid();
        let drawn = Rc::clone(&self.drawn);
        let frame = cx.try_global::<FrameCount>().map_or(0, |f| f.0);
        let measure = canvas(
            move |bounds, _window, _cx| drawn.set(Some((frame, bounds))),
            |_bounds, (), _window, _cx| {},
        )
        .absolute()
        .inset_0();
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
        let body = match (&self.page.failed, &self.snapshot) {
            (Some(why), _) => {
                notice(format!("Can't open {}: {why}", short_url(&self.url))).into_any_element()
            }
            (None, Some(image)) => {
                img(Arc::clone(image)).size_full().object_fit(ObjectFit::Cover).into_any_element()
            }
            (None, None) if waited => {
                notice(format!("Opening {}…", short_url(&self.url))).into_any_element()
            }
            (None, None) => div().size_full().into_any_element(),
        };
        // The page's own area: what is measured for the native view, under the find bar and
        // over the downloads, which it must not cover.
        let page = div()
            .relative()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .child(body)
            .child(measure)
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
            .children(self.render_find(cx))
            .child(page)
            .children(self.render_downloads(cx))
    }
}

impl BrowserView {
    /// The find bar, across the top of the tile's body.
    fn render_find(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let search = self.search.as_ref()?;
        let theme = &self.theme;
        let s = theme.surfaces;
        let id = *self.id.as_uuid();
        let status = SharedString::from(match_status(&search.needle, search.found, search.count));
        let bar = kit::inset_x(div(), theme)
            .id("page-find")
            .debug_selector(move || format!("page-find-{id}"))
            .key_context("PageSearch")
            .role(Role::Group)
            .aria_label("Find in page")
            .flex_none()
            .flex()
            .items_center()
            .gap(px(theme.spacing.sm))
            .py(px(theme.spacing.xs))
            .border_b_1()
            .border_color(hsla(s.border))
            .text_size(px(theme.typography.small()))
            .font_family(theme.typography.ui_family.clone())
            .text_color(hsla(s.text))
            .on_action(cx.listener(|this, _: &CloseFind, _window, cx| this.close_find(cx)))
            .on_action(cx.listener(|this, _: &Escape, _window, cx| this.close_find(cx)))
            .on_action(cx.listener(|this, _: &FindNext, _window, _cx| this.find_step(false)))
            .on_action(cx.listener(|this, _: &FindPrev, _window, _cx| this.find_step(true)))
            .child(crate::icons::icon(
                theme,
                IconName::Search,
                IconSize::Inline,
                hsla(s.text_muted),
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .max_w(px(FIND_WIDTH))
                    .child(Input::new(&search.input).aria_label("Find in page")),
            )
            .child(
                div()
                    .id("page-find-status")
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(hsla(s.text_secondary))
                    .role(Role::Label)
                    .aria_label("Matches")
                    .aria_value(status.clone())
                    .child(status),
            )
            .child(
                kit::icon_button(theme, "page-find-prev", IconName::ChevronUp, "Previous match")
                    .on_click(cx.listener(|this, _ev, _window, _cx| this.find_step(true))),
            )
            .child(
                kit::icon_button(theme, "page-find-next", IconName::ChevronDown, "Next match")
                    .on_click(cx.listener(|this, _ev, _window, _cx| this.find_step(false))),
            )
            .child(
                kit::icon_button(theme, "page-find-close", IconName::X, "Close find")
                    .on_click(cx.listener(|this, _ev, _window, cx| this.close_find(cx))),
            );
        Some(bar.into_any_element())
    }

    /// A script's dialog: a sheet on the page's picture, under the scrim.
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
                    .border_t_1()
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

/// The workspace's frame number, bumped every time it draws: a body measured in an older
/// frame was not drawn in this one.
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameCount(pub u64);

impl gpui::Global for FrameCount {}

/// The platform's web view (`WKWebView` on macOS and iOS) behind the tile's API.
mod native {
    use std::rc::Rc;

    use gpui::Window;
    use slopty_client::layout::Rect;
    pub use slopty_platform::web::{Dialog, DialogKind, Download, WebEvent as Event};

    use super::PageState;

    /// The page, once it is open.
    #[derive(Default)]
    pub struct Native {
        view: Option<slopty_platform::web::WebView>,
    }

    fn frame(r: Rect) -> slopty_platform::web::Frame {
        slopty_platform::web::Frame {
            x: f64::from(r.x),
            y: f64::from(r.y),
            w: f64::from(r.w),
            h: f64::from(r.h),
        }
    }

    impl Native {
        pub const fn open(&self) -> bool {
            self.view.is_some()
        }

        /// Open the page in `window`'s view; `false` when the platform has none to give.
        pub fn create(&mut self, window: &Window, url: &str, sink: Rc<dyn Fn(Event)>) -> bool {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            let host = match HasWindowHandle::window_handle(window).map(|h| h.as_raw()) {
                Ok(RawWindowHandle::AppKit(handle)) => handle.ns_view,
                Ok(RawWindowHandle::UiKit(handle)) => handle.ui_view,
                _ => return false,
            };
            self.view = slopty_platform::web::WebView::new(host, url, sink);
            self.view.is_some()
        }

        pub fn place(&self, clip: Rect, at: Rect, alpha: f32) {
            if let Some(view) = &self.view {
                view.place(frame(clip), frame(at), f64::from(alpha));
            }
        }

        pub fn hide(&self) {
            if let Some(view) = &self.view {
                view.hide();
            }
        }

        pub fn shown(&self) -> bool {
            self.view.as_ref().is_some_and(slopty_platform::web::WebView::shown)
        }

        pub fn focused(&self) -> bool {
            self.view.as_ref().is_some_and(slopty_platform::web::WebView::focused)
        }

        pub fn page(&self) -> Option<PageState> {
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

        pub fn snapshot(&self) {
            if let Some(view) = &self.view {
                view.snapshot();
            }
        }

        pub fn load(&self, url: &str) {
            if let Some(view) = &self.view {
                view.load(url);
            }
        }

        pub fn back(&self) {
            if let Some(view) = &self.view {
                view.back();
            }
        }

        pub fn forward(&self) {
            if let Some(view) = &self.view {
                view.forward();
            }
        }

        pub fn reload(&self) {
            if let Some(view) = &self.view {
                view.reload();
            }
        }

        pub fn focus(&self) {
            if let Some(view) = &self.view {
                view.focus();
            }
        }

        pub fn release(&self) {
            if let Some(view) = &self.view {
                view.release();
            }
        }

        pub fn press(&self, key: &str, command: bool, shift: bool) -> bool {
            self.view.as_ref().is_some_and(|v| v.press(key, command, shift))
        }

        pub fn find(&self, text: &str, backwards: bool) {
            if let Some(view) = &self.view {
                view.find(text, backwards);
            }
        }

        pub fn count(&self, text: &str) {
            if let Some(view) = &self.view {
                view.count(text);
            }
        }

        pub fn inspect(&self) -> bool {
            self.view.as_ref().is_some_and(slopty_platform::web::WebView::inspect)
        }

        pub fn set_zoom(&self, zoom: f64) {
            if let Some(view) = &self.view {
                view.set_zoom(zoom);
            }
        }

        pub fn received(&self, id: u64) -> Option<(u64, u64)> {
            self.view.as_ref()?.received(id)
        }

        pub fn cancel_download(&self, id: u64) {
            if let Some(view) = &self.view {
                view.cancel_download(id);
            }
        }
    }

    /// Show a saved download in the Finder.
    #[cfg(target_os = "macos")]
    pub fn reveal(path: &std::path::Path) {
        slopty_platform::web::reveal(path);
    }

    /// The Files app is the way to a download on iOS, and no row offers this.
    #[cfg(target_os = "ios")]
    pub const fn reveal(_path: &std::path::Path) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    const STRIP: Rect = Rect { x: 0.0, y: 40.0, w: 1200.0, h: 760.0 };
    const BODY: Rect = Rect { x: 8.0, y: 76.0, w: 590.0, h: 700.0 };

    #[test]
    fn a_drawn_uncovered_tile_shows_its_page_cut_to_the_strip() {
        assert_eq!(
            placement(Some(BODY), STRIP, 1.0, Cover::default()),
            Placement::Shown { clip: STRIP, frame: BODY, alpha: 1.0 }
        );
        // Half off the strip's left edge: still shown, the clip cuts it.
        let half = Rect { x: -300.0, ..BODY };
        assert_eq!(
            placement(Some(half), STRIP, 1.0, Cover::default()),
            Placement::Shown { clip: STRIP, frame: half, alpha: 1.0 }
        );
    }

    #[test]
    fn anything_gpui_draws_on_top_hides_the_page() {
        let covered = |cover| placement(Some(BODY), STRIP, 1.0, cover);
        assert_eq!(covered(Cover { overlay: true, ..Cover::default() }), Placement::Hidden);
        assert_eq!(covered(Cover { overview: true, ..Cover::default() }), Placement::Hidden);
    }

    #[test]
    fn a_toast_cuts_the_page_short_above_it() {
        let toast = Rect { x: 400.0, y: 740.0, w: 400.0, h: 36.0 };
        let cover = Cover { toast: Some(toast), ..Cover::default() };
        let above = Rect { h: 700.0, ..STRIP };
        assert_eq!(
            placement(Some(BODY), STRIP, 1.0, cover),
            Placement::Shown { clip: above, frame: BODY, alpha: 1.0 },
            "the page ends where the toast begins"
        );
        let low = Rect { y: 745.0, h: 40.0, ..BODY };
        assert_eq!(placement(Some(low), STRIP, 1.0, cover), Placement::Hidden, "all under it");
        let gone = Rect { y: 900.0, ..toast };
        let cover = Cover { toast: Some(gone), ..Cover::default() };
        assert_eq!(
            placement(Some(BODY), STRIP, 1.0, cover),
            Placement::Shown { clip: STRIP, frame: BODY, alpha: 1.0 },
            "a toast below the strip cuts nothing"
        );
    }

    #[test]
    fn a_tile_off_the_strip_fading_or_not_drawn_hides_the_page() {
        assert_eq!(placement(None, STRIP, 1.0, Cover::default()), Placement::Hidden);
        let gone = Rect { x: 1300.0, ..BODY };
        assert_eq!(placement(Some(gone), STRIP, 1.0, Cover::default()), Placement::Hidden);
        assert_eq!(placement(Some(BODY), STRIP, 0.01, Cover::default()), Placement::Hidden);
        let flat = Rect { h: 0.5, ..BODY };
        assert_eq!(placement(Some(flat), STRIP, 1.0, Cover::default()), Placement::Hidden);
        // Fading in past the threshold: shown, at the tile's opacity.
        assert_eq!(
            placement(Some(BODY), STRIP, 0.5, Cover::default()),
            Placement::Shown { clip: STRIP, frame: BODY, alpha: 0.5 }
        );
    }

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
        assert_eq!(match_status("", Some(false), Some(0)), "", "nothing typed");
        assert_eq!(match_status("x", None, None), "", "no answer yet");
        assert_eq!(match_status("x", Some(false), Some(3)), "No matches", "find is the word");
        assert_eq!(match_status("x", None, Some(0)), "No matches");
        assert_eq!(match_status("x", Some(true), Some(1)), "1 match");
        assert_eq!(match_status("x", None, Some(12)), "12 matches");
        assert_eq!(match_status("x", Some(true), Some(0)), "", "found in a frame it cannot count");
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

//! slopty: pages an agent shows inline (an MCP App's view, a `show_page`, an `.html` file), hosted
//! in each window's [`PageHost`] for the shell's page card (`slopty_shell::live::serve_pages`),
//! and what they ask answered through the person.
//!
//! A page's document is read through its thread: a `show_page`'s HTML is the call's own input,
//! a file's is read on the worker beside the page's folder ([`ContentRef::item_asset`]), and an
//! MCP App's UI resource is read by the agent through its own door ([`ThreadRequest::App`]); its
//! other files come the same way. A page never answers for the person: what it would say goes to
//! the chat's draft, context it offers goes there too, visibly, and a tool call it asks to make
//! waits for the person's click in the page and their Allow on the card
//! ([`calls::ANSWER_PAGE`]). The network its app declares is reached only once the person grants
//! it ([`calls::GRANT_PAGE_NETWORK`]) (`docs/decisions/ui.md`, "A page never answers for the
//! person").
//!
//! A chart fence of a reply is a page too, built here from its JSON and the chart libraries the
//! app vendors ([`libs`]), which every page may load from its own origin. A PDF or a CSV/TSV
//! file the agent shows is drawn by the app itself rather than in a web view: a PDF's pages by
//! `PDFKit`, each as the card or the pane asks, a table parsed here ([`PageDoc`]). A file page
//! follows its file on its worker (`ClientMsg::WatchFiles`): a burst of writes reloads it once,
//! [`QUIET`] after the last, or [`HOLD`] after the first at the latest.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt as _;
use gpui::{App, Context, Subscription, Task, Window, WindowId};
use slopty_client::WorkerKey;
use slopty_platform::web::page::{
    Ask, Declined, Device, HostConfig, Network, Notice, PageEvent, PageHost, PageId, PageSpec,
    Presence, Request, Resource, Responder, Snapshot, Theme, libs,
};
use slopty_proto::ClientMsg;
use slopty_proto::file::FileRead;
use slopty_proto::thread::wire::{Expanded, ThreadRequest};
use slopty_proto::thread::{ContentRef, ThreadId, ToolDetail, ToolState};
use slopty_proto::visual::{AppAnswer, AppAsk, AppBody, McpApp, PageSource, is_ui_uri};
use slopty_shell::live::{HostedPage, PageAct, PagePresence};
use slopty_shell::page::{PageDoc, PageTable, PdfDoc, fence};
use slopty_view_doc::{PageConfirm, PageNetwork, PageState, calls};
use slopty_view_rpc::{RpcError, RpcReply};
use tokio::sync::watch;

use super::param;
use crate::view_model::facts::page_of;
use crate::workspace::WorkspaceView;

/// What a window's pages told the app, queued from inside the web views' callbacks.
enum Heard {
    /// A page's event.
    Event(WindowId, PageId, PageEvent),
    /// A page's request for one of its files.
    File(WindowId, PageId, String, Responder),
    /// A watched file changed on its worker, with what it holds now when that came along.
    Changed(WorkerKey, String, Option<Fresh>),
}

/// What a watched file holds now.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Fresh {
    Text(String),
    Bytes(Vec<u8>),
}

/// How long a file page waits after its file's last change before it reloads.
const QUIET: Duration = Duration::from_millis(100);
/// The longest a file page waits after its file's first change: a file written on and on still
/// reloads this often.
const HOLD: Duration = Duration::from_millis(400);

/// The watched files that changed and wait to reload: a burst of changes is one reload.
#[derive(Debug, Default)]
struct Refresh {
    /// What changed, with what it holds now when known.
    pending: HashMap<(WorkerKey, String), Option<Fresh>>,
    /// When the first and the last change of the burst came.
    first: Option<Instant>,
    last: Option<Instant>,
}

impl Refresh {
    /// File `path` on `worker` changed at `now`.
    fn heard(&mut self, worker: WorkerKey, path: String, fresh: Option<Fresh>, now: Instant) {
        let held = self.pending.entry((worker, path)).or_default();
        if fresh.is_some() {
            *held = fresh;
        }
        self.first.get_or_insert(now);
        self.last = Some(now);
    }

    /// The files to reload at `now`: all of them once the burst went quiet or was held long
    /// enough, else none.
    fn due(&mut self, now: Instant) -> Vec<((WorkerKey, String), Option<Fresh>)> {
        let (Some(first), Some(last)) = (self.first, self.last) else { return Vec::new() };
        let quiet = now.saturating_duration_since(last) >= QUIET;
        let held = now.saturating_duration_since(first) >= HOLD;
        if !quiet && !held {
            return Vec::new();
        }
        self.first = None;
        self.last = None;
        self.pending.drain().collect()
    }

    /// When to look again: when the burst goes quiet, or its hold ends.
    fn next(&self) -> Option<Instant> {
        let (first, last) = (self.first?, self.last?);
        let quiet = last.checked_add(QUIET)?;
        let held = first.checked_add(HOLD)?;
        Some(quiet.min(held))
    }
}

/// A call's page: its thread's worker, its thread and its call.
#[derive(Clone, Debug)]
struct CallOf {
    key: WorkerKey,
    thread: ThreadId,
    item: String,
}

/// A file the app draws itself.
enum Doc {
    /// A PDF: its bytes, its pages as drawn, the pages drawing now, and its first page's
    /// picture as the card copies it.
    Pdf { bytes: Arc<[u8]>, doc: Arc<PdfDoc>, drawing: HashSet<usize>, first: Option<Snapshot> },
    /// A CSV or TSV file's table.
    Table(Arc<PageTable>),
}

impl Doc {
    /// The document as the card draws it.
    fn shown(&self) -> PageDoc {
        match self {
            Self::Pdf { doc, .. } => PageDoc::Pdf(Arc::clone(doc)),
            Self::Table(table) => PageDoc::Table(Arc::clone(table)),
        }
    }
}

/// What a file a page shows is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FileKind {
    /// A document the web view shows (`.html`, `.svg`).
    Page,
    /// A PDF, drawn by the app.
    Pdf,
    /// A table, its fields split by this.
    Table(char),
}

impl FileKind {
    fn of(path: &str) -> Self {
        if path.to_ascii_lowercase().ends_with(".pdf") {
            return Self::Pdf;
        }
        PageTable::delimiter_of(path).map_or(Self::Page, Self::Table)
    }
}

/// What an ask of a page's app waits to answer.
enum AppWait {
    /// The page's own document.
    Document(String),
    /// A page's `resources/read`.
    Read(String, Request),
    /// A page's `tools/call`, which the person allowed.
    Call(String, Request, String),
}

/// An ask of a page's app on its way: where it went and what waits for it. A document or a
/// read is asked again on the next link to its machine, as one asked into a link that went is
/// lost with it; a tool's call is not, as it may have run.
struct AppAsked {
    worker: WorkerKey,
    thread: ThreadId,
    ask: AppAsk,
    wait: AppWait,
}

/// One page, as the app hosts it.
struct Page {
    /// The window it shows in.
    window: WindowId,
    /// Its call, for a page a call shows; a chart fence's has none.
    call: Option<CallOf>,
    /// The file it shows, on its call's worker, which it follows.
    path: Option<String>,
    /// The fence it draws: its language and its JSON as loaded.
    fence: Option<(String, String)>,
    /// What the app draws itself in place of a web view.
    doc: Option<Doc>,
    /// Its MCP App, for a page of a server.
    app: Option<McpApp>,
    /// The host's page, once its document came.
    id: Option<PageId>,
    /// This window's host of its web view, while it is live.
    native: Option<gpui::composition::NativeHost>,
    /// Its last picture.
    snapshot: Option<Arc<gpui::RenderImage>>,
    /// The network its app declares, granted or not.
    network: Option<Network>,
    /// What it asks the person, by the number the card answers it by.
    asked: HashMap<String, Request>,
    /// Its width, as the card last said.
    width: Option<f32>,
    /// Its document and its name, once read: what `slopty.SavePage` saves.
    document: Option<(String, String)>,
    /// When its card first opened it: what its log measures from.
    opened: Instant,
    /// Whether a picture of it came yet.
    pictured: bool,
}

/// The card's watches of each page, by its key, each with its number.
type Watchers = Rc<RefCell<HashMap<String, Vec<(u64, Box<dyn FnMut(&mut App)>)>>>>;

/// The pages of every window, and what watches them.
#[derive(Default)]
pub(super) struct Pages {
    hosts: HashMap<WindowId, Rc<PageHost>>,
    shown: HashMap<String, Page>,
    /// How each page stands, for `slopty.WatchPage`, by its key.
    states: HashMap<String, watch::Sender<PageState>>,
    /// The card's watches, by page key.
    watchers: Watchers,
    next: u64,
    heard: Option<futures::channel::mpsc::UnboundedSender<Heard>>,
    pump: Option<Task<()>>,
    /// What the pages asked of their apps and wait for, by the ask's number.
    apps: HashMap<u64, AppAsked>,
    /// The theme each window's host was last given.
    themes: HashMap<WindowId, Theme>,
    /// The files each worker follows for the pages, as last sent.
    watched: HashMap<WorkerKey, Vec<String>>,
    /// The watched files that changed, and the timer that reloads their pages.
    refresh: Refresh,
    refreshing: bool,
    /// The pages a card asked for that could not open yet, each logged once with why.
    unopened: HashMap<String, &'static str>,
}

impl Pages {
    /// File `path` on `worker` changed (`WorkerMsg::File`, unasked): its pages reload once its
    /// burst of changes ends.
    pub(super) fn file_changed(&self, worker: WorkerKey, path: &str, read: &FileRead) {
        let shows = self.shown.values().any(|page| {
            page.path.as_deref() == Some(path)
                && page.call.as_ref().is_some_and(|c| c.key == worker)
        });
        let Some(heard) = self.heard.as_ref().filter(|_| shows) else { return };
        let fresh = match read {
            FileRead::Text { text, final_newline, .. } => {
                let mut text = text.clone();
                if *final_newline {
                    text.push('\n');
                }
                Some(Fresh::Text(text))
            }
            FileRead::Media { bytes, .. } => Some(Fresh::Bytes(bytes.to_vec())),
            _ => None,
        };
        let _queued = heard.unbounded_send(Heard::Changed(worker, path.to_owned(), fresh));
    }
}

impl std::fmt::Debug for Pages {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pages").field("shown", &self.shown.len()).finish_non_exhaustive()
    }
}

/// A page's presence in the card's words.
const fn presence_of(presence: Presence) -> PagePresence {
    match presence {
        Presence::Cold => PagePresence::Cold,
        Presence::Warm => PagePresence::Warm,
        Presence::Live => PagePresence::Live,
    }
}

/// A page's presence in the watch's words.
const fn presence_words(presence: Presence) -> &'static str {
    match presence {
        Presence::Cold => PageState::COLD,
        Presence::Warm => PageState::WARM,
        Presence::Live => PageState::LIVE,
    }
}

/// A picture of a page as GPUI draws it.
fn picture(snapshot: &Snapshot) -> Option<Arc<gpui::RenderImage>> {
    render_image(snapshot.width, snapshot.height, snapshot.bgra.clone())
}

/// BGRA pixels as GPUI draws them (its images are BGRA).
fn render_image(width: u32, height: u32, bgra: Vec<u8>) -> Option<Arc<gpui::RenderImage>> {
    let pixels = image::RgbaImage::from_raw(width, height, bgra)?;
    Some(Arc::new(gpui::RenderImage::new([image::Frame::new(pixels)])))
}

/// A PDF's page drawn: its size in pixels and its BGRA pixels.
struct Drawn {
    width: u32,
    height: u32,
    bgra: Vec<u8>,
}

/// A PDF's page count and its first page's height over its width.
fn pdf_open(bytes: &[u8]) -> Result<(usize, f32), String> {
    let pdf = slopty_platform::pdf::Pdf::open(bytes).map_err(|e| format!("Not shown: {e}"))?;
    let aspect = pdf.page_size(0).map_or(A4, |(w, h)| if w > 0.0 { narrow(h / w) } else { A4 });
    Ok((pdf.page_count(), aspect))
}

/// Page `index` of a PDF drawn `width` pixels wide.
fn pdf_draw(bytes: &[u8], index: usize, width: u32) -> Option<Drawn> {
    let picture = slopty_platform::pdf::Pdf::open(bytes).ok()?.draw(index, width)?;
    Some(Drawn { width: picture.width, height: picture.height, bgra: picture.bgra })
}

/// An A4 page's height over its width, which a page not yet measured takes.
const A4: f32 = 1.414;

/// A ratio as the card lays it out.
#[expect(clippy::cast_possible_truncation, reason = "a page's aspect is a small ratio")]
const fn narrow(ratio: f64) -> f32 {
    ratio as f32
}

/// The pixels a PDF's page is drawn wide for `points` on a 2× display, at most 4096.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to 1..=4096 first"
)]
fn draw_pixels(points: f32) -> u32 {
    let pixels = (f64::from(points) * 2.0).round();
    (if pixels.is_finite() { pixels.clamp(1.0, 4096.0) } else { 1.0 }) as u32
}

/// The origins an app's resource `_meta` declares (`ui.csp`), when it declares any.
fn declared_network(meta: &str) -> Option<Network> {
    let meta: serde_json::Value = serde_json::from_str(meta).ok()?;
    let csp = meta.get("ui")?.get("csp")?;
    let list = |key: &str| -> Vec<String> {
        csp.get(key)
            .and_then(serde_json::Value::as_array)
            .map(|all| all.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect())
            .unwrap_or_default()
    };
    let network = Network { connect: list("connectDomains"), resources: list("resourceDomains") };
    (!network.connect.is_empty() || !network.resources.is_empty()).then_some(network)
}

/// The words of MCP content blocks: their text, one block a paragraph.
fn content_words(content: &serde_json::Value) -> String {
    let blocks = match content {
        serde_json::Value::Array(blocks) => blocks.iter().collect(),
        other => vec![other],
    };
    blocks
        .into_iter()
        .filter_map(|block| match block {
            serde_json::Value::String(text) => Some(text.clone()),
            block => block.get("text").and_then(serde_json::Value::as_str).map(str::to_owned),
        })
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// A tool call's arguments as the card shows them: a few lines of JSON.
fn arguments_words(arguments: &serde_json::Value) -> String {
    let text = serde_json::to_string_pretty(arguments).unwrap_or_default();
    let mut lines: Vec<&str> = text.lines().take(8).collect();
    if text.lines().nth(8).is_some() {
        lines.push("…");
    }
    lines.join("\n")
}

/// An answer read from an app as `resources/read`'s result.
fn read_result(answer: AppAnswer) -> Result<serde_json::Value, Declined> {
    match answer {
        AppAnswer::Resource { uri, mime_type, body, meta } => {
            let mut content = serde_json::Map::new();
            content.insert("uri".to_owned(), serde_json::Value::String(uri));
            if let Some(mime) = mime_type {
                content.insert("mimeType".to_owned(), serde_json::Value::String(mime));
            }
            let (field, value) = match body {
                AppBody::Text(text) => ("text", text),
                AppBody::Bytes(bytes) => {
                    use base64::Engine as _;
                    ("blob", base64::engine::general_purpose::STANDARD.encode(bytes))
                }
            };
            content.insert(field.to_owned(), serde_json::Value::String(value));
            if let Ok(meta) = serde_json::from_str::<serde_json::Value>(&meta) {
                content.insert("_meta".to_owned(), meta);
            }
            Ok(serde_json::json!({ "contents": [content] }))
        }
        AppAnswer::Called { .. } => Err(declined("not a resource")),
        AppAnswer::Refused(why) => Err(declined(&why)),
    }
}

/// A request declined for `why`.
fn declined(why: &str) -> Declined {
    Declined { code: -32603, message: why.to_owned() }
}

impl Page {
    /// A page of `window`, its document still to come.
    fn new(window: WindowId, call: Option<CallOf>) -> Self {
        Self {
            window,
            call,
            path: None,
            fence: None,
            doc: None,
            app: None,
            id: None,
            native: None,
            snapshot: None,
            network: None,
            asked: HashMap::new(),
            width: None,
            document: None,
            opened: Instant::now(),
            pictured: false,
        }
    }
}

impl WorkspaceView {
    /// The page `key` as it stands in `window`, its host made and its document read the first
    /// time it is asked: the shell's card draws it.
    pub fn hosted_page(
        &mut self,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<HostedPage> {
        let window_id = window.window_handle().window_id();
        if !self.view_model.pages.shown.contains_key(key) {
            self.page_open(key, window_id, window, cx)?;
        } else if fence::is_fence(key) {
            self.page_fence_moved(key, cx);
        }
        self.page_theme(window_id, window, cx);
        let waiting = self.page_waiting(key, cx).map(Into::into);
        let host = self.view_model.pages.hosts.get(&window_id).cloned()?;
        let page = self.view_model.pages.shown.get_mut(key)?;
        let state = self.view_model.pages.states.get(key).map(|s| s.borrow().clone());
        let failed = state.as_ref().and_then(|s| s.failed.clone()).map(Into::into);
        let height = state.as_ref().and_then(|s| s.height).map(points);
        if let Some(doc) = &page.doc {
            return Some(HostedPage {
                presence: PagePresence::Warm,
                native: None,
                snapshot: None,
                height: None,
                failed,
                waiting: None,
                doc: Some(doc.shown()),
            });
        }
        if page.window != window_id {
            // Another window shows its picture; the page lives in the one that opened it.
            let snapshot = page.snapshot.clone();
            return Some(HostedPage {
                presence: PagePresence::Cold,
                native: None,
                snapshot,
                height,
                failed,
                waiting,
                doc: None,
            });
        }
        let presence = page.id.and_then(|id| host.presence(id)).unwrap_or(Presence::Cold);
        if presence == Presence::Live && page.native.is_none() {
            page.native = page.id.and_then(|id| {
                let view = host.view(id)?;
                let options = gpui::composition::NativeHostOptions {
                    opaque: true,
                    interactive: true,
                    label: Some("Page".into()),
                };
                let native = window.create_native_host(options, cx).ok()?;
                // SAFETY: `view` is the page's live WKWebView, mounted while it is live, and this
                // runs on the main thread (gpui-fast's `NativeHost::attach_view` contract).
                unsafe { native.attach_view(view) }.ok()?;
                Some(native)
            });
        }
        if presence != Presence::Live {
            page.native = None;
        }
        Some(HostedPage {
            presence: presence_of(presence),
            native: page.native.clone(),
            snapshot: page.snapshot.clone(),
            height,
            failed,
            waiting,
            doc: None,
        })
    }

    /// The machine page `key`'s document waits for, by its name, while that machine is out of
    /// reach: a page of a call whose document has not come, and that has not failed.
    fn page_waiting(&self, key: &str, cx: &App) -> Option<String> {
        let page = self.view_model.pages.shown.get(key)?;
        let call = page.call.as_ref()?;
        let failed =
            self.view_model.pages.states.get(key).is_some_and(|s| s.borrow().failed.is_some());
        if page.document.is_some() || page.doc.is_some() || failed {
            return None;
        }
        if self.held_hub(call.key)?.read(cx).linked() {
            return None;
        }
        self.workers.get(&call.key).map(|w| w.name.clone())
    }

    /// The link to `worker` went: its pages that wait for a document say so, and what their
    /// apps were asked and may never answer: a tool's call is told its machine went, as it is
    /// not asked again.
    pub(in crate::workspace) fn pages_unlinked(
        &mut self,
        worker: WorkerKey,
        cx: &mut Context<Self>,
    ) {
        let calls: Vec<u64> = self
            .view_model
            .pages
            .apps
            .iter()
            .filter(|(_, asked)| asked.worker == worker && matches!(asked.wait, AppWait::Call(..)))
            .map(|(id, _)| *id)
            .collect();
        for id in calls {
            let why = "Its machine went out of reach before the tool answered".to_owned();
            self.page_app_answer(id, AppAnswer::Refused(why), cx);
        }
        self.pages_of_moved(worker, cx);
    }

    /// The link to `worker` is back: what its pages' apps were asked and did not answer is
    /// asked again; the pages' documents read through the thread are asked again by their own
    /// waits ([`Self::view_whole_within`]).
    pub(in crate::workspace) fn pages_linked(&mut self, worker: WorkerKey, cx: &mut Context<Self>) {
        let again: Vec<ClientMsg> = self
            .view_model
            .pages
            .apps
            .iter()
            .filter(|(_, asked)| asked.worker == worker && !matches!(asked.wait, AppWait::Call(..)))
            .map(|(id, asked)| {
                let (thread, ask) = (asked.thread, asked.ask.clone());
                ClientMsg::Thread(ThreadRequest::App { thread, id: *id, ask })
            })
            .collect();
        for msg in again {
            self.send(worker, msg);
        }
        self.pages_of_moved(worker, cx);
    }

    /// The pages of `worker`'s calls: their cards hear that their machine came or went.
    fn pages_of_moved(&mut self, worker: WorkerKey, cx: &mut Context<Self>) {
        let keys: Vec<String> = self
            .view_model
            .pages
            .shown
            .iter()
            .filter(|(_, page)| page.call.as_ref().is_some_and(|call| call.key == worker))
            .map(|(key, _)| key.clone())
            .collect();
        for key in keys {
            self.page_changed(&key, cx);
        }
    }

    /// Give `window`'s host the shell's theme when it changed.
    fn page_theme(&mut self, window_id: WindowId, window: &Window, cx: &App) {
        let Some(host) = self.view_model.pages.hosts.get(&window_id) else { return };
        let theme = page_theme_of(window, cx);
        if self.view_model.pages.themes.get(&window_id) != Some(&theme) {
            host.set_theme(theme.clone());
            self.view_model.pages.themes.insert(window_id, theme);
        }
    }

    /// What the card asks of page `key`'s host.
    pub fn page_act(&mut self, key: &str, act: PageAct, cx: &mut Context<Self>) {
        if let PageAct::Draw { index, width } = act {
            self.page_draw(key, index, width, cx);
            return;
        }
        if act == PageAct::Reload {
            self.page_forget(key, cx);
            return;
        }
        let Some(page) = self.view_model.pages.shown.get_mut(key) else { return };
        let Some(host) = self.view_model.pages.hosts.get(&page.window).cloned() else { return };
        let Some(id) = page.id else {
            if let PageAct::Width(width) = act {
                page.width = Some(width);
            }
            return;
        };
        let window = page.window;
        let moved = match act {
            PageAct::Engage => host.engage(id),
            PageAct::Rest => host.rest(id),
            PageAct::Width(width) => {
                page.width = Some(width);
                host.resize(id, f64::from(width));
                Vec::new()
            }
            PageAct::Unmount => {
                page.native = None;
                host.unmount(id)
            }
            PageAct::Draw { .. } | PageAct::Reload => Vec::new(),
        };
        // One page engaged past the device's cap rests another: its card hears it too.
        let others: Vec<String> = self
            .view_model
            .pages
            .shown
            .iter()
            .filter(|(other, page)| {
                other.as_str() != key
                    && page.window == window
                    && page.id.is_some_and(|id| moved.iter().any(|change| change.page == id))
            })
            .map(|(other, _)| other.clone())
            .collect();
        self.page_changed(key, cx);
        for other in others {
            self.page_changed(&other, cx);
        }
    }

    /// Let page `key` go, its web view closed and what failed forgotten: the next draw of its
    /// card opens it afresh from what showed it.
    fn page_forget(&mut self, key: &str, cx: &mut Context<Self>) {
        let pages = &mut self.view_model.pages;
        if let Some(page) = pages.shown.remove(key)
            && let (Some(id), Some(host)) = (page.id, pages.hosts.get(&page.window))
        {
            host.close(id);
        }
        if let Some(state) = pages.states.get(key) {
            state.send_modify(|s| s.failed = None);
        }
        self.page_changed(key, cx);
    }

    /// Call `changed` whenever how page `key` stands changes, while the subscription is held.
    pub fn page_watch(&mut self, key: &str, changed: Box<dyn FnMut(&mut App)>) -> Subscription {
        let pages = &mut self.view_model.pages;
        let id = pages.next;
        pages.next = pages.next.saturating_add(1);
        pages.watchers.borrow_mut().entry(key.to_owned()).or_default().push((id, changed));
        let watchers = Rc::downgrade(&pages.watchers);
        let key = key.to_owned();
        Subscription::new(move || {
            if let Some(watchers) = watchers.upgrade()
                && let Some(list) = watchers.borrow_mut().get_mut(&key)
            {
                list.retain(|(had, _)| *had != id);
            }
        })
    }

    /// How page `key` stands changed: its watch and its card hear it.
    fn page_changed(&mut self, key: &str, cx: &mut Context<Self>) {
        let pages = &mut self.view_model.pages;
        let presence = pages.shown.get(key).and_then(|page| {
            let host = pages.hosts.get(&page.window)?;
            host.presence(page.id?)
        });
        let state = pages.states.entry(key.to_owned()).or_insert_with(|| {
            watch::Sender::new(PageState {
                page: key.to_owned(),
                presence: PageState::COLD.to_owned(),
                display_mode: "inline".to_owned(),
                ..PageState::default()
            })
        });
        state.send_if_modified(|now| {
            let words = presence_words(presence.unwrap_or(Presence::Cold));
            let moved = now.presence != words;
            if moved {
                words.clone_into(&mut now.presence);
            }
            moved
        });
        let watchers = Rc::clone(&pages.watchers);
        let key = key.to_owned();
        cx.defer(move |cx| {
            // Taken out while they run: one may watch again or let go.
            let mut list = watchers.borrow_mut().remove(&key).unwrap_or_default();
            for (_, changed) in &mut list {
                changed(cx);
            }
            let mut held = watchers.borrow_mut();
            let now = held.entry(key).or_default();
            list.append(now);
            *now = list;
        });
    }

    /// Change how page `key` stands with `change`, and say so.
    fn page_state(
        &mut self,
        key: &str,
        change: impl FnOnce(&mut PageState),
        cx: &mut Context<Self>,
    ) {
        self.page_changed(key, cx);
        if let Some(state) = self.view_model.pages.states.get(key) {
            state.send_modify(change);
        }
    }

    /// This window's host, made the first time a page shows in it.
    fn page_host(
        &mut self,
        window_id: WindowId,
        window: &Window,
        cx: &Context<Self>,
    ) -> Option<Rc<PageHost>> {
        if let Some(host) = self.view_model.pages.hosts.get(&window_id) {
            return Some(Rc::clone(host));
        }
        let events = self.page_pump(cx);
        let theme = page_theme_of(window, cx);
        self.view_model.pages.themes.insert(window_id, theme.clone());
        let config = HostConfig {
            device: if cfg!(target_os = "ios") { Device::Pad } else { Device::Mac },
            theme,
            viewport: f64::from(window.viewport_size().height),
            locale: std::env::var("LANG")
                .ok()
                .and_then(|lang| lang.split('.').next().map(|tag| tag.replace('_', "-")))
                .filter(|tag| !tag.is_empty() && tag != "C")
                .unwrap_or_else(|| "en-US".to_owned()),
        };
        let host = PageHost::new(config, move |page, event| {
            let _queued = events.unbounded_send(Heard::Event(window_id, page, event));
        })?;
        let host = Rc::new(host);
        self.view_model.pages.hosts.insert(window_id, Rc::clone(&host));
        Some(host)
    }

    /// Where the pages' events go, read on the UI thread.
    fn page_pump(&mut self, cx: &Context<Self>) -> futures::channel::mpsc::UnboundedSender<Heard> {
        if let Some(heard) = &self.view_model.pages.heard {
            return heard.clone();
        }
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        self.view_model.pages.heard = Some(tx.clone());
        self.view_model.pages.pump = Some(cx.spawn(async move |this, cx| {
            while let Some(heard) = rx.next().await {
                if this.update(cx, |view, cx| view.page_heard(heard, cx)).is_err() {
                    return;
                }
            }
        }));
        tx
    }

    /// Open page `key` in `window`: its card's state, then its document, read through its
    /// thread, or built from its fence.
    fn page_open(
        &mut self,
        key: &str,
        window_id: WindowId,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<()> {
        if fence::is_fence(key) {
            let fence = fence::fence(key, cx)?;
            self.page_host(window_id, window, cx)?;
            self.view_model.pages.shown.insert(key.to_owned(), Page::new(window_id, None));
            self.page_changed(key, cx);
            self.page_fence_load(key, &fence, cx);
            return Some(());
        }
        if self.page_host(window_id, window, cx).is_none() {
            return self.page_unopened(key, "its window has no page host");
        }
        self.page_open_call(key, window_id, cx)
    }

    /// Open page `key` of a call in `window_id`, whose host is made: its card's state, then its
    /// document, read through its thread. A document its machine holds is waited for while the
    /// machine is out of reach, and asked again once it is back.
    fn page_open_call(
        &mut self,
        key: &str,
        window_id: WindowId,
        cx: &mut Context<Self>,
    ) -> Option<()> {
        let Some((thread, item)) = page_of(key) else {
            return self.page_unopened(key, "its key names no call");
        };
        let item = item.to_owned();
        let Some(worker) = self.worker_of_thread(thread, cx) else {
            return self.page_unopened(key, "its thread is on no machine's list");
        };
        let Some(call) = self.view_tool_call(worker, thread, &item, cx) else {
            return self.page_unopened(key, "its call is not in its thread as read");
        };
        let Some(ToolDetail::Page(detail)) = call.detail.clone() else {
            return self.page_unopened(key, "its call shows no page");
        };
        if let Some(why) = self.view_model.pages.unopened.remove(key) {
            tracing::info!(page = key, waited_on = why, "a page opens, its call here now");
        }
        let mut page =
            Page::new(window_id, Some(CallOf { key: worker, thread, item: item.clone() }));
        if let PageSource::App(app) = &detail.source {
            page.app = Some(app.clone());
        }
        if let PageSource::File(path) = &detail.source {
            page.path = Some(path.clone());
        }
        let watched = page.path.is_some();
        self.view_model.pages.shown.insert(key.to_owned(), page);
        self.page_changed(key, cx);
        if matches!(call.state, ToolState::Failed | ToolState::Cancelled | ToolState::Rejected) {
            self.page_state(
                key,
                |s| s.failed = Some("The call that showed it failed".to_owned()),
                cx,
            );
            return Some(());
        }
        if watched {
            self.page_watch_files(worker);
        }
        let title = if detail.title.trim().is_empty() { "Page".to_owned() } else { detail.title };
        match detail.source {
            PageSource::Html(html) => {
                let document = match html.full {
                    Some(full) => Self::view_whole_within(worker, thread, full, true, cx),
                    None => Task::ready(Some(Arc::new(Expanded::Text(html.text)))),
                };
                Self::page_document(key.to_owned(), title, document, cx);
            }
            PageSource::File(path) => {
                let name = path.rsplit('/').next().unwrap_or_default().to_owned();
                let content = ContentRef::item_asset(&item, &name);
                let document = Self::view_whole_within(worker, thread, content, true, cx);
                match FileKind::of(&path) {
                    FileKind::Page => Self::page_document(key.to_owned(), title, document, cx),
                    kind => Self::page_doc_document(key.to_owned(), kind, document, cx),
                }
            }
            PageSource::App(app) => {
                let ask = AppAsk::Read { server: app.server, uri: app.resource_uri };
                self.page_ask_app(worker, thread, ask, AppWait::Document(key.to_owned()));
            }
        }
        Some(())
    }

    /// Page `key` could not open yet, for `why`: said once, as its card asks on each frame.
    fn page_unopened(&mut self, key: &str, why: &'static str) -> Option<()> {
        let unopened = &mut self.view_model.pages.unopened;
        if unopened.get(key) != Some(&why) {
            tracing::info!(page = key, why, "a page's card waits: the page cannot open yet");
            unopened.insert(key.to_owned(), why);
        }
        None
    }

    /// Build fence page `key` from `fence`'s JSON and load it.
    fn page_fence_load(&mut self, key: &str, fence: &fence::Fence, cx: &mut Context<Self>) {
        let Some(page) = self.view_model.pages.shown.get_mut(key) else { return };
        page.fence = Some((fence.language.clone(), fence.source.clone()));
        let chart = libs::Chart::of(&fence.language);
        match chart.and_then(|chart| Some((chart, libs::fence_document(chart, &fence.source)?))) {
            Some((chart, html)) => {
                self.page_state(key, |s| s.failed = None, cx);
                self.page_reload(key, chart.name().to_owned(), html, cx);
            }
            None => self.page_state(
                key,
                |s| s.failed = Some("The chart's JSON does not read".to_owned()),
                cx,
            ),
        }
    }

    /// Fence page `key` again when its fence says something else now (an edited reply).
    fn page_fence_moved(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(now) = fence::fence(key, cx) else { return };
        let Some(page) = self.view_model.pages.shown.get(key) else { return };
        let moved = page
            .fence
            .as_ref()
            .is_some_and(|(language, source)| *language != now.language || *source != now.source);
        if moved {
            self.page_fence_load(key, &now, cx);
        }
    }

    /// Load page `key` afresh with `html`, its web view made anew; its picture stands for it
    /// until the new one draws.
    fn page_reload(&mut self, key: &str, title: String, html: String, cx: &mut Context<Self>) {
        let Some(page) = self.view_model.pages.shown.get_mut(key) else { return };
        if let (Some(id), Some(host)) =
            (page.id.take(), self.view_model.pages.hosts.get(&page.window))
        {
            host.close(id);
        }
        page.native = None;
        self.page_load(key, title, html, false, cx);
    }

    /// The files the pages of `worker` show, followed there.
    fn page_watch_files(&mut self, worker: WorkerKey) {
        let pages = &mut self.view_model.pages;
        let mut paths: Vec<String> = pages
            .shown
            .values()
            .filter(|page| page.call.as_ref().is_some_and(|call| call.key == worker))
            .filter_map(|page| page.path.clone())
            .collect();
        paths.sort();
        paths.dedup();
        if pages.watched.get(&worker) == Some(&paths) {
            return;
        }
        pages.watched.insert(worker, paths.clone());
        self.send(worker, ClientMsg::WatchFiles { paths });
    }

    /// Open file page `key`, a PDF or a table, with the bytes `document` brings.
    fn page_doc_document(
        key: String,
        kind: FileKind,
        document: Task<Option<Arc<Expanded>>>,
        cx: &Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            let bytes = match document.await.as_deref() {
                Some(Expanded::Bytes(bytes)) => Some(bytes.clone()),
                Some(Expanded::Text(text)) => Some(text.clone().into_bytes()),
                Some(Expanded::Gone) | None => None,
            };
            let _gone = this.update(cx, |view, cx| match bytes {
                Some(bytes) => view.page_doc_load(&key, kind, bytes, cx),
                None => view.page_state(
                    &key,
                    |s| s.failed = Some("Couldn't read its file".to_owned()),
                    cx,
                ),
            });
        })
        .detach();
    }

    /// File page `key` as `bytes` make it: a table parsed, or a PDF opened off the UI thread.
    /// A PDF read again keeps its pages' pictures until they are drawn anew.
    fn page_doc_load(&mut self, key: &str, kind: FileKind, bytes: Vec<u8>, cx: &mut Context<Self>) {
        match kind {
            FileKind::Page => {}
            FileKind::Table(delimiter) => {
                let table = PageTable::parse(&String::from_utf8_lossy(&bytes), delimiter);
                let Some(page) = self.view_model.pages.shown.get_mut(key) else { return };
                page.doc = Some(Doc::Table(Arc::new(table)));
                self.page_state(key, |s| s.loaded = true, cx);
            }
            FileKind::Pdf => {
                let bytes: Arc<[u8]> = Arc::from(bytes);
                let read = Arc::clone(&bytes);
                let opened = cx.background_executor().spawn(async move { pdf_open(&read) });
                let key = key.to_owned();
                cx.spawn(async move |this, cx| {
                    let opened = opened.await;
                    let _gone =
                        this.update(cx, |view, cx| view.page_pdf_opened(&key, bytes, opened, cx));
                })
                .detach();
            }
        }
    }

    /// PDF page `key` opened: how many pages it has and how they stand.
    fn page_pdf_opened(
        &mut self,
        key: &str,
        bytes: Arc<[u8]>,
        opened: Result<(usize, f32), String>,
        cx: &mut Context<Self>,
    ) {
        let (pages, aspect) = match opened {
            Ok(opened) => opened,
            Err(why) => return self.page_state(key, |s| s.failed = Some(why), cx),
        };
        let Some(page) = self.view_model.pages.shown.get_mut(key) else { return };
        // The pictures drawn before stand until each is drawn again.
        let drawn = match &page.doc {
            Some(Doc::Pdf { doc, .. }) => doc
                .drawn
                .iter()
                .map(|(ix, (_, picture))| (*ix, (-1.0, Arc::clone(picture))))
                .collect(),
            _ => HashMap::new(),
        };
        let first = match page.doc.take() {
            Some(Doc::Pdf { first, .. }) => first,
            _ => None,
        };
        let doc = Arc::new(PdfDoc { pages, aspect, drawn });
        page.doc = Some(Doc::Pdf { bytes, doc, drawing: HashSet::new(), first });
        self.page_state(key, |s| s.loaded = true, cx);
    }

    /// Draw page `index` of PDF page `key` `width` points wide, off the UI thread, unless it is
    /// drawing.
    fn page_draw(&mut self, key: &str, index: usize, width: f32, cx: &Context<Self>) {
        let Some(Page { doc: Some(Doc::Pdf { bytes, doc, drawing, .. }), .. }) =
            self.view_model.pages.shown.get_mut(key)
        else {
            return;
        };
        if index >= doc.pages || !drawing.insert(index) {
            return;
        }
        let bytes = Arc::clone(bytes);
        let read = Arc::clone(&bytes);
        let pixels = draw_pixels(width);
        let drawn = cx.background_executor().spawn(async move { pdf_draw(&read, index, pixels) });
        let key = key.to_owned();
        cx.spawn(async move |this, cx| {
            let drawn = drawn.await;
            let _gone = this.update(cx, |view, cx| {
                view.page_drawn(&key, (index, width), &bytes, drawn, cx);
            });
        })
        .detach();
    }

    /// Page `index` of PDF page `key` drawn `width` points wide from `bytes`; dropped when the
    /// file changed meanwhile.
    fn page_drawn(
        &mut self,
        key: &str,
        (index, width): (usize, f32),
        bytes: &Arc<[u8]>,
        drawn: Option<Drawn>,
        cx: &mut Context<Self>,
    ) {
        let Some(Page { doc: Some(Doc::Pdf { bytes: now, doc, drawing, first }), .. }) =
            self.view_model.pages.shown.get_mut(key)
        else {
            return;
        };
        if !Arc::ptr_eq(now, bytes) {
            return;
        }
        drawing.remove(&index);
        let Some(drawn) = drawn else { return };
        let Some(image) = render_image(drawn.width, drawn.height, drawn.bgra.clone()) else {
            return;
        };
        if index == 0 {
            *first = Some(Snapshot {
                width: drawn.width,
                height: drawn.height,
                bgra: drawn.bgra,
                took: Duration::ZERO,
            });
        }
        Arc::make_mut(doc).drawn.insert(index, (width, image));
        self.page_changed(key, cx);
    }

    /// The watched files that changed reload their pages once their burst ends.
    fn page_refresh_soon(&mut self, cx: &Context<Self>) {
        if self.view_model.pages.refreshing {
            return;
        }
        self.view_model.pages.refreshing = true;
        cx.spawn(async move |this, cx| {
            loop {
                let wait = this.update(cx, |view, cx| {
                    let now = cx.background_executor().now();
                    let due = view.view_model.pages.refresh.due(now);
                    for ((worker, path), fresh) in due {
                        view.page_reload_file(worker, &path, fresh, cx);
                    }
                    let next = view.view_model.pages.refresh.next();
                    if next.is_none() {
                        view.view_model.pages.refreshing = false;
                    }
                    next.map(|at| at.saturating_duration_since(now))
                });
                match wait {
                    Ok(Some(wait)) => cx.background_executor().timer(wait).await,
                    _ => return,
                }
            }
        })
        .detach();
    }

    /// The pages of file `path` on `worker` load it again: as it came with its change, or read
    /// afresh.
    fn page_reload_file(
        &mut self,
        worker: WorkerKey,
        path: &str,
        fresh: Option<Fresh>,
        cx: &mut Context<Self>,
    ) {
        let keys: Vec<String> = self
            .view_model
            .pages
            .shown
            .iter()
            .filter(|(_, page)| {
                page.path.as_deref() == Some(path)
                    && page.call.as_ref().is_some_and(|call| call.key == worker)
            })
            .map(|(key, _)| key.clone())
            .collect();
        if let Some(fresh) = fresh {
            for key in &keys {
                self.page_fresh(key, fresh.clone(), cx);
            }
            return;
        }
        let read = self.surface_read(worker, path.to_owned());
        cx.spawn(async move |this, cx| {
            let fresh = match read.await {
                Ok(FileRead::Text { mut text, final_newline, .. }) => {
                    if final_newline {
                        text.push('\n');
                    }
                    Fresh::Text(text)
                }
                Ok(FileRead::Media { bytes, .. }) => Fresh::Bytes(bytes.to_vec()),
                _ => return,
            };
            let _gone = this.update(cx, |view, cx| {
                for key in &keys {
                    view.page_fresh(key, fresh.clone(), cx);
                }
            });
        })
        .detach();
    }

    /// File page `key` with what its file holds now.
    fn page_fresh(&mut self, key: &str, fresh: Fresh, cx: &mut Context<Self>) {
        let Some(page) = self.view_model.pages.shown.get(key) else { return };
        let kind = FileKind::of(page.path.as_deref().unwrap_or_default());
        let title = page.document.as_ref().map_or_else(|| "Page".to_owned(), |(t, _)| t.clone());
        let bytes = match fresh {
            Fresh::Text(text) => text.into_bytes(),
            Fresh::Bytes(bytes) => bytes,
        };
        match kind {
            FileKind::Page => {
                let html = String::from_utf8_lossy(&bytes).into_owned();
                self.page_reload(key, title, html, cx);
            }
            kind => self.page_doc_load(key, kind, bytes, cx),
        }
    }

    /// Open page `key` in its host with the document `document` brings.
    fn page_document(
        key: String,
        title: String,
        document: Task<Option<Arc<Expanded>>>,
        cx: &Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            let html = match document.await.as_deref() {
                Some(Expanded::Text(text)) => Some(text.clone()),
                Some(Expanded::Bytes(bytes)) => String::from_utf8(bytes.clone()).ok(),
                Some(Expanded::Gone) | None => None,
            };
            let _gone = this.update(cx, |view, cx| {
                let Some(html) = html else {
                    tracing::info!(page = key, "a page's document did not come from its machine");
                    view.page_state(
                        &key,
                        |s| s.failed = Some("Couldn't read its page".to_owned()),
                        cx,
                    );
                    return;
                };
                view.page_load(&key, title, html, false, cx);
            });
        })
        .detach();
    }

    /// Hand page `key`'s document to its window's host, warm: its picture stands for it until
    /// the person engages it.
    fn page_load(
        &mut self,
        key: &str,
        title: String,
        html: String,
        server: bool,
        cx: &mut Context<Self>,
    ) {
        let heard = self.page_pump(cx);
        let Some(page) = self.view_model.pages.shown.get_mut(key) else { return };
        page.document = Some((title.clone(), html.clone()));
        let Some(host) = self.view_model.pages.hosts.get(&page.window).cloned() else { return };
        let window = page.window;
        let ids: Rc<RefCell<Option<PageId>>> = Rc::default();
        let id_of = Rc::clone(&ids);
        let resources: slopty_platform::web::page::Resources = Rc::new(move |path, responder| {
            let Some(id) = *id_of.borrow() else { return };
            let _queued = heard.unbounded_send(Heard::File(window, id, path.to_owned(), responder));
        });
        let id = host.open(PageSpec { title, html, server, resources: Some(resources) });
        *ids.borrow_mut() = Some(id);
        page.id = Some(id);
        if let Some(width) = page.width {
            host.resize(id, f64::from(width));
        }
        let moved = host.mount(id);
        tracing::info!(
            page = key,
            bytes = page.document.as_ref().map_or(0, |(_, html)| html.len()),
            after_ms = page.opened.elapsed().as_millis(),
            ?moved,
            "a page's document went to its web view"
        );
        self.page_changed(key, cx);
    }

    /// The page `id` of `window`, by its key.
    fn page_key(&self, window: WindowId, id: PageId) -> Option<String> {
        self.view_model
            .pages
            .shown
            .iter()
            .find(|(_, page)| page.window == window && page.id == Some(id))
            .map(|(key, _)| key.clone())
    }

    /// What a window's pages told the app.
    fn page_heard(&mut self, heard: Heard, cx: &mut Context<Self>) {
        match heard {
            Heard::File(window, id, path, responder) => {
                self.page_file(window, id, &path, responder, cx);
            }
            Heard::Event(window, id, event) => {
                let Some(key) = self.page_key(window, id) else { return };
                self.page_event(&key, window, id, event, cx);
            }
            Heard::Changed(worker, path, fresh) => {
                let now = cx.background_executor().now();
                self.view_model.pages.refresh.heard(worker, path, fresh, now);
                self.page_refresh_soon(cx);
            }
        }
    }

    /// A page's file, read on its worker beside its call's folder.
    fn page_file(
        &self,
        window: WindowId,
        id: PageId,
        path: &str,
        responder: Responder,
        cx: &Context<Self>,
    ) {
        let Some(key) = self.page_key(window, id) else { return };
        let Some(page) = self.view_model.pages.shown.get(&key) else { return };
        // The chart libraries the app vendors, which any page may load.
        if let Some(lib) = libs::lib(path) {
            return responder.answer(Some(lib));
        }
        // An app's view is its one resource: anything else it needs it reads as a resource. A
        // fence's page has nothing beside it.
        let Some(call) = page.call.as_ref().filter(|_| page.app.is_none()) else {
            return responder.answer(None);
        };
        let content = ContentRef::item_asset(&call.item, path);
        let whole = Self::view_whole_within(call.key, call.thread, content, true, cx);
        let mime = mime_guess(path);
        cx.spawn(async move |_, _| {
            let file = match whole.await.as_deref() {
                Some(Expanded::Bytes(bytes)) => Some(bytes.clone()),
                Some(Expanded::Text(text)) => Some(text.clone().into_bytes()),
                Some(Expanded::Gone) | None => None,
            };
            responder.answer(file.map(|bytes| Resource { mime: mime.to_owned(), bytes }));
        })
        .detach();
    }

    /// One event of page `key`'s.
    fn page_event(
        &mut self,
        key: &str,
        window: WindowId,
        id: PageId,
        event: PageEvent,
        cx: &mut Context<Self>,
    ) {
        let host = self.view_model.pages.hosts.get(&window).cloned();
        match event {
            PageEvent::Loaded | PageEvent::TornDown | PageEvent::Log { .. } => {}
            PageEvent::Refused(refusal) => tracing::debug!(?refusal, "a page's sandbox refused"),
            PageEvent::ViewLoaded => {
                tracing::info!(page = key, after_ms = self.page_age(key), "a page's view loaded");
                self.page_state(key, |s| s.loaded = true, cx);
                if let Some(host) = host {
                    let _asked = host.snapshot(id);
                }
            }
            PageEvent::Initialized => self.page_tell(key, cx),
            PageEvent::Resized { height, .. } => {
                // Whole points: the card reserves its place by them.
                let height = Some(whole_points(height));
                self.page_state(key, |s| s.height = height, cx);
                if let Some(host) = host {
                    let _asked = host.snapshot(id);
                }
            }
            PageEvent::Snapshot(snapshot) => {
                if let Some(page) = self.view_model.pages.shown.get_mut(key) {
                    page.snapshot = picture(&snapshot);
                    if !std::mem::replace(&mut page.pictured, true) {
                        tracing::info!(
                            page = key,
                            after_ms = page.opened.elapsed().as_millis(),
                            took_ms = snapshot.took.as_millis(),
                            "a page's first picture came"
                        );
                    }
                }
                self.page_changed(key, cx);
            }
            PageEvent::NetworkSet(_) => self.page_state(key, |s| s.network = None, cx),
            PageEvent::Failed(why) => {
                tracing::info!(page = key, why, after_ms = self.page_age(key), "a page failed");
                self.page_state(key, |s| s.failed = Some(why), cx);
            }
            PageEvent::Crashed => {
                tracing::info!(page = key, after_ms = self.page_age(key), "a page's process ended");
                self.page_state(key, |s| s.failed = Some("The page stopped".to_owned()), cx);
            }
            PageEvent::Asked(request) => self.page_asked(key, request, cx),
        }
    }

    /// How long since page `key`'s card opened it, in milliseconds, for its log.
    fn page_age(&self, key: &str) -> Option<u128> {
        Some(self.view_model.pages.shown.get(key)?.opened.elapsed().as_millis())
    }

    /// Tell an MCP App's view the call it shows: its arguments and its result.
    fn page_tell(&self, key: &str, cx: &Context<Self>) {
        let Some(page) = self.view_model.pages.shown.get(key) else { return };
        let (Some(host), Some(id)) = (self.view_model.pages.hosts.get(&page.window), page.id)
        else {
            return;
        };
        let Some(of) = page.call.as_ref() else { return };
        let Some(call) = self.view_tool_call(of.key, of.thread, &of.item, cx) else { return };
        let arguments: serde_json::Value =
            serde_json::from_str(&call.input.text).unwrap_or(serde_json::Value::Null);
        let _told = host.notify(id, &Notice::Input(serde_json::json!({ "arguments": arguments })));
        let result = page.app.as_ref().and_then(|app| app.result.as_ref()).or(call.output.as_ref());
        if let Some(result) = result {
            let result = serde_json::from_str(&result.text).unwrap_or_else(
                |_| serde_json::json!({ "content": [{ "type": "text", "text": result.text }] }),
            );
            let _told = host.notify(id, &Notice::Output(result));
        }
    }

    /// What page `key` asks of the person.
    fn page_asked(&mut self, key: &str, request: Request, cx: &mut Context<Self>) {
        let Some(page) = self.view_model.pages.shown.get(key) else { return };
        let Some(host) = self.view_model.pages.hosts.get(&page.window).cloned() else { return };
        let Some(id) = page.id else { return };
        let Some(CallOf { key: worker, thread, .. }) = page.call.clone() else {
            // A fence's page is the app's own chart: it asks nothing of the person.
            let _answered = host.answer(id, &request.id, Err(Declined::by_the_person()));
            return;
        };
        let chat = self.chat_id(thread);
        let app = page.app.clone();
        match &request.ask {
            // Words for the chat, and context offered for its next turn, become the person's
            // draft: theirs to send, edit or drop.
            Ask::Message { content, .. } | Ask::UpdateContext { params: content } => {
                let words = content_words(content.get("content").unwrap_or(content));
                let put = !words.is_empty() && slopty_shell::live::attach(&chat, &words, cx);
                let answer = if put {
                    Ok(serde_json::json!({}))
                } else {
                    Err(declined("The chat is not open"))
                };
                let _answered = host.answer(id, &request.id, answer);
            }
            Ask::OpenLink { url } => {
                let web = url.starts_with("https://") || url.starts_with("http://");
                let answer = if web && request.activated {
                    cx.open_url(url);
                    Ok(serde_json::json!({}))
                } else {
                    Err(Declined::by_the_person())
                };
                let _answered = host.answer(id, &request.id, answer);
            }
            // Inline is the one place a page shows.
            Ask::DisplayMode { .. } => {
                let _answered =
                    host.answer(id, &request.id, Ok(serde_json::json!({ "mode": "inline" })));
            }
            Ask::ReadResource { uri } => match app {
                Some(app) if is_ui_uri(uri) => {
                    let ask = AppAsk::Read { server: app.server, uri: uri.clone() };
                    self.page_ask_app(worker, thread, ask, AppWait::Read(key.to_owned(), request));
                }
                _ => {
                    let _answered =
                        host.answer(id, &request.id, Err(declined("Not a resource of this page")));
                }
            },
            // A tool call waits for the person's click in the page, then their Allow.
            Ask::CallTool { name, arguments } => {
                if app.is_none() || !request.activated {
                    let _answered = host.answer(id, &request.id, Err(Declined::by_the_person()));
                    return;
                }
                let pages = &mut self.view_model.pages;
                let number = format!("r{}", pages.next);
                pages.next = pages.next.saturating_add(1);
                let confirm = PageConfirm {
                    request: number.clone(),
                    tool: name.clone(),
                    arguments: arguments_words(arguments),
                };
                if let Some(page) = pages.shown.get_mut(key) {
                    page.asked.insert(number, request);
                }
                self.page_state(key, |s| s.confirm = Some(confirm), cx);
            }
        }
    }

    /// Ask the agent of `thread` on `worker` what a page asks of its app.
    fn page_ask_app(&mut self, worker: WorkerKey, thread: ThreadId, ask: AppAsk, wait: AppWait) {
        let pages = &mut self.view_model.pages;
        let id = pages.next;
        pages.next = pages.next.saturating_add(1);
        pages.apps.insert(id, AppAsked { worker, thread, ask: ask.clone(), wait });
        self.send(worker, ClientMsg::Thread(ThreadRequest::App { thread, id, ask }));
    }

    /// The agent's answer to what a page asked of its app.
    pub(in crate::workspace) fn page_app_answer(
        &mut self,
        id: u64,
        answer: AppAnswer,
        cx: &mut Context<Self>,
    ) {
        let Some(AppAsked { wait, .. }) = self.view_model.pages.apps.remove(&id) else { return };
        let host_of = |view: &Self, key: &str| {
            let page = view.view_model.pages.shown.get(key)?;
            Some((view.view_model.pages.hosts.get(&page.window).cloned()?, page.id?))
        };
        match wait {
            AppWait::Document(key) => match answer {
                AppAnswer::Resource { body, meta, .. } => {
                    let html = match body {
                        AppBody::Text(text) => Some(text),
                        AppBody::Bytes(bytes) => String::from_utf8(bytes).ok(),
                    };
                    let network = declared_network(&meta);
                    let Some(page) = self.view_model.pages.shown.get_mut(&key) else { return };
                    page.network.clone_from(&network);
                    let title = page.app.as_ref().map(|app| app.tool.clone()).unwrap_or_default();
                    let wants =
                        network.map(|n| PageNetwork { connect: n.connect, resources: n.resources });
                    self.page_state(&key, |s| s.network = wants, cx);
                    match html {
                        Some(html) => self.page_load(&key, title, html, true, cx),
                        None => self.page_state(
                            &key,
                            |s| s.failed = Some("Its view is not text".to_owned()),
                            cx,
                        ),
                    }
                }
                AppAnswer::Refused(why) => self.page_state(&key, |s| s.failed = Some(why), cx),
                AppAnswer::Called { .. } => {
                    self.page_state(
                        &key,
                        |s| s.failed = Some("Couldn't read its view".to_owned()),
                        cx,
                    );
                }
            },
            AppWait::Read(key, request) => {
                if let Some((host, page)) = host_of(self, &key) {
                    let _answered = host.answer(page, &request.id, read_result(answer));
                }
            }
            AppWait::Call(key, request, tool) => {
                let outcome = match answer {
                    AppAnswer::Called { result } => serde_json::from_str(&result)
                        .map_err(|e| declined(&format!("The tool's result did not read: {e}"))),
                    AppAnswer::Refused(why) => Err(declined(&why)),
                    AppAnswer::Resource { .. } => Err(declined("not a tool's result")),
                };
                if outcome.is_ok() {
                    self.page_state(&key, |s| s.called.push(tool), cx);
                }
                if let Some((host, page)) = host_of(self, &key) {
                    let _answered = host.answer(page, &request.id, outcome);
                }
            }
        }
    }

    /// `WatchPage`, `AnswerPage` and `GrantPageNetwork`; `None` for any other call.
    pub(super) fn page_call(
        &mut self,
        method: &str,
        params: &serde_json::Value,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<RpcReply, RpcError>>> {
        let ours = [
            calls::WATCH_PAGE,
            calls::ANSWER_PAGE,
            calls::GRANT_PAGE_NETWORK,
            calls::MOVE_PAGE,
            calls::SAVE_PAGE,
            calls::COPY_PAGE_IMAGE,
        ];
        if !ours.contains(&method) {
            return None;
        }
        let Some(key) = param(params, "page").map(str::to_owned) else {
            return Some(Task::ready(Err(RpcError::BadParams("page is required".to_owned()))));
        };
        if method == calls::SAVE_PAGE {
            return Some(self.page_save(&key, cx));
        }
        Some(Task::ready(match method {
            calls::MOVE_PAGE => {
                let pane = param(params, "to") == Some("pane");
                self.page_move(&key, pane, cx)
            }
            calls::COPY_PAGE_IMAGE => self.page_copy_image(&key, cx),
            calls::WATCH_PAGE => {
                self.page_changed(&key, cx);
                let rx = self.view_model.pages.states.get(&key).map(watch::Sender::subscribe);
                rx.map(|rx| {
                    let frames = futures::stream::unfold((rx, true), async |(mut rx, first)| {
                        if !first {
                            rx.changed().await.ok()?;
                        }
                        let value = serde_json::to_value(&*rx.borrow_and_update()).ok()?;
                        Some((value, (rx, false)))
                    });
                    RpcReply::Stream(frames.boxed())
                })
                .ok_or_else(|| RpcError::Failed("No such page".to_owned()))
            }
            calls::ANSWER_PAGE => {
                let allow =
                    params.get("allow").and_then(serde_json::Value::as_bool).unwrap_or(false);
                param(params, "request")
                    .map(str::to_owned)
                    .ok_or_else(|| RpcError::BadParams("request is required".to_owned()))
                    .and_then(|request| self.page_answer(&key, &request, allow, cx))
            }
            _ => self.page_grant(&key, cx),
        }))
    }

    /// Page `key` shown in the right pane (`pane`), or back inline: laid out to fill where it
    /// shows, its web view and state kept.
    fn page_move(
        &mut self,
        key: &str,
        pane: bool,
        cx: &mut Context<Self>,
    ) -> Result<RpcReply, RpcError> {
        let page = self
            .view_model
            .pages
            .shown
            .get(key)
            .ok_or_else(|| RpcError::Failed("No such page".to_owned()))?;
        if let (Some(host), Some(id)) = (self.view_model.pages.hosts.get(&page.window), page.id) {
            host.set_display_mode(id, if pane { "fullscreen" } else { "inline" });
        }
        let mode = if pane { "pane" } else { "inline" };
        self.page_state(key, |s| mode.clone_into(&mut s.display_mode), cx);
        Ok(RpcReply::Value(serde_json::json!({})))
    }

    /// Page `key`'s document saved in its chat's folder on its machine, through the workspace's
    /// own write.
    fn page_save(&mut self, key: &str, cx: &mut Context<Self>) -> Task<Result<RpcReply, RpcError>> {
        let Some(page) = self.view_model.pages.shown.get(key) else {
            return Task::ready(Err(RpcError::Failed("No such page".to_owned())));
        };
        let Some(call) = page.call.as_ref().filter(|_| page.doc.is_none()) else {
            return Task::ready(Err(RpcError::Failed("It is its source already".to_owned())));
        };
        let Some((title, html)) = page.document.clone() else {
            return Task::ready(Err(RpcError::Failed("It has not loaded yet".to_owned())));
        };
        let path = format!("{}-{}.html", file_stem(&title), file_stem(&call.item));
        let params = serde_json::json!({
            "chatId": self.chat_id(call.thread),
            "expectedCheckoutId": "",
            "path": path,
            "text": html,
            "expectedContentHash": "",
            "encoding": "utf8",
            "lineEnding": "lf",
        });
        let method = slopty_view_rpc::methods::WRITE_WORKSPACE_FILE;
        self.view_surface_call(method, &params, cx).unwrap_or_else(|| {
            Task::ready(Err(RpcError::Failed("Nothing writes files".to_owned())))
        })
    }

    /// Page `key`'s last picture on the clipboard, as a PNG.
    fn page_copy_image(&self, key: &str, cx: &Context<Self>) -> Result<RpcReply, RpcError> {
        let none = || RpcError::Failed("It has no picture yet".to_owned());
        let page = self.view_model.pages.shown.get(key).ok_or_else(none)?;
        let snapshot = match &page.doc {
            Some(Doc::Pdf { first, .. }) => Rc::new(first.clone().ok_or_else(none)?),
            Some(Doc::Table(_)) => {
                return Err(RpcError::Failed("A table copies as text".to_owned()));
            }
            None => {
                let host = self.view_model.pages.hosts.get(&page.window).ok_or_else(none)?;
                page.id.and_then(|id| host.last_snapshot(id)).ok_or_else(none)?
            }
        };
        let png = png_of(&snapshot).ok_or_else(none)?;
        let image = gpui::Image::from_bytes(gpui::ImageFormat::Png, png);
        cx.write_to_clipboard(gpui::ClipboardItem::new_image(&image));
        Ok(RpcReply::Value(serde_json::json!({
            "width": snapshot.width,
            "height": snapshot.height,
        })))
    }

    /// The person's answer to the tool call page `key` asked to make.
    fn page_answer(
        &mut self,
        key: &str,
        request: &str,
        allow: bool,
        cx: &mut Context<Self>,
    ) -> Result<RpcReply, RpcError> {
        let gone = || RpcError::Failed("The page no longer asks".to_owned());
        let page = self.view_model.pages.shown.get_mut(key).ok_or_else(gone)?;
        let asked = page.asked.remove(request).ok_or_else(gone)?;
        let CallOf { key: worker, thread, .. } = page.call.clone().ok_or_else(gone)?;
        let app = page.app.clone();
        let host = self.view_model.pages.hosts.get(&page.window).cloned();
        let id = page.id;
        self.page_state(key, |s| s.confirm = None, cx);
        let Ask::CallTool { name, arguments } = &asked.ask else { return Err(gone()) };
        match (allow, app) {
            (true, Some(app)) => {
                let ask = AppAsk::Call {
                    server: app.server,
                    tool: name.clone(),
                    arguments: arguments.to_string(),
                };
                let tool = name.clone();
                self.page_ask_app(worker, thread, ask, AppWait::Call(key.to_owned(), asked, tool));
            }
            _ => {
                if let (Some(host), Some(id)) = (host, id) {
                    let _answered = host.answer(id, &asked.id, Err(Declined::by_the_person()));
                }
            }
        }
        Ok(RpcReply::Value(serde_json::json!({})))
    }

    /// Let page `key` reach the network its app declared.
    fn page_grant(&mut self, key: &str, cx: &mut Context<Self>) -> Result<RpcReply, RpcError> {
        let gone = || RpcError::Failed("No such page".to_owned());
        let page = self.view_model.pages.shown.get(key).ok_or_else(gone)?;
        let network = page
            .network
            .clone()
            .ok_or_else(|| RpcError::Failed("It declares no network".to_owned()))?;
        let host = self.view_model.pages.hosts.get(&page.window).cloned().ok_or_else(gone)?;
        let id = page.id.ok_or_else(|| RpcError::Failed("It has not loaded yet".to_owned()))?;
        host.set_network(id, &network).map_err(|e| RpcError::Failed(e.to_string()))?;
        self.page_state(key, |s| s.network = None, cx);
        Ok(RpcReply::Value(serde_json::json!({})))
    }
}

/// The theme pages draw in: the window's appearance and the shell's tokens as MCP Apps' CSS
/// variables, the six chart colours with them.
fn page_theme_of(window: &Window, cx: &App) -> Theme {
    let dark = matches!(
        window.appearance(),
        gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark
    );
    let variables = slopty_shell::page::theme_variables(slopty_shell::theme::Theme::of(cx));
    Theme { dark, variables }
}

/// A height in whole points, as a page's watch says it.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to 0..=65535 first"
)]
const fn whole_points(height: f64) -> u32 {
    let height = if height.is_finite() { height.round().clamp(0.0, 65_535.0) } else { 0.0 };
    height as u32
}

/// Whole points as the card lays them out.
fn points(height: u32) -> f32 {
    f32::from(u16::try_from(height).unwrap_or(u16::MAX))
}

/// A page's picture as a PNG: its BGRA pixels in RGBA order.
fn png_of(snapshot: &Snapshot) -> Option<Vec<u8>> {
    let mut rgba = snapshot.bgra.clone();
    let (pixels, _) = rgba.as_chunks_mut::<4>();
    for pixel in pixels {
        pixel.swap(0, 2);
    }
    let image = image::RgbaImage::from_raw(snapshot.width, snapshot.height, rgba)?;
    let mut png = std::io::Cursor::new(Vec::new());
    image.write_to(&mut png, image::ImageFormat::Png).ok()?;
    Some(png.into_inner())
}

/// `text` as a file name's stem: letters, digits and dashes, at most 40 of them.
fn file_stem(text: &str) -> String {
    let stem: String = text
        .chars()
        .map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
        .chars()
        .take(40)
        .collect();
    if stem.is_empty() { "page".to_owned() } else { stem }
}

/// The MIME type a page's file is served as, by its extension.
fn mime_guess(path: &str) -> &'static str {
    let ext = path.rsplit_once('.').map(|(_, ext)| ext.to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" | "mjs" => "text/javascript",
        "json" => "application/json",
        "csv" => "text/csv",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "wasm" => "application/wasm",
        "glb" => "model/gltf-binary",
        "gltf" => "model/gltf+json",
        "txt" | "md" => "text/plain",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod link_tests;

#[cfg(test)]
mod tests {
    use super::*;

    /// The network an app's UI resource declares is read from its `ui.csp`; none declared is no
    /// grant to ask for.
    #[test]
    fn an_apps_declared_network_is_read() {
        let meta =
            r#"{"ui":{"csp":{"connectDomains":["https://api.example"],"resourceDomains":[]}}}"#;
        let network = declared_network(meta).expect("declared");
        assert_eq!(network.connect, ["https://api.example"]);
        assert!(declared_network(r#"{"ui":{}}"#).is_none());
        assert!(declared_network("not json").is_none());
    }

    /// What a page would say becomes words for the draft; blocks with none say nothing.
    #[test]
    fn a_pages_words_are_its_text_blocks() {
        let content = serde_json::json!([
            { "type": "text", "text": "Plot p99 by region" },
            { "type": "image", "data": "…" },
            { "type": "text", "text": "  " },
        ]);
        assert_eq!(content_words(&content), "Plot p99 by region");
        assert_eq!(content_words(&serde_json::json!("hi")), "hi");
    }

    /// A tool's arguments show as a few lines on the card, cut with an ellipsis.
    #[test]
    fn a_calls_arguments_are_cut_to_a_few_lines() {
        let many: serde_json::Map<String, serde_json::Value> =
            (0..20).map(|n| (format!("k{n:02}"), serde_json::json!(n))).collect();
        let words = arguments_words(&serde_json::Value::Object(many));
        assert_eq!(words.lines().count(), 9);
        assert!(words.ends_with('…'));
    }

    /// A refused read is declined with the agent's words; a read resource keeps its `_meta`.
    #[test]
    fn a_read_answers_as_mcp_says() {
        let refused = read_result(AppAnswer::Refused("no such server".into()));
        assert_eq!(refused.map_err(|d| d.message), Err("no such server".to_owned()));
        let read = read_result(AppAnswer::Resource {
            uri: "ui://charts/plot".into(),
            mime_type: Some("text/html;profile=mcp-app".into()),
            body: AppBody::Text("<p>".into()),
            meta: r#"{"ui":{"prefersBorder":true}}"#.into(),
        })
        .expect("read");
        assert_eq!(read["contents"][0]["text"], "<p>");
        assert_eq!(read["contents"][0]["_meta"]["ui"]["prefersBorder"], true);
        assert_eq!(mime_guess("data/a.CSV"), "text/csv");
    }

    /// A saved page is named by its title and call, in letters a file name keeps.
    #[test]
    fn a_saved_page_has_a_plain_name() {
        assert_eq!(file_stem("Latency by region (p99)!"), "latency-by-region-p99");
        assert_eq!(file_stem("toolu_01ABC"), "toolu-01abc");
        assert_eq!(file_stem("…"), "page");
        assert_eq!(file_stem(&"x".repeat(90)).len(), 40);
    }

    /// A burst of a thousand changes to a watched file reloads its page once, within 500 ms of
    /// the burst's start; a file written on and on still reloads every [`HOLD`]; nothing heard,
    /// nothing reloads.
    #[test]
    fn a_burst_of_writes_reloads_once() {
        let worker = WorkerKey::new(1);
        let start = Instant::now();
        let at = |micros: u32| start.checked_add(Duration::from_micros(u64::from(micros))).unwrap();
        let mut refresh = Refresh::default();
        assert!(refresh.due(start).is_empty() && refresh.next().is_none(), "nothing heard");
        let mut reloads = Vec::new();
        // A thousand writes over 50 ms, the clock stepping through them as the timer would.
        for n in 0..1000_u32 {
            let now = at(n.saturating_mul(50));
            let fresh = Some(Fresh::Text(format!("v{n}")));
            refresh.heard(worker, "/w/out/plot.html".to_owned(), fresh, now);
            for ((_, path), fresh) in refresh.due(now) {
                reloads.push((now, path, fresh));
            }
        }
        while let Some(next) = refresh.next() {
            for ((_, path), fresh) in refresh.due(next) {
                reloads.push((next, path, fresh));
            }
        }
        assert_eq!(reloads.len(), 1, "one reload for the burst");
        let (when, path, fresh) = reloads.remove(0);
        assert_eq!(path, "/w/out/plot.html");
        assert_eq!(fresh, Some(Fresh::Text("v999".to_owned())), "the burst's last version");
        let after = when.saturating_duration_since(start);
        assert!(after < Duration::from_millis(500), "reloaded {after:?} after the first write");
        println!("MEASURE refresh: 1000 writes over 50 ms → 1 reload, {after:?} after the first");

        // Written every 50 ms for two seconds: a reload every HOLD, never a starved page.
        let mut refresh = Refresh::default();
        let mut times = Vec::new();
        for n in 0..40_u32 {
            let now = at(n.saturating_mul(50_000));
            refresh.heard(worker, "/w/out/plot.html".to_owned(), None, now);
            if !refresh.due(now).is_empty() {
                times.push(now);
            }
        }
        assert!((4..=6).contains(&times.len()), "{} reloads in 2 s", times.len());
    }

    /// A file page is drawn by what its file is: a PDF, a table by its delimiter, else a page.
    #[test]
    fn a_files_kind_is_its_extension() {
        assert_eq!(FileKind::of("/w/out/Report.PDF"), FileKind::Pdf);
        assert_eq!(FileKind::of("/w/out/latency.csv"), FileKind::Table(','));
        assert_eq!(FileKind::of("/w/out/latency.tsv"), FileKind::Table('\t'));
        assert_eq!(FileKind::of("/w/out/plot.html"), FileKind::Page);
    }

    /// Every language the shell draws a chart fence for builds a page here, and nothing else.
    #[test]
    fn the_shell_and_the_libraries_name_the_same_charts() {
        for language in fence::CHART_LANGUAGES {
            assert!(libs::Chart::of(language).is_some(), "{language}");
        }
        assert!(fence::chart_language("mermaid").is_none() && libs::Chart::of("mermaid").is_none());
    }

    /// A picture copies as a PNG of its own size, its colours in RGBA order.
    #[test]
    fn a_picture_copies_as_a_png() {
        let snapshot = Snapshot {
            width: 2,
            height: 1,
            bgra: vec![255, 0, 0, 255, 0, 0, 255, 255],
            took: Duration::ZERO,
        };
        let png = png_of(&snapshot).expect("a png");
        let read = image::load_from_memory(&png).expect("it reads back").to_rgba8();
        assert_eq!(read.dimensions(), (2, 1));
        assert_eq!(read.get_pixel(0, 0).0, [0, 0, 255, 255], "blue stays blue");
        let short = Snapshot { bgra: vec![0; 4], ..snapshot };
        assert!(png_of(&short).is_none(), "too few pixels for its size");
    }
}

//! `WorkspaceView`: every worker's items, tiled.
//!
//! One view for every worker this client reaches: each worker's item registry is mirrored
//! ([`slopty_client::ItemDoc`]) and each item is a tile in this device's layout
//! ([`slopty_client::layout::Layout`]), a niri-style strip of columns per workspace, workspaces
//! stacked vertically. The registry is the worker's; the arrangement is this device's alone.
//!
//! * [`actions`] — the actions, the key table and the palette lines.
//! * `workers` — connecting, losing and forgetting a worker; the sync that follows.
//! * `commands` — what the actions do.
//! * `agents` — coding agents in terminals: badges, banners, "needs you".
//! * `browsers` — web pages in tiles: opening them, and the native view over each.
//! * `overlays` — the command palette, find in every tile, the window picker.
//! * `toast` — the one-line notices, undo close, pointing.
//! * [`remote`] — the clipboard shared with the workers, files dropped on tiles, forwarded ports.
//! * `strip` — the tiles laid out from the layout's frame, and the pointer and gestures.
//! * `tile` — one tile's chrome and body.
//! * `titlebar` — the bar across the top.

pub mod actions;
mod agents;
mod browsers;
mod commands;
mod overlays;
pub mod remote;
mod strip;
mod tile;
mod titlebar;
mod toast;
mod workers;

use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::time::{Duration, Instant};

pub use actions::*;
pub use agents::{agent_status_text, banner_title, program_banner};
use gpui::{
    App, Bounds, Context, Entity, EventEmitter, FocusHandle, Focusable, Pixels, SharedString, Task,
    Window,
};
use slopty_client::ItemDoc;
use slopty_client::layout::{Layout, LayoutConfig, Saved, TileRef, WorkerKey};
use slopty_core::{ClientId, ItemId, SessionId};
use slopty_proto::ClientMsg;
use slopty_proto::agent::AgentEvent;
use slopty_proto::items::{Item, ItemOp};
use slopty_proto::screen::CaptureTarget;
use slopty_proto::terminal::SessionSummary;
use slopty_theme::Theme;
#[cfg(test)]
pub(crate) use tile::{INSTALL_HOOKS, TAKE_OVER};
pub use tile::{NOTE_TITLE_CHARS, file_title, note_progress, note_title};
pub use titlebar::TITLEBAR_H;
use tokio::sync::mpsc;

use crate::file::FileView;
use crate::note::NoteView;
use crate::palette::{CommandPalette, PaletteItem, PaletteRun};
use crate::picker::WindowPicker;
use crate::screen::{ScreenFactory, ScreenView};
use crate::terminal::TerminalView;

/// The program a "new agent" terminal runs.
pub const AGENT_COMMAND: &str = "claude";

/// How long a shell command has to run before its end, unwatched, is worth a badge: shorter
/// commands end before the human has looked away.
pub const SLOW_COMMAND: Duration = Duration::from_secs(5);

/// How long a closed tile can be taken back (⌘Z) before a shell's session is closed for good.
const UNDO_CLOSE: Duration = Duration::from_secs(5);

/// How long a remote window or display may sit off screen before its stream is let go: long
/// enough for a glance at the next column and back, short enough that a stream nobody sees
/// does not keep a worker encoding for long.
const STREAM_GRACE: Duration = Duration::from_secs(5);

/// How long the layout waits after its last change before it is written to disk.
const SAVE_AFTER: Duration = Duration::from_millis(500);

/// Things the surrounding app reacts to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WorkspaceEvent {
    /// A terminal rang its bell.
    Bell(SessionId),
    /// A coding agent in this session needs the human (permission, question, finished turn).
    Attention(SessionId),
    /// How many agents are waiting on the human right now, across every worker (the Dock
    /// badge).
    NeedsYou(usize),
}

/// Where the phone key bar sends its keys (see [`WorkspaceView::active_key_target`]).
#[derive(Clone, Debug)]
pub enum KeyTarget {
    /// A terminal: keys go through the grid and the predictor.
    Terminal(Entity<TerminalView>),
    /// A remote window: keys are injected on the worker.
    Screen(Entity<ScreenView>),
}

/// Where a worker's link stands.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum WorkerStatus {
    /// First attempt in progress.
    Connecting,
    /// Link up.
    Connected,
    /// Link up but nothing heard for this many seconds (keep-alives come every few).
    Silent(u64),
    /// Link lost or the attempt failed; retrying, with the reason.
    Reconnecting(String),
    /// The server says the worker went quiet; dialled again when it is back online.
    Unreachable,
    /// The server has not heard from the worker for long enough to presume it gone.
    Gone,
}

impl WorkerStatus {
    /// Short text for the bar and the menus.
    #[must_use]
    pub fn text(&self) -> String {
        match self {
            Self::Connecting => "connecting…".to_owned(),
            Self::Connected => "connected".to_owned(),
            Self::Silent(secs) => format!("silent {secs} s"),
            Self::Reconnecting(why) => format!("{why}; reconnecting…"),
            Self::Unreachable => "unreachable".to_owned(),
            Self::Gone => "gone".to_owned(),
        }
    }

    /// Whether the worker is reachable right now.
    #[must_use]
    pub const fn is_up(&self) -> bool {
        matches!(self, Self::Connected)
    }
}

/// A live connection to one worker: who this client is there and where its messages go.
#[derive(Clone)]
pub struct WorkerLink {
    /// This client's id on that worker's wire.
    pub me: ClientId,
    /// The control stream.
    pub out: mpsc::Sender<ClientMsg>,
    /// Opens a stream view for an `Opened` screen.
    pub open_screen: ScreenFactory,
    /// Files and clipboard bytes to and from the worker; `None` in a test without them.
    pub remote: Option<std::sync::Arc<dyn slopty_client::remote::Remote>>,
}

impl std::fmt::Debug for WorkerLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerLink").field("me", &self.me).finish_non_exhaustive()
    }
}

/// One worker as the workspace keeps it: its registry, its sessions and its link, which comes
/// and goes while the registry mirror (and so the tiles) stays.
struct Worker {
    name: String,
    status: WorkerStatus,
    link: Option<WorkerLink>,
    doc: ItemDoc,
    sessions: HashMap<SessionId, SessionSummary>,
    rtt: Option<Duration>,
    /// `slopty hook install` has been offered on this worker once.
    hooks_offered: bool,
    /// The paths the worker was last asked to watch for its file cards, sorted.
    watched: Vec<String>,
    /// A `List` is in flight to name restored window items.
    titles_requested: bool,
    /// A `List` is in flight for the picker.
    picker_wanted: bool,
    /// The next listing adds its first display straight away (the self-test socket's way to
    /// put a remote display in the workspace without the picker).
    display_wanted: bool,
    /// Streams requested but not yet `Opened`, by target.
    pending_opens: HashMap<CaptureTarget, ItemId>,
    /// The first snapshot since the link came up has not been applied yet.
    awaiting_snapshot: bool,
}

impl Worker {
    fn new(name: String) -> Self {
        Self {
            name,
            status: WorkerStatus::Connecting,
            link: None,
            doc: ItemDoc::default(),
            sessions: HashMap::new(),
            rtt: None,
            hooks_offered: false,
            watched: Vec::new(),
            titles_requested: false,
            picker_wanted: false,
            display_wanted: false,
            pending_opens: HashMap::new(),
            awaiting_snapshot: false,
        }
    }

    fn send(&self, msg: ClientMsg) {
        let Some(link) = &self.link else {
            tracing::debug!(kind = msg.kind(), "worker down; not sent");
            return;
        };
        if let Err(e) = link.out.try_send(msg) {
            tracing::warn!(error = %e, "outbound queue");
        }
    }
}

/// A shell command that finished while nobody was looking: what the header badge says.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Finished {
    /// What was typed.
    pub command: String,
    /// Its exit status, when the shell said.
    pub exit: Option<u8>,
    /// How long it ran.
    pub elapsed: Duration,
}

impl Finished {
    /// The badge text: "done 3.2 s", "failed (1) 1 m 04 s" (the row caption's clock).
    #[must_use]
    pub fn label(&self) -> String {
        let took = crate::terminal::took_label(self.elapsed);
        match self.exit {
            Some(0) | None => format!("done {took}"),
            Some(code) => format!("failed ({code}) {took}"),
        }
    }
}

/// What a menu row does when clicked.
pub type MenuRun = Rc<dyn Fn(&mut Window, &mut App)>;

/// An entry of the titlebar's "…" menu the app adds (settings, workers).
#[derive(Clone)]
pub struct MenuEntry {
    /// What the row says.
    pub label: SharedString,
    /// Muted text on the right ("connected", "⌘,").
    pub detail: SharedString,
    /// What a click does.
    pub run: MenuRun,
}

impl std::fmt::Debug for MenuEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MenuEntry").field("label", &self.label).finish_non_exhaustive()
    }
}

/// The name field open in a tile's header.
struct Rename {
    tile: TileRef,
    input: Entity<gpui_kit::component::input::InputState>,
    /// Who had the keyboard before the field: it goes back there after ↩ or Esc.
    return_to: Option<FocusHandle>,
    _subscription: gpui::Subscription,
}

/// A tile taken off, until [`UNDO_CLOSE`] passes or ⌘Z puts it back where it was. A live
/// shell's session runs on through the wait, so its rows come back untouched.
struct ClosedTile {
    tile: TileRef,
    item: Item,
    /// Where it was in the layout, to put it back there.
    at: Option<slopty_client::layout::Pos>,
    /// The session kept alive for the wait, for a live shell.
    session: Option<SessionId>,
    seq: u64,
}

/// The workspace.
pub struct WorkspaceView {
    /// The theme in use: the settings' with ⌘=/⌘- applied to the terminal text.
    theme: Theme,
    /// The settings' theme, as the app last set it.
    base_theme: Theme,
    /// Points ⌘=/⌘- moved the terminal text by.
    font_delta: f32,
    layout: Layout,
    /// The layout's clock starts here.
    epoch: Instant,
    /// Whether moves animate. Off under the self-test, where a frame is a step.
    animate: bool,
    workers: BTreeMap<WorkerKey, Worker>,
    // Per-item views. Item and session ids are UUIDv7, unique across workers, so one map
    // each serves every worker.
    terminals: HashMap<SessionId, Entity<TerminalView>>,
    screens: HashMap<ItemId, Entity<ScreenView>>,
    notes: HashMap<ItemId, Entity<NoteView>>,
    files: HashMap<ItemId, Entity<FileView>>,
    browsers: HashMap<ItemId, Entity<crate::browser::BrowserView>>,
    /// The link each browser tile's port is served by: a new link serves it anew.
    browser_links: HashMap<ItemId, std::sync::Weak<dyn slopty_client::remote::Remote>>,
    /// The app draws a dialog over the workspace: a page's native view must hide under it.
    covered: bool,
    /// Bumped every time the workspace draws, so a browser tile knows whether it was drawn.
    frames_drawn: u64,
    /// Where the toast was drawn, and in which frame: pages stop above it.
    toast_drawn: crate::browser::Drawn,
    /// The line a file card opened at, for a view not made yet.
    file_focus: HashMap<ItemId, u32>,
    /// Window titles the picker or a listing gave (the registry stores ids).
    titles: HashMap<ItemId, String>,
    /// Coding agents the workers observed, by session.
    agents: HashMap<SessionId, AgentEvent>,
    /// Coding agents the server reported, by session: they stand in for a worker whose own
    /// link is down or whose session has no tile here.
    server_agents: HashMap<SessionId, (WorkerKey, AgentEvent)>,
    /// The quiet line about the server in the titlebar ("server unreachable"), if any.
    server_status: Option<SharedString>,
    /// Long shell commands that finished unwatched, by session.
    finished: HashMap<SessionId, Finished>,
    slow_command: Duration,
    /// Terminal sessions oldest first, moved to the end when one is focused: the "run in
    /// shell" target is the most recently focused shell, else the newest.
    shell_recency: Vec<SessionId>,
    /// Holds the device out of idle sleep while an agent works.
    awake: Option<Task<()>>,
    /// Remote tiles off screen since when; their streams go after [`STREAM_GRACE`].
    unseen: HashMap<ItemId, Instant>,
    /// How long a remote tile may be off screen before its stream goes ([`STREAM_GRACE`]).
    stream_grace: Duration,
    /// Remote tiles whose streams were let go for being off screen.
    parked: std::collections::HashSet<ItemId>,
    picker: Option<(WorkerKey, Entity<WindowPicker>)>,
    palette: Option<Entity<CommandPalette>>,
    find_needle: Option<String>,
    find_hits: HashMap<ItemId, (u32, PaletteRun)>,
    pending_find: Option<(SessionId, String)>,
    pending_find_file: Option<(ItemId, String)>,
    last_find: String,
    rename: Option<Rename>,
    rename_return: Option<FocusHandle>,
    pending_focus_rename: bool,
    palette_return: Option<FocusHandle>,
    palette_action: Option<Box<dyn gpui::Action>>,
    palette_extra: Vec<PaletteItem>,
    pending_focus_palette: bool,
    /// Which titlebar menu is open.
    menu: Option<titlebar::MenuKind>,
    /// The app's rows in the "…" menu.
    more_entries: Vec<MenuEntry>,
    show_stats: bool,
    toast: Option<toast::Toast>,
    closed: Vec<ClosedTile>,
    closed_seq: u64,
    /// The pointer or finger in progress over the strip.
    drag: Option<strip::Drag>,
    /// A trackpad or touch gesture in progress over the strip.
    gesture: strip::Gesture,
    /// Where each drawn tile was last frame, in window coordinates.
    placed: Vec<(TileRef, Bounds<Pixels>)>,
    /// The strip's bounds in the window, as of the last frame.
    viewport: Bounds<Pixels>,
    /// The tile focused when the tiles were last drawn.
    drawn_focus: Option<TileRef>,
    /// The zoom the tiles were last drawn at (the overview's).
    drawn_zoom: f32,
    /// A timer is out to park the streams of remote tiles off screen.
    park_pending: bool,
    /// A terminal to focus on the next frame.
    pending_focus: Option<SessionId>,
    pending_focus_note: Option<ItemId>,
    /// A file tile whose editor takes the keyboard on the next frame.
    pending_focus_file: Option<ItemId>,
    pending_focus_picker: bool,
    pending_focus_self: bool,
    /// Where the layout is saved (`layout.json` in the client's data directory), if anywhere.
    layout_path: Option<std::path::PathBuf>,
    /// What was last written there.
    layout_saved: Option<Saved>,
    save_pending: bool,
    /// Workers whose empty registry was given a shell this run.
    given_shell: std::collections::HashSet<WorkerKey>,
    /// The clipboard kept in step with the workers; `None` where the platform has none here.
    clip: Option<Rc<std::cell::RefCell<crate::clipboard::ClipSync>>>,
    /// The app is frontmost.
    app_active: bool,
    /// Workers told this client wants their clipboard.
    watching: std::collections::HashSet<WorkerKey>,
    /// Uploads in flight.
    uploads: HashMap<slopty_core::XferId, remote::Upload>,
    /// Forwarded ports, by session.
    ports: HashMap<SessionId, Vec<slopty_client::tunnel::Forward>>,
    focus: FocusHandle,
    subscriptions: Vec<gpui::Subscription>,
}

impl std::fmt::Debug for WorkspaceView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceView")
            .field("workers", &self.workers.len())
            .field("tiles", &self.layout.tiles().count())
            .finish_non_exhaustive()
    }
}

impl EventEmitter<WorkspaceEvent> for WorkspaceView {}

impl Focusable for WorkspaceView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// Whether the system asks for motion to be reduced, read afresh each frame so a change takes
/// effect at once. Always false under test: an assertion about a spring must not turn on the
/// machine's setting.
fn reduced_motion() -> bool {
    !cfg!(test) && slopty_platform::reduce_motion()
}

impl WorkspaceView {
    /// An empty workspace, arranged as `saved` left it (the layout of the last run on this
    /// device), its tiles waiting for their workers.
    pub fn new(theme: Theme, saved: Option<Saved>, cx: &Context<Self>) -> Self {
        let layout = match saved.clone() {
            Some(saved) => Layout::restore(saved, LayoutConfig::default()),
            None => Layout::new(LayoutConfig::default()),
        };
        Self {
            base_theme: theme.clone(),
            font_delta: 0.0,
            theme,
            layout,
            epoch: Instant::now(),
            animate: true,
            workers: BTreeMap::new(),
            terminals: HashMap::new(),
            screens: HashMap::new(),
            notes: HashMap::new(),
            files: HashMap::new(),
            browsers: HashMap::new(),
            browser_links: HashMap::new(),
            covered: false,
            frames_drawn: 0,
            toast_drawn: Rc::default(),
            file_focus: HashMap::new(),
            titles: HashMap::new(),
            agents: HashMap::new(),
            server_agents: HashMap::new(),
            server_status: None,
            finished: HashMap::new(),
            slow_command: SLOW_COMMAND,
            shell_recency: Vec::new(),
            awake: None,
            unseen: HashMap::new(),
            stream_grace: STREAM_GRACE,
            parked: std::collections::HashSet::new(),
            picker: None,
            palette: None,
            find_needle: None,
            find_hits: HashMap::new(),
            pending_find: None,
            pending_find_file: None,
            last_find: String::new(),
            rename: None,
            rename_return: None,
            pending_focus_rename: false,
            palette_return: None,
            palette_action: None,
            palette_extra: Vec::new(),
            pending_focus_palette: false,
            menu: None,
            more_entries: Vec::new(),
            show_stats: false,
            toast: None,
            closed: Vec::new(),
            closed_seq: 0,
            drag: None,
            gesture: strip::Gesture::default(),
            placed: Vec::new(),
            viewport: Bounds::default(),
            drawn_focus: None,
            drawn_zoom: 1.0,
            park_pending: false,
            pending_focus: None,
            pending_focus_note: None,
            pending_focus_file: None,
            pending_focus_picker: false,
            pending_focus_self: false,
            layout_path: None,
            layout_saved: saved,
            save_pending: false,
            given_shell: std::collections::HashSet::new(),
            clip: None,
            app_active: true,
            watching: std::collections::HashSet::new(),
            uploads: HashMap::new(),
            ports: HashMap::new(),
            focus: cx.focus_handle(),
            subscriptions: Vec::new(),
        }
    }

    // ----- reading ---------------------------------------------------------------------------

    /// The layout (read only; tests and the self-test dump).
    #[must_use]
    pub const fn layout(&self) -> &Layout {
        &self.layout
    }

    /// The focused tile.
    #[must_use]
    pub fn focused(&self) -> Option<TileRef> {
        self.layout.focused()
    }

    /// The focused tile's item id.
    #[must_use]
    pub fn active_item(&self) -> Option<ItemId> {
        self.focused().map(|t| t.item)
    }

    /// An item, whichever worker holds it.
    #[must_use]
    pub fn item(&self, tile: TileRef) -> Option<&Item> {
        self.workers.get(&tile.worker)?.doc.get(tile.item)
    }

    /// Every item of every worker, with its worker, in worker then creation order.
    pub fn items(&self) -> impl Iterator<Item = (WorkerKey, &Item)> {
        self.workers.iter().flat_map(|(key, w)| w.doc.items().map(move |i| (*key, i)))
    }

    /// The tile showing `item`, whichever worker holds it.
    #[must_use]
    pub fn tile_of(&self, item: ItemId) -> Option<TileRef> {
        self.workers
            .iter()
            .find(|(_, w)| w.doc.get(item).is_some())
            .map(|(key, _)| TileRef { worker: *key, item })
    }

    /// The tile showing `session`.
    #[must_use]
    pub fn tile_of_session(&self, session: SessionId) -> Option<TileRef> {
        self.workers.iter().find_map(|(key, w)| {
            w.doc.item_for_session(session).map(|i| TileRef { worker: *key, item: i.id })
        })
    }

    /// The worker running `session`.
    #[must_use]
    pub fn worker_of_session(&self, session: SessionId) -> Option<WorkerKey> {
        self.workers
            .iter()
            .find(|(_, w)| {
                w.sessions.contains_key(&session) || w.doc.item_for_session(session).is_some()
            })
            .map(|(key, _)| *key)
    }

    /// Where a tile was drawn last frame, in window points.
    #[must_use]
    pub fn tile_bounds(&self, tile: TileRef) -> Option<Bounds<Pixels>> {
        self.placed.iter().find(|(t, _)| *t == tile).map(|(_, b)| *b)
    }

    /// Number of items across every worker.
    #[must_use]
    pub fn len(&self) -> usize {
        self.workers.values().map(|w| w.doc.len()).sum()
    }

    /// True when no worker has anything.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The terminal view for `session`, if it has one.
    #[must_use]
    pub fn terminal(&self, session: SessionId) -> Option<&Entity<TerminalView>> {
        self.terminals.get(&session)
    }

    /// The stream view of one item, if it has one open.
    #[must_use]
    pub fn screen(&self, item: ItemId) -> Option<&Entity<ScreenView>> {
        self.screens.get(&item)
    }

    /// The file cards, for tests and the self-test dump.
    #[must_use]
    pub fn file(&self, id: ItemId) -> Option<&Entity<FileView>> {
        self.files.get(&id)
    }

    /// What the worker last said about a session's agent, else what the server relayed.
    #[must_use]
    pub fn agent(&self, session: SessionId) -> Option<&AgentEvent> {
        self.agent_state(session)
    }

    /// This client's id on `worker`'s wire, while linked.
    #[must_use]
    pub fn me(&self, worker: WorkerKey) -> Option<ClientId> {
        self.workers.get(&worker)?.link.as_ref().map(|l| l.me)
    }

    /// Whether `slopty hook install` has been offered on `worker`.
    #[must_use]
    pub fn hooks_offered(&self, worker: WorkerKey) -> bool {
        self.workers.get(&worker).is_some_and(|w| w.hooks_offered)
    }

    /// The workers, their names and states, in key order.
    pub fn workers(&self) -> impl Iterator<Item = (WorkerKey, &str, &WorkerStatus)> {
        self.workers.iter().map(|(key, w)| (*key, w.name.as_str(), &w.status))
    }

    /// Link RTT of one worker, sampled once a second while connected.
    #[must_use]
    pub fn rtt(&self, worker: WorkerKey) -> Option<Duration> {
        self.workers.get(&worker)?.rtt
    }

    /// Whether the palette is up.
    #[must_use]
    pub const fn palette_open(&self) -> bool {
        self.palette.is_some()
    }

    /// Whether `session` is a terminal here (which worker a banner belongs to).
    #[must_use]
    pub fn has_session(&self, session: SessionId) -> bool {
        self.terminals.contains_key(&session)
    }

    /// The badge a session's last long command left, if the tile has not been looked at since.
    #[must_use]
    pub fn finished(&self, session: SessionId) -> Option<&Finished> {
        self.finished.get(&session)
    }

    /// How long a command has to run before its unwatched end is badged.
    pub const fn set_slow_command(&mut self, after: Duration) {
        self.slow_command = after;
    }

    /// Whether moves animate. The self-test turns this off so a dump right after an action
    /// sees where things landed, not where they were passing through.
    pub fn set_animation(&mut self, on: bool) {
        self.animate = on;
        self.layout.set_animate(on && !reduced_motion());
    }

    /// Lines the app adds to the palette after the workspace's own (settings, workers).
    pub fn extend_palette(&mut self, items: Vec<PaletteItem>) {
        self.palette_extra = items;
    }

    /// The app's rows in the titlebar's "…" menu.
    pub fn set_more_menu(&mut self, entries: Vec<MenuEntry>, cx: &mut Context<Self>) {
        self.more_entries = entries;
        cx.notify();
    }

    /// Save the layout to `path` whenever it changes (debounced).
    pub fn set_layout_path(&mut self, path: std::path::PathBuf) {
        self.layout_path = Some(path);
    }

    // ----- plumbing --------------------------------------------------------------------------

    /// The worker a new session, note or lookup goes to: the focused tile's, else the first
    /// one that is up, else the first known.
    fn context_worker(&self) -> Option<WorkerKey> {
        self.focused()
            .map(|t| t.worker)
            .filter(|w| self.workers.get(w).is_some_and(|w| w.link.is_some()))
            .or_else(|| self.workers.iter().find(|(_, w)| w.link.is_some()).map(|(k, _)| *k))
            .or_else(|| self.workers.keys().next().copied())
    }

    fn send(&self, worker: WorkerKey, msg: ClientMsg) {
        if let Some(w) = self.workers.get(&worker) {
            w.send(msg);
        }
    }

    /// Send to the worker running `session`.
    fn send_session(&self, session: SessionId, msg: ClientMsg) {
        if let Some(worker) = self.worker_of_session(session) {
            self.send(worker, msg);
        }
    }

    /// Apply an item op here at once and propose it to its worker.
    fn propose(&mut self, worker: WorkerKey, op: ItemOp, cx: &mut Context<Self>) {
        let Some(w) = self.workers.get_mut(&worker) else { return };
        let change = w.doc.apply_op(&op, true);
        w.send(ClientMsg::Items(op));
        self.item_changed(worker, change, cx);
    }

    /// The layout's clock, advanced to now.
    fn tick(&mut self) {
        self.layout.set_clock(self.epoch.elapsed());
    }

    /// Something in the layout may have changed: save it once things settle.
    fn layout_touched(&mut self, cx: &Context<Self>) {
        if self.layout_path.is_none() || self.save_pending {
            return;
        }
        self.save_pending = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_AFTER).await;
            let write = this
                .update(cx, |this, _cx| {
                    this.save_pending = false;
                    let saved = this.layout.save();
                    if this.layout_saved.as_ref() == Some(&saved) {
                        return None;
                    }
                    this.layout_saved = Some(saved.clone());
                    this.layout_path.clone().map(|path| (path, saved))
                })
                .ok()
                .flatten();
            if let Some((path, saved)) = write {
                cx.background_executor().spawn(async move { write_layout(&path, &saved) }).await;
            }
        })
        .detach();
    }

    /// Swap the theme everywhere: every terminal (which re-fits its grid to the new font on
    /// its next frame), every window's chrome, every note, file card and the picker.
    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        self.base_theme = theme;
        self.apply_font(cx);
    }

    /// The settings' theme with the terminal text moved by ⌘=/⌘-.
    fn apply_font(&mut self, cx: &mut Context<Self>) {
        let mut theme = self.base_theme.clone();
        let size = theme.typography.mono_size + self.font_delta;
        theme.typography.mono_size = size.clamp(commands::FONT_MIN, commands::FONT_MAX);
        self.apply_theme(theme, cx);
    }

    fn apply_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        if self.theme == theme {
            return;
        }
        for view in self.terminals.values() {
            view.update(cx, |v, cx| v.set_theme(theme.clone(), cx));
        }
        for view in self.screens.values() {
            view.update(cx, |v, cx| v.set_theme(theme.clone(), cx));
        }
        for view in self.notes.values() {
            view.update(cx, |v, cx| v.set_theme(theme.clone(), cx));
        }
        for view in self.files.values() {
            view.update(cx, |v, cx| v.set_theme(theme.clone(), cx));
        }
        for view in self.browsers.values() {
            view.update(cx, |v, cx| v.set_theme(theme.clone(), cx));
        }
        if let Some((_, picker)) = &self.picker {
            picker.update(cx, |p, cx| p.set_theme(theme.clone(), cx));
        }
        if self.theme.terminal != theme.terminal {
            // Every shell hears the new colours; the worker applies the driver's.
            let colors = theme.terminal.wire();
            for session in self.terminals.keys() {
                self.send_session(
                    *session,
                    ClientMsg::Term {
                        session: *session,
                        req: slopty_proto::terminal::TermRequest::Colors(colors),
                    },
                );
            }
        }
        self.theme = theme;
        cx.notify();
    }

    /// The theme in use (terminal size included).
    #[must_use]
    pub const fn theme(&self) -> &Theme {
        &self.theme
    }
}

impl WorkspaceView {
    /// Tab from the workspace itself (nothing else focused) enters the keyboard ring; inside
    /// a terminal or a text field Tab is theirs.
    fn key_down(&mut self, ev: &gpui::KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.focus.is_focused(window) && crate::a11y::cycle(ev, window, cx) {
            cx.stop_propagation();
        }
    }

    /// After a ring step: a remote window with no tab stop around it kept the keys, so the
    /// workspace itself takes them — ⌃Tab is always the way out of a window, whose chords are
    /// all the worker's while it has the focus.
    fn leave_screen(&self, window: &mut Window, cx: &mut Context<Self>) {
        let on_screen = self
            .active_screen()
            .is_some_and(|screen| screen.read(cx).focus_handle(cx).is_focused(window));
        if on_screen {
            window.focus(&self.focus, cx);
        }
    }

    /// Focus asked for since the last frame, now that there is a window to give it in.
    fn apply_pending_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(session) = self.pending_focus.take()
            && let Some(view) = self.terminals.get(&session)
        {
            let handle = view.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        }
        if let Some((session, needle)) = self.pending_find.take()
            && let Some(view) = self.terminals.get(&session).cloned()
        {
            // After this frame: the tile is drawn and focused first, then its find bar takes
            // the keyboard.
            window.defer(cx, move |window, cx| {
                view.update(cx, |v, cx| v.find_with(&needle, window, cx));
            });
        }
        if let Some((item, needle)) = self.pending_find_file.take()
            && let Some(view) = self.files.get(&item).cloned()
        {
            window.defer(cx, move |window, cx| {
                view.update(cx, |v, cx| v.find_with(&needle, window, cx));
            });
        }
        if std::mem::take(&mut self.pending_focus_picker)
            && let Some((_, picker)) = &self.picker
        {
            let handle = picker.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        }
        if std::mem::take(&mut self.pending_focus_self) {
            window.focus(&self.focus, cx);
        }
        if std::mem::take(&mut self.pending_focus_palette)
            && let Some(palette) = &self.palette
        {
            let handle = palette.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        }
        if std::mem::take(&mut self.pending_focus_rename)
            && let Some(rename) = &self.rename
        {
            rename.input.update(cx, |input, cx| {
                input.focus(window, cx);
                input.select_all(window, cx);
            });
        }
        if self.rename.is_none()
            && let Some(handle) = self.rename_return.take()
        {
            window.focus(&handle, cx);
        }
        if self.palette.is_none()
            && let Some(handle) = self.palette_return.take()
        {
            window.focus(&handle, cx);
            if let Some(action) = self.palette_action.take() {
                // Once this frame is done, from the element that had the keyboard.
                cx.defer_in(window, move |_this, window, cx| window.dispatch_action(action, cx));
            }
        }
        if let Some(id) = self.pending_focus_note.take()
            && let Some(view) = self.notes.get(&id)
        {
            view.update(cx, |v, cx| v.focus(window, cx));
        }
        if let Some(id) = self.pending_focus_file.take()
            && let Some(view) = self.files.get(&id)
        {
            view.update(cx, |v, cx| v.focus(window, cx));
        }
    }
}

impl gpui::Render for WorkspaceView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        use gpui::prelude::FluentBuilder as _;
        use gpui::{
            InteractiveElement as _, ParentElement as _, StatefulInteractiveElement as _,
            Styled as _,
        };

        let animate = self.animate && !reduced_motion();
        if self.layout.config().animate != animate {
            self.layout.set_animate(animate);
        }
        self.reconcile_notes_and_files(window, cx);
        self.reconcile_browsers(cx);
        self.frames_drawn = self.frames_drawn.wrapping_add(1);
        cx.set_global(crate::browser::FrameCount(self.frames_drawn));
        self.apply_pending_focus(window, cx);
        self.sync_clipboard_watch();
        let titlebar = self.render_titlebar(window, cx);
        let strip = self.render_strip(window, cx);
        let toast = self.render_toast(cx);
        let menu = self.render_menu(window, cx);
        let picker = self.picker.as_ref().map(|(_, p)| p.clone());
        let palette = self.palette.clone();
        let mut key_context = gpui::KeyContext::new_with_defaults();
        key_context.add("Workspace");
        let root = gpui::div()
            .id("workspace")
            .debug_selector(|| "workspace".to_owned())
            .key_context(key_context)
            .track_focus(&self.focus)
            .role(gpui::accesskit::Role::Group)
            .aria_label("Workspace")
            .on_key_down(cx.listener(Self::key_down))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(crate::colors::hsla(self.theme.surfaces.canvas));
        let root = Self::register_layout_actions(root, cx);
        root.on_action(cx.listener(Self::new_terminal))
            .on_action(cx.listener(Self::new_agent))
            .on_action(cx.listener(Self::new_note))
            .on_action(cx.listener(Self::add_window))
            .on_action(cx.listener(Self::open_file_palette))
            .on_action(cx.listener(Self::open_url_palette))
            .on_action(cx.listener(Self::list_workers))
            .on_action(cx.listener(Self::list_ports))
            .on_action(cx.listener(Self::close_item))
            .on_action(cx.listener(Self::undo_close))
            .on_action(cx.listener(Self::next_attention))
            .on_action(cx.listener(Self::toggle_mute))
            .on_action(cx.listener(Self::toggle_stats))
            .on_action(cx.listener(Self::find_in_active))
            .on_action(cx.listener(Self::open_palette))
            .on_action(cx.listener(Self::rename_item))
            .on_action(cx.listener(Self::point_others))
            .on_action(cx.listener(Self::find_everywhere))
            // Esc in the name field: the input's own action, taken here so the field closes
            // without a change and the workspace has the keyboard.
            .capture_action(cx.listener(
                |this, _: &gpui_kit::component::input::Escape, _window, cx| {
                    if this.rename.is_some() {
                        this.finish_rename(false, true, cx);
                        cx.stop_propagation();
                    }
                },
            ))
            .on_action(cx.listener(|this, _: &FocusNext, window, cx| {
                window.focus_next(cx);
                this.leave_screen(window, cx);
            }))
            .on_action(cx.listener(|this, _: &FocusPrev, window, cx| {
                window.focus_prev(cx);
                this.leave_screen(window, cx);
            }))
            .child(titlebar)
            .child(
                gpui::div()
                    .relative()
                    .flex_1()
                    .w_full()
                    .flex()
                    .flex_col()
                    .child(strip)
                    .children(toast),
            )
            .children(menu)
            .when_some(picker, gpui::ParentElement::child)
            .when_some(palette, gpui::ParentElement::child)
            .child(Self::browser_sync(cx))
    }
}

/// Write `saved` to `path` atomically; a failure is logged, never fatal: the layout is this
/// device's convenience, and the next change tries again.
fn write_layout(path: &std::path::Path, saved: &Saved) {
    let bytes = match serde_json::to_vec_pretty(saved) {
        Ok(bytes) => bytes,
        Err(e) => {
            tracing::warn!(error = %e, "layout serialise");
            return;
        }
    };
    let tmp = path.with_extension("json.tmp");
    let written = std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, path));
    if let Err(e) = written {
        tracing::warn!(path = %path.display(), error = %e, "layout save");
    }
}

/// Read the layout a previous run saved at `path`; nothing (and a warning) when it is missing
/// or does not parse, so a bad file costs an arrangement, never the app.
#[must_use]
pub fn read_layout(path: &std::path::Path) -> Option<Saved> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "layout read");
            return None;
        }
    };
    serde_json::from_slice(&bytes)
        .map_err(|e| tracing::warn!(path = %path.display(), error = %e, "layout parse"))
        .ok()
}

#[cfg(test)]
mod tests;

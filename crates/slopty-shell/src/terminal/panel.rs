// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! The terminal panel: session-scoped tabs over engine PTYs.
//!
//! Feature-inventory §1.10: tabs are per selected chat (or the new-session
//! canvas, keyed per space) and restored on return (emulators — and their
//! server-side PTYs — survive navigation; detach is not close). Tab bar
//! supports pointer drag-reorder with 150 ms sliding transforms, middle-click
//! close, and a "+" new-tab button; Cmd/Ctrl+J toggles the panel (the shell
//! owns the height animation + persistence).
//!
//! slopty: data path per tab: `OpenTerminal` names a session of Slopty's, and the
//! body is Slopty's own terminal view of it ([`crate::live`]), which owns the
//! session's link, prediction, latency path, copy mode and scrollback. The
//! panel keeps zeron's tabs, chrome, padding, font and palette, and hands the
//! view the look ([`crate::live::TerminalLook`]).

use std::collections::HashMap;

use futures::FutureExt as _;
use futures::channel::oneshot;
use gpui::{
    App, AsyncApp, Context, Entity, FocusHandle, IntoElement, KeyBinding, MouseButton, Pixels,
    Render, SharedString, Size, Subscription, Task, WeakEntity, Window, actions, div, prelude::*,
    px,
};

use slopty_view_proto::TerminalSession;
use slopty_view_rpc::methods;

use crate::live::{self, TerminalLook};
use crate::motion::{self, AnimationExt as _, TAB_SLIDE};
use crate::popover;
use crate::settings::widgets::{ActionHint as _, Keys};
use crate::settings::{TERMINAL_MAX_VH, TERMINAL_MIN_HEIGHT};
use crate::shell::TabCloseAction;
use crate::state::{AppState, CANVAS_PANEL_PREFIX, EngineHandle};
use crate::theme::Theme;

use super::view::terminal_panel_bg;

pub const TAB_BAR_HEIGHT: f32 = 40.0;

// slopty: ⌘T opens a terminal tab where the panel has the keyboard (Terminal.app, Ghostty).
actions!(terminal, [ToggleTerminal, NewTerminalTab]);

/// slopty: the panel's key context, where its own tab keys bind.
pub const KEY_CONTEXT: &str = "TerminalPanel";

/// slopty: the session panel's contexts while it shows an agent's session, whose terminal's ⌘K
/// opens the palette (`crate::shell::apply_keymap`).
const AGENT_SESSION_CONTEXT: &str = "TerminalPanel AgentSession";

/// Bind the terminal keymap (global): Cmd+J on Apple keyboards, Ctrl+J elsewhere.
pub fn init(cx: &mut App) {
    let toggle = if crate::keys::MOD_IS_COMMAND { // slopty: an iPad's ⌘ too
        "cmd-j"
    } else {
        "ctrl-j"
    };
    cx.bind_keys([KeyBinding::new(toggle, ToggleTerminal, None)]);
    bind_keys(cx);
}

/// slopty: the panel's own keys (⌘T, a new tab), bound again with each keymap.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new(
        &crate::settings::platform_combo("mod-t"),
        NewTerminalTab,
        Some(KEY_CONTEXT),
    )]);
}

// ---------------------------------------------------------------------------
// Pure logic (unit-tested)
// ---------------------------------------------------------------------------

/// Panel height clamp: 160 px … 55 % of the viewport (§1.10).
pub fn clamp_terminal_height(height: f32, viewport_h: f32) -> f32 {
    let max = (viewport_h * TERMINAL_MAX_VH).max(TERMINAL_MIN_HEIGHT);
    if height.is_finite() {
        height.clamp(TERMINAL_MIN_HEIGHT, max)
    } else {
        TERMINAL_MIN_HEIGHT
    }
}

/// Move a tab from `from` to `to` (indices into the same vec).
pub fn reorder_tabs<T>(tabs: &mut Vec<T>, from: usize, to: usize) {
    if from >= tabs.len() || to >= tabs.len() || from == to {
        return;
    }
    let tab = tabs.remove(from);
    tabs.insert(to, tab);
}

/// Where a drag hovering at `rel_x` inside the tab strip would land.
pub fn drop_index(rel_x: f32, tab_w: f32, count: usize) -> usize {
    if count == 0 || tab_w <= 0.0 {
        return 0;
    }
    ((rel_x / tab_w).floor().max(0.0) as usize).min(count - 1)
}

/// Sliding transform (in tab-width units) for tab `ix` while `from` is dragged
/// over `over`: tabs between the two shift one slot toward the vacated gap.
pub fn slide_offset(ix: usize, from: usize, over: usize) -> f32 {
    if from < over && ix > from && ix <= over {
        -1.0
    } else if over < from && ix >= over && ix < from {
        1.0
    } else {
        0.0
    }
}

/// Active index after a reorder commit.
pub fn active_after_reorder(active: usize, from: usize, to: usize) -> usize {
    if active == from {
        to
    } else if from < active && to >= active {
        active - 1
    } else if from > active && to <= active {
        active + 1
    } else {
        active
    }
}

/// Merge the `targetDeviceId` passthrough into RPC params (no-op for chats on
/// the connected engine's own device).
fn with_target(mut params: serde_json::Value, target: &Option<String>) -> serde_json::Value {
    if let (Some(target), Some(object)) = (target, params.as_object_mut()) {
        object.insert(
            "targetDeviceId".into(),
            serde_json::Value::String(target.clone()),
        );
    }
    params
}

/// Active index after closing `closed` (given the new, shorter length).
pub fn active_after_close(active: usize, closed: usize, len_after: usize) -> usize {
    let shifted = if closed < active { active - 1 } else { active };
    if len_after == 0 {
        0
    } else {
        shifted.min(len_after - 1)
    }
}

/// Tab title from the session's shell path ("/bin/zsh" → "zsh").
pub fn shell_title(shell: &str) -> String {
    let name = shell.rsplit(['/', '\\']).next().unwrap_or(shell).trim();
    if name.is_empty() {
        "terminal".to_string()
    } else {
        name.to_string()
    }
}

// ---------------------------------------------------------------------------
// Entity
// ---------------------------------------------------------------------------

struct TerminalTab {
    key: u64,
    title: SharedString,
    terminal_id: Option<String>,
    target_device_id: Option<String>,
    // slopty: what Slopty's view says of itself: its OSC title, why it failed to open.
    shown_title: Option<SharedString>,
    failure: Option<SharedString>,
    exited: Option<i32>,
    /// slopty: how far the program says it has got (`OSC 9;4`), drawn on the tab.
    progress: Option<live::Progress>,
    /// slopty: hears the view's title and end.
    _watch: Option<Subscription>,
    /// The open; dropping it cancels it.
    _run: Option<Task<()>>,
}

#[derive(Default)]
struct ChatTabs {
    tabs: Vec<TerminalTab>,
    active: usize,
}

/// Drag-reorder state; `epoch` keys the 150 ms slide animation restarts.
struct DragState {
    from: usize,
    over: usize,
    epoch: usize,
    prev_over: usize,
}

/// The dragged-tab payload (gpui drag-and-drop).
struct TabDragPayload {
    chat: String,
    from: usize,
    title: SharedString,
}

struct TabGhost {
    title: SharedString,
    width: f32,
}

impl Render for TabGhost {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::surface_chrome::tab_ghost(self.title.clone(), self.width, Theme::of(cx))
    }
}

pub struct TerminalPanel {
    state: Entity<AppState>,
    focus_handle: FocusHandle,
    focus_pending: bool,
    /// slopty: the terminal view the body drew last, asked whether the person has words in it
    /// unsent ([`Self::drafting`]).
    drawn: Option<gpui::AnyView>,
    chats: HashMap<String, ChatTabs>,
    /// Shell-driven visibility gate: no RPC happens while closed (lazy).
    open: bool,
    /// Right-pane surface host mode: the SHELL owns the tab strip (surface
    /// tabs), so the internal bar hides, tabs are only ever created
    /// explicitly (no ensure-on-open/chat-switch), and closing the last tab
    /// must not dispatch the bottom drawer's [`ToggleTerminal`].
    embedded: bool,
    /// slopty: the main column's panel: each chat's one terminal, the session itself (its
    /// agent's TUI, or a plain shell), opened as the chat is shown (`docs/decisions/ui.md`,
    /// "A session is its terminal").
    session: bool,
    /// slopty: in the session panel, the terminal each chat's tab was opened on: a chat whose
    /// session moved to another terminal (`/clear`, `/resume`) gets a tab of that one.
    session_asked: HashMap<String, String>,
    /// The right pane is in its width tween. Keep painting the retained grid
    /// through the changing clip, but do not feed transient widths into the
    /// emulator: alternate-screen rows truncate rather than reflow.
    resize_suspended: bool,
    /// Whether this docked panel's bottom-left/right corners sit at the
    /// WINDOW's corners — the shell sets these per frame so the panel's fill
    /// can carry the CSD window's rounded corners (gpui cannot clip children
    /// rounded; each full-bleed layer rounds itself).
    window_corner_bl: bool,
    window_corner_br: bool,
    tab_seq: u64,
    drag: Option<DragState>,
    last_selected: Option<String>,
    /// slopty: a tab's right-click menu.
    tab_menu: popover::Popup<TabMenu>,
    /// slopty: the grid a terminal view draws in the body, as last drawn, and the size and look
    /// it was measured for.
    fitted: Option<(Size<Pixels>, TerminalLook, (u16, u16))>,
    /// slopty: sessions waiting for the body's first draw to open at its grid.
    awaiting_fit: Vec<oneshot::Sender<(u16, u16)>>,
    _observe: Subscription,
}

/// slopty: what an open asks for: a fresh shell, a terminal shown when the app last ran, or
/// the terminal a session is (`AppState::session_terminal`).
enum Open {
    Fresh,
    Again(String),
    Session(String),
}

/// slopty: the grid a session opens at when the panel's body is not drawn: the view fits it
/// once it is.
const UNFITTED: (u16, u16) = (80, 24);

/// slopty: how long an open waits for the body's first draw before it opens at [`UNFITTED`].
const FIT_WAIT: std::time::Duration = std::time::Duration::from_millis(500);

/// slopty: a terminal tab's right-click menu: which tab, where.
#[derive(Clone)]
struct TabMenu {
    chat: String,
    key: u64,
    position: gpui::Point<gpui::Pixels>,
}

impl TerminalPanel {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&state, |this: &mut Self, _, cx| this.on_state_changed(cx));
        Self {
            state,
            focus_handle: cx.focus_handle(),
            focus_pending: false,
            drawn: None,
            chats: HashMap::new(),
            open: false,
            embedded: false,
            session: false,
            session_asked: HashMap::new(),
            resize_suspended: false,
            window_corner_bl: false,
            window_corner_br: false,
            tab_seq: 0,
            drag: None,
            last_selected: None,
            tab_menu: popover::Popup::default(),
            fitted: None,
            awaiting_fit: Vec::new(),
            _observe: observe,
        }
    }

    /// slopty: the grid a session opened in `panel` starts at: what a terminal view draws in
    /// its body, waiting for the body's first draw, so the shell never starts wider than it is
    /// shown. One told wider pads its first prompt past the grid's edge, and zsh's partial-line
    /// mark is left on a line of its own. A body not drawn within [`FIT_WAIT`] (a panel not
    /// shown) gives [`UNFITTED`].
    pub(crate) async fn grid(panel: &WeakEntity<Self>, cx: &mut AsyncApp) -> (u16, u16) {
        let waiting = panel.update(cx, |panel, _| match &panel.fitted {
            Some((.., grid)) => Ok(*grid),
            None => {
                let (tx, rx) = oneshot::channel();
                panel.awaiting_fit.push(tx);
                Err(rx)
            }
        });
        let rx = match waiting {
            Ok(Ok(grid)) => return grid,
            Ok(Err(rx)) => rx,
            Err(_) => return UNFITTED,
        };
        let timer = cx.background_executor().timer(FIT_WAIT);
        futures::select_biased! {
            grid = rx.fuse() => grid.unwrap_or(UNFITTED),
            () = timer.fuse() => UNFITTED,
        }
    }

    /// slopty: the body was laid out at `size`: the grid a view draws there, for the sessions
    /// opened here. A width the right pane passes through in its tween is not one.
    fn measured(&mut self, size: Size<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        if self.resize_suspended {
            return;
        }
        let look = TerminalLook::of(cx);
        let known = self.fitted.as_ref().is_some_and(|(at, was, _)| *at == size && *was == look);
        if known && self.awaiting_fit.is_empty() {
            return;
        }
        // With no host to fit by, there is no grid to wait for.
        let grid = live::fit(size, window, cx).unwrap_or(UNFITTED);
        self.fitted = Some((size, look, grid));
        for waiting in self.awaiting_fit.drain(..) {
            let _gone = waiting.send(grid);
        }
    }

    /// A panel in right-pane surface-host mode (see the `embedded` field).
    pub fn new_embedded(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let mut panel = Self::new(state, cx);
        panel.embedded = true;
        panel
    }

    /// slopty: the main column's panel (see the `session` field): no strip, always open, and
    /// each chat shown gets its session's terminal.
    pub fn new_session(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let mut panel = Self::new_embedded(state, cx);
        panel.session = true;
        panel.open = true;
        panel
    }

    /// slopty: the session terminal the selected chat shows, once it is attached.
    pub fn session_terminal_id(&self, cx: &App) -> Option<&str> {
        self.active_tab(cx)?.terminal_id.as_deref()
    }

    pub fn focus_handle(&self) -> FocusHandle {
        self.focus_handle.clone()
    }

    /// slopty: whether the terminal shown holds words the person typed and has not sent.
    pub fn drafting(&self, cx: &App) -> bool {
        self.drawn.as_ref().is_some_and(|view| live::drafting(view, cx))
    }

    /// Claim focus once the terminal body mounts, after opening or selecting it.
    pub fn request_focus(&mut self, cx: &mut Context<Self>) {
        self.focus_pending = true;
        cx.notify();
    }

    pub fn set_resize_suspended(&mut self, suspended: bool) {
        self.resize_suspended = suspended;
    }

    /// Shell hook: whether the panel's bottom corners sit at the window's
    /// corners (Linux CSD floating window). Notifies only on change so the
    /// per-frame shell call stays cheap.
    pub fn set_window_corners(&mut self, bl: bool, br: bool, cx: &mut Context<Self>) {
        if self.window_corner_bl != bl || self.window_corner_br != br {
            self.window_corner_bl = bl;
            self.window_corner_br = br;
            cx.notify();
        }
    }

    /// Shell toggle hook. Opening lazily creates the first tab for the
    /// selected chat or new-session canvas (drawer mode; embedded tabs are
    /// explicit); closing keeps every session alive (detach ≠ close).
    pub fn set_open(&mut self, open: bool, cx: &mut Context<Self>) {
        self.open = open;
        if !open {
            self.focus_pending = false;
        }
        if open && !self.embedded {
            self.ensure_tab(cx);
        }
        cx.notify();
    }

    /// Whether the drawer is currently shown (the shell's hide-before-navigate
    /// checks this to avoid redundant updates).
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// A tab's display label: the live OSC 0/2 title when the running
    /// program set one (shells title themselves with the cwd / running
    /// command — the contextual name, user request), else the fixed
    /// "Terminal N".
    fn display_title(tab: &TerminalTab) -> SharedString {
        match tab.shown_title.as_ref().map(|title| title.trim()) {
            Some(title) if !title.is_empty() => title.to_string().into(),
            _ => tab.title.clone(),
        }
    }

    // ---- externally managed session API. Project Actions use these helpers
    // ---- in the bottom drawer; the right-pane host also uses the keyed tab
    // ---- operations because its surface strip lives in Shell.

    /// `(key, title, exited)` for the selected chat's tabs, in tab order.
    pub fn tab_summaries(&self, cx: &App) -> Vec<(u64, SharedString, bool)> {
        let chat = self.selected_chat(cx);
        self.chats
            .get(&chat)
            .map(|tabs| {
                tabs.tabs
                    .iter()
                    .map(|t| (t.key, Self::display_title(t), t.exited.is_some()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Open a fresh tab for the selected chat and return its key.
    pub fn open_tab_for_selected(&mut self, cx: &mut Context<Self>) -> Option<u64> {
        let chat = self.selected_chat(cx);
        self.open_tab(chat, cx);
        self.request_focus(cx);
        Some(self.tab_seq)
    }

    /// Create a named placeholder tab without opening a PTY. Project Actions
    /// use this before their host-side run RPC completes.
    pub fn reserve_tab_for_chat(
        &mut self,
        chat: String,
        title: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) -> u64 {
        self.tab_seq += 1;
        let key = self.tab_seq;
        let entry = self.chats.entry(chat).or_default();
        entry.tabs.push(TerminalTab {
            key,
            title: title.into(),
            terminal_id: None,
            target_device_id: None,
            shown_title: None,
            failure: None,
            exited: None,
            progress: None,
            _watch: None,
            _run: None,
        });
        entry.active = entry.tabs.len() - 1;
        cx.notify();
        key
    }

    /// Attach and stream a PTY that was already opened by the owning engine.
    pub fn attach_reserved_session(
        &mut self,
        chat: &str,
        key: u64,
        session: TerminalSession,
        target_device_id: Option<String>,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.tab_mut(chat, key).is_none() {
            return false;
        }
        let Some(engine) = self.engine(cx) else {
            return false;
        };
        let run = Self::spawn_session(
            chat.to_string(),
            key,
            engine,
            target_device_id,
            Some(session),
            None,
            Open::Fresh,
            cx,
        );
        if let Some(tab) = self.tab_mut(chat, key) {
            tab._run = Some(run);
            true
        } else {
            false
        }
    }

    /// Turn a placeholder into a visible failed tab without opening a PTY.
    pub fn fail_reserved_tab(
        &mut self,
        chat: &str,
        key: u64,
        message: &str,
        cx: &mut Context<Self>,
    ) {
        if let Some(tab) = self.tab_mut(chat, key) {
            tab.failure = Some(format!("failed to run action: {message}").into());
            tab.exited = Some(-1);
            cx.notify();
        }
    }

    /// Make `key` the rendered tab of the selected chat.
    pub fn select_tab_by_key(&mut self, key: u64, cx: &mut Context<Self>) {
        let chat = self.selected_chat(cx);
        let Some(ix) = self
            .chats
            .get(&chat)
            .and_then(|tabs| tabs.tabs.iter().position(|t| t.key == key))
        else {
            return;
        };
        self.select_tab(&chat, ix, cx);
    }

    /// Close the selected chat's tab `key` (surface-tab ✕).
    pub fn close_tab_by_key(&mut self, key: u64, window: &mut Window, cx: &mut Context<Self>) {
        let chat = self.selected_chat(cx);
        self.close_tab(&chat, key, window, cx);
    }

    fn on_state_changed(&mut self, cx: &mut Context<Self>) {
        let selected_key = self.selected_chat(cx);
        let prev = self.last_selected.clone();
        let switched = Some(selected_key.clone()) != prev;
        if switched {
            self.last_selected = Some(selected_key.clone());
            self.drag = None;
        }
        if self.session {
            self.sync_session_tab(&selected_key, cx);
        } else if self.open && !self.embedded {
            // Returning to a chat with tabs restores them; a fresh chat (or an
            // engine that only just finished booting) gets its first tab —
            // ensure_tab is idempotent, so calling on every state change is safe.
            // Embedded: surface tabs are explicit — a chat switch just shows
            // that chat's own tabs (or the shell's surface picker).
            // Entering the canvas from a chat never auto-creates: the shell
            // hides the drawer on canvas entry, so creating here would open a
            // PTY just to hide it (observer order vs. the shell is not
            // guaranteed).
            let entering_canvas = switched
                && selected_key.starts_with(CANVAS_PANEL_PREFIX)
                && prev
                    .as_ref()
                    .is_some_and(|prev| !prev.starts_with(CANVAS_PANEL_PREFIX));
            if !entering_canvas {
                self.ensure_tab(cx);
            }
        }
        if switched {
            cx.notify();
        }
    }

    fn engine(&self, cx: &App) -> Option<EngineHandle> {
        self.state.read(cx).engine().cloned()
    }

    /// The chat's host device when it differs from the connected engine's own —
    /// the PTY lives on the chat's device (feature-inventory §2.1 "terminals
    /// live on the chat's host device"), so every terminal RPC for a remote
    /// chat needs the `targetDeviceId` passthrough. Without it the local
    /// engine checks the chat's cwd against its OWN filesystem and fails with
    /// "Session working directory is unavailable" (user report).
    fn chat_target(&self, chat: &str, cx: &App) -> Option<String> {
        self.state.read(cx).terminal_target_device(chat)
    }

    fn selected_chat(&self, cx: &App) -> String {
        self.state.read(cx).panel_session_key()
    }

    /// slopty: the session panel's chat `chat` shows the terminal its session is now, once it
    /// has one (an ended session has none, and its tab stays as it was until the shell stops
    /// showing it).
    fn sync_session_tab(&mut self, chat: &str, cx: &mut Context<Self>) {
        let Some(terminal) = self.state.read(cx).session_terminals.get(chat).cloned() else {
            return;
        };
        if self.session_asked.get(chat) == Some(&terminal) {
            return;
        }
        let Some(engine) = self.engine(cx) else { return };
        // The tab of the terminal it was is let go here; that terminal runs on.
        self.chats.remove(chat);
        self.session_asked.insert(chat.to_owned(), terminal.clone());
        let key = self.reserve_tab_for_chat(chat.to_owned(), "Session", cx);
        let target = self.chat_target(chat, cx);
        let open = Open::Session(terminal);
        let run = Self::spawn_session(chat.to_owned(), key, engine, target, None, None, open, cx);
        if let Some(tab) = self.tab_mut(chat, key) {
            tab._run = Some(run);
        }
        cx.notify();
    }

    fn ensure_tab(&mut self, cx: &mut Context<Self>) {
        let chat = self.selected_chat(cx);
        if self.should_wait_for_canvas_project(&chat, cx) {
            return;
        }
        if self.chats.get(&chat).is_none_or(|c| c.tabs.is_empty()) {
            self.open_tab(chat, cx);
        }
    }

    /// A restored project id with no WatchSpaces row yet: opening now would
    /// send `~` and (before the engine resolved `space-canvas:` itself)
    /// stick a home-dir PTY on this canvas key.
    fn should_wait_for_canvas_project(&self, session_key: &str, cx: &App) -> bool {
        let state = self.state.read(cx);
        session_key.starts_with(CANVAS_PANEL_PREFIX)
            && session_key != CANVAS_PANEL_PREFIX
            && !state.spaces_synced
            && state.terminal_open_cwd_for(session_key).is_none()
    }

    fn tab_mut(&mut self, chat: &str, key: u64) -> Option<&mut TerminalTab> {
        self.chats
            .get_mut(chat)?
            .tabs
            .iter_mut()
            .find(|t| t.key == key)
    }

    fn active_tab(&self, cx: &App) -> Option<&TerminalTab> {
        let chat = self.selected_chat(cx);
        let tabs = self.chats.get(&chat)?;
        tabs.tabs.get(tabs.active)
    }

    // ---- open / stream lifecycle ----

    fn open_tab(&mut self, chat: String, cx: &mut Context<Self>) {
        let Some(engine) = self.engine(cx) else {
            return;
        };
        let tab_no = self
            .chats
            .get(&chat)
            .map_or(1, |entry| entry.tabs.len() + 1);
        let key = self.reserve_tab_for_chat(chat.clone(), format!("Terminal {tab_no}"), cx);
        let target = self.chat_target(&chat, cx);
        let cwd = self.state.read(cx).terminal_open_cwd_for(&chat);
        let run =
            Self::spawn_session(chat.clone(), key, engine, target, None, cwd, Open::Fresh, cx);
        if let Some(tab) = self.tab_mut(&chat, key) {
            tab._run = Some(run);
        }
        cx.notify();
    }

    /// slopty: a tab of the selected chat on terminal `id` again, which it showed when the app
    /// last ran. A terminal that has ended takes its tab with it.
    pub fn reopen_tab_for_selected(&mut self, id: String, cx: &mut Context<Self>) -> Option<u64> {
        let chat = self.selected_chat(cx);
        let engine = self.engine(cx)?;
        let title =
            format!("Terminal {}", self.chats.get(&chat).map_or(1, |entry| entry.tabs.len() + 1));
        let key = self.reserve_tab_for_chat(chat.clone(), title, cx);
        let target = self.chat_target(&chat, cx);
        let run =
            Self::spawn_session(chat.clone(), key, engine, target, None, None, Open::Again(id), cx);
        if let Some(tab) = self.tab_mut(&chat, key) {
            tab._run = Some(run);
        }
        Some(key)
    }

    /// slopty: the name of the machine chat `chat`'s terminals open on.
    fn connecting_to(&self, chat: &str, cx: &App) -> Option<String> {
        let state = self.state.read(cx);
        let device = self.chat_target(chat, cx).or_else(|| state.local_device_id.clone())?;
        state.device_name(&device).map(str::to_owned)
    }

    /// slopty: how far tab `key`'s program says it has got, for the right pane's chip.
    pub fn tab_progress(&self, key: u64, cx: &App) -> Option<live::Progress> {
        let chat = self.selected_chat(cx);
        self.chats.get(&chat)?.tabs.iter().find(|tab| tab.key == key)?.progress
    }

    /// slopty: the session tab `key` of chat `chat` shows, once it is attached.
    pub fn terminal_id(&self, chat: &str, key: u64) -> Option<&str> {
        let tabs = self.chats.get(chat)?;
        tabs.tabs.iter().find(|tab| tab.key == key)?.terminal_id.as_deref()
    }

    /// OpenTerminal, then show the session's view. slopty: the view streams
    /// its own session; nothing is subscribed here. `again` names a terminal
    /// the tab showed when the app last ran, which it shows again, or closes
    /// when it has ended.
    #[expect(clippy::too_many_arguments, reason = "slopty: zeron's open plus the one again")]
    fn spawn_session(
        chat: String,
        key: u64,
        engine: EngineHandle,
        target: Option<String>,
        existing_session: Option<TerminalSession>,
        cwd: Option<String>,
        open: Open,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        // slopty: a session's terminal is the agent's, never the tab's to end.
        let own = matches!(open, Open::Session(_));
        cx.spawn(async move |this, cx| {
            let session = match existing_session {
                Some(session) => session,
                None => {
                    // slopty: a session shown before keeps its grid; a new one starts at the
                    // body's.
                    let (cols, rows) = if matches!(open, Open::Fresh) {
                        Self::grid(&this, cx).await
                    } else {
                        UNFITTED
                    };
                    let mut params =
                        serde_json::json!({ "chatId": chat, "cols": cols, "rows": rows });
                    if let (Some(cwd), Some(object)) = (cwd, params.as_object_mut()) {
                        object.insert("cwd".into(), serde_json::Value::String(cwd));
                    }
                    let again = match (open, params.as_object_mut()) {
                        (Open::Again(id), Some(object)) => {
                            object.insert("terminalId".into(), serde_json::Value::String(id));
                            true
                        }
                        (Open::Session(id), Some(object)) => {
                            let chat_id = format!("{}{id}", crate::inspector::TERMINAL_CHAT);
                            object.insert("chatId".into(), serde_json::Value::String(chat_id));
                            object.insert("terminalId".into(), serde_json::Value::String(id));
                            object.insert("session".into(), serde_json::Value::Bool(true));
                            true
                        }
                        _ => false,
                    };
                    match engine
                        .client()
                        .call_as::<TerminalSession>(
                            methods::OPEN_TERMINAL,
                            with_target(params, &target),
                        )
                        .await
                    {
                        Ok(session) => session,
                        // slopty: a terminal shown before that has ended goes with its tab.
                        Err(_) if again => {
                            let _ = this.update(cx, |panel, cx| {
                                // slopty: a session's terminal is asked for again as it next moves.
                                if own {
                                    panel.session_asked.remove(&chat);
                                }
                                if let Some(tabs) = panel.chats.get_mut(&chat) {
                                    tabs.tabs.retain(|tab| tab.key != key);
                                    tabs.active = tabs.active.min(tabs.tabs.len().saturating_sub(1));
                                }
                                cx.notify();
                            });
                            return;
                        }
                        Err(err) => {
                            tracing::warn!(error = %err, "OpenTerminal failed");
                            let _ = this.update(cx, |panel, cx| {
                                if let Some(tab) = panel.tab_mut(&chat, key) {
                                    tab.failure =
                                        Some(format!("failed to open terminal: {err}").into());
                                    tab.exited = Some(-1);
                                    cx.notify();
                                }
                            });
                            return;
                        }
                    }
                }
            };
            let terminal_id = session.id.clone();
            let attached = this
                .update(cx, |panel, cx| {
                    panel.attach_view(&chat, key, terminal_id.clone(), target.clone(), cx)
                })
                .unwrap_or(false);
            if !attached && !own {
                // Tab was closed before the open completed — release the PTY.
                let _ = engine
                    .client()
                    .call(
                        methods::CLOSE_TERMINAL,
                        with_target(
                            serde_json::json!({ "terminalId": terminal_id }),
                            &target,
                        ),
                    )
                    .await;
            }
        })
    }

    /// slopty: the tab shows the session `id`'s view from now on, and hears its
    /// title and end.
    fn attach_view(
        &mut self,
        chat: &str,
        key: u64,
        id: String,
        target: Option<String>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(tab) = self.tab_mut(chat, key) else {
            return false;
        };
        tab.terminal_id = Some(id.clone());
        tab.target_device_id = target;
        self.watch_view(chat, key, &id, cx);
        self.refresh_view(chat, key, cx);
        true
    }

    /// slopty: tab `key` hears the session `id`'s view say something new of itself (its title,
    /// its end, its progress). `false` while the view is not there yet.
    fn watch_view(&mut self, chat: &str, key: u64, id: &str, cx: &mut Context<Self>) -> bool {
        let panel = cx.weak_entity();
        let heard = chat.to_owned();
        let watch = live::watch(
            id,
            Box::new(move |cx| {
                let _ = panel.update(cx, |panel, cx| panel.refresh_view(&heard, key, cx));
            }),
            cx,
        );
        let watching = watch.is_some();
        if let Some(tab) = self.tab_mut(chat, key) {
            tab._watch = watch;
        }
        watching
    }

    /// slopty: a session's view can arrive after its tab took the session (the worker opens it
    /// on its own time): a tab not yet heard from starts hearing once its view is there, and reads
    /// it once after the frame, or a progress report its program sent never reached the tab.
    fn watch_arrived_views(&mut self, chat: &str, cx: &mut Context<Self>) {
        let unheard: Vec<(u64, String)> = self
            .chats
            .get(chat)
            .map(|tabs| {
                tabs.tabs
                    .iter()
                    .filter(|tab| tab._watch.is_none())
                    .filter_map(|tab| Some((tab.key, tab.terminal_id.clone()?)))
                    .collect()
            })
            .unwrap_or_default();
        for (key, id) in unheard {
            if self.watch_view(chat, key, &id, cx) {
                let chat = chat.to_owned();
                cx.spawn(async move |panel, cx| {
                    let _ = panel.update(cx, |panel, cx| panel.refresh_view(&chat, key, cx));
                })
                .detach();
            }
        }
    }

    /// slopty: read again what the tab's view says of itself.
    fn refresh_view(&mut self, chat: &str, key: u64, cx: &mut Context<Self>) {
        let Some(id) = self.tab_mut(chat, key).and_then(|tab| tab.terminal_id.clone()) else {
            return;
        };
        let look = TerminalLook::of(cx);
        let hosted = live::terminal(&id, &look, cx);
        if let Some(tab) = self.tab_mut(chat, key) {
            let (title, ended, exit, progress) = hosted
                .map_or((None, false, None, None), |h| (h.title, h.ended, h.exit, h.progress));
            tab.shown_title = title;
            tab.progress = progress;
            // The status the program ended with, when it said one.
            if ended && tab.exited.is_none() {
                tab.exited = Some(exit.unwrap_or(0));
            }
            cx.notify();
        }
    }

    /// slopty: ⌘W in the panel closes its tab in front; the panel inside the side pane leaves
    /// ⌘W to the pane, whose tab it is.
    fn close_active_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.embedded {
            return false;
        }
        let chat = self.selected_chat(cx);
        let Some(key) = self.active_tab(cx).map(|tab| tab.key) else { return false };
        self.close_tab(&chat, key, window, cx);
        true
    }

    /// slopty: the tabs `action` closes, from tab `key` of chat `chat`, in the strip's order.
    fn tabs_to_close(&self, chat: &str, key: u64, action: TabCloseAction) -> Vec<u64> {
        let Some(tabs) = self.chats.get(chat) else { return Vec::new() };
        let Some(index) = tabs.tabs.iter().position(|tab| tab.key == key) else {
            return Vec::new();
        };
        tabs.tabs
            .iter()
            .enumerate()
            .filter(|(i, _)| action.takes(*i, index))
            .map(|(_, tab)| tab.key)
            .collect()
    }

    fn close_tab_menu(&mut self, cx: &mut Context<Self>) {
        if self.tab_menu.begin_close(crate::clock::now(cx)) {
            popover::reap_popup(cx, |panel: &mut Self| &mut panel.tab_menu);
        }
        cx.notify();
    }

    /// slopty: a tab's right-click menu, as the side pane's tabs have: close it, the others,
    /// those to its left or right. Each row says its key when it has one.
    fn render_tab_menu(&mut self, window: &Window, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let TabMenu { chat, key, position } = self.tab_menu.get().cloned()?;
        let closing = self.tab_menu.closing_at(crate::clock::now(cx));
        let theme = Theme::of(cx).for_popup();
        let close_keys = crate::keys::for_action(
            &crate::app_menus::CloseTab,
            Some(&self.focus_handle),
            window,
            cx,
        );
        let mut list = div().flex().flex_col().gap(px(popover::MENU_GAP));
        for (action, label) in [
            (TabCloseAction::This, "Close tab"),
            (TabCloseAction::Others, "Close other tabs"),
            (TabCloseAction::Left, "Close tabs to the left"),
            (TabCloseAction::Right, "Close tabs to the right"),
        ] {
            let closes = self.tabs_to_close(&chat, key, action);
            let enabled = !closes.is_empty();
            let chat = chat.clone();
            // ⌘W closes the tab in front, so only its own row says so.
            let active = self.active_tab(cx).is_some_and(|tab| tab.key == key);
            let hint = (matches!(action, TabCloseAction::This) && active)
                .then(|| close_keys.clone())
                .flatten();
            list = list.child(
                popover::menu_row(&theme, false, format!("terminal-tab-menu-{label}"))
                    .id(SharedString::from(format!("terminal-tab-menu-{label}")))
                    .role(gpui::Role::MenuItem)
                    .aria_label(label)
                    .when(!enabled, |el| {
                        el.opacity(crate::theme::DISABLED_OPACITY).cursor_default()
                    })
                    .when(enabled, |el| {
                        el.on_click(cx.listener(move |this, _, window, cx| {
                            this.close_tab_menu(cx);
                            for key in &closes {
                                this.close_tab(&chat, *key, window, cx);
                            }
                        }))
                    })
                    .child(div().flex_1().child(label))
                    .children(hint.map(|hint| popover::kbd_hint(&theme, &hint))),
            );
        }
        let card = popover::popover_card(&theme)
            .id("terminal-tab-menu")
            .role(gpui::Role::Menu)
            .aria_label("Terminal tab")
            .w(px(216.0))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_tab_menu(cx)))
            .child(list)
            .into_any_element();
        Some(popover::menu_at("terminal-tab-menu", position, card, closing))
    }

    fn select_tab(&mut self, chat: &str, ix: usize, cx: &mut Context<Self>) {
        if let Some(tabs) = self.chats.get_mut(chat)
            && ix < tabs.tabs.len()
            && tabs.active != ix
        {
            tabs.active = ix;
            cx.notify();
        }
    }

    fn close_tab(&mut self, chat: &str, key: u64, window: &mut Window, cx: &mut Context<Self>) {
        let engine = self.engine(cx);
        let Some(tabs) = self.chats.get_mut(chat) else {
            return;
        };
        let Some(ix) = tabs.tabs.iter().position(|t| t.key == key) else {
            return;
        };
        let tab = tabs.tabs.remove(ix);
        let target = tab.target_device_id.clone();
        tabs.active = active_after_close(tabs.active, ix, tabs.tabs.len());
        let now_empty = tabs.tabs.is_empty();
        self.drag = None;
        // Closing the LAST terminal closes the drawer too — an empty dock is
        // dead space (user request). Same path as the collapse chevron.
        // Embedded, the SHELL owns emptiness (it falls back to the surface
        // picker) — dispatching here would toggle the bottom drawer instead.
        if now_empty && self.open && !self.embedded {
            window.dispatch_action(Box::new(ToggleTerminal), cx);
        }
        if let (Some(engine), Some(id)) = (engine, tab.terminal_id.clone()) {
            cx.spawn(async move |_, _| {
                let _ = engine
                    .client()
                    .call(
                        methods::CLOSE_TERMINAL,
                        with_target(serde_json::json!({ "terminalId": id }), &target),
                    )
                    .await;
            })
            .detach();
        }
        cx.notify();
    }

    fn commit_reorder(&mut self, chat: &str, from: usize, to: usize, cx: &mut Context<Self>) {
        if let Some(tabs) = self.chats.get_mut(chat) {
            let active = tabs.active;
            reorder_tabs(&mut tabs.tabs, from, to);
            tabs.active = active_after_reorder(active, from, to);
        }
        self.drag = None;
        cx.notify();
    }

    fn update_drag_over(&mut self, from: usize, over: usize, cx: &mut Context<Self>) {
        match &mut self.drag {
            Some(drag) if drag.over != over => {
                drag.prev_over = drag.over;
                drag.over = over;
                drag.epoch += 1;
                cx.notify();
            }
            Some(_) => {}
            None => {
                self.drag = Some(DragState {
                    from,
                    over,
                    epoch: 0,
                    prev_over: from,
                });
                cx.notify();
            }
        }
    }

    // ---- render ----

    fn render_tab_bar(&mut self, chat: &str, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = Theme::of(cx).clone();
        let tabs = self.chats.get(chat);
        let (active, count) = tabs.map(|t| (t.active, t.tabs.len())).unwrap_or((0, 0));
        let drag = self
            .drag
            .as_ref()
            .map(|d| (d.from, d.over, d.epoch, d.prev_over));
        let chat_owned = chat.to_string();

        let tab_elements: Vec<_> = tabs
            .map(|tabs| {
                tabs.tabs
                    .iter()
                    .enumerate()
                    .map(|(ix, tab)| {
                        let selected = ix == active;
                        let key = tab.key;
                        // Contextual label (user request): the OSC title —
                        // the shell's own cwd/command name — wins over the
                        // fixed "Terminal N" fallback.
                        let title = Self::display_title(tab);
                        let exited = tab.exited.is_some();
                        (ix, key, title, selected, exited, tab.progress)
                    })
                    .collect()
            })
            .unwrap_or_default();

        // slopty: the chips fit the widest title, as the side pane's do.
        let crate::surface_chrome::TabWidths { chip: chip_w, cut } = crate::surface_chrome::tab_widths(
            tab_elements.iter().map(|(_, _, title, ..)| title.as_ref()),
            cx,
        );
        let chip_slot = chip_w + crate::surface_chrome::CONTROL_GAP;
        let (reduced, now) = (motion::reduced_motion(cx), cx.frame_time());
        let bar_chat = chat_owned.clone();
        let drop_chat = chat_owned.clone();
        // Slopty terminal-panel.tsx: `flex h-10 items-center border-b
        // border-white/[0.07] pl-2 pr-1.5` on the #090909 panel — no separate
        // bar fill.
        div()
            .id("terminal-tab-bar")
            .h(px(TAB_BAR_HEIGHT))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.0))
            .pl(px(8.0))
            .pr(px(6.0))
            .border_b_1()
            .border_color(crate::theme::hairline(0.07))
            .on_drag_move::<TabDragPayload>(cx.listener(
                move |this, event: &gpui::DragMoveEvent<TabDragPayload>, _, cx| {
                    let payload = event.drag(cx);
                    if payload.chat != bar_chat {
                        return;
                    }
                    let from = payload.from;
                    let rel_x = f32::from(event.event.position.x) - f32::from(event.bounds.left());
                    let over = drop_index(rel_x, chip_slot, count);
                    this.update_drag_over(from, over, cx);
                },
            ))
            .on_drop::<TabDragPayload>(cx.listener(move |this, payload: &TabDragPayload, _, cx| {
                if payload.chat != drop_chat {
                    this.drag = None;
                    cx.notify();
                    return;
                }
                let to = this.drag.as_ref().map(|d| d.over).unwrap_or(payload.from);
                let chat = drop_chat.clone();
                this.commit_reorder(&chat, payload.from, to, cx);
            }))
            .children(
                tab_elements
                    .into_iter()
                    .map(|(ix, key, title, selected, exited, progress)| {
                        let chat_select = chat_owned.clone();
                        let chat_close = chat_owned.clone();
                        let chat_close2 = chat_owned.clone();
                        let chat_drag = chat_owned.clone();
                        let ghost_title = title.clone();
                        // slopty: the side pane's tab chip (24, r 6, 12 px, the ✕ on hover), so the
                        // dock's strip and the pane's read as one; the glyph slot keeps the
                        // program's progress.
                        let tip = cut.get(ix).copied().unwrap_or(false).then(|| title.clone());
                        // slopty: an exited tab dims over the quick fade, not at once.
                        let dim = motion::state_t(
                            &format!("terminal-tab-exited-{key}"),
                            exited,
                            motion::FADE_QUICK,
                            reduced,
                            now,
                        );
                        let leading = tab_glyph(
                            key,
                            progress,
                            if selected { theme.text_muted } else { theme.text_muted.opacity(0.7) },
                            &theme,
                            cx.entity_id(),
                            cx,
                        );
                        let chat_menu = chat_owned.clone();
                        let tab_el = crate::surface_chrome::tab_chip(
                            crate::surface_chrome::TabChip {
                                id: "terminal-tab",
                                close_id: "terminal-tab-close",
                                ix: usize::try_from(key).unwrap_or(usize::MAX),
                                active: selected,
                                title: title.clone(),
                                dirty: false,
                                leading,
                                width: chip_w,
                                close_hint: "Close terminal",
                            },
                            &theme,
                            cx.listener(move |this, _, window, cx| {
                                this.close_tab(&chat_close2, key, window, cx);
                            }),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.select_tab(&chat_select, ix, cx);
                            this.request_focus(cx);
                        }))
                        // Middle-click closes (§1.10).
                        .on_mouse_down(
                            MouseButton::Middle,
                            cx.listener(move |this, _, window, cx| {
                                this.close_tab(&chat_close, key, window, cx);
                            }),
                        )
                        // slopty: right-click offers what the side pane's tabs offer.
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                                cx.stop_propagation();
                                this.tab_menu.open(TabMenu {
                                    chat: chat_menu.clone(),
                                    key,
                                    position: event.position,
                                });
                                cx.notify();
                            }),
                        )
                        .on_drag(
                            TabDragPayload {
                                chat: chat_drag,
                                from: ix,
                                title: ghost_title,
                            },
                            move |payload, _point, _, cx| {
                                let title = payload.title.clone();
                                cx.stop_propagation();
                                cx.new(|_| TabGhost { title, width: chip_w })
                            },
                        )
                        .opacity(motion::lerp(1.0, 0.55, dim))
                        // slopty: the tab is named for a screen reader as for the eye.
                        .role(gpui::Role::Tab)
                        .aria_label(title)
                        .when_some(tip, |el, tip| {
                            el.tooltip(crate::settings::widgets::text_tooltip(tip))
                                .tooltip_show_delay(crate::settings::widgets::TOOLTIP_DELAY)
                        });

                        // Sliding transform while a sibling is dragged over: animate
                        // 150 ms between committed offsets.
                        match drag {
                            Some((from, over, epoch, prev_over)) if ix != from => {
                                let target = slide_offset(ix, from, over) * chip_slot;
                                let start = slide_offset(ix, from, prev_over) * chip_slot;
                                div()
                                    .relative()
                                    .child(tab_el.with_animation(
                                        ("terminal-tab-slide", key | ((epoch as u64) << 32)),
                                        TAB_SLIDE.animation(),
                                        move |el, t| el.left(px(motion::lerp(start, target, t))),
                                    ))
                                    .into_any_element()
                            }
                            // Invisible spacer — the ghost carries the tab; a
                            // dimmed original overlapped the sibling that
                            // slides into the vacated slot.
                            Some((from, ..)) if ix == from => div()
                                .w(px(chip_w))
                                .h(px(crate::surface_chrome::TAB_CHIP_HEIGHT))
                                .flex_none()
                                .into_any_element(),
                            _ => tab_el.into_any_element(),
                        }
                    }),
            )
            // slopty: the strip's controls are the side pane's: 24 square, r 6, a faded wash, a 14
            // glyph, reached by Tab, named with their keys.
            .child(
                crate::surface_chrome::icon_button("terminal-new-tab", crate::icons::PLUS, &theme)
                    .on_click(cx.listener(|this, _, _, cx| {
                        let chat = this.selected_chat(cx);
                        this.open_tab(chat, cx);
                        this.request_focus(cx);
                    }))
                    .action_hint(
                        "New terminal",
                        Keys::ActionIn(Box::new(NewTerminalTab), self.focus_handle.clone()),
                        cx,
                    ),
            )
            // Collapse chevron pinned right (slopty "Hide terminal" ⌘J).
            .child(div().flex_1())
            .child(
                crate::surface_chrome::icon_button("terminal-collapse", crate::icons::ALT_ARROW_DOWN, &theme)
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(ToggleTerminal), cx);
                    })
                    .action_hint("Hide terminal", Keys::Action(Box::new(ToggleTerminal)), cx),
            )
    }
}

/// slopty: a tab's leading glyph. While its program reports progress (`OSC 9;4`) the slot holds
/// it, as a sending chip's icon well does: an arc for a share (the danger's while failed, muted
/// while paused), the mono spinner while under way, named "Progress" with its percent for a
/// screen reader; otherwise zeron's terminal glyph.
fn tab_glyph(
    key: u64,
    progress: Option<live::Progress>,
    ink: gpui::Hsla,
    theme: &Theme,
    view: gpui::EntityId,
    cx: &mut App,
) -> gpui::AnyElement {
    let Some(progress) = progress else {
        return crate::icons::icon(crate::icons::TERMINAL)
            .size(px(crate::surface_chrome::ICON_SIZE))
            .flex_none()
            .text_color(ink)
            .into_any_element();
    };
    let percent = progress.percent();
    let arc = match progress {
        live::Progress::Failed(_) => theme.danger,
        live::Progress::Paused(_) => theme.text_muted,
        live::Progress::Share(_) | live::Progress::Busy => theme.text,
    };
    let mark = match percent {
        Some(percent) => crate::loaders::upload_progress_arc(
            percent,
            12.0,
            theme.text_muted.opacity(0.25),
            arc,
        ),
        None => crate::loaders::mini_mono_spinner(
            SharedString::from(format!("terminal-tab-progress-{key}")),
            2.0,
            arc,
            view,
            cx,
        )
        .into_any_element(),
    };
    div()
        .id(("terminal-tab-progress", key))
        .size(px(crate::surface_chrome::ICON_SIZE))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .role(gpui::Role::ProgressIndicator)
        .aria_label("Progress")
        .when_some(percent, |el, percent| {
            el.aria_numeric_value(f64::from(percent))
                .aria_value(SharedString::from(format!("{percent} %")))
        })
        .child(mark)
        .into_any_element()
}

impl Render for TerminalPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        // Heal drag state if the pointer was released outside the bar.
        if self.drag.is_some() && !cx.has_active_drag() {
            self.drag = None;
        }
        // Embedded, the RIGHT PANE's own surface shows through — a second
        // fill here stacked another shade on the pane (user report); the
        // drawer keeps its own tone.
        let panel_bg: Option<gpui::Hsla> =
            (!self.embedded || self.session).then(|| terminal_panel_bg(&theme));
        // Docked at the window's bottom edge, the panel's own fill carries
        // the CSD window's bottom corners when it sits at them (the shell
        // decides per frame; embedded panels never do).
        let corner_bl = self.window_corner_bl;
        let corner_br = self.window_corner_br;
        let corner = px(crate::shell::LINUX_WINDOW_CORNER_RADIUS);
        let chat = self.selected_chat(cx);
        self.watch_arrived_views(&chat, cx);
        // slopty: the body is Slopty's view of the active tab's session.
        let (id, failure) = self
            .active_tab(cx)
            .map(|tab| (tab.terminal_id.clone(), tab.failure.clone()))
            .unwrap_or_default();
        let look = TerminalLook::of(cx);
        let hosted = id.and_then(|id| live::terminal(&id, &look, cx));
        self.drawn = hosted.as_ref().map(|hosted| hosted.view.clone()); // slopty
        // Focus lands on the panel's body once, and passes on to the terminal when it is there.
        if self.open && std::mem::take(&mut self.focus_pending) {
            window.focus(hosted.as_ref().map_or(&self.focus_handle, |h| &h.focus), cx);
        } else if let Some(hosted) = &hosted
            && self.open
            && self.focus_handle.is_focused(window)
        {
            window.focus(&hosted.focus, cx);
        }

        // Embedded (right-pane surface host): the shell's surface tabs
        // replace the internal bar.
        let tab_bar: Option<gpui::AnyElement> =
            (!self.embedded).then(|| self.render_tab_bar(&chat, cx).into_any_element());
        // slopty: what a drop of files on the terminal does, as its view says.
        let drop = hosted.as_ref().and_then(|hosted| hosted.drop.clone());
        let body = match (hosted, failure) {
            (Some(hosted), _) => hosted.view.into_any_element(),
            (None, Some(failure)) => div()
                .p(px(super::view::TERM_PADDING))
                .font_family(theme.font_terminal.clone())
                .text_size(px(theme.terminal_font_size))
                .text_color(theme.terminal.ansi[1])
                .child(failure)
                .into_any_element(),
            // slopty: on its way, zeron's loading anatomy names the machine it opens on.
            (None, None) => {
                let caption = self.connecting_to(&chat, cx).map_or_else(
                    || "Connecting…".to_owned(),
                    |name| format!("Connecting to {name}…"),
                );
                live::chrome::loading(caption, &theme, cx.entity_id(), cx).into_any_element()
            }
        };
        let tab_menu = self.render_tab_menu(window, cx);
        let measure = cx.weak_entity();
        div()
            .size_full()
            .flex()
            .flex_col()
            // slopty: ⌘T and ⌘W act on the panel's tabs while it has the keyboard.
            .key_context(if self.session
                && crate::inspector::is_agent_session(&chat)
                && !chat.starts_with(CANVAS_PANEL_PREFIX)
            {
                AGENT_SESSION_CONTEXT
            } else {
                KEY_CONTEXT
            })
            .on_action(cx.listener(|this, _: &NewTerminalTab, _, cx| {
                if this.embedded {
                    cx.propagate();
                    return;
                }
                let chat = this.selected_chat(cx);
                this.open_tab(chat, cx);
                this.request_focus(cx);
            }))
            .on_action(cx.listener(|this, _: &crate::app_menus::CloseTab, window, cx| {
                if !this.close_active_tab(window, cx) {
                    cx.propagate();
                }
            }))
            .children(tab_menu)
            // Terminal chrome is fixed Geist; TerminalElement measures and
            // paints its viewport independently with the technical mono role.
            .font_family(theme.font_sans_fixed.clone())
            .when_some(panel_bg, |el, bg| el.bg(bg))
            .when(corner_bl, |el| el.rounded_bl(corner))
            .when(corner_br, |el| el.rounded_br(corner))
            .children(tab_bar)
            .child(
                div()
                    .id("terminal-body")
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .track_focus(&self.focus_handle)
                    // ⌃C/⌘C in a terminal is the terminal's (an interrupt, its own copy),
                    // never the transcript selection's copy further up.
                    .on_key_down(|event: &gpui::KeyDownEvent, _, cx| {
                        let keys = &event.keystroke;
                        if keys.key == "c" && (keys.modifiers.control || keys.modifiers.platform) {
                            cx.stop_propagation();
                        }
                    })
                    .child(body)
                    // slopty: the body's grid, for the sessions opened here.
                    .child(
                        gpui::canvas(
                            move |bounds, window, cx| {
                                let _gone = measure.update(cx, |panel, cx| {
                                    panel.measured(bounds.size, window, cx);
                                });
                            },
                            |_, (), _, _| {},
                        )
                        .absolute()
                        .size_full(),
                    )
                    .children(drop.map(|words| drop_overlay(words, &theme))),
            )
            .into_any_element()
    }
}

/// slopty: the hint while files are carried over a terminal, zeron's attachment overlay
/// (`shell/chat_dropzone.rs`) in the view's own words: the drop is the view's, so the overlay only
/// shows, under no pointer.
fn drop_overlay(words: SharedString, theme: &Theme) -> gpui::AnyElement {
    div()
        .id("terminal-drop-overlay")
        .absolute()
        .inset_0()
        .opacity(0.0)
        .bg(theme.scrim().opacity(0.4 / 0.6))
        .flex()
        .items_center()
        .justify_center()
        .text_size(crate::typography::ui_rems(13.0))
        .text_color(theme.text)
        .drag_over::<gpui::ExternalPaths>(|style, _, _, _| style.opacity(1.0))
        .child(words)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn height_clamps_between_160_and_55vh() {
        assert_eq!(clamp_terminal_height(300.0, 900.0), 300.0);
        assert_eq!(clamp_terminal_height(10.0, 900.0), 160.0);
        assert_eq!(clamp_terminal_height(4000.0, 900.0), 900.0 * 0.55);
        // Tiny windows: min wins over the 55vh cap.
        assert_eq!(clamp_terminal_height(200.0, 100.0), 160.0);
        assert_eq!(clamp_terminal_height(f32::NAN, 900.0), 160.0);
    }

    #[test]
    fn reorder_moves_forward_and_backward() {
        let mut v = vec!["a", "b", "c", "d"];
        reorder_tabs(&mut v, 0, 2);
        assert_eq!(v, ["b", "c", "a", "d"]);
        reorder_tabs(&mut v, 3, 0);
        assert_eq!(v, ["d", "b", "c", "a"]);
        // Out-of-range / no-op moves leave the vec untouched.
        reorder_tabs(&mut v, 9, 0);
        reorder_tabs(&mut v, 1, 1);
        assert_eq!(v, ["d", "b", "c", "a"]);
    }

    #[test]
    fn drop_index_quantizes_and_clamps() {
        assert_eq!(drop_index(-10.0, 150.0, 3), 0);
        assert_eq!(drop_index(0.0, 150.0, 3), 0);
        assert_eq!(drop_index(149.0, 150.0, 3), 0);
        assert_eq!(drop_index(150.0, 150.0, 3), 1);
        assert_eq!(drop_index(700.0, 150.0, 3), 2);
        assert_eq!(drop_index(50.0, 150.0, 0), 0);
    }

    #[test]
    fn slide_offsets_shift_toward_the_gap() {
        // Dragging 0 over 2: tabs 1 and 2 slide left one slot.
        assert_eq!(slide_offset(0, 0, 2), 0.0);
        assert_eq!(slide_offset(1, 0, 2), -1.0);
        assert_eq!(slide_offset(2, 0, 2), -1.0);
        assert_eq!(slide_offset(3, 0, 2), 0.0);
        // Dragging 3 over 1: tabs 1 and 2 slide right.
        assert_eq!(slide_offset(0, 3, 1), 0.0);
        assert_eq!(slide_offset(1, 3, 1), 1.0);
        assert_eq!(slide_offset(2, 3, 1), 1.0);
        assert_eq!(slide_offset(3, 3, 1), 0.0);
        // Hovering the origin: nothing moves.
        for ix in 0..4 {
            assert_eq!(slide_offset(ix, 2, 2), 0.0);
        }
    }

    #[test]
    fn active_index_tracks_reorders() {
        // The active tab itself moves.
        assert_eq!(active_after_reorder(1, 1, 3), 3);
        // A tab hopping over the active one from the left shifts it down.
        assert_eq!(active_after_reorder(2, 0, 3), 1);
        // …and from the right shifts it up.
        assert_eq!(active_after_reorder(1, 3, 0), 2);
        // Disjoint moves leave it alone.
        assert_eq!(active_after_reorder(0, 2, 3), 0);
    }

    #[test]
    fn active_index_tracks_closes() {
        assert_eq!(active_after_close(2, 0, 3), 1); // close left of active
        assert_eq!(active_after_close(1, 1, 2), 1); // close active mid-list
        assert_eq!(active_after_close(2, 2, 2), 1); // close active at tail
        assert_eq!(active_after_close(0, 0, 0), 0); // last tab closed
    }

    #[test]
    fn shell_titles() {
        assert_eq!(shell_title("/bin/zsh"), "zsh");
        assert_eq!(shell_title("/usr/local/bin/fish"), "fish");
        assert_eq!(shell_title("C:\\Windows\\System32\\cmd.exe"), "cmd.exe");
        assert_eq!(shell_title("bash"), "bash");
        assert_eq!(shell_title(""), "terminal");
    }
}

// slopty: a running program's progress (`OSC 9;4`) reaches its drawer tab: the tab reads it
// again each time its view says something new of itself, and lets it go when the report ends.
#[cfg(test)]
mod progress_tests {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;

    use gpui::AppContext as _;
    use slopty_view_rpc::{RpcError, RpcReply, methods};

    use super::TerminalPanel;
    use crate::live::{Hosted, Progress};

    /// A machine that opens terminal 7 for every tab.
    struct Machine;

    #[async_trait::async_trait]
    impl slopty_view_rpc::RpcService for Machine {
        async fn handle(&self, method: &str, _: serde_json::Value) -> Result<RpcReply, RpcError> {
            let answer = match method {
                methods::OPEN_TERMINAL => {
                    serde_json::json!({ "id": "7", "cwd": "/w", "shell": "zsh" })
                }
                methods::CLOSE_TERMINAL => serde_json::json!({}),
                _ => return Err(RpcError::Failed(format!("not asked here: {method}"))),
            };
            Ok(RpcReply::Value(answer))
        }
    }

    struct Blank;

    impl gpui::Render for Blank {
        fn render(
            &mut self,
            _: &mut gpui::Window,
            _: &mut gpui::Context<Self>,
        ) -> impl gpui::IntoElement {
            gpui::div()
        }
    }

    type Heard = Rc<RefCell<Vec<Box<dyn FnMut(&mut gpui::App)>>>>;

    #[gpui::test]
    fn a_running_programs_progress_reaches_its_tab(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let _guard = runtime.enter();
        let dir = tempfile::tempdir().unwrap();
        let reported = Rc::new(RefCell::new(None::<Progress>));
        let heard: Heard = Rc::default();
        cx.update(|cx| {
            crate::settings::init(Default::default(), dir.path(), cx);
            gpui_base::init(cx);
            cx.set_global(crate::theme::Theme::default());
            let (reported, heard) = (Rc::clone(&reported), Rc::clone(&heard));
            crate::live::install(
                cx,
                move |id, _, cx| {
                    (id == "7").then(|| Hosted {
                        view: cx.new(|_| Blank).into(),
                        focus: cx.focus_handle(),
                        title: None,
                        ended: false,
                        exit: None,
                        progress: *reported.borrow(),
                        drop: None,
                    })
                },
                |_, _| None,
                move |_, changed, _| {
                    heard.borrow_mut().push(changed);
                    None
                },
            );
        });
        let engine = crate::state::EngineHandle::from_test_client(slopty_view_rpc::memory_client(
            Arc::new(Machine) as Arc<dyn slopty_view_rpc::RpcService>,
        ));
        let (panel, cx) = cx.add_window_view(|_, cx| {
            let state = cx.new(|_| {
                let mut state = crate::state::AppState::new();
                state.set_test_engine(engine);
                state
            });
            TerminalPanel::new(state, cx)
        });
        let settle = |cx: &mut gpui::VisualTestContext,
                      done: &dyn Fn(&mut gpui::VisualTestContext) -> bool| {
            for _ in 0..300 {
                cx.run_until_parked();
                runtime.block_on(async {
                    for _ in 0..20 {
                        tokio::task::yield_now().await;
                    }
                });
                cx.run_until_parked();
                if done(cx) {
                    return;
                }
            }
            panic!("the panel never settled");
        };
        let tab = |cx: &mut gpui::VisualTestContext| {
            panel.update(cx, |panel, cx| {
                let chat = panel.selected_chat(cx);
                let key = panel.chats.get(&chat)?.tabs.first()?.key;
                Some((key, panel.terminal_id(&chat, key)?.to_owned(), panel.tab_progress(key, cx)))
            })
        };
        panel.update(cx, |panel, cx| panel.set_open(true, cx));
        settle(cx, &|cx| tab(cx).is_some());
        assert_eq!(tab(cx).map(|(_, id, progress)| (id, progress)), Some(("7".to_owned(), None)));

        // The program reports 40 %: its view says so, and the tab shows it.
        *reported.borrow_mut() = Some(Progress::Share(0.4));
        cx.update(|_, cx| heard.borrow_mut().iter_mut().for_each(|changed| changed(cx)));
        settle(cx, &|cx| tab(cx).is_some_and(|(_, _, progress)| progress.is_some()));
        assert_eq!(tab(cx).and_then(|(_, _, progress)| progress), Some(Progress::Share(0.4)));

        // The report ends (the program cleared it, or the shell's next prompt did): it goes.
        *reported.borrow_mut() = None;
        cx.update(|_, cx| heard.borrow_mut().iter_mut().for_each(|changed| changed(cx)));
        settle(cx, &|cx| tab(cx).is_some_and(|(_, _, progress)| progress.is_none()));
    }

    /// A machine that opens terminal 7 and keeps what each open asked.
    struct Asked(Arc<std::sync::Mutex<Vec<serde_json::Value>>>);

    #[async_trait::async_trait]
    impl slopty_view_rpc::RpcService for Asked {
        async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
            if method == methods::OPEN_TERMINAL {
                self.0.lock().unwrap().push(params.clone());
            }
            Machine.handle(method, params).await
        }
    }

    /// A tab's session opens at the grid a view draws in the panel's body, not at 80 × 24: a
    /// shell told it is wider than it is drawn pads its first prompt past the edge. A panel
    /// whose body is never drawn opens at 80 × 24 once its wait is over.
    #[gpui::test]
    fn a_session_opens_at_the_grid_its_body_draws(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let _guard = runtime.enter();
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            crate::settings::init(Default::default(), dir.path(), cx);
            gpui_base::init(cx);
            cx.set_global(crate::theme::Theme::default());
            crate::live::install(cx, |_, _, _| None, |_, _| None, |_, _, _| None);
            // A cell 8 × 16 points.
            #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "test")]
            crate::live::serve_fit(cx, |size, _, _| {
                ((f32::from(size.width) / 8.0) as u16, (f32::from(size.height) / 16.0) as u16)
            });
        });
        let asked = Arc::new(std::sync::Mutex::new(Vec::new()));
        let engine = crate::state::EngineHandle::from_test_client(slopty_view_rpc::memory_client(
            Arc::new(Asked(Arc::clone(&asked))) as Arc<dyn slopty_view_rpc::RpcService>,
        ));
        let state = cx.new(|_| {
            let mut state = crate::state::AppState::new();
            state.set_test_engine(engine);
            state
        });
        let unshown = cx.new(|cx| TerminalPanel::new(state.clone(), cx));
        let (panel, cx) = cx.add_window_view(|_, cx| TerminalPanel::new(state, cx));
        cx.simulate_resize(gpui::size(gpui::px(360.0), gpui::px(400.0)));
        panel.update(cx, |panel, cx| panel.set_open(true, cx));
        let mut opened = None;
        for _ in 0..300 {
            cx.run_until_parked();
            runtime.block_on(async {
                for _ in 0..20 {
                    tokio::task::yield_now().await;
                }
            });
            opened = asked.lock().unwrap().first().cloned();
            if opened.is_some() {
                break;
            }
        }
        let opened = opened.expect("the tab's session opened");
        let fitted = panel.read_with(cx, |panel, _| panel.fitted.as_ref().map(|(.., grid)| *grid));
        let (cols, rows) = fitted.expect("the body measured");
        assert_eq!(cols, 45, "the body's width over the cell's");
        assert_eq!((opened["cols"].as_u64(), opened["rows"].as_u64()), (Some(45), Some(rows.into())));

        let grid = Rc::new(RefCell::new(None));
        cx.update(|_, cx| {
            let (panel, grid) = (unshown.downgrade(), Rc::clone(&grid));
            cx.spawn(async move |cx| *grid.borrow_mut() = Some(TerminalPanel::grid(&panel, cx).await))
                .detach();
        });
        cx.run_until_parked();
        assert_eq!(*grid.borrow(), None, "waits for the body's first draw");
        cx.executor().advance_clock(super::FIT_WAIT);
        cx.run_until_parked();
        assert_eq!(*grid.borrow(), Some(super::UNFITTED), "never drawn");
    }
}

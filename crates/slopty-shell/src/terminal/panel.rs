// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! A terminal pane: the session's own terminal in the main pane, or a shell opened beside it.
//!
//! See `docs/decisions/layout.md`, "One session per view, splits on demand". The session's panel
//! shows each chat's session terminal as the chat is shown; a shell's panel opens one terminal
//! for the chat it was opened in and shows it while that chat is shown.
//!
//! Data path per tab: `OpenTerminal` names a session of Slopty's, and the
//! body is Slopty's own terminal view of it ([`crate::live`]), which owns the
//! session's link, prediction, latency path, copy mode and scrollback. The
//! panel hands the view the look ([`crate::live::TerminalLook`]).

use std::collections::HashMap;

use futures::FutureExt as _;
use futures::channel::oneshot;
use gpui::prelude::*;
use gpui::{
    App, AsyncApp, Context, Entity, FocusHandle, IntoElement, Pixels, Render, SharedString, Size,
    Subscription, Task, WeakEntity, Window, div, px,
};
use slopty_view_proto::TerminalSession;
use slopty_view_rpc::methods;

use super::view::terminal_panel_bg;
use crate::live::{self, TerminalLook};
use crate::state::{AppState, EngineHandle};
use crate::theme::Theme;

/// What the shown terminal says of itself that the window's chrome wears changed: its ground
/// ([`TerminalPanel::shown_ground`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShownChanged;

impl gpui::EventEmitter<ShownChanged> for TerminalPanel {}

/// Merge the `targetDeviceId` passthrough into RPC params (no-op for chats on
/// the connected engine's own device).
fn with_target(mut params: serde_json::Value, target: Option<&str>) -> serde_json::Value {
    if let (Some(target), Some(object)) = (target, params.as_object_mut()) {
        object.insert("targetDeviceId".into(), serde_json::Value::String(target.to_owned()));
    }
    params
}

/// Active index after closing `closed` (given the new, shorter length).
pub fn active_after_close(active: usize, closed: usize, len_after: usize) -> usize {
    let shifted = if closed < active { active.saturating_sub(1) } else { active };
    shifted.min(len_after.saturating_sub(1))
}

// ---------------------------------------------------------------------------
// Entity
// ---------------------------------------------------------------------------

struct TerminalTab {
    key: u64,
    title: SharedString,
    terminal_id: Option<String>,
    target_device_id: Option<String>,
    // What Slopty's view says of itself: its OSC title, why it failed to open.
    shown_title: Option<SharedString>,
    failure: Option<SharedString>,
    exited: Option<i32>,
    /// How far the program says it has got (`OSC 9;4`), drawn on the tab.
    progress: Option<live::Progress>,
    /// The background its program paints with (OSC 11, else the theme's), which the titlebar
    /// row wears while this tab is the session's shown terminal.
    ground: Option<gpui::Hsla>,
    /// Hears the view's title and end.
    watch: Option<Subscription>,
    /// The open; dropping it cancels it.
    _run: Option<Task<()>>,
}

impl TerminalTab {
    /// Keeps the tab's open running for as long as the tab lives.
    #[expect(clippy::used_underscore_binding, reason = "the open is kept, not read")]
    fn hold(&mut self, run: Task<()>) {
        self._run = Some(run);
    }
}

#[derive(Default)]
struct ChatTabs {
    tabs: Vec<TerminalTab>,
    active: usize,
}

/// A pane of terminal tabs per chat: the session's own terminal, or shells beside it.
pub struct TerminalPanel {
    state: Entity<AppState>,
    focus_handle: FocusHandle,
    focus_pending: bool,
    /// The terminal view the body drew last, asked whether the person has words in it
    /// unsent ([`Self::drafting`]).
    drawn: Option<gpui::AnyView>,
    chats: HashMap<String, ChatTabs>,
    /// The main column's panel: each chat's one terminal, the session itself (its
    /// agent's TUI, or a plain shell), opened as the chat is shown (`docs/decisions/ui.md`,
    /// "A session is its terminal").
    session: bool,
    /// In the session panel, the terminal each chat's tab was opened on: a chat whose
    /// session moved to another terminal (`/clear`, `/resume`) gets a tab of that one.
    session_asked: HashMap<String, String>,
    /// Whether the pane is dimmed (a split's pane without the keyboard), and what the shown
    /// view was last told of it, by its entity.
    dimmed: bool,
    told_dim: Option<(gpui::EntityId, bool)>,
    /// How much narrower and shorter than the body the shown terminal fits its grid
    /// while the panes around the panel glide, and the view it holds ([`live::hold_fit`]).
    fit_hold: Size<Pixels>,
    held_view: Option<gpui::EntityId>,
    tab_seq: u64,
    last_selected: Option<String>,
    /// The grid a terminal view draws in the body, as last drawn, and the size and look
    /// it was measured for.
    fitted: Option<(Size<Pixels>, TerminalLook, (u16, u16))>,
    /// Sessions waiting for the body's first draw to open at its grid.
    awaiting_fit: Vec<oneshot::Sender<(u16, u16)>>,
    _observe: Subscription,
}

/// What an open asks for: a fresh shell, a terminal shown when the app last ran, or
/// the terminal a session is (`AppState::session_terminal`).
enum Open {
    Fresh,
    Again(String),
    Session(String),
}

/// The grid a session opens at when the panel's body is not drawn: the view fits it
/// once it is.
const UNFITTED: (u16, u16) = (80, 24);

/// How long an open waits for the body's first draw before it opens at [`UNFITTED`].
const FIT_WAIT: std::time::Duration = std::time::Duration::from_millis(500);

impl std::fmt::Debug for TerminalPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalPanel")
            .field("session", &self.session)
            .field("chats", &self.chats.len())
            .field("last_selected", &self.last_selected)
            .field("dimmed", &self.dimmed)
            .finish_non_exhaustive()
    }
}

impl TerminalPanel {
    /// A panel that opens terminals only as it is asked to.
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&state, |this: &mut Self, _, cx| this.on_state_changed(cx));
        Self {
            state,
            focus_handle: cx.focus_handle(),
            focus_pending: false,
            drawn: None,
            chats: HashMap::new(),
            session: false,
            session_asked: HashMap::new(),
            dimmed: false,
            told_dim: None,
            fit_hold: Size::default(),
            held_view: None,
            tab_seq: 0,
            last_selected: None,
            fitted: None,
            awaiting_fit: Vec::new(),
            _observe: observe,
        }
    }

    /// The grid a session opened in `panel` starts at: what a terminal view draws in
    /// its body, waiting for the body's first draw, so the shell never starts wider than it is
    /// shown. One told wider pads its first prompt past the grid's edge, and zsh's partial-line
    /// mark is left on a line of its own. A body not drawn within [`FIT_WAIT`] (a panel not
    /// shown) gives [`UNFITTED`].
    pub(crate) async fn grid(panel: &WeakEntity<Self>, cx: &mut AsyncApp) -> (u16, u16) {
        let waiting = panel.update(cx, |panel, _| {
            if let Some((.., grid)) = &panel.fitted {
                Ok(*grid)
            } else {
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

    /// The body was laid out at `size`: the grid a view draws there, for the sessions opened
    /// here, less what a glide under way holds back ([`Self::hold_fit`]).
    fn measured(&mut self, size: Size<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let size = Size {
            width: (size.width - self.fit_hold.width).max(Pixels::ZERO),
            height: (size.height - self.fit_hold.height).max(Pixels::ZERO),
        };
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

    /// A shell's pane beside the session (⌘D): one fresh shell for the chat shown, on its
    /// machine and in its folder, which takes the keyboard once it is drawn.
    pub fn new_pane(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let mut panel = Self::new(state, cx);
        let chat = panel.selected_chat(cx);
        panel.open_tab(&chat, cx);
        panel.request_focus(cx);
        panel
    }

    /// The main column's panel (see the `session` field): each chat shown gets its
    /// session's terminal.
    pub fn new_session(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let mut panel = Self::new(state, cx);
        panel.session = true;
        panel
    }

    /// Dim the pane, or bring it back: a split's pane without the keyboard is dimmed by
    /// brightness alone, in the terminal's own element ([`live::dim`]).
    pub fn set_dimmed(&mut self, dimmed: bool, cx: &mut Context<Self>) {
        if self.dimmed != dimmed {
            self.dimmed = dimmed;
            cx.notify();
        }
    }

    /// The shown terminal's title: its program's own (OSC 0/2), else the shell's name.
    pub fn shown_title(&self, cx: &App) -> Option<SharedString> {
        self.active_tab(cx).map(Self::display_title)
    }

    /// Close the shown terminal: its pane is closing. A session's own terminal is the agent's
    /// and is never closed here.
    pub fn close_shown(&mut self, cx: &mut Context<Self>) {
        if self.session {
            return;
        }
        let chat = self.selected_chat(cx);
        if let Some(key) = self.active_tab(cx).map(|tab| tab.key) {
            self.close_tab(&chat, key, cx);
        }
    }

    /// The terminal a shell's pane streams, once it is attached, whichever session is
    /// shown: what the pane opens again on the next launch, and what an agent hears it is.
    pub fn pane_terminal_id(&self) -> Option<&str> {
        self.chats
            .values()
            .flat_map(|chat| chat.tabs.iter())
            .find_map(|tab| tab.terminal_id.as_deref())
    }

    /// The background the shown terminal's program paints with, once its view said: the
    /// ground the titlebar row and the session's column wear (`docs/decisions/layout.md`, "One
    /// row of chrome"). [`ShownChanged`] says when it changes.
    pub fn shown_ground(&self, cx: &App) -> Option<gpui::Hsla> {
        self.active_tab(cx)?.ground
    }

    /// The handle the terminal body focuses through.
    pub fn focus_handle(&self) -> FocusHandle {
        self.focus_handle.clone()
    }

    /// Whether the terminal shown holds words the person typed and has not sent.
    pub fn drafting(&self, cx: &App) -> bool {
        self.drawn.as_ref().is_some_and(|view| live::drafting(view, cx))
    }

    /// Claim focus once the terminal body mounts, after opening or selecting it.
    pub fn request_focus(&mut self, cx: &mut Context<Self>) {
        self.focus_pending = true;
        cx.notify();
    }

    /// What [`Self::hold_fit`] holds the grid by (tests).
    #[cfg(test)]
    pub(crate) const fn fit_hold(&self) -> Size<Pixels> {
        self.fit_hold
    }

    /// The shell's hold on the shown terminal's grid while the panes around the panel
    /// glide: `by` narrower and shorter than the body, zero once they rest ([`live::hold_fit`]).
    pub fn hold_fit(&mut self, by: Size<Pixels>, cx: &mut Context<Self>) {
        if self.fit_hold == by {
            return;
        }
        self.fit_hold = by;
        if let Some(view) = self.held_view {
            live::hold_fit(view, by, cx);
        }
    }

    /// A tab's display label: the live OSC 0/2 title when the running
    /// program set one (shells title themselves with the cwd / running
    /// command — the contextual name, user request), else the fixed
    /// "Terminal N".
    fn display_title(tab: &TerminalTab) -> SharedString {
        match tab.shown_title.as_ref().map(|title| title.trim()) {
            Some(title) if !title.is_empty() => title.to_owned().into(),
            _ => tab.title.clone(),
        }
    }

    /// Create a named placeholder tab without opening a PTY. Project Actions
    /// use this before their host-side run RPC completes.
    pub fn reserve_tab_for_chat(
        &mut self,
        chat: String,
        title: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) -> u64 {
        self.tab_seq = self.tab_seq.saturating_add(1);
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
            ground: None,
            watch: None,
            _run: None,
        });
        entry.active = entry.tabs.len().saturating_sub(1);
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
        cx: &Context<Self>,
    ) -> bool {
        if self.tab_mut(chat, key).is_none() {
            return false;
        }
        let Some(engine) = self.engine(cx) else {
            return false;
        };
        let run = Self::spawn_session(
            chat.to_owned(),
            key,
            engine,
            target_device_id,
            Some(session),
            None,
            Open::Fresh,
            cx,
        );
        if let Some(tab) = self.tab_mut(chat, key) {
            tab.hold(run);
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

    fn on_state_changed(&mut self, cx: &mut Context<Self>) {
        let selected_key = self.selected_chat(cx);
        let prev = self.last_selected.clone();
        let switched = Some(selected_key.clone()) != prev;
        if switched {
            self.last_selected = Some(selected_key.clone());
        }
        if self.session {
            self.sync_session_tab(&selected_key, cx);
        }
        if switched {
            cx.notify();
        }
    }

    fn engine(&self, cx: &App) -> Option<EngineHandle> {
        self.state.read(cx).engine().cloned()
    }

    /// The chat's host device when it differs from the connected engine's own —
    /// the PTY lives on the chat's device, so every terminal RPC for a remote
    /// chat needs the `targetDeviceId` passthrough. Without it the local
    /// engine checks the chat's cwd against its OWN filesystem and fails with
    /// "Session working directory is unavailable" (user report).
    fn chat_target(&self, chat: &str, cx: &App) -> Option<String> {
        self.state.read(cx).terminal_target_device(chat)
    }

    fn selected_chat(&self, cx: &App) -> String {
        self.state.read(cx).panel_session_key()
    }

    /// The session panel's chat `chat` shows the terminal its session is now, once it
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
            tab.hold(run);
        }
        cx.notify();
    }

    fn tab_mut(&mut self, chat: &str, key: u64) -> Option<&mut TerminalTab> {
        self.chats.get_mut(chat)?.tabs.iter_mut().find(|t| t.key == key)
    }

    fn active_tab(&self, cx: &App) -> Option<&TerminalTab> {
        let chat = self.selected_chat(cx);
        let tabs = self.chats.get(&chat)?;
        tabs.tabs.get(tabs.active)
    }

    // ---- open / stream lifecycle ----

    /// The title of the next tab chat `chat` opens.
    fn next_tab_title(&self, chat: &str) -> String {
        let tab_no = self.chats.get(chat).map_or(1, |entry| entry.tabs.len().saturating_add(1));
        format!("Terminal {tab_no}")
    }

    fn open_tab(&mut self, chat: &str, cx: &mut Context<Self>) {
        let Some(engine) = self.engine(cx) else {
            return;
        };
        let key = self.reserve_tab_for_chat(chat.to_owned(), self.next_tab_title(chat), cx);
        let target = self.chat_target(chat, cx);
        let cwd = self.state.read(cx).terminal_open_cwd_for(chat);
        let run =
            Self::spawn_session(chat.to_owned(), key, engine, target, None, cwd, Open::Fresh, cx);
        if let Some(tab) = self.tab_mut(chat, key) {
            tab.hold(run);
        }
        cx.notify();
    }

    /// A tab of the selected chat on terminal `id` again, which it showed when the app
    /// last ran. A terminal that has ended takes its tab with it.
    pub fn reopen_tab_for_selected(&mut self, id: String, cx: &mut Context<Self>) -> Option<u64> {
        let chat = self.selected_chat(cx);
        let engine = self.engine(cx)?;
        let title = self.next_tab_title(&chat);
        let key = self.reserve_tab_for_chat(chat.clone(), title, cx);
        let target = self.chat_target(&chat, cx);
        let run =
            Self::spawn_session(chat.clone(), key, engine, target, None, None, Open::Again(id), cx);
        if let Some(tab) = self.tab_mut(&chat, key) {
            tab.hold(run);
        }
        Some(key)
    }

    /// The name of the machine chat `chat`'s terminals open on.
    fn connecting_to(&self, chat: &str, cx: &App) -> Option<String> {
        let state = self.state.read(cx);
        let device = self.chat_target(chat, cx).or_else(|| state.local_device_id.clone())?;
        state.device_name(&device).map(str::to_owned)
    }

    /// How far tab `key`'s program says it has got, for the right pane's chip.
    pub fn tab_progress(&self, key: u64, cx: &App) -> Option<live::Progress> {
        let chat = self.selected_chat(cx);
        self.chats.get(&chat)?.tabs.iter().find(|tab| tab.key == key)?.progress
    }

    /// The session tab `key` of chat `chat` shows, once it is attached.
    pub fn terminal_id(&self, chat: &str, key: u64) -> Option<&str> {
        let tabs = self.chats.get(chat)?;
        tabs.tabs.iter().find(|tab| tab.key == key)?.terminal_id.as_deref()
    }

    /// `OpenTerminal`, then show the session's view. The view streams
    /// its own session; nothing is subscribed here. `again` names a terminal
    /// the tab showed when the app last ran, which it shows again, or closes
    /// when it has ended.
    #[expect(
        clippy::too_many_arguments,
        reason = "an open's arguments plus the terminal to show again"
    )]
    fn spawn_session(
        chat: String,
        key: u64,
        engine: EngineHandle,
        target: Option<String>,
        existing_session: Option<TerminalSession>,
        cwd: Option<String>,
        open: Open,
        cx: &Context<Self>,
    ) -> Task<()> {
        // A session's terminal is the agent's, never the tab's to end.
        let own = matches!(open, Open::Session(_));
        cx.spawn(async move |this, cx| {
            let session = if let Some(session) = existing_session {
                session
            } else {
                // A session shown before keeps its grid; a new one starts at the
                // body's.
                let (cols, rows) = if matches!(open, Open::Fresh) {
                    Self::grid(&this, cx).await
                } else {
                    UNFITTED
                };
                let mut params = serde_json::json!({ "chatId": chat, "cols": cols, "rows": rows });
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
                        with_target(params, target.as_deref()),
                    )
                    .await
                {
                    Ok(session) => session,
                    // A terminal shown before that has ended goes with its tab.
                    Err(_) if again => {
                        let _gone = this.update(cx, |panel, cx| {
                            // A session's terminal is asked for again as it next moves.
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
                        let _gone = this.update(cx, |panel, cx| {
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
            };
            let terminal_id = session.id.clone();
            let attached = this
                .update(cx, |panel, cx| {
                    panel.attach_view(&chat, key, &terminal_id, target.clone(), cx)
                })
                .unwrap_or(false);
            if !attached && !own {
                // Tab was closed before the open completed — release the PTY.
                let params = serde_json::json!({ "terminalId": terminal_id });
                let closing = engine
                    .client()
                    .call(methods::CLOSE_TERMINAL, with_target(params, target.as_deref()));
                if let Err(error) = closing.await {
                    tracing::debug!(%error, "CloseTerminal failed");
                }
            }
        })
    }

    /// The tab shows the session `id`'s view from now on, and hears its
    /// title and end.
    fn attach_view(
        &mut self,
        chat: &str,
        key: u64,
        id: &str,
        target: Option<String>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(tab) = self.tab_mut(chat, key) else {
            return false;
        };
        tab.terminal_id = Some(id.to_owned());
        tab.target_device_id = target;
        self.watch_view(chat, key, id, cx);
        self.refresh_view(chat, key, cx);
        true
    }

    /// Tab `key` hears the session `id`'s view say something new of itself (its title,
    /// its end, its progress). `false` while the view is not there yet.
    fn watch_view(&mut self, chat: &str, key: u64, id: &str, cx: &mut Context<Self>) -> bool {
        let panel = cx.weak_entity();
        let heard = chat.to_owned();
        let watch = live::watch(
            id,
            Box::new(move |cx| {
                let _gone = panel.update(cx, |panel, cx| panel.refresh_view(&heard, key, cx));
            }),
            cx,
        );
        let watching = watch.is_some();
        if let Some(tab) = self.tab_mut(chat, key) {
            tab.watch = watch;
        }
        watching
    }

    /// A session's view can arrive after its tab took the session (the worker opens it
    /// on its own time): a tab not yet heard from starts hearing once its view is there, and reads
    /// it once after the frame, or a progress report its program sent never reached the tab.
    fn watch_arrived_views(&mut self, chat: &str, cx: &mut Context<Self>) {
        let unheard: Vec<(u64, String)> = self
            .chats
            .get(chat)
            .map(|tabs| {
                tabs.tabs
                    .iter()
                    .filter(|tab| tab.watch.is_none())
                    .filter_map(|tab| Some((tab.key, tab.terminal_id.clone()?)))
                    .collect()
            })
            .unwrap_or_default();
        for (key, id) in unheard {
            if self.watch_view(chat, key, &id, cx) {
                let chat = chat.to_owned();
                cx.spawn(async move |panel, cx| {
                    let _gone = panel.update(cx, |panel, cx| panel.refresh_view(&chat, key, cx));
                })
                .detach();
            }
        }
    }

    /// Read again what the tab's view says of itself.
    fn refresh_view(&mut self, chat: &str, key: u64, cx: &mut Context<Self>) {
        let Some(id) = self.tab_mut(chat, key).and_then(|tab| tab.terminal_id.clone()) else {
            return;
        };
        let look = TerminalLook::of(cx);
        let hosted = live::terminal(&id, &look, cx);
        let shown = self.active_tab(cx).is_some_and(|tab| tab.key == key);
        if let Some(tab) = self.tab_mut(chat, key) {
            let (title, ended, exit, progress, ground) = hosted
                .map_or((None, false, None, None, None), |h| {
                    (h.title, h.ended, h.exit, h.progress, h.ground)
                });
            tab.shown_title = title;
            tab.progress = progress;
            if tab.ground != ground {
                tab.ground = ground;
                if shown {
                    cx.emit(ShownChanged);
                }
            }
            // The status the program ended with, when it said one.
            if ended && tab.exited.is_none() {
                tab.exited = Some(exit.unwrap_or(0));
            }
            cx.notify();
        }
    }

    fn close_tab(&mut self, chat: &str, key: u64, cx: &mut Context<Self>) {
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
        if let (Some(engine), Some(id)) = (engine, tab.terminal_id) {
            cx.spawn(async move |_, _| {
                let params = serde_json::json!({ "terminalId": id });
                let closing = engine
                    .client()
                    .call(methods::CLOSE_TERMINAL, with_target(params, target.as_deref()));
                if let Err(error) = closing.await {
                    tracing::debug!(%error, "CloseTerminal failed");
                }
            })
            .detach();
        }
        cx.notify();
    }
}

impl Render for TerminalPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let chat = self.selected_chat(cx);
        self.watch_arrived_views(&chat, cx);
        // The body is Slopty's view of the active tab's session.
        let (id, failure) = self
            .active_tab(cx)
            .map(|tab| (tab.terminal_id.clone(), tab.failure.clone()))
            .unwrap_or_default();
        let look = TerminalLook::of(cx);
        let hosted = id.and_then(|id| live::terminal(&id, &look, cx));
        self.drawn = hosted.as_ref().map(|hosted| hosted.view.clone());
        // The hold follows the view shown, and lets go of one no longer shown.
        let shown = hosted.as_ref().map(|hosted| hosted.view.entity_id());
        if let Some(was) = self.held_view.filter(|was| Some(*was) != shown) {
            live::hold_fit(was, Size::default(), cx);
        }
        if let Some(view) = shown {
            live::hold_fit(view, self.fit_hold, cx);
        }
        self.held_view = shown;
        // A split's pane without the keyboard is dimmed in its terminal's own element; the view
        // is told when it or the dim changes.
        if let Some(hosted) = &hosted {
            let told = (hosted.view.entity_id(), self.dimmed);
            if self.told_dim != Some(told) {
                self.told_dim = Some(told);
                live::dim(&hosted.view, self.dimmed, cx);
            }
        }
        // Focus lands on the panel's body once, and passes on to the terminal when it is there.
        if std::mem::take(&mut self.focus_pending) {
            window.focus(hosted.as_ref().map_or(&self.focus_handle, |h| &h.focus), cx);
        } else if let Some(hosted) = &hosted
            && self.focus_handle.is_focused(window)
        {
            window.focus(&hosted.focus, cx);
        }
        // What a drop of files on the terminal does, as its view says.
        let drop = hosted.as_ref().and_then(|hosted| hosted.drop.clone());
        let progress =
            self.active_tab(cx).and_then(|tab| tab.progress.map(|progress| (tab.key, progress)));
        // A figureless report sweeps on the loaders' 30 Hz clock, which parks once it goes.
        let view = cx.entity_id();
        let sweep = match progress {
            Some((_, live::Progress::Busy)) => {
                crate::motion::pulse_delta(&crate::motion::SLOPTY_PULSE, view, cx)
            }
            _ => 0.0,
        };
        let body = match (hosted, failure) {
            (Some(hosted), _) => hosted.view.into_any_element(),
            (None, Some(failure)) => div()
                .p(px(super::view::TERM_PADDING))
                .font_family(theme.font_terminal.clone())
                .text_size(px(theme.terminal_font_size))
                .text_color(theme.terminal.ansi[1])
                .child(failure)
                .into_any_element(),
            // On its way, the loading state names the machine it opens on.
            (None, None) => {
                let caption = self.connecting_to(&chat, cx).map_or_else(
                    || "Connecting…".to_owned(),
                    |name| format!("Connecting to {name}…"),
                );
                live::chrome::loading(caption, &theme, cx.entity_id(), cx).into_any_element()
            }
        };
        let measure = cx.weak_entity();
        div()
            .size_full()
            .flex()
            .flex_col()
            // Terminal chrome is fixed Geist; TerminalElement measures and
            // paints its viewport independently with the technical mono role.
            .font_family(theme.font_sans_fixed.clone())
            .bg(terminal_panel_bg(&theme))
            .child(
                div()
                    .id("terminal-body")
                    .debug_selector(|| "terminal-body-bounds".into())
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
                    // The body's grid, for the sessions opened here.
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
                    .children(drop.map(|words| drop_overlay(words, &theme)))
                    .children(
                        progress.map(|(key, progress)| progress_bar(key, progress, sweep, &theme)),
                    ),
            )
            .into_any_element()
    }
}

/// A running program's progress (`OSC 9;4`) as Ghostty draws it where there is no tab
/// (`SurfaceProgressBar`): a 2 pt bar along the pane's top edge in the accent, in the danger hue
/// on a failure and the warning's on a pause, a figureless pause or failure full. With no figure
/// at all a quarter-wide segment crosses a faint track and back, 1.2 s each way on an ease-in-out,
/// at `sweep` of its round (still under Reduce Motion). It fades in where it sits, takes no
/// pointer, and a screen reader hears its share.
fn progress_bar(key: u64, progress: live::Progress, sweep: f32, theme: &Theme) -> gpui::AnyElement {
    const HEIGHT: f32 = 2.0;
    let (hue, share) = match progress {
        live::Progress::Share(share) => (theme.accent, Some(share)),
        live::Progress::Busy => (theme.accent, None),
        live::Progress::Paused(share) => (theme.warning, Some(share.unwrap_or(1.0))),
        live::Progress::Failed(share) => (theme.danger, Some(share.unwrap_or(1.0))),
    };
    let (left, width) = if let Some(share) = share {
        (0.0, share.clamp(0.0, 1.0))
    } else {
        let leg = if sweep < 0.5 { sweep * 2.0 } else { sweep.mul_add(-2.0, 2.0) };
        (0.75 * crate::motion::EASE_IN_OUT.eval(leg), 0.25)
    };
    let fill = div()
        .absolute()
        .top_0()
        .h_full()
        .left(gpui::relative(left))
        .w(gpui::relative(width))
        .bg(hue);
    let percent = progress.percent();
    let bar = div()
        .id(("terminal-progress", key))
        .debug_selector(|| "terminal-progress".into())
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .h(px(HEIGHT))
        .overflow_hidden()
        .when(share.is_none(), |el| el.bg(hue.opacity(0.3)))
        .role(gpui::Role::ProgressIndicator)
        .aria_label("Progress")
        .when_some(percent, |el, percent| {
            el.aria_numeric_value(f64::from(percent))
                .aria_value(SharedString::from(format!("{percent} %")))
        })
        .child(fill);
    crate::motion::fade_quick(("terminal-progress-in", key), bar).into_any_element()
}

/// The hint while files are carried over a terminal, in the view's own words: the drop is the
/// view's, so the overlay only shows, under no pointer.
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
    fn active_index_tracks_closes() {
        assert_eq!(active_after_close(2, 0, 3), 1); // close left of active
        assert_eq!(active_after_close(1, 1, 2), 1); // close active mid-list
        assert_eq!(active_after_close(2, 2, 2), 1); // close active at tail
        assert_eq!(active_after_close(0, 0, 0), 0); // last tab closed
    }
}

// A running program's progress (`OSC 9;4`) reaches its tab: the tab reads it
// again each time its view says something new of itself, and lets it go when the report ends.
#[cfg(test)]
mod progress_tests {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;

    use gpui::AppContext as _;
    use slopty_view_rpc::{RpcError, RpcReply, methods};

    use super::{ShownChanged, TerminalPanel};
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

    /// What a terminal's view says of its program reaches its tab: how far it has got, and the
    /// background it paints, which the panel tells the window's chrome of once per change.
    #[gpui::test]
    fn a_running_programs_progress_and_ground_reach_its_tab(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let _guard = runtime.enter();
        let dir = tempfile::tempdir().unwrap();
        let reported = Rc::new(RefCell::new(None::<Progress>));
        let painted = Rc::new(RefCell::new(None::<gpui::Hsla>));
        let heard: Heard = Rc::default();
        cx.update(|cx| {
            crate::settings::init(crate::settings::UiSettings::default(), dir.path(), cx);
            gpui_base::init(cx);
            cx.set_global(crate::theme::Theme::default());
            let (reported, painted, heard) =
                (Rc::clone(&reported), Rc::clone(&painted), Rc::clone(&heard));
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
                        ground: *painted.borrow(),
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
            Arc::new(Machine),
        ));
        let (panel, cx) = cx.add_window_view(|_, cx| {
            let state = cx.new(|_| {
                let mut state = crate::state::AppState::new();
                state.set_test_engine(engine);
                state
            });
            TerminalPanel::new_pane(state, cx)
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
        settle(cx, &|cx| tab(cx).is_some());
        assert_eq!(tab(cx).map(|(_, id, progress)| (id, progress)), Some(("7".to_owned(), None)));

        // The program reports 40 %: its view says so, and the tab shows it.
        *reported.borrow_mut() = Some(Progress::Share(0.4));
        cx.update(|_, cx| heard.borrow_mut().iter_mut().for_each(|changed| changed(cx)));
        settle(cx, &|cx| tab(cx).is_some_and(|(_, _, progress)| progress.is_some()));
        assert_eq!(tab(cx).and_then(|(_, _, progress)| progress), Some(Progress::Share(0.4)));
        // Its bar along the pane's top edge, as wide as its share.
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let bar = cx.debug_bounds("terminal-progress").expect("the progress bar");
        let body = cx.debug_bounds("terminal-body-bounds").expect("the body");
        assert_eq!((bar.top(), f32::from(bar.size.height)), (body.top(), 2.0), "{bar:?} {body:?}");
        assert_eq!(bar.size.width, body.size.width, "across the pane");

        // The report ends (the program cleared it, or the shell's next prompt did): it goes.
        *reported.borrow_mut() = None;
        cx.update(|_, cx| heard.borrow_mut().iter_mut().for_each(|changed| changed(cx)));
        settle(cx, &|cx| tab(cx).is_some_and(|(_, _, progress)| progress.is_none()));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("terminal-progress").is_none(), "and its bar with it");

        // The program paints its own background (OSC 11): the tab keeps it as the shown ground,
        // and the chrome hears it once; the same ground said again is no news.
        let told = Rc::new(std::cell::Cell::new(0_u32));
        let _told = cx.update(|_, cx| {
            let told = Rc::clone(&told);
            cx.subscribe(&panel, move |_, _: &ShownChanged, _| {
                told.set(told.get().saturating_add(1));
            })
        });
        let dark = gpui::hsla(0.55, 0.4, 0.08, 1.0);
        *painted.borrow_mut() = Some(dark);
        cx.update(|_, cx| heard.borrow_mut().iter_mut().for_each(|changed| changed(cx)));
        settle(cx, &|cx| panel.read_with(cx, |panel, cx| panel.shown_ground(cx).is_some()));
        assert_eq!(panel.read_with(cx, TerminalPanel::shown_ground), Some(dark));
        assert_eq!(told.get(), 1, "told once");
        cx.update(|_, cx| heard.borrow_mut().iter_mut().for_each(|changed| changed(cx)));
        cx.run_until_parked();
        assert_eq!(told.get(), 1, "the same ground again is no news");
    }

    /// A machine that opens terminal 7 and keeps what each open asked.
    struct Asked(Arc<parking_lot::Mutex<Vec<serde_json::Value>>>);

    #[async_trait::async_trait]
    impl slopty_view_rpc::RpcService for Asked {
        async fn handle(
            &self,
            method: &str,
            params: serde_json::Value,
        ) -> Result<RpcReply, RpcError> {
            if method == methods::OPEN_TERMINAL {
                self.0.lock().push(params.clone());
            }
            Machine.handle(method, params).await
        }
    }

    /// A pane's shell opens at the grid a view draws in the panel's body, not at 80 × 24: a
    /// shell told it is wider than it is drawn pads its first prompt past the edge. A panel
    /// whose body is never drawn opens at 80 × 24 once its wait is over.
    #[gpui::test]
    fn a_session_opens_at_the_grid_its_body_draws(cx: &mut gpui::TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let _guard = runtime.enter();
        let dir = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            crate::settings::init(crate::settings::UiSettings::default(), dir.path(), cx);
            gpui_base::init(cx);
            cx.set_global(crate::theme::Theme::default());
            crate::live::install(cx, |_, _, _| None, |_, _| None, |_, _, _| None);
            // A cell 8 × 16 points.
            #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "test")]
            crate::live::serve_fit(cx, |size, _, _| {
                ((f32::from(size.width) / 8.0) as u16, (f32::from(size.height) / 16.0) as u16)
            });
        });
        let asked = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let engine = crate::state::EngineHandle::from_test_client(slopty_view_rpc::memory_client(
            Arc::new(Asked(Arc::clone(&asked))),
        ));
        let state = cx.new(|_| {
            let mut state = crate::state::AppState::new();
            state.set_test_engine(engine);
            state
        });
        let unshown = cx.new(|cx| TerminalPanel::new(state.clone(), cx));
        // The pane comes into a window already its size, as a split's pane comes into its place.
        let (_, cx) = cx.add_window_view(|_, cx| TerminalPanel::new(state.clone(), cx));
        cx.simulate_resize(gpui::size(gpui::px(360.0), gpui::px(400.0)));
        let panel = cx.update(|window, cx| {
            window.replace_root(cx, |_, cx| TerminalPanel::new_pane(state, cx))
        });
        let mut opened = None;
        for _ in 0..300 {
            cx.run_until_parked();
            runtime.block_on(async {
                for _ in 0..20 {
                    tokio::task::yield_now().await;
                }
            });
            opened = asked.lock().first().cloned();
            if opened.is_some() {
                break;
            }
        }
        let opened = opened.expect("the tab's session opened");
        let fitted = panel.read_with(cx, |panel, _| panel.fitted.as_ref().map(|(.., grid)| *grid));
        let (cols, rows) = fitted.expect("the body measured");
        assert_eq!(cols, 45, "the body's width over the cell's");
        assert_eq!(
            (opened["cols"].as_u64(), opened["rows"].as_u64()),
            (Some(45), Some(rows.into()))
        );

        let grid = Rc::new(RefCell::new(None));
        cx.update(|_, cx| {
            let (panel, grid) = (unshown.downgrade(), Rc::clone(&grid));
            cx.spawn(async move |cx| {
                *grid.borrow_mut() = Some(TerminalPanel::grid(&panel, cx).await);
            })
            .detach();
        });
        cx.run_until_parked();
        assert_eq!(*grid.borrow(), None, "waits for the body's first draw");
        cx.executor().advance_clock(super::FIT_WAIT);
        cx.run_until_parked();
        assert_eq!(*grid.borrow(), Some(super::UNFITTED), "never drawn");
    }
}

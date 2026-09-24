//! What the actions do: opening things, closing and taking back, naming, moving the focus and
//! the columns, the terminal text size.

use gpui::{AppContext as _, Context, Entity, Window};
use gpui_kit::component::input::{InputEvent, InputState};
use slopty_client::layout::{DropTarget, TileRef, WorkerKey};
use slopty_core::{ItemId, SessionId};
use slopty_proto::ClientMsg;
use slopty_proto::items::{Item, ItemKind, ItemOp};
use slopty_proto::screen::{CaptureTarget, ScreenRequest};
use slopty_proto::terminal::{OpenSession, SessionKind, TermRequest, TermSize};

use super::actions::{
    AddWindow, CenterColumn, CloseItem, ConsumeOrExpelLeft, ConsumeOrExpelRight, CycleWidth,
    CycleWidthBack, FocusColumn, FocusColumnFirst, FocusColumnLast, FocusColumnLeft,
    FocusColumnRight, FocusDown, FocusUp, FontLarger, FontReset, FontSmaller, FullscreenTile,
    MaximizeColumn, MoveColumnLeft, MoveColumnRight, MoveDown, MoveUp, NarrowColumn, NewAgent,
    NewNote, NewTerminal, RenameItem, ToggleMute, ToggleOverview, ToggleStats, ToggleTabbed,
    UndoClose, WidenColumn,
};
use super::toast::ToastKind;
use super::{AGENT_COMMAND, ClosedTile, KeyTarget, Rename, UNDO_CLOSE, WorkspaceView};
use crate::file::LineMove;
use crate::screen::ScreenView;
use crate::terminal::TerminalView;

/// The smallest terminal text ⌘- goes to, in points.
pub(super) const FONT_MIN: f32 = 8.0;
/// The largest terminal text ⌘= goes to, in points.
pub(super) const FONT_MAX: f32 = 32.0;

impl WorkspaceView {
    // ----- focus -----------------------------------------------------------------------------

    /// Focus a tile: the layout moves to it, and its terminal (if it is one) takes the keyboard.
    pub fn focus_tile(&mut self, tile: TileRef, cx: &mut Context<Self>) {
        self.tick();
        self.layout.focus(tile);
        self.after_focus_moved(cx);
        self.layout_touched(cx);
        cx.notify();
    }

    /// The layout's focus moved (a key, a click, a close): note the shell as the one last
    /// used, clear its "finished" badge, and hand the keyboard to what now has the focus.
    pub(super) fn after_focus_moved(&mut self, cx: &mut Context<Self>) {
        let Some(tile) = self.focused() else {
            self.pending_focus_self = true;
            return;
        };
        match self.item(tile).map(|i| i.kind.clone()) {
            Some(ItemKind::Terminal { session }) => {
                self.touch_session(session);
                self.finished.remove(&session);
                self.pending_focus = Some(session);
            }
            // A remote window takes the keyboard only when clicked: its chords are the
            // worker's, and a key walk through the strip must not land in one by accident.
            Some(_) | None => self.pending_focus_self = true,
        }
        cx.notify();
    }

    /// Remember that `session` was just focused, so a "run in shell" goes to the shell the
    /// human was last in rather than whichever opened last.
    fn touch_session(&mut self, session: SessionId) {
        if let Some(at) = self.shell_recency.iter().position(|s| *s == session) {
            let s = self.shell_recency.remove(at);
            self.shell_recency.push(s);
        }
    }

    /// Bring a session's terminal into view, focus it and give it the keyboard (the "go"
    /// button on a waiting badge: the reply has to be typed).
    pub fn reveal_session(&mut self, session: SessionId, cx: &mut Context<Self>) {
        let Some(tile) = self.tile_of_session(session) else { return };
        self.focus_tile(tile, cx);
        self.pending_focus = Some(session);
    }

    /// Focus the tile showing `item`.
    pub(super) fn go_to(&mut self, item: ItemId, cx: &mut Context<Self>) {
        if let Some(tile) = self.tile_of(item) {
            self.focus_tile(tile, cx);
        }
    }

    /// The user activated a notification: it reveals the session.
    pub fn notification_response(&mut self, session: SessionId, cx: &mut Context<Self>) {
        self.reveal_session(session, cx);
    }

    /// Run a layout action at the clock, then follow the focus and save.
    fn layout_action(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut slopty_client::layout::Layout),
    ) {
        self.tick();
        let before = self.focused();
        f(&mut self.layout);
        if self.focused() != before {
            self.after_focus_moved(cx);
        }
        self.layout_touched(cx);
        cx.notify();
    }

    pub(super) fn register_layout_actions<T: gpui::InteractiveElement>(
        el: T,
        cx: &Context<Self>,
    ) -> T {
        el.on_action(cx.listener(|this, _: &FocusColumnLeft, _w, cx| {
            this.layout_action(cx, slopty_client::layout::Layout::focus_column_left);
        }))
        .on_action(cx.listener(|this, _: &FocusColumnRight, _w, cx| {
            this.layout_action(cx, slopty_client::layout::Layout::focus_column_right);
        }))
        .on_action(cx.listener(|this, _: &FocusColumnFirst, _w, cx| {
            this.layout_action(cx, slopty_client::layout::Layout::focus_column_first);
        }))
        .on_action(cx.listener(|this, _: &FocusColumnLast, _w, cx| {
            this.layout_action(cx, slopty_client::layout::Layout::focus_column_last);
        }))
        .on_action(cx.listener(|this, a: &FocusColumn, _w, cx| {
            let index = a.index;
            this.layout_action(cx, |l| l.focus_column(index));
        }))
        .on_action(cx.listener(|this, _: &FocusUp, _w, cx| {
            this.layout_action(cx, slopty_client::layout::Layout::focus_window_or_workspace_up);
        }))
        .on_action(cx.listener(|this, _: &FocusDown, _w, cx| {
            this.layout_action(cx, slopty_client::layout::Layout::focus_window_or_workspace_down);
        }))
        .on_action(cx.listener(|this, _: &MoveColumnLeft, _w, cx| {
            this.layout_action(cx, slopty_client::layout::Layout::move_column_left);
        }))
        .on_action(cx.listener(|this, _: &MoveColumnRight, _w, cx| {
            this.layout_action(cx, slopty_client::layout::Layout::move_column_right);
        }))
        .on_action(cx.listener(|this, _: &MoveUp, _w, cx| {
            this.layout_action(
                cx,
                slopty_client::layout::Layout::move_window_up_or_to_workspace_up,
            );
        }))
        .on_action(cx.listener(|this, _: &MoveDown, _w, cx| {
            this.layout_action(
                cx,
                slopty_client::layout::Layout::move_window_down_or_to_workspace_down,
            );
        }))
        .on_action(cx.listener(|this, _: &ConsumeOrExpelLeft, _w, cx| {
            this.layout_action(cx, slopty_client::layout::Layout::consume_or_expel_window_left);
        }))
        .on_action(cx.listener(|this, _: &ConsumeOrExpelRight, _w, cx| {
            this.layout_action(cx, slopty_client::layout::Layout::consume_or_expel_window_right);
        }))
        .on_action(cx.listener(|this, _: &CycleWidth, _w, cx| {
            this.width_action(cx, |l| l.switch_preset_width(true));
        }))
        .on_action(cx.listener(|this, _: &CycleWidthBack, _w, cx| {
            this.width_action(cx, |l| l.switch_preset_width(false));
        }))
        .on_action(cx.listener(|this, _: &NarrowColumn, _w, cx| {
            this.width_action(cx, |l| l.set_width_delta(-10.0));
        }))
        .on_action(cx.listener(|this, _: &WidenColumn, _w, cx| {
            this.width_action(cx, |l| l.set_width_delta(10.0));
        }))
        .on_action(cx.listener(|this, _: &MaximizeColumn, _w, cx| {
            this.width_action(cx, slopty_client::layout::Layout::toggle_full_width);
        }))
        .on_action(cx.listener(|this, _: &FullscreenTile, _w, cx| {
            this.width_action(cx, slopty_client::layout::Layout::toggle_fullscreen);
        }))
        .on_action(cx.listener(|this, _: &CenterColumn, _w, cx| {
            this.layout_action(cx, slopty_client::layout::Layout::center_column);
        }))
        .on_action(cx.listener(|this, _: &ToggleTabbed, _w, cx| {
            this.layout_action(cx, slopty_client::layout::Layout::toggle_tabbed);
        }))
        .on_action(cx.listener(|this, _: &ToggleOverview, _w, cx| {
            this.layout_action(cx, slopty_client::layout::Layout::toggle_overview);
        }))
        .on_action(cx.listener(|this, _: &FontLarger, _w, cx| this.font_by(1.0, cx)))
        .on_action(cx.listener(|this, _: &FontSmaller, _w, cx| this.font_by(-1.0, cx)))
        .on_action(cx.listener(|this, _: &FontReset, _w, cx| {
            this.font_delta = 0.0;
            this.apply_font(cx);
        }))
    }

    /// A width change: the layout's, then the remote windows of the column asked to take the
    /// size their tile now has.
    fn width_action(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut slopty_client::layout::Layout),
    ) {
        let before = self.column_sizes();
        self.layout_action(cx, f);
        self.resize_remote_windows(&before, cx);
    }

    /// Every remote window tile's target size now, to compare after a width change.
    pub(super) fn column_sizes(&self) -> Vec<(ItemId, (f32, f32))> {
        let frame = self.layout.frame();
        frame
            .tiles
            .iter()
            .filter(|p| self.screens.contains_key(&p.tile.item))
            .map(|p| (p.tile.item, (p.target.w, p.target.h)))
            .collect()
    }

    /// A remote window whose tile changed size is asked to take the new size, at the scale the
    /// tile drew it at before: a tile made twice as wide asks for a window twice as wide. The
    /// worker's answer comes back as `Geometry`. A display cannot be resized, and is
    /// letterboxed in its tile instead.
    pub(super) fn resize_remote_windows(
        &self,
        before: &[(ItemId, (f32, f32))],
        cx: &Context<Self>,
    ) {
        let after = self.column_sizes();
        for (id, (w0, h0)) in before {
            let Some((_, (w1, h1))) = after.iter().find(|(i, _)| i == id) else { continue };
            if (w1 - w0).abs() < 1.0 && (h1 - h0).abs() < 1.0 {
                continue;
            }
            let Some(view) = self.screens.get(id).map(|v| v.read(cx)) else { continue };
            if !matches!(view.target(), CaptureTarget::Window(_)) {
                continue;
            }
            let (native_w, native_h) = view.size();
            let header = super::tile::HEADER_H;
            let scale = |native: u32, from: f32, to: f32| {
                #[expect(clippy::cast_precision_loss, reason = "pixel counts are small")]
                let per_point = native as f32 / (from.max(1.0));
                #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "≥ 1")]
                let out = (to * per_point).round().max(1.0) as u32;
                out
            };
            let width = scale(native_w, *w0, *w1);
            let height = scale(native_h, h0 - header, h1 - header);
            if (width, height) == (native_w, native_h) {
                continue;
            }
            if let Some(tile) = self.tile_of(*id) {
                self.send(
                    tile.worker,
                    ClientMsg::Screen(ScreenRequest::Resize {
                        stream: view.stream(),
                        width,
                        height,
                    }),
                );
            }
        }
    }

    fn font_by(&mut self, delta: f32, cx: &mut Context<Self>) {
        let size = self.base_theme.typography.mono_size + self.font_delta + delta;
        if !(FONT_MIN..=FONT_MAX).contains(&size) {
            return;
        }
        self.font_delta += delta;
        self.apply_font(cx);
    }

    // ----- the focused tile ------------------------------------------------------------------

    /// The terminal of the focused tile, if it is one.
    #[must_use]
    pub fn active_terminal(&self) -> Option<Entity<TerminalView>> {
        match self.item(self.focused()?)?.kind {
            ItemKind::Terminal { session } => self.terminals.get(&session).cloned(),
            _ => None,
        }
    }

    /// The stream view of the focused tile, if it is a window or display.
    #[must_use]
    pub fn active_screen(&self) -> Option<Entity<ScreenView>> {
        let tile = self.focused()?;
        match self.item(tile)?.kind {
            ItemKind::Window { .. } | ItemKind::Display { .. } => {
                self.screens.get(&tile.item).cloned()
            }
            _ => None,
        }
    }

    /// What the phone key bar drives: the focused terminal, or the focused remote window.
    #[must_use]
    pub fn active_key_target(&self) -> Option<KeyTarget> {
        if let Some(t) = self.active_terminal() {
            return Some(KeyTarget::Terminal(t));
        }
        self.active_screen().map(KeyTarget::Screen)
    }

    /// ⌘⇧I: the stats overlay on every remote window.
    pub fn toggle_stats(&mut self, _: &ToggleStats, _window: &mut Window, cx: &mut Context<Self>) {
        self.show_stats = !self.show_stats;
        for view in self.screens.values() {
            view.update(cx, |v, cx| v.set_hud(self.show_stats, cx));
        }
        cx.notify();
    }

    /// ⌘⇧M: silence or resume the focused remote window's audio (this client only).
    pub fn toggle_mute(&mut self, _: &ToggleMute, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = self.active_screen() {
            view.read(cx).toggle_mute();
            cx.notify();
        }
    }

    /// Drive the terminal in `tile` from here: take the PTY size.
    pub fn take_over(&mut self, tile: TileRef, cx: &mut Context<Self>) {
        let Some(ItemKind::Terminal { session }) = self.item(tile).map(|i| i.kind.clone()) else {
            return;
        };
        if let Some(view) = self.terminals.get(&session) {
            view.read(cx).drive();
        }
        self.focus_tile(tile, cx);
    }

    // ----- shells ------------------------------------------------------------------------------

    /// `session` is a plain shell: a terminal session (not one the worker drives as an agent),
    /// drawn here, and one no coding agent has been seen in.
    pub(super) fn is_shell(&self, session: SessionId) -> bool {
        self.terminals.contains_key(&session)
            && !self.agents.contains_key(&session)
            && self.summary(session).is_some_and(|s| s.kind == SessionKind::Terminal)
    }

    /// The shell a fenced block runs in: the most recently focused one, else the newest.
    pub(super) fn run_target(&self) -> Option<SessionId> {
        self.shell_recency.iter().rev().copied().find(|s| self.is_shell(*s))
    }

    /// Tell every note whether there is a shell to run a fenced block in.
    pub(super) fn update_run_targets(&self, cx: &mut Context<Self>) {
        let can = self.run_target().is_some();
        for note in self.notes.values() {
            note.update(cx, |n, cx| n.set_can_run(can, cx));
        }
    }

    /// A "run" button on a fenced block was pressed: reveal the shell it goes to and type the
    /// code into it — a paste, then ↩ once.
    pub fn run_in_shell(&mut self, code: String, cx: &mut Context<Self>) {
        let Some(target) = self.run_target() else { return };
        self.reveal_session(target, cx);
        if let Some(view) = self.terminals.get(&target).cloned() {
            view.update(cx, |v, cx| v.run_text(code, cx));
        }
    }

    // ----- opening -----------------------------------------------------------------------------

    /// ⌘T: a shell on the focused tile's worker, in the focused shell's directory.
    pub fn new_terminal(&mut self, _: &NewTerminal, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.context_worker() else { return };
        let cwd = self.active_cwd();
        self.open_session_on(key, cwd, Vec::new(), None, cx);
    }

    /// ⌘⇧T: a terminal running Claude Code. The bare name resolves on the worker through the
    /// user's login shell.
    pub fn new_agent(&mut self, _: &NewAgent, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.context_worker() else { return };
        let cwd = self.active_cwd();
        let command = vec![AGENT_COMMAND.to_owned()];
        self.open_session_on(key, cwd, command, Some(AGENT_COMMAND.to_owned()), cx);
    }

    /// Open a session running `command` (the login shell when empty) on the context worker.
    /// The self-test socket's way to put load in the workspace.
    pub fn open_command(&self, command: Vec<String>, cx: &mut Context<Self>) {
        let Some(key) = self.context_worker() else { return };
        let title = command.first().cloned();
        let cwd = self.active_cwd();
        self.open_session_on(key, cwd, command, title, cx);
    }

    /// Open a session on `key` in `cwd` (the worker's default when `None`; `~` its home). The
    /// worker makes its item, and the item's arrival (ours) opens a column right of the focus.
    pub(super) fn open_session_on(
        &self,
        key: WorkerKey,
        cwd: Option<String>,
        command: Vec<String>,
        title: Option<String>,
        cx: &mut Context<Self>,
    ) {
        tracing::debug!(?command, ?cwd, %key, "open session");
        self.send(
            key,
            ClientMsg::OpenSession(OpenSession {
                size: TermSize::default(),
                cwd,
                command,
                env: Vec::new(),
                title,
                attach: false,
            }),
        );
        cx.notify();
    }

    /// Where a session opened from the keyboard starts: the focused terminal's directory
    /// when there is one, else the worker's default.
    pub(super) fn active_cwd(&self) -> Option<String> {
        self.focused().and_then(|t| self.item(t)).and_then(|item| self.cwd_of(item))
    }

    /// The working directory of an item: a terminal's session's, a file card's directory.
    pub(super) fn cwd_of(&self, item: &Item) -> Option<String> {
        match &item.kind {
            ItemKind::Terminal { session } => self.summary(*session).and_then(|s| s.cwd.clone()),
            // A file card's directory, when the path is spelled from the root.
            ItemKind::File { path } if path.starts_with('/') => path
                .rsplit_once('/')
                .map(|(dir, _)| if dir.is_empty() { "/" } else { dir }.to_owned()),
            _ => None,
        }
    }

    /// ⌘⇧N: an empty note right of the focused column, focused and editing.
    pub fn new_note(&mut self, _: &NewNote, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.context_worker() else { return };
        let id = self.put_note(key, String::new(), cx);
        self.pending_focus_note = Some(id);
    }

    fn put_note(&mut self, key: WorkerKey, text: String, cx: &mut Context<Self>) -> ItemId {
        let item =
            Item { id: ItemId::new(), kind: ItemKind::Note { text }, sleeping: false, name: None };
        let id = item.id;
        self.propose(key, ItemOp::Upsert(item), cx);
        id
    }

    /// A block saved as a note: a note with `text` beside the shell, focused but not editing,
    /// since its content is what was saved, not what is about to be typed.
    pub(super) fn note_beside(&mut self, session: SessionId, text: String, cx: &mut Context<Self>) {
        let Some(shell) = self.tile_of_session(session) else { return };
        // A new tile opens right of the focused column: the shell's, once it has the focus.
        self.tick();
        self.layout.focus(shell);
        self.put_note(shell.worker, text, cx);
    }

    /// A note's editor settled: write its text into the registry.
    pub(super) fn commit_note(&mut self, id: ItemId, text: String, cx: &mut Context<Self>) {
        let Some(tile) = self.tile_of(id) else { return };
        let Some(mut item) = self.item(tile).cloned() else { return };
        item.kind = ItemKind::Note { text };
        self.propose(tile.worker, ItemOp::Upsert(item), cx);
    }

    /// ⌘O: ask the context worker for its windows, then show the picker.
    pub fn add_window(&mut self, _: &AddWindow, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.context_worker() else { return };
        if let Some(w) = self.workers.get_mut(&key) {
            w.picker_wanted = true;
            w.send(ClientMsg::Screen(ScreenRequest::List));
        }
        cx.notify();
    }

    /// Add the context worker's first display, as picking it in the picker would.
    pub fn add_first_display(&mut self, cx: &mut Context<Self>) {
        let Some(key) = self.context_worker() else { return };
        if let Some(w) = self.workers.get_mut(&key) {
            w.display_wanted = true;
            w.send(ClientMsg::Screen(ScreenRequest::List));
        }
        cx.notify();
    }

    /// Put a window or display item on `key`; the reconcile opens its stream.
    pub(super) fn add_screen_item(
        &mut self,
        key: WorkerKey,
        target: CaptureTarget,
        title: String,
        cx: &mut Context<Self>,
    ) {
        let kind = match target {
            CaptureTarget::Window(window) => ItemKind::Window { window },
            CaptureTarget::Display(display) => ItemKind::Display { display },
        };
        let item = Item { id: ItemId::new(), kind, sleeping: false, name: None };
        self.titles.insert(item.id, title);
        self.propose(key, ItemOp::Upsert(item), cx);
    }

    /// A file card for `path` on `key` (the context worker when `None`): an existing card for
    /// it is focused, else a new one opens right of the focus and asks the worker for the text.
    /// `line` (1-based) is where the card lands.
    pub fn open_file_on(
        &mut self,
        key: Option<WorkerKey>,
        path: &str,
        line: Option<u32>,
        cx: &mut Context<Self>,
    ) {
        let Some(key) = key.or_else(|| self.context_worker()) else { return };
        let existing = self.workers.get(&key).and_then(|w| {
            w.doc.items().find_map(|i| match &i.kind {
                ItemKind::File { path: p } if p == path => Some(i.id),
                _ => None,
            })
        });
        let id = if let Some(id) = existing {
            self.request_file(id);
            self.go_to(id, cx);
            id
        } else {
            let item = Item {
                id: ItemId::new(),
                kind: ItemKind::File { path: path.to_owned() },
                sleeping: false,
                name: None,
            };
            let id = item.id;
            tracing::info!(%id, %path, ?line, "open file card");
            self.propose(key, ItemOp::Upsert(item), cx);
            id
        };
        match self.files.get(&id) {
            Some(view) => view.update(cx, |v, cx| v.focus_line(line, cx)),
            // The view is made on the next frame; it lands there then.
            None => {
                if let Some(line) = line {
                    self.file_focus.insert(id, line);
                }
            }
        }
        cx.notify();
    }

    /// A file card on the context worker (the self-test socket's and the palette's way).
    pub fn open_file(&mut self, path: &str, line: Option<u32>, cx: &mut Context<Self>) {
        self.open_file_on(None, path, line, cx);
    }

    /// `path` made absolute against the session's directory as the worker last reported it;
    /// a path with no directory known stays as it is.
    pub(super) fn absolute_in_session(&self, session: SessionId, path: &str) -> String {
        if path.starts_with('/') {
            return path.to_owned();
        }
        match self.summary(session).and_then(|s| s.cwd.as_deref()) {
            Some(cwd) => format!("{}/{path}", cwd.trim_end_matches('/')),
            None => path.to_owned(),
        }
    }

    /// `path` made absolute against the focused shell's directory, when it is a shell; `~` is
    /// left for the worker, whose home it names.
    pub(super) fn absolute_in_active_shell(&self, path: &str) -> String {
        let session = self.focused().and_then(|t| self.item(t)).and_then(|i| match i.kind {
            ItemKind::Terminal { session } => Some(session),
            _ => None,
        });
        match session {
            Some(session) if !path.starts_with('~') => self.absolute_in_session(session, path),
            _ => path.to_owned(),
        }
    }

    /// ↑/↓/⇞/⇟/Home/End with a file card focused: its reading line moves. Nothing while an
    /// overlay has the keys.
    pub(super) fn move_file_line(&self, mv: LineMove, cx: &mut Context<Self>) {
        if self.palette.is_some() || self.picker.is_some() {
            return;
        }
        if let Some(view) = self.active_item().and_then(|id| self.files.get(&id)).cloned() {
            view.update(cx, |v, cx| v.move_line(mv, cx));
        }
    }

    /// The card's "edit" pill: the file in `$EDITOR` in the shell the human was last in, at
    /// the line being read.
    pub(super) fn edit_file(&mut self, id: ItemId, cx: &mut Context<Self>) {
        let Some(ItemKind::File { path }) =
            self.tile_of(id).and_then(|t| self.item(t)).map(|i| i.kind.clone())
        else {
            return;
        };
        let line = self.files.get(&id).and_then(|v| v.read(cx).reading_line());
        self.run_in_shell(crate::terminal::url::editor_command(&path, line), cx);
    }

    /// Ask the worker for a file card's text (again).
    pub(super) fn request_file(&self, id: ItemId) {
        let Some(tile) = self.tile_of(id) else { return };
        if let Some(ItemKind::File { path }) = self.item(tile).map(|i| &i.kind) {
            self.send(tile.worker, ClientMsg::ReadFile { path: path.clone() });
        }
    }

    /// ⌘F with the workspace (not a terminal) focused: find in the focused terminal, or in
    /// the focused file card.
    pub fn find_in_active(
        &mut self,
        action: &crate::terminal::Find,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(view) = self.active_terminal() {
            view.update(cx, |view, cx| view.find(action, window, cx));
        } else if let Some(view) = self.active_item().and_then(|id| self.files.get(&id)).cloned() {
            view.update(cx, |view, cx| view.find(window, cx));
        }
    }

    // ----- closing -----------------------------------------------------------------------------

    /// ⌘W: close the focused tile.
    pub fn close_item(&mut self, _: &CloseItem, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(tile) = self.focused() else { return };
        let Some(item) = self.item(tile).cloned() else { return };
        tracing::debug!(item = %tile.item, kind = ?item.kind, "close tile");
        match item.kind {
            // A live session closes through the worker, which removes the item; a shell whose
            // command still runs asks first (its view's bar), and closes on `CloseConfirmed`.
            ItemKind::Terminal { session } if self.summary(session).is_some() => {
                let asked = self
                    .terminals
                    .get(&session)
                    .is_some_and(|view| view.update(cx, TerminalView::ask_close));
                if !asked {
                    self.close_shell(session, cx);
                }
            }
            // An ended shell has no session to keep and nothing a worker could replay.
            ItemKind::Terminal { .. } => self.propose(tile.worker, ItemOp::Remove(tile.item), cx),
            // A note's editor commits on a timer: take the last keystrokes from the field.
            ItemKind::Note { .. } => {
                let mut item = item;
                if let Some(text) = self.notes.get(&tile.item).map(|v| v.read(cx).live_text(cx)) {
                    item.kind = ItemKind::Note { text };
                }
                self.remember_closed(tile, item, None, cx);
            }
            ItemKind::Window { .. } | ItemKind::Display { .. } | ItemKind::File { .. } => {
                self.remember_closed(tile, item, None, cx);
            }
        }
        cx.notify();
    }

    /// Take a live shell off, its session kept for [`UNDO_CLOSE`]. A shell without a view has
    /// nothing to keep and closes at once.
    pub(super) fn close_shell(&mut self, session: SessionId, cx: &mut Context<Self>) {
        let tile = self.tile_of_session(session);
        let item = tile.and_then(|t| self.item(t)).cloned();
        let (Some(tile), Some(item), true) = (tile, item, self.terminals.contains_key(&session))
        else {
            self.send_session(session, ClientMsg::Term { session, req: TermRequest::Close });
            return;
        };
        self.remember_closed(tile, item, Some(session), cx);
    }

    /// Take a tile off and offer it back for [`UNDO_CLOSE`].
    fn remember_closed(
        &mut self,
        tile: TileRef,
        item: Item,
        session: Option<SessionId>,
        cx: &mut Context<Self>,
    ) {
        self.closed_seq = self.closed_seq.wrapping_add(1);
        let seq = self.closed_seq;
        let title = self.card_title(tile, &item, cx);
        let at = self.layout.position(tile);
        self.closed.push(ClosedTile { tile, item, at, session, seq });
        self.propose(tile.worker, ItemOp::Remove(tile.item), cx);
        self.show_toast_for(ToastKind::Closed { seq, title }, UNDO_CLOSE, cx);
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(UNDO_CLOSE).await;
            let _gone = this.update(cx, |this, cx| this.forget_closed(seq, cx));
        })
        .detach();
    }

    /// [`UNDO_CLOSE`] passed: a shell's session is closed by the worker and its view goes.
    fn forget_closed(&mut self, seq: u64, cx: &mut Context<Self>) {
        let Some(ix) = self.closed.iter().position(|c| c.seq == seq) else { return };
        let closed = self.closed.remove(ix);
        if let Some(session) = closed.session {
            if self.summary(session).is_some() {
                self.send(closed.tile.worker, ClientMsg::Term { session, req: TermRequest::Close });
            }
            self.terminals.remove(&session);
        }
        self.dismiss_closed_toast(Some(seq));
        cx.notify();
    }

    /// ⌘Z: the tile closed last comes back where it was.
    pub fn undo_close(&mut self, _: &UndoClose, _window: &mut Window, cx: &mut Context<Self>) {
        self.take_back(None, cx);
    }

    /// Put back the closing `seq` (the toast's), or the latest.
    pub(super) fn take_back(&mut self, seq: Option<u64>, cx: &mut Context<Self>) {
        let ix = match seq {
            Some(seq) => self.closed.iter().position(|c| c.seq == seq),
            None => self.closed.len().checked_sub(1),
        };
        let Some(ix) = ix else { return };
        let closed = self.closed.remove(ix);
        self.dismiss_closed_toast(None);
        tracing::debug!(item = %closed.item.id, session = ?closed.session, "tile taken back");
        let tile = closed.tile;
        self.propose(tile.worker, ItemOp::Upsert(closed.item), cx);
        if let Some(at) = closed.at {
            self.tick();
            self.layout.move_tile(
                tile,
                DropTarget::NewColumn { workspace: at.workspace, index: at.column },
            );
            self.layout.focus(tile);
            self.layout_touched(cx);
        }
        match closed.session {
            Some(session) => self.reveal_session(session, cx),
            None => self.focus_tile(tile, cx),
        }
    }

    // ----- naming ------------------------------------------------------------------------------

    /// ⌘E (or a double-click on a header): name the focused tile.
    pub fn rename_item(&mut self, _: &RenameItem, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tile) = self.focused() {
            self.start_rename(tile, window, cx);
        }
    }

    pub(super) fn start_rename(
        &mut self,
        tile: TileRef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(item) = self.item(tile).cloned() else { return };
        let placeholder = self.derived_title(&item, cx);
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(placeholder)
                .default_value(item.name.clone().unwrap_or_default())
        });
        let subscription = cx.subscribe(&input, |this, _input, event, cx| match event {
            InputEvent::PressEnter { .. } => this.finish_rename(true, true, cx),
            // A click elsewhere: the field goes, the name stays, whoever was clicked keeps
            // the keyboard.
            InputEvent::Blur => this.finish_rename(false, false, cx),
            InputEvent::Change | InputEvent::Focus => {}
        });
        let return_to = window.focused(cx);
        self.tick();
        self.layout.focus(tile);
        self.rename = Some(Rename { tile, input, return_to, _subscription: subscription });
        self.pending_focus_rename = true;
        cx.notify();
    }

    /// The name field closes. `keep` writes its text as the item's name (blank clears it);
    /// `back` gives the keyboard back to whoever had it before the field.
    pub(super) fn finish_rename(&mut self, keep: bool, back: bool, cx: &mut Context<Self>) {
        let Some(rename) = self.rename.take() else { return };
        if keep && let Some(mut item) = self.item(rename.tile).cloned() {
            let text = rename.input.read(cx).value().trim().to_owned();
            item.name = (!text.is_empty()).then_some(text);
            self.propose(rename.tile.worker, ItemOp::Upsert(item), cx);
        }
        if back {
            self.rename_return = rename.return_to.or_else(|| Some(self.focus.clone()));
        }
        cx.notify();
    }
}

//! What the actions do: opening things, closing and taking back, naming, moving the focus and
//! the columns, the terminal text size.

use std::time::Duration;

use gpui::{App, AppContext as _, Context, Entity, Window};
use gpui_kit::component::input::{InputEvent, InputState};
use slopty_client::layout::{DropTarget, TileRef, WorkerKey};
use slopty_core::{ItemId, SessionId, WallMs};
use slopty_proto::ClientMsg;
use slopty_proto::items::{Item, ItemKind, ItemOp};
use slopty_proto::screen::{CaptureTarget, ScreenRequest};
use slopty_proto::server::Os;
use slopty_proto::terminal::{OpenSession, TermRequest, TermSize};

use super::actions::{
    AddWindow, Applies, CenterColumn, CloseItem, ConsumeOrExpelLeft, ConsumeOrExpelRight,
    CycleWidth, FocusColumn, FocusColumnLeft, FocusColumnRight, FocusDown, FocusUp, FocusWorkspace,
    FocusWorkspaceDown, FocusWorkspaceUp, FontLarger, FontReset, FontSmaller, FullscreenTile,
    MaximizeColumn, MoveColumnLeft, MoveColumnRight, MoveColumnToFirst, MoveColumnToLast, MoveDown,
    MoveUp, NewNote, NewTerminal, RenameItem, ToggleMute, ToggleOverview, ToggleStats,
    ToggleTabbed, UndoClose,
};
use super::toast::ToastKind;
use super::{
    CLOSED_KEPT, ClosedTile, Field, IDLE_SHELL_KEPT, KeyTarget, Rename, Reshell, UNDO_CLOSE,
    WorkspaceView,
};
use crate::file::FileView;
use crate::palette::PaletteItem;
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
        self.note_recent(tile.item);
        self.navigated_to(tile);
        match self.item(tile).map(|i| i.kind.clone()) {
            Some(ItemKind::Terminal { session }) => {
                self.finished.remove(&session);
                self.pending_focus = Some(session);
            }
            // A file tile is an editor: the keyboard goes into its text, as into a shell.
            Some(ItemKind::File { .. }) => self.pending_focus_file = Some(tile.item),
            // A folder tile walks its rows by key, and looks again at what it holds.
            Some(ItemKind::Folder { .. }) => {
                self.pending_focus_folder = Some(tile.item);
                self.refresh_folder(tile.item, cx);
            }
            // A review walks its files and hunks by key.
            Some(ItemKind::Review { thread }) => self.pending_focus_review = Some(thread),
            // A thread's keyboard is its composer's.
            Some(ItemKind::Thread { .. }) => self.focus_thread_item(tile.item, cx),
            // A remote window takes the keyboard only when clicked: its chords are the
            // worker's, and a key walk through the strip must not land in one by accident.
            Some(_) | None => self.pending_focus_self = true,
        }
        cx.notify();
    }

    /// Give the keyboard back to where the focused tile keeps it (its shell, its editor, else
    /// the workspace), now: for whatever held it a moment (a menu, the settings, a dialog)
    /// and is gone. Handing it to the workspace's own handle left a shell's cursor hollow.
    pub fn return_keyboard(&self, window: &mut Window, cx: &mut Context<Self>) {
        let kind = self.focused().and_then(|tile| Some((tile, self.item(tile)?.kind.clone())));
        match kind {
            Some((_, ItemKind::Terminal { session }))
                if self.board_shown(session)
                    && let Some(board) = self.board_view(session).cloned() =>
            {
                board.update(cx, |v, cx| v.focus(window, cx));
            }
            Some((_, ItemKind::Terminal { session }))
                if let Some(view) = self.terminals.get(&session) =>
            {
                let handle = gpui::Focusable::focus_handle(view.read(cx), cx);
                window.focus(&handle, cx);
            }
            Some((tile, ItemKind::File { .. }))
                if let Some(view) = self.files.get(&tile.item).cloned() =>
            {
                view.update(cx, |v, cx| v.focus(window, cx));
            }
            Some((tile, ItemKind::Folder { .. }))
                if let Some(view) = self.folders.get(&tile.item).cloned() =>
            {
                view.update(cx, |v, cx| v.focus(window, cx));
            }
            Some((_, ItemKind::Review { thread })) if self.review_of(thread).is_some() => {
                self.focus_review(thread, window, cx);
            }
            Some((tile, ItemKind::Thread { .. }))
                if let Some(view) = self.thread_item(tile.item).cloned() =>
            {
                view.update(cx, |v, cx| v.focus(window, cx));
            }
            _ => window.focus(&self.focus, cx),
        }
    }

    /// Remember that tile `item` was just focused or just came: a "run in shell" goes to the
    /// shell the human was last in, and the palette lists tiles from the latest.
    pub(super) fn note_recent(&mut self, item: ItemId) {
        self.recency.retain(|r| *r != item);
        self.recency.push(item);
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

    /// The layout's actions: those about the focused column or tile only while a tile has the
    /// focus ([`Applies`]), the workspaces' and the text size's always.
    pub(super) fn register_layout_actions(
        el: gpui::Stateful<gpui::Div>,
        applies: Applies,
        cx: &Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        use gpui::InteractiveElement as _;
        use gpui::prelude::FluentBuilder as _;
        use slopty_client::layout::Layout;
        el.on_action(cx.listener(|this, _: &FocusUp, _w, cx| {
            this.layout_action(cx, Layout::focus_window_or_workspace_up);
        }))
        .on_action(cx.listener(|this, _: &FocusDown, _w, cx| {
            this.layout_action(cx, Layout::focus_window_or_workspace_down);
        }))
        .on_action(cx.listener(|this, _: &FocusWorkspaceUp, _w, cx| {
            this.layout_action(cx, Layout::focus_workspace_up);
        }))
        .on_action(cx.listener(|this, _: &FocusWorkspaceDown, _w, cx| {
            this.layout_action(cx, Layout::focus_workspace_down);
        }))
        .on_action(cx.listener(|this, a: &FocusWorkspace, _w, cx| {
            let index = a.index;
            this.layout_action(cx, |l| l.focus_workspace(index));
        }))
        .on_action(cx.listener(|this, _: &ToggleOverview, _w, cx| {
            this.layout_action(cx, Layout::toggle_overview);
        }))
        // A focused page zooms; everywhere else the text size moves.
        .on_action(cx.listener(|this, _: &FontLarger, _w, cx| {
            if !this.zoom_page(crate::browser::Zoom::In, cx) {
                this.font_by(1.0, cx);
            }
        }))
        .on_action(cx.listener(|this, _: &FontSmaller, _w, cx| {
            if !this.zoom_page(crate::browser::Zoom::Out, cx) {
                this.font_by(-1.0, cx);
            }
        }))
        .on_action(cx.listener(|this, _: &FontReset, _w, cx| {
            if !this.zoom_page(crate::browser::Zoom::Reset, cx) {
                this.font_delta = 0.0;
                this.apply_font(cx);
            }
        }))
        .when(applies.tile, |el| Self::register_column_actions(el, cx))
        .when(applies.page, |el| el.on_action(cx.listener(Self::inspect_page)))
        .when(applies.own_window, |el| el.on_action(cx.listener(Self::toggle_own_window)))
    }

    /// The actions on the focused column and tile.
    fn register_column_actions(
        el: gpui::Stateful<gpui::Div>,
        cx: &Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        use gpui::InteractiveElement as _;
        use slopty_client::layout::Layout;
        el.on_action(cx.listener(|this, _: &FocusColumnLeft, _w, cx| {
            this.layout_action(cx, Layout::focus_column_left);
        }))
        .on_action(cx.listener(|this, _: &FocusColumnRight, _w, cx| {
            this.layout_action(cx, Layout::focus_column_right);
        }))
        .on_action(cx.listener(|this, a: &FocusColumn, _w, cx| {
            let index = a.index;
            this.layout_action(cx, |l| l.focus_column(index));
        }))
        .on_action(cx.listener(|this, _: &MoveColumnLeft, _w, cx| {
            this.layout_action(cx, Layout::move_column_left);
        }))
        .on_action(cx.listener(|this, _: &MoveColumnRight, _w, cx| {
            this.layout_action(cx, Layout::move_column_right);
        }))
        .on_action(cx.listener(|this, _: &MoveUp, _w, cx| {
            this.layout_action(cx, Layout::move_window_up_or_to_workspace_up);
        }))
        .on_action(cx.listener(|this, _: &MoveDown, _w, cx| {
            this.layout_action(cx, Layout::move_window_down_or_to_workspace_down);
        }))
        .on_action(cx.listener(|this, _: &ConsumeOrExpelLeft, _w, cx| {
            this.layout_action(cx, Layout::consume_or_expel_window_left);
        }))
        .on_action(cx.listener(|this, _: &ConsumeOrExpelRight, _w, cx| {
            this.layout_action(cx, Layout::consume_or_expel_window_right);
        }))
        .on_action(cx.listener(|this, _: &CycleWidth, _w, cx| {
            this.width_action(cx, |l| l.switch_preset_width(true));
        }))
        .on_action(cx.listener(|this, _: &MaximizeColumn, _w, cx| {
            this.width_action(cx, Layout::toggle_full_width);
        }))
        .on_action(cx.listener(|this, _: &FullscreenTile, _w, cx| {
            this.width_action(cx, Layout::toggle_fullscreen);
        }))
        .on_action(cx.listener(|this, _: &CenterColumn, _w, cx| {
            this.layout_action(cx, Layout::center_column);
        }))
        .on_action(cx.listener(|this, _: &MoveColumnToFirst, _w, cx| {
            this.layout_action(cx, Layout::move_column_to_first);
        }))
        .on_action(cx.listener(|this, _: &MoveColumnToLast, _w, cx| {
            this.layout_action(cx, Layout::move_column_to_last);
        }))
        .on_action(cx.listener(|this, _: &ToggleTabbed, _w, cx| {
            this.layout_action(cx, Layout::toggle_tabbed);
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
            let header = self.theme.density.header;
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
                        scale: None,
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
        self.bar.forget_frame_time();
        for view in self.screens.values() {
            view.update(cx, |v, cx| v.set_hud(self.show_stats, cx));
        }
        cx.notify();
    }

    /// Trackpad mode for the active picture, when the palette runs it with the focus elsewhere:
    /// the focused picture takes the action itself.
    pub fn toggle_trackpad(
        &mut self,
        _: &crate::screen::ToggleTrackpad,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(view) = self.active_screen() {
            view.update(cx, ScreenView::toggle_trackpad);
        }
    }

    /// Gestures to the remote app for the active picture, when the palette runs it with the
    /// focus elsewhere: the focused picture takes the action itself.
    pub fn toggle_remote_gestures(
        &mut self,
        _: &crate::screen::ToggleRemoteGestures,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(view) = self.active_screen() {
            view.update(cx, ScreenView::toggle_remote_gestures);
        }
    }

    /// ⌘⇧M: silence or resume the sound of the focused remote window's worker (this client
    /// only).
    pub fn toggle_mute(&mut self, _: &ToggleMute, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = self.active_screen() {
            view.read(cx).toggle_mute();
            self.sound_changed(cx);
        }
    }

    /// Drive the terminal in `tile` from here: take the PTY size.
    pub fn take_over(&mut self, tile: TileRef, cx: &mut Context<Self>) {
        let Some(ItemKind::Terminal { session }) = self.item(tile).map(|i| i.kind.clone()) else {
            return;
        };
        if let Some(view) = self.terminals.get(&session) {
            view.update(cx, |view, cx| view.drive(cx));
        }
        self.focus_tile(tile, cx);
    }

    // ----- shells ------------------------------------------------------------------------------

    /// `session` is a plain shell: a live session drawn here that no coding agent has been seen in.
    pub(super) fn is_shell(&self, session: SessionId) -> bool {
        self.terminals.contains_key(&session)
            && !self.agents.contains_key(&session)
            && self.summary(session).is_some()
    }

    /// The shell a fenced block runs in: the most recently focused one, else the newest.
    pub(super) fn run_target(&self) -> Option<SessionId> {
        self.recency.iter().rev().find_map(|id| {
            let tile = self.tile_of(*id)?;
            let ItemKind::Terminal { session } = self.item(tile)?.kind else { return None };
            self.is_shell(session).then_some(session)
        })
    }

    /// Tell every file whether there is a shell to run a fenced block in.
    pub(super) fn update_run_targets(&self, cx: &mut Context<Self>) {
        let can = self.run_target().is_some();
        for file in self.files.values() {
            file.update(cx, |f, cx| f.set_can_run(can, cx));
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

    /// ⌘T: a shell on the focused tile's worker (or the one "+" chose), in the focused shell's
    /// directory when it is on that worker.
    pub fn new_terminal(&mut self, _: &NewTerminal, _window: &mut Window, cx: &mut Context<Self>) {
        let Some((key, cwd)) = self.new_tile_target() else { return };
        self.open_session_on(key, cwd, Vec::new(), None, cx);
    }

    /// Where a new tile goes, taking the choice "+" made: the worker, and the focused shell's
    /// directory when it is on that worker.
    fn new_tile_target(&mut self) -> Option<(WorkerKey, Option<String>)> {
        let chosen = self.new_on.take().filter(|k| self.workers.contains_key(k));
        let key = chosen.or_else(|| self.context_worker())?;
        let here = self.focused().is_some_and(|t| t.worker == key);
        Some((key, here.then(|| self.active_cwd()).flatten()))
    }

    /// A shell on `key`, in the active workspace: the empty workspace's rows, one per worker.
    pub(super) fn new_terminal_on(&mut self, key: WorkerKey, cx: &mut Context<Self>) {
        self.open_session_on(key, None, Vec::new(), None, cx);
    }

    /// Whether `key` is linked, so what is asked of it now is heard: one out of reach says so
    /// in words, naming `what` would have opened, rather than dropping it unseen.
    pub(super) fn reachable_for(
        &mut self,
        key: WorkerKey,
        what: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(w) = self.workers.get(&key) else { return false };
        if w.link.is_some() {
            return true;
        }
        let what = crate::palette::sentence_case(what);
        let text = format!("{what} did not open: {} is {}", w.name, w.status.text());
        self.show_notice(text, cx);
        false
    }

    /// Open a session running `command` (the login shell when empty) on the context worker.
    /// The self-test socket's way to put load in the workspace.
    pub fn open_command(&mut self, command: Vec<String>, cx: &mut Context<Self>) {
        let Some(key) = self.context_worker() else { return };
        let title = command.first().cloned();
        let cwd = self.active_cwd();
        self.open_session_on(key, cwd, command, title, cx);
    }

    /// Open a session on `key` in `cwd` (the worker's default when `None`; `~` its home). The
    /// worker makes its item, and the item's arrival (ours) opens a column right of the focus.
    /// A worker out of reach makes nothing, so that is said ([`Self::reachable_for`]).
    pub(super) fn open_session_on(
        &mut self,
        key: WorkerKey,
        cwd: Option<String>,
        command: Vec<String>,
        title: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let what = if command.is_empty() { "the terminal" } else { "the command" };
        if !self.reachable_for(key, what, cx) {
            return;
        }
        let request = self.next_open.get();
        self.next_open.set(request.wrapping_add(1));
        tracing::debug!(?command, ?cwd, %key, request, "open session");
        let spec = OpenSession {
            size: TermSize::default(),
            cwd,
            command,
            env: Vec::new(),
            title,
            attach: false,
        };
        self.send(key, ClientMsg::OpenSession { request, spec });
        cx.notify();
    }

    /// Where a session opened from the keyboard starts: the focused terminal's directory
    /// when there is one, else the worker's default.
    pub(super) fn active_cwd(&self) -> Option<String> {
        self.focused().and_then(|t| self.item(t)).and_then(|item| self.cwd_of(item))
    }

    /// The working directory of an item: a terminal's session's, a file tile's directory, the
    /// folder a folder tile is at.
    pub(super) fn cwd_of(&self, item: &Item) -> Option<String> {
        match &item.kind {
            ItemKind::Terminal { session } => self.summary(*session).and_then(|s| s.cwd.clone()),
            // A file tile's directory, when the path is spelled from the root.
            ItemKind::File { path } if path.starts_with('/') => path
                .rsplit_once('/')
                .map(|(dir, _)| if dir.is_empty() { "/" } else { dir }.to_owned()),
            ItemKind::Folder { path } if path.starts_with('/') => Some(path.clone()),
            _ => None,
        }
    }

    /// ⌘⇧N: a new note, which is a Markdown file: in the focused shell's directory on the
    /// target worker (else the worker's home), named for this moment, right of the focused
    /// column with its source taking the keys. Nothing is on disk until it is saved.
    pub fn new_note(&mut self, _: &NewNote, _window: &mut Window, cx: &mut Context<Self>) {
        let Some((key, cwd)) = self.new_tile_target() else { return };
        let Some(path) = note_path(cwd.as_deref(), WallMs::now(), local_offset()) else { return };
        if let Some(id) = self.show_file(Some(key), &path, None, cx) {
            self.pending_focus_file = Some(id);
        }
    }

    /// ⌘O: ask the context worker for its windows, and show the picker while it answers.
    pub fn add_window(&mut self, _: &AddWindow, _window: &mut Window, cx: &mut Context<Self>) {
        let Some((key, _)) = self.new_tile_target() else { return };
        // Asked of a worker out of reach, the list would come only once it is back, and its
        // picker then, unasked.
        if !self.reachable_for(key, "the window picker", cx) {
            return;
        }
        let Some(w) = self.workers.get_mut(&key) else { return };
        // A worker that cannot capture lists no window worth picking: say why rather than show
        // an empty picker (a Mac without Screen Recording, a Linux worker with no capture).
        if let Some(caps) = w.caps.as_ref().filter(|c| !c.can_capture) {
            let why = if caps.os == Os::MacOs {
                "Screen Recording is off"
            } else {
                "it has no screen capture"
            };
            let text = format!("{} can\u{2019}t share its screen: {why}", w.name);
            self.show_notice(text, cx);
            return;
        }
        w.picker_wanted = true;
        w.send(ClientMsg::Screen(ScreenRequest::List));
        self.show_picker_loading(key, cx);
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

    /// Put window `window` of the focused tile's worker in the strip, titled `title`, as
    /// picking it in the ⌘O picker does.
    pub fn pick_window(
        &mut self,
        window: slopty_core::WindowId,
        title: String,
        cx: &mut Context<Self>,
    ) {
        let Some(key) = self.context_worker() else { return };
        self.add_screen_item(key, CaptureTarget::Window(window), title, cx);
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
        let item =
            Item { id: ItemId::new(), kind, name: None, facts: std::collections::BTreeMap::new() };
        self.titles.insert(item.id, title);
        self.propose(key, ItemOp::Add(item), cx);
    }

    /// A file tile for `path` on `key` (the context worker when `None`): an existing tile for
    /// it is focused, else a new one opens right of the focus and asks the worker for the text.
    /// `line` (1-based) is where the tile lands.
    pub fn open_file_on(
        &mut self,
        key: Option<WorkerKey>,
        path: &str,
        line: Option<u32>,
        cx: &mut Context<Self>,
    ) {
        let _shown = self.show_file(key, path, line, cx);
    }

    /// [`Self::open_file_on`], saying which item shows the file.
    pub(super) fn show_file(
        &mut self,
        key: Option<WorkerKey>,
        path: &str,
        line: Option<u32>,
        cx: &mut Context<Self>,
    ) -> Option<ItemId> {
        let key = key.or_else(|| self.context_worker())?;
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
                name: None,
                facts: std::collections::BTreeMap::new(),
            };
            let id = item.id;
            tracing::info!(%id, %path, ?line, "open file tile");
            self.propose(key, ItemOp::Add(item), cx);
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
        Some(id)
    }

    /// A file tile on the context worker (the self-test socket's and the palette's way).
    pub fn open_file(&mut self, path: &str, line: Option<u32>, cx: &mut Context<Self>) {
        self.open_file_on(None, path, line, cx);
    }

    /// `path` made absolute against the session's directory as the worker last reported it;
    /// a path with no directory known stays as it is, and so does one under `~`, which the
    /// worker expands to its own home.
    pub(super) fn absolute_in_session(&self, session: SessionId, path: &str) -> String {
        if path.starts_with('/') || path == "~" || path.starts_with("~/") {
            return path.to_owned();
        }
        match self.summary(session).and_then(|s| s.cwd.as_deref()) {
            Some(cwd) => format!("{}/{path}", cwd.trim_end_matches('/')),
            None => path.to_owned(),
        }
    }

    /// `path` made absolute against the focused shell's directory, when it is a shell.
    pub(super) fn absolute_in_active_shell(&self, path: &str) -> String {
        let session = self.focused().and_then(|t| self.item(t)).and_then(|i| match i.kind {
            ItemKind::Terminal { session } => Some(session),
            _ => None,
        });
        match session {
            Some(session) => self.absolute_in_session(session, path),
            None => path.to_owned(),
        }
    }

    /// Ask the worker for a file tile's text (again).
    pub(super) fn request_file(&self, id: ItemId) {
        let Some(tile) = self.tile_of(id) else { return };
        if let Some(ItemKind::File { path }) = self.item(tile).map(|i| &i.kind) {
            self.send(tile.worker, ClientMsg::ReadFile { path: path.clone() });
        }
    }

    /// ⌘F with the workspace (not a terminal) focused: find in the focused terminal, or in
    /// the focused file tile.
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
        } else {
            self.find_in_page(window, cx);
        }
    }

    // ----- closing -----------------------------------------------------------------------------

    /// ⌘W: close the focused tile.
    pub fn close_item(&mut self, _: &CloseItem, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(tile) = self.focused() else { return };
        let Some(item) = self.item(tile).cloned() else {
            self.close_starting(tile, cx);
            return;
        };
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
            ItemKind::Window { .. }
            | ItemKind::Display { .. }
            | ItemKind::Browser { .. }
            | ItemKind::Folder { .. }
            | ItemKind::Review { .. }
            | ItemKind::Thread { .. } => {
                self.remember_closed(tile, item, None, cx);
            }
            // A file's edit lives in its editor, not in the registry: the editor waits with the
            // closed tile, so ⌘Z brings back the edit and not the disk's text.
            // A program waiting on the file is answered as "Done" would: saved, then told.
            ItemKind::File { .. } => {
                if let Some(view) = self.files.get(&tile.item).cloned() {
                    view.update(cx, FileView::finish_edit);
                }
                self.remember_closed(tile, item, None, cx);
            }
        }
        cx.notify();
    }

    /// Take a live shell off, its session kept for a while ([`Self::session_kept_for`]). A
    /// shell without a view has nothing to keep and closes at once, unless its worker is out of
    /// reach: then the tile goes now, and the session is closed when the worker is back.
    pub(super) fn close_shell(&mut self, session: SessionId, cx: &mut Context<Self>) {
        let tile = self.tile_of_session(session);
        let item = tile.and_then(|t| self.item(t)).cloned();
        let away =
            tile.is_some_and(|t| self.workers.get(&t.worker).is_some_and(|w| !w.is_linked()));
        let keep = self.terminals.contains_key(&session) || away;
        let (Some(tile), Some(item), true) = (tile, item, keep) else {
            self.send_session(session, ClientMsg::Term { session, req: TermRequest::Close });
            return;
        };
        self.remember_closed(tile, item, Some(session), cx);
    }

    /// How long closed shell `session` keeps running for ⌘Z to bring it back whole: a plain
    /// shell idle at its prompt runs nothing, so [`IDLE_SHELL_KEPT`]; one running a command, a
    /// program or an agent, only while its notice is up ([`UNDO_CLOSE`]), so what the person
    /// closed stops as they meant it to. An exited one has nothing left to keep.
    fn session_kept_for(&self, session: SessionId, cx: &App) -> Duration {
        let idle = self.plain_shell(session)
            && self.terminals.get(&session).is_some_and(|v| {
                let state = v.read(cx).state();
                state.exited().is_none() && !state.command_running()
            });
        if idle { IDLE_SHELL_KEPT } else { UNDO_CLOSE }
    }

    /// Whether `session` is the login shell and no agent: one that, once ended, a new shell in
    /// its directory stands in for.
    fn plain_shell(&self, session: SessionId) -> bool {
        self.summary(session).is_some_and(|s| s.command.is_empty() && s.agent.is_none())
            && self.session_agent(session).is_none()
    }

    /// Take a tile off, offer it back in a notice for [`UNDO_CLOSE`], and keep it among the
    /// last [`CLOSED_KEPT`] closed, for ⌘Z and the palette's "Reopen" to bring back with no
    /// clock running. A live shell's session runs on for [`Self::session_kept_for`]; after
    /// that a plain shell comes back as a new shell in its directory.
    pub(super) fn remember_closed(
        &mut self,
        tile: TileRef,
        item: Item,
        session: Option<SessionId>,
        cx: &mut Context<Self>,
    ) {
        self.closed_seq = self.closed_seq.wrapping_add(1);
        let seq = self.closed_seq;
        let title = self.tile_title(&item);
        let at = self.layout.position(tile);
        // A file tile's editor goes with it before the tile leaves, so its going is a close.
        let file = self.files.get(&tile.item).cloned();
        let shell = session.filter(|s| self.plain_shell(*s)).map(|s| Reshell {
            cwd: self.summary(s).and_then(|summary| summary.cwd.clone()),
            name: item.name.clone(),
        });
        let thread = session.and_then(|s| self.session_thread(s));
        let kept = session.map(|s| self.session_kept_for(s, cx));
        self.closed.push(ClosedTile {
            tile,
            item,
            at,
            session,
            file,
            seq,
            title: title.clone(),
            offered: true,
            shell,
            thread,
        });
        while self.closed.len() > CLOSED_KEPT {
            let oldest = self.closed.remove(0);
            self.let_go_closed(&oldest, cx);
        }
        self.propose(tile.worker, ItemOp::Remove(tile.item), cx);
        self.show_toast_for(ToastKind::Closed { seq, title }, UNDO_CLOSE, cx);
        Self::after_closing(seq, UNDO_CLOSE, Self::notice_passed, cx);
        if let Some(kept) = kept {
            Self::after_closing(seq, kept, Self::session_ends, cx);
        }
    }

    /// Run `then` on closing `seq` once `wait` has passed.
    fn after_closing(
        seq: u64,
        wait: Duration,
        then: fn(&mut Self, u64, &mut Context<Self>),
        cx: &Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let _gone = this.update(cx, |this, cx| then(this, seq, cx));
        })
        .detach();
    }

    /// Closing `seq`'s notice is gone: ⌘Z no longer means it alone, and a file tile's editor
    /// goes, its edit with it, as it did when the tile closed for good. A file tile a program
    /// waits on stays while its save is out, so the program hears how that ended; one whose
    /// save did not land (refused, failed, its link lost) tells the program it was given up and
    /// keeps its edit here. The tile itself stays on the list, its file read again when it
    /// comes back.
    fn notice_passed(&mut self, seq: u64, cx: &mut Context<Self>) {
        let Some(ix) = self.closed.iter().position(|c| c.seq == seq) else { return };
        let waits = |v: &FileView| v.waiting().is_some();
        if self
            .closed
            .get(ix)
            .and_then(|c| c.file.as_ref())
            .is_some_and(|f| waits(f.read(cx)) && f.read(cx).saving())
        {
            Self::after_closing(seq, UNDO_CLOSE, Self::notice_passed, cx);
            return;
        }
        let Some(closed) = self.closed.get_mut(ix) else { return };
        closed.offered = false;
        if let Some(view) = closed.file.take() {
            self.let_go_closed_file(&view, cx);
        }
        self.dismiss_closed_toast(Some(seq));
        cx.notify();
    }

    /// Closing `seq`'s session has run as long as it is kept: the worker closes it and its view
    /// goes. A plain shell stays on the list, to come back as a new shell in its directory, and
    /// an agent's to come back as its thread taken up again; anything else ran a program that
    /// cannot come back, and leaves the list.
    fn session_ends(&mut self, seq: u64, cx: &mut Context<Self>) {
        let Some(ix) = self.closed.iter().position(|c| c.seq == seq) else { return };
        let Some(closed) = self.closed.get_mut(ix) else { return };
        let (tile, session) = (closed.tile, closed.session.take());
        if closed.shell.is_none() && closed.thread.is_none() {
            let gone = self.closed.remove(ix);
            self.let_go_closed(&gone, cx);
        }
        if let Some(session) = session {
            self.end_closed_session(tile.worker, session);
        }
        cx.notify();
    }

    /// A closed shell's session is closed by its worker, and its view goes.
    fn end_closed_session(&mut self, worker: WorkerKey, session: SessionId) {
        if self.summary(session).is_some() {
            self.close_session_on(worker, session);
        }
        self.terminals.remove(&session);
    }

    /// A file tile's editor kept for a closed tile goes: a program waiting on it is told it was
    /// given up, and an unsaved edit is let go.
    fn let_go_closed_file(&mut self, view: &Entity<FileView>, cx: &mut Context<Self>) {
        if view.read(cx).waiting().is_some() {
            self.file_tile_gone(view, cx);
        } else {
            self.let_go_unsaved(view, cx);
        }
    }

    /// `closed` leaves the list for good: what it still holds goes.
    fn let_go_closed(&mut self, closed: &ClosedTile, cx: &mut Context<Self>) {
        if let Some(view) = &closed.file {
            self.let_go_closed_file(view, cx);
        }
        if let Some(session) = closed.session {
            self.end_closed_session(closed.tile.worker, session);
        }
        self.dismiss_closed_toast(Some(closed.seq));
    }

    /// The palette's "Reopen" line for each tile on the list, the latest first.
    pub(super) fn closed_lines(&self) -> Vec<PaletteItem> {
        self.closed.iter().rev().map(|c| PaletteItem::reopen(&c.title, c.seq)).collect()
    }

    /// Whether a closing's notice is up: ⌘Z then takes that tile back.
    pub(super) fn closing_offered(&self) -> bool {
        self.closed.iter().any(|c| c.offered)
    }

    /// ⌘Z: the tile closed last comes back where it was.
    pub fn undo_close(&mut self, _: &UndoClose, _window: &mut Window, cx: &mut Context<Self>) {
        self.take_back(None, cx);
    }

    /// Put back the closing `seq` (the toast's, a palette line's), or the latest. A shell whose
    /// session has ended comes back as a new shell in its directory, under its name; an agent's
    /// as its thread's tile where it was, the agent taken up again through its own door.
    pub(super) fn take_back(&mut self, seq: Option<u64>, cx: &mut Context<Self>) {
        let ix = match seq {
            Some(seq) => self.closed.iter().position(|c| c.seq == seq),
            None => self.closed.len().checked_sub(1),
        };
        let Some(ix) = ix else { return };
        let closed = self.closed.remove(ix);
        // Only this closing's offer goes: another tile closed meanwhile can still be taken back.
        self.dismiss_closed_toast(Some(closed.seq));
        tracing::debug!(item = %closed.item.id, session = ?closed.session, "tile taken back");
        let tile = closed.tile;
        if let (ItemKind::Terminal { .. }, None, Some(shell)) =
            (&closed.item.kind, closed.session, closed.shell)
        {
            self.open_session_on(tile.worker, shell.cwd, Vec::new(), shell.name, cx);
            return;
        }
        if let (ItemKind::Terminal { .. }, None, Some(thread)) =
            (&closed.item.kind, closed.session, closed.thread)
        {
            self.reopen_thread(tile.worker, thread, closed.at, cx);
            return;
        }
        // The editor the file tile closed with, edit and all, is its view again; it reads the
        // file once more, and weighs its edit against what the disk has now.
        if let Some(file) = closed.file {
            self.files.insert(tile.item, file);
        }
        self.propose(tile.worker, ItemOp::Add(closed.item), cx);
        if matches!(self.item(tile).map(|i| &i.kind), Some(ItemKind::File { .. })) {
            self.request_file(tile.item);
        }
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
        let placeholder = self.derived_title(&item);
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(placeholder)
                .default_value(item.name.clone().unwrap_or_default())
        });
        self.open_field(tile, Field::Name, input, window, cx);
    }

    /// `input` in `tile`'s header in place of its title, holding the keyboard with its text
    /// selected, until ↩, Esc or a click elsewhere.
    pub(super) fn open_field(
        &mut self,
        tile: TileRef,
        field: Field,
        input: Entity<InputState>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
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
        self.rename = Some(Rename { tile, field, input, return_to, _subscription: subscription });
        self.pending_focus_rename = true;
        cx.notify();
    }

    /// The header's field closes. `keep` writes its text: a name as the item's (blank clears
    /// it), an address as the page's, where an address that is none keeps the field open to
    /// be put right. `back` gives the keyboard back to whoever had it before the field.
    pub(super) fn finish_rename(&mut self, keep: bool, back: bool, cx: &mut Context<Self>) {
        if keep
            && let Some(field) = self.rename.as_ref().filter(|r| r.field == Field::Address)
            && crate::browser::web_url(&field.input.read(cx).value()).is_none()
        {
            let text = field.input.read(cx).value().trim().to_owned();
            self.show_notice(format!("Not a web address: {text}"), cx);
            return;
        }
        let Some(rename) = self.rename.take() else { return };
        if keep && self.item(rename.tile).is_some() {
            let text = rename.input.read(cx).value().trim().to_owned();
            match rename.field {
                Field::Name => {
                    let name = (!text.is_empty()).then_some(text);
                    let op = ItemOp::Rename { id: rename.tile.item, name };
                    self.propose(rename.tile.worker, op, cx);
                }
                Field::Address => self.load_address(rename.tile, &text, cx),
                Field::Project => self.name_project_as(rename.tile, &text, cx),
            }
        }
        if back {
            self.rename_return = rename.return_to.or_else(|| Some(self.focus.clone()));
        }
        cx.notify();
    }
}

/// Where a new note goes: `dir` (else the home), as `note-2026-10-05-143210.md` for `now` read
/// `offset_s` east of UTC. To the second, so two notes made apart never share a file.
pub(super) fn note_path(dir: Option<&str>, now: WallMs, offset_s: i64) -> Option<String> {
    let at = now.civil(offset_s)?;
    let dir = dir.map_or("~", |d| d.trim_end_matches('/'));
    let dir = if dir.is_empty() { "" } else { dir };
    Some(format!(
        "{dir}/note-{:04}-{:02}-{:02}-{:02}{:02}{:02}.md",
        at.year, at.month, at.day, at.hour, at.minute, at.second
    ))
}

/// How far east of UTC this device's clock reads now, in seconds.
fn local_offset() -> i64 {
    use objc2_core_foundation::{CFAbsoluteTimeGetCurrent, CFTimeZone};
    let Some(zone) = CFTimeZone::system() else { return 0 };
    // Whole seconds east of UTC: no zone's offset has a fraction.
    #[expect(clippy::cast_possible_truncation, reason = "an offset is a few hours of seconds")]
    let offset = zone.seconds_from_gmt(CFAbsoluteTimeGetCurrent()) as i64;
    offset
}

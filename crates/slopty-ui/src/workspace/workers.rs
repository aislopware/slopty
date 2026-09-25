//! Workers coming and going, and what they say: the registry sync that places tiles, the
//! sessions, the streams and the files behind the tiles.

use gpui::{AppContext as _, Context, Entity, Window};
use slopty_client::ItemChange;
use slopty_client::layout::{Placement, TileRef, WorkerKey};
use slopty_core::{ItemId, SessionId, StreamId};
use slopty_proto::ClientMsg;
use slopty_proto::file::{FileRead, WriteResult};
use slopty_proto::items::{ItemKind, ItemSync};
use slopty_proto::screen::{CaptureTarget, Quality, ScreenEvent, ScreenRequest};
use slopty_proto::terminal::{SessionState, SessionSummary, TermEvent, TermRequest, TermSize};

use super::{Finished, Worker, WorkerLink, WorkerStatus, WorkspaceEvent, WorkspaceView};
use crate::file::{FileView, FileViewEvent};
use crate::note::{NoteView, NoteViewEvent};
use crate::screen::ScreenView;
use crate::terminal::{TerminalView, TerminalViewEvent};

impl WorkspaceView {
    /// A worker this client has added, before its first connection: its tiles (from the
    /// saved layout) stay where they were, waiting.
    pub fn add_worker(&mut self, key: WorkerKey, name: String, cx: &mut Context<Self>) {
        match self.workers.get_mut(&key) {
            Some(w) => w.name = name,
            None => {
                self.workers.insert(key, Worker::new(name));
            }
        }
        cx.notify();
    }

    /// The link to `key` is up: this client's id there, where its messages go, and the
    /// sessions the worker has. The registry snapshot follows on the link.
    pub fn connect_worker(
        &mut self,
        key: WorkerKey,
        name: String,
        link: WorkerLink,
        sessions: Vec<SessionSummary>,
        cx: &mut Context<Self>,
    ) {
        let known: Vec<SessionId> = self
            .workers
            .get(&key)
            .map(|w| w.sessions.keys().copied().collect())
            .unwrap_or_default();
        let w = self.workers.entry(key).or_insert_with(|| Worker::new(name.clone()));
        w.name = name;
        w.status = WorkerStatus::Connected;
        w.link = Some(link);
        w.awaiting_snapshot = true;
        w.titles_requested = false;
        w.watched.clear();
        w.pending_opens.clear();
        for s in &sessions {
            if !self.shell_recency.contains(&s.id) {
                self.shell_recency.push(s.id);
            }
        }
        let agents: Vec<SessionSummary> =
            sessions.iter().filter(|s| s.agent.is_some()).cloned().collect();
        w.sessions = sessions.into_iter().map(|s| (s.id, s)).collect();
        self.items_dirty = true;
        self.reset_remote(key, &known, cx);
        self.seed_agents(&agents, cx);
        // The disk may have moved on while the worker was away: every card of it reads again,
        // and one holding an edit weighs it against what is there now.
        let cards: Vec<ItemId> = self
            .workers
            .get(&key)
            .map(|w| w.doc.items().map(|i| i.id).filter(|id| self.files.contains_key(id)).collect())
            .unwrap_or_default();
        for id in cards {
            self.request_file(id);
        }
        cx.notify();
    }

    /// The link to `key` dropped (or never came up): its tiles stay where they are and say
    /// the worker is away; the views that spoke to the old link go, and come back with the
    /// next one. Notes and file cards keep what they show.
    pub fn disconnect_worker(
        &mut self,
        key: WorkerKey,
        status: WorkerStatus,
        cx: &mut Context<Self>,
    ) {
        let Some(w) = self.workers.get_mut(&key) else { return };
        w.status = status;
        w.link = None;
        w.rtt = None;
        w.pending_opens.clear();
        w.picker_wanted = false;
        w.display_wanted = false;
        let sessions: Vec<SessionId> = w.sessions.keys().copied().collect();
        let items: Vec<ItemId> = w.doc.items().map(|i| i.id).collect();
        self.reset_remote(key, &sessions, cx);
        for session in &sessions {
            self.terminals.remove(session);
            self.agents.remove(session);
        }
        for item in &items {
            self.screens.remove(item);
            // A save sent on the link that dropped has no answer coming.
            if let Some(view) = self.files.get(item) {
                view.update(cx, FileView::link_lost);
            }
        }
        for closed in self.closed.iter().filter(|c| c.tile.worker == key) {
            if let Some(view) = &closed.file {
                view.update(cx, FileView::link_lost);
            }
        }
        self.items_dirty = true;
        if self.picker.as_ref().is_some_and(|(k, _)| *k == key) {
            self.picker = None;
            self.pending_focus_self = true;
        }
        // The focused shell's or window's view went with the link: the workspace takes the
        // keyboard, so ⌘W, ⌘T and the rest still answer while the worker is away. A note or a
        // file card keeps its view, and the keyboard with it.
        let viewless = self.focused().filter(|t| t.worker == key).and_then(|t| self.item(t));
        if viewless.is_some_and(|i| {
            matches!(
                i.kind,
                ItemKind::Terminal { .. } | ItemKind::Window { .. } | ItemKind::Display { .. }
            )
        }) {
            self.pending_focus_self = true;
        }
        self.update_awake(cx);
        self.count_needs_you(cx);
        cx.notify();
    }

    /// A worker's link state changed without a link coming or going (a failed attempt).
    pub fn set_worker_status(
        &mut self,
        key: WorkerKey,
        status: WorkerStatus,
        cx: &mut Context<Self>,
    ) {
        if let Some(w) = self.workers.get_mut(&key)
            && w.status != status
        {
            w.status = status;
            cx.notify();
        }
    }

    /// The quiet line about the server in the titlebar ("server unreachable"); `None` hides
    /// it.
    pub fn set_server_status(&mut self, text: Option<String>, cx: &mut Context<Self>) {
        let text = text.map(gpui::SharedString::from);
        if self.server_status != text {
            self.server_status = text;
            cx.notify();
        }
    }

    /// The server's line, as shown.
    #[must_use]
    pub fn server_status(&self) -> Option<&str> {
        self.server_status.as_deref()
    }

    /// The human forgot a worker: its tiles leave the layout with it.
    pub fn remove_worker(&mut self, key: WorkerKey, cx: &mut Context<Self>) {
        self.disconnect_worker(key, WorkerStatus::Connecting, cx);
        let Some(w) = self.workers.remove(&key) else { return };
        for item in w.doc.items() {
            self.drop_item_views(item.id);
            if let ItemKind::Terminal { session } = item.kind {
                self.finished.remove(&session);
            }
        }
        for session in w.sessions.keys() {
            self.finished.remove(session);
        }
        self.closed.retain(|c| c.tile.worker != key);
        self.items_dirty = true;
        self.tick();
        self.layout.retain_worker(key, |_| false);
        self.layout_touched(cx);
        self.after_focus_moved(cx);
        cx.notify();
    }

    /// Link RTT of `key`, fanned out to its terminals' predictors and its windows' overlays.
    pub fn set_rtt(
        &mut self,
        key: WorkerKey,
        rtt: Option<std::time::Duration>,
        cx: &mut Context<Self>,
    ) {
        let Some(w) = self.workers.get_mut(&key) else { return };
        let label = super::navigator::rtt_label;
        let changed = w.rtt.map(label) != rtt.map(label);
        w.rtt = rtt;
        let sessions: Vec<SessionId> = w.sessions.keys().copied().collect();
        let items: Vec<ItemId> = w.doc.items().map(|i| i.id).collect();
        for session in sessions {
            if let Some(view) = self.terminals.get(&session) {
                view.update(cx, |v, _| v.set_rtt(rtt));
            }
        }
        for item in items {
            if let Some(view) = self.screens.get(&item) {
                view.update(cx, |v, _| v.set_rtt(rtt));
            }
        }
        // The status bar and the navigator show it: repaint only when what they print moved.
        if changed && self.rtt_shown(key) {
            cx.notify();
        }
    }

    /// A registry snapshot, delta or pointing from `key`.
    pub fn apply_sync(&mut self, key: WorkerKey, sync: ItemSync, cx: &mut Context<Self>) {
        let Some(w) = self.workers.get_mut(&key) else { return };
        let Some(me) = w.link.as_ref().map(|l| l.me) else { return };
        // A pointing carries the pointer's name only on the wire: taken before the registry
        // reduces it to the item. One at an item this client does not have is nothing.
        if let ItemSync::Pointed { client, name, item } = &sync
            && *client != me
            && w.doc.get(*item).is_some()
        {
            let tile = TileRef { worker: key, item: *item };
            self.show_toast(super::toast::ToastKind::Pointed { name: name.clone(), tile }, cx);
            return;
        }
        let snapshot = matches!(sync, ItemSync::Snapshot { .. });
        let change = w.doc.apply_sync(sync, me);
        tracing::debug!(?change, version = w.doc.version(), "item sync");
        if snapshot {
            w.awaiting_snapshot = false;
            // What was done here while the worker was away goes over its snapshot, here at
            // once and to the worker in the order it was done. A session closed meanwhile is
            // closed only if the worker still runs it.
            let queued = std::mem::take(&mut w.queued);
            if !queued.is_empty() {
                tracing::info!(ops = queued.len(), "replaying what was done while away");
            }
            for msg in queued {
                match &msg {
                    ClientMsg::Items(op) => {
                        w.doc.apply_op(op, true);
                    }
                    ClientMsg::Term { session, .. } if !w.sessions.contains_key(session) => {
                        continue;
                    }
                    _ => {}
                }
                w.send(msg);
            }
        }
        self.item_changed(key, change, cx);
        if snapshot {
            self.first_snapshot(key, cx);
        }
    }

    /// Bring the layout and the views in line with one change to `key`'s registry.
    pub(super) fn item_changed(
        &mut self,
        key: WorkerKey,
        change: ItemChange,
        cx: &mut Context<Self>,
    ) {
        self.items_dirty = true;
        self.tick();
        match change {
            ItemChange::Reset => {
                let Some(w) = self.workers.get(&key) else { return };
                let present: std::collections::HashSet<ItemId> =
                    w.doc.items().map(|i| i.id).collect();
                // A tile whose item the worker no longer has leaves; the rest stay where this
                // device put them, and anything new joins at the end of its workspace.
                self.layout.retain_worker(key, |id| present.contains(&id));
                let new: Vec<ItemId> = w
                    .doc
                    .items()
                    .map(|i| i.id)
                    .filter(|id| !self.layout.contains(TileRef { worker: key, item: *id }))
                    .collect();
                for item in new {
                    self.layout.open(TileRef { worker: key, item }, Placement::Remote);
                }
                let gone: Vec<ItemId> = self
                    .notes
                    .keys()
                    .chain(self.files.keys())
                    .chain(self.screens.keys())
                    .copied()
                    .filter(|id| self.tile_of(*id).is_none())
                    .collect();
                for id in gone {
                    self.drop_item_views(id);
                }
            }
            ItemChange::Added { id, by_me } => {
                let tile = TileRef { worker: key, item: id };
                let placement = if by_me { Placement::Local } else { Placement::Remote };
                self.layout.open(tile, placement);
                // A worker's given shell opens beside the rest and leaves the focus where it
                // was: a worker coming up must not take the keys someone is typing elsewhere.
                match (by_me, self.given_pending.remove(&key)) {
                    (true, Some(Some(before))) => self.layout.focus(before),
                    (true, _) => self.after_focus_moved(cx),
                    (false, _) => {}
                }
            }
            ItemChange::Removed(id) => {
                self.layout.remove(TileRef { worker: key, item: id });
                self.drop_item_views(id);
                self.after_focus_moved(cx);
            }
            ItemChange::Changed(_) | ItemChange::Echo | ItemChange::Pointed(_) => {}
        }
        self.layout_touched(cx);
        self.reconcile(cx);
        cx.notify();
    }

    /// A worker's first snapshot since its link came up: a worker with nothing on it gets one
    /// shell, once per run, so a newly added worker has something to type into (there is no
    /// other way to open the first tile on a worker, since a new tile goes to the focused
    /// tile's worker).
    fn first_snapshot(&mut self, key: WorkerKey, cx: &mut Context<Self>) {
        let empty = self.workers.get(&key).is_some_and(|w| w.doc.is_empty());
        if !empty || !self.given_shell.insert(key) {
            return;
        }
        self.given_pending.insert(key, self.focused());
        self.open_session_on(key, None, Vec::new(), None, cx);
    }

    fn drop_item_views(&mut self, id: ItemId) {
        self.notes.remove(&id);
        self.files.remove(&id);
        self.screens.remove(&id);
        self.titles.remove(&id);
        self.unseen.remove(&id);
        self.parked.remove(&id);
    }

    /// A session appeared on `key` (this client's or another's).
    pub fn session_opened(
        &mut self,
        key: WorkerKey,
        summary: SessionSummary,
        cx: &mut Context<Self>,
    ) {
        if !self.shell_recency.contains(&summary.id) {
            self.shell_recency.push(summary.id);
        }
        self.seed_agents([&summary], cx);
        if let Some(w) = self.workers.get_mut(&key) {
            w.sessions.insert(summary.id, summary);
        }
        self.reconcile(cx);
        cx.notify();
    }

    /// A session is gone.
    pub fn session_closed(&mut self, session: SessionId, cx: &mut Context<Self>) {
        for w in self.workers.values_mut() {
            w.sessions.remove(&session);
        }
        self.agents.remove(&session);
        // Its "finished" badge has no tile to clear it by looking: the bell must not keep it.
        self.finished.remove(&session);
        self.shell_recency.retain(|s| *s != session);
        self.update_awake(cx);
        self.reconcile(cx);
        self.count_needs_you(cx);
        cx.notify();
    }

    /// A session's program exited: the tile stays with its last screen and says so, until the
    /// human closes or restarts it. The worker announces the exit too; this is the attached
    /// view's word for it, a round trip sooner.
    fn session_exited(&mut self, session: SessionId, status: i32, cx: &mut Context<Self>) {
        for w in self.workers.values_mut() {
            if let Some(summary) = w.sessions.get_mut(&session) {
                summary.state = SessionState::Exited { status };
            }
        }
        cx.notify();
    }

    /// A session changed directory (OSC 7), and the worker says which repository that is in.
    pub fn session_moved(&mut self, session: SessionId, cwd: &str, repo: Option<&str>) {
        for w in self.workers.values_mut() {
            if let Some(summary) = w.sessions.get_mut(&session) {
                summary.cwd = Some(cwd.to_owned());
                summary.repo = repo.map(str::to_owned);
            }
        }
    }

    /// The session's summary, whichever worker runs it.
    pub(super) fn summary(&self, session: SessionId) -> Option<&SessionSummary> {
        self.workers.values().find_map(|w| w.sessions.get(&session))
    }

    /// A session-stream event.
    pub fn term_event(&mut self, session: SessionId, event: TermEvent, cx: &mut Context<Self>) {
        if let TermEvent::Matches { needle, total, .. } = &event
            && self.find_needle.is_some()
        {
            let needle = needle.clone();
            self.find_answered(session, &needle, *total, cx);
        }
        match (self.terminals.get(&session), event) {
            (Some(view), event) => view.update(cx, |v, cx| v.apply(event, cx)),
            (None, TermEvent::Error(e)) => tracing::warn!(%session, error = %e, "worker"),
            (None, _other) => {}
        }
    }

    /// Views for every tile whose content is alive, none for the rest: terminals attached,
    /// streams open for the remote tiles that are on screen (or were, within the grace).
    pub(super) fn reconcile(&mut self, cx: &mut Context<Self>) {
        let workers = &self.workers;
        self.closed.retain(|c| {
            c.session.is_none_or(|s| workers.values().any(|w| w.sessions.contains_key(&s)))
        });
        let wanted: Vec<(WorkerKey, SessionId)> = self
            .workers
            .iter()
            .filter(|(_, w)| w.link.is_some())
            .flat_map(|(key, w)| {
                w.doc.items().filter_map(move |i| match i.kind {
                    ItemKind::Terminal { session } if !i.sleeping => Some((*key, session)),
                    _ => None,
                })
            })
            .chain(self.closed.iter().filter_map(|c| c.session.map(|s| (c.tile.worker, s))))
            .filter(|(key, s)| self.workers.get(key).is_some_and(|w| w.sessions.contains_key(s)))
            .collect();
        for &(key, session) in &wanted {
            if self.terminals.contains_key(&session) {
                continue;
            }
            self.attach_terminal(key, session, cx);
        }
        let gone: Vec<SessionId> = self
            .terminals
            .keys()
            .filter(|s| !wanted.iter().any(|(_, w)| w == *s))
            .copied()
            .collect();
        for session in gone {
            if self.summary(session).is_some() {
                self.send_session(session, ClientMsg::Term { session, req: TermRequest::Detach });
            }
            self.terminals.remove(&session);
        }
        self.reconcile_screens();
        self.update_run_targets(cx);
    }

    fn attach_terminal(&mut self, key: WorkerKey, session: SessionId, cx: &mut Context<Self>) {
        let Some(w) = self.workers.get(&key) else { return };
        let Some(link) = w.link.clone() else { return };
        let size = w.sessions.get(&session).map_or_else(TermSize::default, |s| TermSize {
            cols: s.cols,
            rows: s.rows,
            ..TermSize::default()
        });
        let theme = self.theme.clone();
        let files = self.files_hook();
        let view = cx.new(|cx| {
            let mut view = TerminalView::new(session, size, link.out.clone(), theme, cx);
            if let Some(files) = files {
                view.set_files_hook(files);
            }
            view
        });
        let sid = session;
        cx.subscribe(&view, move |this, _view, event, cx| match event {
            TerminalViewEvent::Bell => cx.emit(WorkspaceEvent::Bell(sid)),
            TerminalViewEvent::Notification { title, body } => {
                this.notify_program(sid, title, body, cx);
            }
            TerminalViewEvent::Exited(status) => this.session_exited(sid, *status, cx),
            TerminalViewEvent::CloseConfirmed => this.close_shell(sid, cx),
            TerminalViewEvent::Title(_) => cx.notify(),
            TerminalViewEvent::Cwd { path, repo } => {
                this.session_moved(sid, path, repo.as_deref());
            }
            TerminalViewEvent::Notice(text) => this.show_notice(text.clone(), cx),
            TerminalViewEvent::CommandFinished { command, exit, elapsed } => {
                let done = Finished { command: command.clone(), exit: *exit, elapsed: *elapsed };
                this.command_finished(sid, done, cx);
            }
            TerminalViewEvent::NoteBlock(text) => this.note_beside(sid, text.clone(), cx),
            TerminalViewEvent::ViewFile { path, line } => {
                let path = this.absolute_in_session(sid, path);
                this.open_file_on(this.worker_of_session(sid), &path, *line, cx);
            }
            TerminalViewEvent::DragOut { path } => {
                let path = this.absolute_in_session(sid, path);
                if let Some(worker) = this.worker_of_session(sid) {
                    this.drag_out(worker, &path);
                }
            }
            TerminalViewEvent::PasteFiles(files) => {
                this.paste_files_in_shell(sid, files.clone(), cx);
            }
        })
        .detach();
        w.send(ClientMsg::Term { session, req: TermRequest::Attach { size } });
        // What this client paints with, so the driver's colours answer colour queries.
        w.send(ClientMsg::Term { session, req: TermRequest::Colors(self.theme.terminal.wire()) });
        // A view born after the worker reported the agent starts with its state.
        if let Some(agent) = self.agents.get(&session) {
            let status = agent.status.clone();
            view.update(cx, |v, cx| v.set_agent_status(Some(status), cx));
        }
        if let Some(rtt) = w.rtt {
            view.update(cx, |v, _| v.set_rtt(Some(rtt)));
        }
        self.terminals.insert(session, view);
    }

    /// Requested quality for a new stream: the settings' rate, ceiling and depth at full
    /// scale; the view then asks for a scale matching the width it paints at.
    const fn quality_for(&self) -> Quality {
        crate::screen::quality_of(self.theme.behaviour.stream, 1.0)
    }

    /// Open streams for remote tiles that should have one and lack it; let go of the rest.
    /// A tile off screen for [`super::STREAM_GRACE`] is parked: its stream goes and comes back
    /// when the tile is on screen again.
    pub(super) fn reconcile_screens(&mut self) {
        let now = std::time::Instant::now();
        let parked: Vec<ItemId> = self
            .unseen
            .iter()
            .filter(|(_, since)| now.saturating_duration_since(**since) >= self.stream_grace)
            .map(|(id, _)| *id)
            .collect();
        for id in parked {
            self.parked.insert(id);
        }
        let quality = self.quality_for();
        let mut keep: Vec<ItemId> = Vec::new();
        for w in self.workers.values_mut() {
            if w.link.is_none() {
                continue;
            }
            let untitled = w.doc.items().any(|i| {
                matches!(i.kind, ItemKind::Window { .. }) && !self.titles.contains_key(&i.id)
            });
            if untitled && !w.titles_requested {
                w.titles_requested = true;
                w.send(ClientMsg::Screen(ScreenRequest::List));
            }
            let wanted: Vec<(ItemId, CaptureTarget)> = w
                .doc
                .items()
                .filter(|i| !i.sleeping && !self.parked.contains(&i.id))
                .filter_map(|i| match i.kind {
                    ItemKind::Window { window } => Some((i.id, CaptureTarget::Window(window))),
                    ItemKind::Display { display } => Some((i.id, CaptureTarget::Display(display))),
                    ItemKind::Terminal { .. }
                    | ItemKind::Note { .. }
                    | ItemKind::File { .. }
                    | ItemKind::Browser { .. } => None,
                })
                .collect();
            for &(id, target) in &wanted {
                keep.push(id);
                if self.screens.contains_key(&id) || w.pending_opens.values().any(|&p| p == id) {
                    continue;
                }
                if w.pending_opens.contains_key(&target) {
                    continue;
                }
                w.pending_opens.insert(target, id);
                w.send(ClientMsg::Screen(ScreenRequest::Open { target, quality }));
            }
            w.pending_opens.retain(|_, id| wanted.iter().any(|(k, _)| k == id));
        }
        // Dropping a view sends `Close` for its stream.
        self.screens.retain(|id, _| keep.contains(id));
    }

    /// A remote-window event from `key`.
    pub fn screen_event(&mut self, key: WorkerKey, event: ScreenEvent, cx: &mut Context<Self>) {
        match event {
            ScreenEvent::Listing { windows, displays } => {
                self.fill_titles(key, &windows);
                let Some(w) = self.workers.get_mut(&key) else { return };
                w.titles_requested = false;
                if std::mem::take(&mut w.display_wanted)
                    && let Some(d) = displays.first()
                {
                    self.add_screen_item(
                        key,
                        CaptureTarget::Display(d.id),
                        format!("Display {}", d.id),
                        cx,
                    );
                }
                let Some(w) = self.workers.get_mut(&key) else { return };
                if std::mem::take(&mut w.picker_wanted) {
                    self.show_picker(key, windows, displays, cx);
                }
            }
            ScreenEvent::Opened { stream, target, codec, width, height, .. } => {
                let theme = self.theme.clone();
                let quality = self.quality_for();
                let show_stats = self.show_stats;
                let Some(w) = self.workers.get_mut(&key) else { return };
                let Some(link) = w.link.clone() else { return };
                let Some(id) = w.pending_opens.remove(&target) else {
                    tracing::debug!(%stream, ?target, "opened stream nobody asked for; closing");
                    w.send(ClientMsg::Screen(ScreenRequest::Close(stream)));
                    return;
                };
                let handle = (link.open_screen)(stream, codec);
                let opened =
                    crate::screen::Opened { stream, target, size: (width, height), quality };
                let rtt = w.rtt;
                let hook = self.paste_hook(key);
                let view = cx.new(|cx| {
                    let mut view = ScreenView::new(opened, handle, link.out.clone(), theme, cx);
                    if let Some(hook) = hook {
                        view.set_paste_hook(hook);
                    }
                    view
                });
                view.update(cx, |v, cx| {
                    v.set_rtt(rtt);
                    if show_stats {
                        v.set_hud(true, cx);
                    }
                });
                let tile = TileRef { worker: key, item: id };
                cx.subscribe(&view, move |this, view, event, cx| match event {
                    crate::screen::ScreenViewEvent::Pressed => this.focus_tile(tile, cx),
                    crate::screen::ScreenViewEvent::Ready => cx.notify(),
                    crate::screen::ScreenViewEvent::PasteFiles(files) => {
                        let view = view.downgrade();
                        this.paste_files_in_window(tile, &view, files.clone(), cx);
                    }
                })
                .detach();
                self.screens.insert(id, view);
            }
            ScreenEvent::Closed { stream, reason } => {
                let gone: Vec<ItemId> = self.streams_of(key, stream, cx);
                for id in gone {
                    tracing::info!(%stream, %reason, "screen closed by worker");
                    self.screens.remove(&id);
                }
            }
            ScreenEvent::Geometry { stream, width, height } => {
                if width > 0 && height > 0 {
                    for id in self.streams_of(key, stream, cx) {
                        if let Some(view) = self.screens.get(&id) {
                            view.update(cx, |v, _| v.set_geometry(width, height));
                        }
                    }
                }
            }
            ScreenEvent::Rate { stream, target_bps, verdict, capped } => {
                for id in self.streams_of(key, stream, cx) {
                    if let Some(view) = self.screens.get(&id) {
                        view.update(cx, |v, _| v.set_rate(target_bps, verdict, capped));
                    }
                }
            }
            ScreenEvent::Source { stream, state } => {
                for id in self.streams_of(key, stream, cx) {
                    if let Some(view) = self.screens.get(&id) {
                        view.update(cx, |v, cx| v.set_source_state(state, cx));
                    }
                }
            }
            ScreenEvent::Cursor { stream, shape } => {
                for id in self.streams_of(key, stream, cx) {
                    if let Some(view) = self.screens.get(&id) {
                        view.update(cx, |v, cx| v.set_cursor_shape(shape.clone(), cx));
                    }
                }
            }
            ScreenEvent::ListingChanged => {}
        }
        cx.notify();
    }

    /// The items of `key` whose view shows `stream` (stream ids are per worker).
    fn streams_of(&self, key: WorkerKey, stream: StreamId, cx: &Context<Self>) -> Vec<ItemId> {
        let Some(w) = self.workers.get(&key) else { return Vec::new() };
        w.doc
            .items()
            .filter(|i| self.screens.get(&i.id).is_some_and(|v| v.read(cx).stream() == stream))
            .map(|i| i.id)
            .collect()
    }

    /// Window items restored from the registry have no title until a `Listing` names them.
    fn fill_titles(&mut self, key: WorkerKey, windows: &[slopty_proto::screen::WindowInfo]) {
        let Some(w) = self.workers.get(&key) else { return };
        for item in w.doc.items() {
            let ItemKind::Window { window } = item.kind else { continue };
            if self.titles.contains_key(&item.id) {
                continue;
            }
            if let Some(info) = windows.iter().find(|w| w.id == window) {
                let title =
                    if info.title.is_empty() { info.app.clone() } else { info.title.clone() };
                self.titles.insert(item.id, title);
            }
        }
    }

    /// The worker read a file: every tile for that path shows it.
    pub fn file_read(&self, key: WorkerKey, path: &str, read: &FileRead, cx: &mut Context<Self>) {
        for view in self.files_at(key, path, cx) {
            view.update(cx, |v, cx| v.set_read(read.clone(), cx));
        }
    }

    /// The worker answered a save: the tile that sent it hears how it went.
    pub fn file_written(
        &self,
        key: WorkerKey,
        path: &str,
        result: &WriteResult,
        cx: &mut Context<Self>,
    ) {
        for view in self.files_at(key, path, cx) {
            view.update(cx, |v, cx| v.written(result.clone(), cx));
        }
    }

    /// The tiles showing `path` on `key`, and those closed a moment ago that ⌘Z may bring
    /// back: a save sent before the close is answered to the buffer that sent it.
    fn files_at(&self, key: WorkerKey, path: &str, cx: &Context<Self>) -> Vec<Entity<FileView>> {
        let Some(w) = self.workers.get(&key) else { return Vec::new() };
        let closed = self.closed.iter().filter(|c| c.tile.worker == key);
        w.doc
            .items()
            .filter_map(|item| self.files.get(&item.id))
            .chain(closed.filter_map(|c| c.file.as_ref()))
            .filter(|view| view.read(cx).path() == path)
            .cloned()
            .collect()
    }

    /// Editors for note items and cards for file items; the ones whose items are gone go.
    /// Needs the window (a note's editor does), so it runs from `render`, and only on a frame
    /// after a registry or a link changed ([`WorkspaceView::items_dirty`]).
    pub(super) fn reconcile_notes_and_files(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut notes: Vec<ItemId> = Vec::new();
        let mut files: Vec<(WorkerKey, ItemId, &str)> = Vec::new();
        for (key, item) in self.items() {
            match &item.kind {
                ItemKind::Note { text } => {
                    notes.push(item.id);
                    // A note another client changed: taken now, or once this one stops editing.
                    if let Some(view) = self.notes.get(&item.id) {
                        view.update(cx, |v, cx| v.offer_text(text, window, cx));
                    }
                }
                ItemKind::File { path } => files.push((key, item.id, path)),
                _ => {}
            }
        }
        let new_notes: Vec<(ItemId, String)> = notes
            .iter()
            .filter(|id| !self.notes.contains_key(id))
            .filter_map(|id| match self.tile_of(*id).and_then(|t| self.item(t)).map(|i| &i.kind) {
                Some(ItemKind::Note { text }) => Some((*id, text.clone())),
                _ => None,
            })
            .collect();
        let new_files: Vec<(WorkerKey, ItemId, String)> = files
            .iter()
            .filter(|(key, id, _)| {
                !self.files.contains_key(id)
                    && self.workers.get(key).is_some_and(|w| w.link.is_some())
            })
            .map(|(key, id, path)| (*key, *id, (*path).to_owned()))
            .collect();
        // Each worker watches the set behind its cards and re-reads one that changes on disk.
        let mut watch: Vec<(WorkerKey, Vec<String>)> = Vec::new();
        for (key, w) in &self.workers {
            if w.link.is_none() {
                continue;
            }
            let mut paths: Vec<&str> =
                files.iter().filter(|(k, ..)| k == key).map(|(_, _, p)| *p).collect();
            paths.sort_unstable();
            paths.dedup();
            if paths != w.watched {
                watch.push((*key, paths.into_iter().map(str::to_owned).collect()));
            }
        }
        let file_ids: Vec<ItemId> = files.iter().map(|(_, id, _)| *id).collect();
        for (key, paths) in watch {
            if let Some(w) = self.workers.get_mut(&key) {
                w.watched.clone_from(&paths);
                w.send(ClientMsg::WatchFiles { paths });
            }
        }
        for (id, text) in new_notes {
            self.make_note(id, &text, window, cx);
        }
        self.notes.retain(|id, _| notes.contains(id));
        for (key, id, path) in new_files {
            self.make_file(key, id, path, window, cx);
        }
        self.files.retain(|id, _| file_ids.contains(id));
    }

    /// The editor of note `id`.
    fn make_note(&mut self, id: ItemId, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let theme = self.theme.clone();
        let view = cx.new(|cx| NoteView::new(id, text, theme, window, cx));
        // Detached: GPUI drops a subscription with the view it listens to.
        cx.subscribe(&view, move |this, _view, event, cx| {
            match event {
                NoteViewEvent::Commit(text) => this.commit_note(id, text.clone(), cx),
                NoteViewEvent::Run(code) => this.run_in_shell(code.clone(), cx),
            }
            cx.notify();
        })
        .detach();
        view.update(cx, |n, cx| n.set_can_run(self.run_target().is_some(), cx));
        self.notes.insert(id, view);
    }

    /// The card of file item `id` at `path` on `worker`, which it asks for the text.
    fn make_file(
        &mut self,
        worker: WorkerKey,
        id: ItemId,
        path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let theme = self.theme.clone();
        let view = cx.new(|cx| FileView::new(id, &path, theme, window, cx));
        let file = path.clone();
        cx.subscribe(&view, move |this, view, event, cx| {
            match event {
                FileViewEvent::FindClosed => this.pending_focus_file = Some(id),
                FileViewEvent::Save { text, base_modified_ms } => {
                    let write = ClientMsg::WriteFile {
                        path: file.clone(),
                        text: text.clone(),
                        base_modified_ms: *base_modified_ms,
                    };
                    let sent = this.workers.get(&worker).is_some_and(|w| w.send(write));
                    // Out of reach: said at once, not left saving for an answer never coming.
                    if !sent {
                        let name =
                            this.workers.get(&worker).map_or("The worker", |w| w.name.as_str());
                        let error = format!("{name} is out of reach");
                        view.update(cx, |v, cx| v.written(WriteResult::Failed { error }, cx));
                    }
                }
                FileViewEvent::Reload => this.request_file(id),
            }
            cx.notify();
        })
        .detach();
        if let Some(line) = self.file_focus.remove(&id) {
            view.update(cx, |v, cx| v.focus_line(Some(line), cx));
        }
        self.files.insert(id, view);
        if let Some(w) = self.workers.get(&worker) {
            w.send(ClientMsg::ReadFile { path });
        }
    }

    /// Mark which remote tiles are off screen this frame, and park or wake streams: called
    /// with the frame's visible items.
    pub(super) fn note_visible(&mut self, visible: &[ItemId]) {
        let now = std::time::Instant::now();
        let remote: Vec<ItemId> = self
            .items()
            .filter(|(_, i)| matches!(i.kind, ItemKind::Window { .. } | ItemKind::Display { .. }))
            .map(|(_, i)| i.id)
            .collect();
        let mut changed = false;
        for id in remote {
            if visible.contains(&id) {
                self.unseen.remove(&id);
                changed |= self.parked.remove(&id);
            } else {
                self.unseen.entry(id).or_insert(now);
            }
        }
        let due = self.unseen.iter().any(|(id, since)| {
            !self.parked.contains(id) && now.saturating_duration_since(*since) >= self.stream_grace
        });
        if changed || due {
            self.reconcile_screens();
        }
    }
}

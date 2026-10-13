//! Workers coming and going, and what they say: the registry sync that places tiles, the
//! sessions, the streams and the files behind the tiles.

use std::time::Duration;

use gpui::{AppContext as _, Context, Entity};
use slopty_client::{ItemChange, ItemRef, WorkerKey};
use slopty_core::{ItemId, SessionId, StreamId};
use slopty_proto::ClientMsg;
use slopty_proto::handshake::HelloAck;
use slopty_proto::items::{ItemKind, ItemSync};
use slopty_proto::screen::{
    CaptureTarget, OpenAsk, Quality, ScreenEvent, ScreenFailure, ScreenRequest,
};
use slopty_proto::server::WorkerCaps;
use slopty_proto::tailnet::LinkPath;
use slopty_proto::terminal::{SessionState, SessionSummary, TermEvent, TermRequest, TermSize};
use slopty_terminal::{TerminalView, TerminalViewEvent};

use super::attention::About;
use super::{Finished, Worker, WorkerLink, WorkerStatus, WorkspaceEvent, WorkspaceView, desktop};
use crate::screen::ScreenView;

/// A stream the worker ends unasked again within this of the last is not asked for again: its
/// tile says it stopped and offers to reopen, rather than flicker between a picture and none.
pub(super) const STOP_AGAIN: Duration = Duration::from_secs(10);

/// A remote tile whose stream the worker ended unasked ([`Worker::stopped`]).
#[derive(Debug)]
pub(super) struct Stopped {
    /// When it last ended.
    at: std::time::Instant,
    /// Set once it ended twice within [`STOP_AGAIN`]: the worker's words, shown in the pane,
    /// and the tile is not asked for again until the person reopens it or the link comes back.
    why: Option<String>,
}

impl Stopped {}

impl WorkspaceView {
    /// Whether the clipboard is shared with `key`, as the settings say by its worker id.
    #[must_use]
    pub fn clipboard_shared(&self, key: WorkerKey) -> bool {
        !self.workers.contains_key(&key) || shares_clipboard(&self.clip_sharing, key)
    }

    /// A worker this client has added, before its first connection.
    pub fn add_worker(&mut self, key: WorkerKey, name: String, cx: &mut Context<Self>) {
        if let Some(w) = self.workers.get_mut(&key) {
            w.name = name;
        } else {
            let mut w = Worker::new(name);
            w.away_since = Some(self.now());
            self.workers.insert(key, w);
        }
        // The settings share the clipboard by the worker's name.
        self.clip_sharing_changed();
        cx.notify();
    }

    /// The link to `key` is up: this client's id there and where its messages go, and what the
    /// worker said on it: its name, home, capabilities and sessions. The registry snapshot
    /// follows on the link.
    pub fn connect_worker(
        &mut self,
        key: WorkerKey,
        link: WorkerLink,
        ack: HelloAck,
        cx: &mut Context<Self>,
    ) {
        let HelloAck { name, home, settings, caps, load, sessions, .. } = ack;
        let known: Vec<SessionId> = self
            .workers
            .get(&key)
            .map(|w| w.sessions.keys().copied().collect())
            .unwrap_or_default();
        let w = self.workers.entry(key).or_insert_with(|| Worker::new(name.clone()));
        w.name = name;
        w.status = WorkerStatus::Connected;
        w.away_since = None;
        self.clip_sharing_changed();
        let Some(w) = self.workers.get_mut(&key) else { return };
        w.outbox =
            Some(crate::outbox::Outbox::new(link.out.clone(), crate::outbox::hold_control, cx));
        w.link = Some(link);
        w.links = w.links.saturating_add(1);
        let Some(w) = self.workers.get_mut(&key) else { return };
        w.home = (!home.is_empty()).then_some(home);
        w.settings = (!settings.is_empty()).then_some(settings);
        w.caps = Some(caps);
        w.load = Some(load);
        w.relay.reset();
        w.awaiting_snapshot = true;
        w.titles_requested = false;
        w.pending_opens.clear();
        w.failed_opens.clear();
        w.stopped.clear();
        w.sessions = sessions.into_iter().map(|s| (s.id, s)).collect();
        if let Some(sized) = w.sized.as_mut() {
            sized.lost();
        }
        self.desktop_relinked(key);
        self.reset_remote(key, &known, cx);
        self.relink_terminals(key, &known, cx);
        self.sync_approvals(cx);
        self.hub_agents(key, cx);
        cx.notify();
    }

    /// The link to `key` dropped or never came up, whatever dropped it: a silence, the worker
    /// restarting, a probe unanswered after a resume, the server saying the worker went away.
    /// Its live views keep what they showed: a shell its last rows, a remote window its last
    /// picture. The next link takes them up in place ([`Self::connect_worker`]): the shells
    /// attach again from the worker's checkpoint and the streams open again behind their last
    /// pictures.
    pub fn disconnect_worker(
        &mut self,
        key: WorkerKey,
        status: WorkerStatus,
        cx: &mut Context<Self>,
    ) {
        let now = self.now();
        let Some(w) = self.workers.get_mut(&key) else { return };
        let unanswered = w.fs_ops.lost();
        let said: Vec<String> =
            unanswered.iter().map(|op| slopty_client::folders::unanswered(&w.name, op)).collect();
        w.status = status;
        w.away_since.get_or_insert(now);
        w.link = None;
        w.outbox = None;
        w.rtt = None;
        w.relay.reset();
        w.pending_opens.clear();
        if let Some(sized) = w.sized.as_mut() {
            sized.lost();
        }
        // A stream opened on the link that dropped has nothing more coming.
        w.fresh_screens.clear();
        let sessions: Vec<SessionId> = w.sessions.keys().copied().collect();
        let items: Vec<ItemId> = w.doc.items().map(|i| i.id).collect();
        let screens = &self.screens;
        w.stale_screens.extend(items.iter().copied().filter(|id| screens.contains_key(id)));
        self.reset_remote(key, &sessions, cx);
        for line in said {
            Self::show_notice(line, cx);
        }
        self.sync_approvals(cx);
        // A machine out of reach starts nothing.
        self.hub_agents(key, cx);
        self.update_awake(cx);
        self.agents_moved(cx);
        cx.notify();
    }

    /// The shells of `key` kept through its last link's drop, among `known`, whose sessions
    /// the new link still runs: each attaches again from where it stands, showing what it
    /// showed until the worker's first frame replaces it, with the new link's clipboard and
    /// the agent the worker says runs there now.
    fn relink_terminals(&self, key: WorkerKey, known: &[SessionId], cx: &mut Context<Self>) {
        let Some(w) = self.workers.get(&key) else { return };
        let Some(link) = w.link.clone() else { return };
        let kept: Vec<(SessionId, Entity<TerminalView>)> = known
            .iter()
            .filter(|s| w.sessions.contains_key(s))
            .filter_map(|s| Some((*s, self.terminals.get(s)?.clone())))
            .collect();
        let colors = self.theme.terminal.wire();
        for (session, view) in kept {
            let clip = self.clip_hook(key);
            view.update(cx, |v, cx| v.relink(link.out.clone(), clip, cx));
            let req = TermRequest::Colors(colors);
            if let Some(w) = self.workers.get(&key) {
                w.send(ClientMsg::Term { session, req });
            }
        }
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

    /// The quiet line about the server in the titlebar ("server offline"); `None` hides
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

    /// The human forgot a worker: its views go with it.
    pub fn remove_worker(&mut self, key: WorkerKey, cx: &mut Context<Self>) {
        self.disconnect_worker(key, WorkerStatus::Connecting, cx);
        let Some(w) = self.workers.remove(&key) else { return };
        self.faces.threads.forget_meters(key);
        for item in w.doc.items() {
            self.screens.remove(&item.id);
            self.titles.remove(&item.id);
            if let ItemKind::Terminal { session } = item.kind {
                self.finished.remove(&About::Session(session));
                self.terminals.remove(&session);
            }
        }
        for session in w.sessions.keys() {
            self.finished.remove(&About::Session(*session));
            self.terminals.remove(session);
        }
        cx.notify();
    }

    /// Show `rtt` in the readouts for every worker with a round trip, in place of its link's
    /// (`None` shows the link's again). The e2e harness pins it, so what a golden shows never
    /// depends on how busy the machine was; the predictors and the dump keep the live figure.
    pub fn pin_rtt_readout(&mut self, rtt: Option<Duration>, cx: &mut Context<Self>) {
        self.pinned_rtt = rtt;
        let keys: Vec<WorkerKey> = self.workers.keys().copied().collect();
        for key in keys {
            self.tell_screens_rtt(key, cx);
        }
        cx.notify();
    }

    /// The round trip the readouts show for `w`: its link's, or the pinned one once it has one.
    pub(super) fn shown_rtt(&self, w: &Worker) -> Option<Duration> {
        shown_rtt(self.pinned_rtt, w.rtt)
    }

    /// Hand `key`'s remote windows its link's round trip, which times their pointer hold, and
    /// the one their overlays print.
    fn tell_screens_rtt(&self, key: WorkerKey, cx: &mut Context<Self>) {
        let Some(w) = self.workers.get(&key) else { return };
        let (live, shown) = (w.rtt, self.shown_rtt(w));
        for item in w.doc.items() {
            if let Some(view) = self.screens.get(&item.id) {
                view.update(cx, |v, cx| v.set_rtt(live, shown, cx));
            }
        }
    }

    /// Link RTT of `key`, fanned out to its terminals' predictors and its windows' overlays.
    pub fn set_rtt(&mut self, key: WorkerKey, rtt: Option<Duration>, cx: &mut Context<Self>) {
        let Some(w) = self.workers.get_mut(&key) else { return };
        w.rtt = rtt;
        let sessions: Vec<SessionId> = w.sessions.keys().copied().collect();
        for session in sessions {
            if let Some(view) = self.terminals.get(&session) {
                view.update(cx, |v, _| v.set_rtt(rtt));
            }
        }
        self.tell_screens_rtt(key, cx);
    }

    /// How the tailnet carries the link to `key`, as its worker last said; the link that
    /// brings it is the one it describes, so it goes with the link.
    pub fn set_link_path(&mut self, key: WorkerKey, path: LinkPath, cx: &mut Context<Self>) {
        let now = cx.background_executor().now();
        let Some(w) = self.workers.get_mut(&key).filter(|w| w.link.is_some()) else { return };
        let changed = w.relay.path() != Some(&path);
        w.relay.observe(path, now);
        if changed {
            cx.notify();
        }
    }

    /// What `key` can do changed: a permission granted, a display attached. The link's word
    /// and the server directory's land here alike.
    pub fn set_worker_caps(&mut self, key: WorkerKey, caps: WorkerCaps, cx: &mut Context<Self>) {
        let Some(w) = self.workers.get_mut(&key) else { return };
        if w.caps.as_ref() != Some(&caps) {
            let agents_moved = w.caps.as_ref().is_none_or(|was| was.agents != caps.agents);
            w.caps = Some(caps);
            // What a machine can start is what its link says.
            if agents_moved {
                self.hub_agents(key, cx);
            }
            self.refresh_offers(cx);
            cx.notify();
        }
    }

    /// `key` runs an older build on this wire (`Some`, what updates it), or no longer does: it
    /// links and works, and its rows offer Update as a refused one's do.
    pub fn set_worker_behind(
        &mut self,
        key: WorkerKey,
        notice: Option<slopty_client::update::UpdateNotice>,
        cx: &mut Context<Self>,
    ) {
        let Some(w) = self.workers.get_mut(&key) else { return };
        if w.behind != notice {
            w.behind = notice;
            cx.notify();
        }
    }

    /// What updates `key`: the notice of a worker that refused this build, else of one that
    /// links on an older build.
    #[must_use]
    pub fn update_notice(&self, key: WorkerKey) -> Option<&slopty_client::update::UpdateNotice> {
        let w = self.workers.get(&key)?;
        match &w.status {
            WorkerStatus::NeedsUpdate(notice) => Some(notice),
            _ => w.behind.as_ref(),
        }
    }

    /// `key`'s load average moved, as its link or the server says.
    pub fn set_worker_load(&mut self, key: WorkerKey, load: f32, cx: &mut Context<Self>) {
        let Some(w) = self.workers.get_mut(&key) else { return };
        if w.load != Some(load) {
            w.load = Some(load);
            cx.notify();
        }
    }

    /// How the tailnet carries the link to `key`, when its worker has said.
    #[must_use]
    pub fn link_path(&self, key: WorkerKey) -> Option<&LinkPath> {
        self.workers.get(&key)?.relay.path()
    }

    /// A registry snapshot or delta from `key`.
    pub fn apply_sync(&mut self, key: WorkerKey, sync: ItemSync, cx: &mut Context<Self>) {
        let Some(w) = self.workers.get_mut(&key) else { return };
        let Some(me) = w.link.as_ref().map(|l| l.me) else { return };
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
    }

    /// Bring the views in line with one change to `key`'s registry.
    pub(super) fn item_changed(
        &mut self,
        _key: WorkerKey,
        change: ItemChange,
        cx: &mut Context<Self>,
    ) {
        match change {
            ItemChange::Reset => {
                let gone: Vec<ItemId> = self
                    .screens
                    .keys()
                    .chain(self.titles.keys())
                    .copied()
                    .filter(|id| self.tile_of(*id).is_none())
                    .collect();
                for id in gone {
                    self.drop_item_views(id);
                }
            }
            ItemChange::Removed(id) => self.drop_item_views(id),
            ItemChange::Added { .. } | ItemChange::Changed(_) | ItemChange::Echo => {}
        }
        self.reconcile(cx);
        cx.notify();
    }

    /// The item `id` is gone: its stream and its title go with it.
    fn drop_item_views(&mut self, id: ItemId) {
        self.screens.remove(&id);
        self.titles.remove(&id);
    }

    /// A session appeared on `key` (this client's or another's).
    pub fn session_opened(
        &mut self,
        key: WorkerKey,
        summary: SessionSummary,
        cx: &mut Context<Self>,
    ) {
        let session = summary.id;
        if let Some(w) = self.workers.get_mut(&key) {
            w.sessions.insert(session, summary);
        }
        self.reconcile(cx);
        cx.notify();
    }

    /// A session is gone.
    pub fn session_closed(&mut self, session: SessionId, cx: &mut Context<Self>) {
        for w in self.workers.values_mut() {
            w.sessions.remove(&session);
        }
        // Its "finished" mark has nothing to clear it by looking: the bell must not keep it.
        self.finished.remove(&About::Session(session));
        self.update_awake(cx);
        self.reconcile(cx);
        self.agents_moved(cx);
        cx.notify();
    }

    /// A session's program exited: its view stays with its last screen and says so, until the
    /// human closes or restarts it. The worker announces the exit too; this is the attached
    /// view's word for it, a round trip sooner.
    fn session_exited(&mut self, session: SessionId, status: Option<i32>, cx: &mut Context<Self>) {
        for w in self.workers.values_mut() {
            if let Some(summary) = w.sessions.get_mut(&session) {
                summary.state = SessionState::Exited { status };
            }
        }
        cx.notify();
    }

    /// A session changed directory (OSC 7) or branch, and the worker says which repository
    /// and branch that is.
    pub fn session_moved(
        &mut self,
        session: SessionId,
        cwd: &str,
        repo: Option<&str>,
        branch: Option<&str>,
    ) {
        for w in self.workers.values_mut() {
            if let Some(summary) = w.sessions.get_mut(&session) {
                summary.cwd = Some(cwd.to_owned());
                summary.repo = repo.map(str::to_owned);
                summary.branch = branch.map(str::to_owned);
            }
        }
    }

    /// The session's summary, whichever worker runs it.
    pub(super) fn summary(&self, session: SessionId) -> Option<&SessionSummary> {
        self.workers.values().find_map(|w| w.sessions.get(&session))
    }

    /// A session-stream event.
    pub fn term_event(&self, session: SessionId, event: TermEvent, cx: &mut Context<Self>) {
        // A refusal the person caused (typing into a program that stopped reading, a request
        // of a terminal that ended) is theirs to see, not only the log's.
        if let TermEvent::Error(error) = &event {
            tracing::warn!(%session, %error, "worker");
            Self::show_failure(error.to_string(), cx);
        }
        if let Some(view) = self.terminals.get(&session) {
            view.update(cx, |v, cx| v.apply(event, cx));
        }
    }

    /// Views for every item whose content is alive, none for the rest: terminals attached,
    /// streams open for the remote windows.
    pub(super) fn reconcile(&mut self, cx: &mut Context<Self>) {
        let wanted: Vec<(WorkerKey, SessionId)> = self
            .workers
            .iter()
            .filter(|(_, w)| w.link.is_some())
            .flat_map(|(key, w)| {
                w.doc.items().filter_map(move |i| match i.kind {
                    ItemKind::Terminal { session } => Some((*key, session)),
                    _ => None,
                })
            })
            .filter(|(key, s)| self.workers.get(key).is_some_and(|w| w.sessions.contains_key(s)))
            .collect();
        for &(key, session) in &wanted {
            if self.terminals.contains_key(&session) {
                continue;
            }
            self.attach_terminal(key, session, cx);
        }
        // A worker away keeps its shells' views, showing their last rows, for its next link to
        // attach again; one closed meanwhile lets go of its view.
        let away: std::collections::HashSet<SessionId> = self
            .workers
            .iter()
            .filter(|(_, w)| w.link.is_none())
            .flat_map(|(_, w)| {
                w.doc
                    .items()
                    .filter_map(|i| match i.kind {
                        ItemKind::Terminal { session } => Some(session),
                        _ => None,
                    })
                    .filter(|s| w.sessions.contains_key(s))
            })
            .collect();
        let gone: Vec<SessionId> = self
            .terminals
            .keys()
            .filter(|s| !away.contains(s) && !wanted.iter().any(|(_, w)| w == *s))
            .copied()
            .collect();
        for session in gone {
            if self.summary(session).is_some() {
                self.send_session(session, ClientMsg::Term { session, req: TermRequest::Detach });
            }
            self.terminals.remove(&session);
        }
        self.reconcile_screens();
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
        let clip = self.clip_hook(key);
        let view = cx.new(|cx| {
            let mut view = TerminalView::new(session, size, link.out.clone(), theme, cx);
            if let Some(clip) = clip {
                view.set_clip_hook(clip);
            }
            view
        });
        let sid = session;
        cx.subscribe(&view, move |this, _view, event, cx| match event {
            TerminalViewEvent::Bell => cx.emit(WorkspaceEvent::Bell(sid)),
            // The Dock bounce; the banner while the app is away is the app's
            // (`attention::Attention::program`).
            TerminalViewEvent::Notification { .. } => cx.emit(WorkspaceEvent::Program(sid)),
            TerminalViewEvent::Exited(status) => this.session_exited(sid, *status, cx),
            TerminalViewEvent::CloseConfirmed => {
                if let Some(key) = this.worker_of_session(sid) {
                    this.close_session_on(key, sid);
                }
            }
            TerminalViewEvent::Cwd { path, repo, branch } => {
                this.session_moved(sid, path, repo.as_deref(), branch.as_deref());
                cx.notify();
            }
            TerminalViewEvent::Notice(text) => Self::show_notice(text.clone(), cx),
            TerminalViewEvent::CommandFinished { command, exit, elapsed } => {
                let done = Finished {
                    command: command.clone(),
                    exit: *exit,
                    elapsed: *elapsed,
                    turn: None,
                };
                this.command_finished(sid, done, cx);
            }
            TerminalViewEvent::PasteFiles(files) => {
                this.paste_files_in_shell(sid, files.clone(), cx);
            }
            // The shell draws the title and the progress; a block, a file, a page or a drag out
            // of a live terminal is the shell's to take up.
            TerminalViewEvent::Title(_)
            | TerminalViewEvent::Progress
            | TerminalViewEvent::Attach(_)
            | TerminalViewEvent::ViewFile { .. }
            | TerminalViewEvent::OpenPage { .. }
            | TerminalViewEvent::DragOut { .. } => {}
        })
        .detach();
        w.send(ClientMsg::Term { session, req: TermRequest::Attach { size } });
        // What this client paints with, so the driver's colours answer colour queries.
        w.send(ClientMsg::Term { session, req: TermRequest::Colors(self.theme.terminal.wire()) });
        if let Some(rtt) = w.rtt {
            view.update(cx, |v, _| v.set_rtt(Some(rtt)));
        }
        self.terminals.insert(session, view);
    }

    /// Open streams for remote windows that should have one and lack it; let go of the rest.
    pub(super) fn reconcile_screens(&mut self) {
        let quality = quality_for();
        let key = self.desktop_key();
        let mut keep: Vec<ItemId> = Vec::new();
        for w in self.workers.values_mut() {
            if w.link.is_none() {
                // Away, its windows keep their last pictures for the next link to replace.
                let screens = &self.screens;
                keep.extend(w.doc.items().map(|i| i.id).filter(|id| screens.contains_key(id)));
                continue;
            }
            // A desktop streamed from a display made for this device opens its own way; one
            // gone from the registry goes back to nothing.
            let sized = w.sized.as_ref().map(desktop::Sized::item);
            if let Some(id) = sized {
                if key.is_none() || !w.doc.items().any(|i| i.id == id) {
                    w.sized = None;
                } else {
                    keep.push(id);
                    let streaming = (self.screens.contains_key(&id)
                        && !w.stale_screens.contains(&id))
                        || w.fresh_screens.contains_key(&id);
                    let open =
                        w.sized.as_mut().zip(key).and_then(|(s, key)| {
                            (!streaming).then(|| s.open(key, quality)).flatten()
                        });
                    if let Some(open) = open {
                        w.send(open);
                    }
                }
            }
            let sized = w.sized.as_ref().map(desktop::Sized::item);
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
                .filter(|i| Some(i.id) != sized)
                .filter_map(|i| match i.kind {
                    ItemKind::Window { window } => Some((i.id, CaptureTarget::Window(window))),
                    ItemKind::Display { display } => Some((i.id, CaptureTarget::Display(display))),
                    ItemKind::Terminal { .. } => None,
                })
                .collect();
            for &(id, target) in &wanted {
                keep.push(id);
                let streaming = (self.screens.contains_key(&id) && !w.stale_screens.contains(&id))
                    || w.fresh_screens.contains_key(&id);
                if streaming
                    || w.pending_opens.values().any(|&p| p == id)
                    || w.failed_opens.contains_key(&id)
                    || w.stopped.get(&id).is_some_and(|s| s.why.is_some())
                {
                    continue;
                }
                if w.pending_opens.contains_key(&target) {
                    continue;
                }
                w.pending_opens.insert(target, id);
                w.send(ClientMsg::Screen(ScreenRequest::Open { target, quality }));
            }
            w.pending_opens.retain(|_, id| wanted.iter().any(|(k, _)| k == id));
            w.failed_opens.retain(|id, _| wanted.iter().any(|(k, _)| k == id));
            w.stopped.retain(|id, _| wanted.iter().any(|(k, _)| k == id));
        }
        // Dropping a view sends `Close` for its stream.
        self.screens.retain(|id, _| keep.contains(id));
        for w in self.workers.values_mut() {
            w.stale_screens.retain(|id| keep.contains(id));
            w.fresh_screens.retain(|id, _| keep.contains(id));
        }
    }

    /// A remote-window event from `key`.
    pub fn screen_event(&mut self, key: WorkerKey, event: ScreenEvent, cx: &mut Context<Self>) {
        match event {
            ScreenEvent::Listing { windows, displays } => {
                self.live_listing(key, &windows, &displays, cx);
                self.fill_titles(key, &windows);
                let Some(w) = self.workers.get_mut(&key) else { return };
                w.titles_requested = false;
            }
            ScreenEvent::Display { stream, key: display_key, display } => {
                self.display_told(key, stream, display_key, display, cx);
            }
            ScreenEvent::Curtain(state) => self.curtain_heard(key, state, cx),
            ScreenEvent::Opened { stream, target, codec, width, height, .. } => {
                let theme = self.theme.clone();
                let quality = quality_for();
                let pinned = self.pinned_rtt;
                let Some(w) = self.workers.get_mut(&key) else { return };
                let Some(link) = w.link.clone() else { return };
                let sized = w.sized.as_mut().and_then(|s| s.opened(stream).then(|| s.item()));
                let Some(id) = sized.or_else(|| w.pending_opens.remove(&target)) else {
                    tracing::debug!(%stream, ?target, "opened stream nobody asked for; closing");
                    w.send(ClientMsg::Screen(ScreenRequest::Close(stream)));
                    return;
                };
                let handle = (link.open_screen)(stream, codec);
                let opened =
                    crate::screen::Opened { stream, target, size: (width, height), quality };
                let (rtt, shown) = (w.rtt, shown_rtt(pinned, w.rtt));
                let hook = self.paste_hook(key);
                let view = cx.new(|cx| {
                    let mut view = ScreenView::new(opened, handle, link.out.clone(), theme, cx);
                    if let Some(hook) = hook {
                        view.set_paste_hook(hook);
                    }
                    view
                });
                view.update(cx, |v, cx| v.set_rtt(rtt, shown, cx));
                let tile = ItemRef { worker: key, item: id };
                let behind_stale = self.screens.contains_key(&id)
                    && self.workers.get(&key).is_some_and(|w| w.stale_screens.contains(&id));
                cx.subscribe(&view, move |this, view, event, cx| match event {
                    crate::screen::ScreenViewEvent::Pressed => {}
                    crate::screen::ScreenViewEvent::Ready => {
                        this.fresh_stream_shows(key, id, &view, cx);
                        cx.notify();
                    }
                    crate::screen::ScreenViewEvent::Health => cx.notify(),
                    crate::screen::ScreenViewEvent::PasteFiles(files) => {
                        let view = view.downgrade();
                        this.paste_files_in_window(tile, &view, files.clone(), cx);
                    }
                    // A drag out of a live window is the shell's to take up.
                    crate::screen::ScreenViewEvent::DragOut(_) => {
                        Self::show_notice("The drag could not go on here".to_owned(), cx);
                    }
                    crate::screen::ScreenViewEvent::CancelPaste => this.cancel_paste_of(&view, cx),
                    crate::screen::ScreenViewEvent::DragOutFailed(why) => {
                        Self::show_failure(format!("The drag did not come out: {why}"), cx);
                    }
                    crate::screen::ScreenViewEvent::Control(control) => {
                        this.screen_control(id, *control, cx);
                    }
                    crate::screen::ScreenViewEvent::Keyboard(_) => this.arm_system_keys(cx),
                    crate::screen::ScreenViewEvent::Rested => this.screen_rested(id, cx),
                })
                .detach();
                if behind_stale {
                    if let Some(w) = self.workers.get_mut(&key) {
                        w.fresh_screens.insert(id, view.clone());
                    }
                } else {
                    self.screens.insert(id, view.clone());
                }
                self.screen_arrived(id, &view, cx);
            }
            ScreenEvent::Closed { stream, reason } => {
                // A display made for this device that ends goes back to the physical one,
                // rather than being asked for again and again.
                if let Some(w) = self.workers.get_mut(&key)
                    && w.sized.as_ref().is_some_and(|s| s.streams(stream))
                {
                    w.sized = None;
                }
                // A view still held was not let go by this client: the worker ended it.
                let now = std::time::Instant::now();
                for (id, _) in self.stream_views(key, stream, cx) {
                    tracing::info!(%stream, %reason, "screen closed by worker");
                    // Its last picture goes with it: what it showed has ended.
                    self.screens.remove(&id);
                    let Some(w) = self.workers.get_mut(&key) else { continue };
                    w.fresh_screens.remove(&id);
                    w.stale_screens.remove(&id);
                    // Asked for again at once, unless it ended just before as well.
                    let again = w
                        .stopped
                        .get(&id)
                        .is_some_and(|s| now.saturating_duration_since(s.at) < STOP_AGAIN);
                    let why = again.then(|| reason.clone());
                    w.stopped.insert(id, Stopped { at: now, why });
                }
                self.reconcile_screens();
            }
            ScreenEvent::Geometry { stream, width, height, .. } => {
                if width > 0 && height > 0 {
                    for (_, view) in self.stream_views(key, stream, cx) {
                        view.update(cx, |v, cx| v.set_geometry(width, height, cx));
                    }
                }
            }
            ScreenEvent::Rate { stream, target_bps, verdict, capped } => {
                for (_, view) in self.stream_views(key, stream, cx) {
                    view.update(cx, |v, cx| v.set_rate(target_bps, verdict, capped, cx));
                }
            }
            ScreenEvent::Source { stream, state } => {
                for (id, view) in self.stream_views(key, stream, cx) {
                    view.update(cx, |v, cx| v.set_source_state(state, cx));
                    // The worker has said how its target stands: the new stream knows more
                    // than the last picture, even before (or without) a picture of its own.
                    self.fresh_stream_shows(key, id, &view, cx);
                }
            }
            ScreenEvent::KeyboardSource { stream, source, applied } => {
                for (_, view) in self.stream_views(key, stream, cx) {
                    view.update(cx, |v, _| v.set_keyboard_source(&source, applied));
                }
            }
            ScreenEvent::Cursor { stream, shape } => {
                for (_, view) in self.stream_views(key, stream, cx) {
                    view.update(cx, |v, cx| v.set_cursor_shape(shape.clone(), cx));
                }
            }
            ScreenEvent::Field { stream, field } => {
                for (_, view) in self.stream_views(key, stream, cx) {
                    view.update(cx, |v, cx| v.set_field(field, cx));
                }
            }
            ScreenEvent::OpenFailed { asked, why } => self.screen_refused(key, asked, why, cx),
            ScreenEvent::ListFailed { why } => {
                self.live_listing_failed(key, &why, cx);
                let Some(w) = self.workers.get_mut(&key) else { return };
                w.titles_requested = false;
            }
            ScreenEvent::Drag { stream, event } => {
                for (_, view) in self.stream_views(key, stream, cx) {
                    let ended = view.update(cx, |v, cx| v.drag_heard(&event, cx));
                    if let Some((drag, outcome)) = ended {
                        tracing::debug!(?drag, ?outcome, "a drag into a window ended");
                    }
                }
            }
        }
        cx.notify();
    }

    /// An open the worker refused: the window waiting on it stops asking until the next link,
    /// and the shell's live view says why ([`Self::live_open_failed`] for its own asks).
    fn screen_refused(
        &mut self,
        key: WorkerKey,
        asked: OpenAsk,
        why: ScreenFailure,
        _cx: &mut Context<Self>,
    ) {
        let OpenAsk::Target(target) = asked else {
            // Not made: its desktop goes back to its physical display, and this worker is not
            // asked again unasked on this link.
            tracing::info!(?why, "a display made for this device refused");
            if self.display_declined(key).is_some() {
                self.reconcile_screens();
            }
            return;
        };
        let Some(w) = self.workers.get_mut(&key) else { return };
        let Some(id) = w.pending_opens.remove(&target) else {
            tracing::debug!(?asked, ?why, "a refused open nobody waits on");
            return;
        };
        tracing::info!(%id, ?asked, ?why, "screen open refused");
        w.failed_opens.insert(id, why);
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

    /// The views of `key`'s items that show `stream` on its current link, each with its item:
    /// one opened behind a stale picture, or the tile's own. Stream ids are per link, so a
    /// picture kept from a link that has gone never hears the new one's.
    fn stream_views(
        &self,
        key: WorkerKey,
        stream: StreamId,
        cx: &Context<Self>,
    ) -> Vec<(ItemId, Entity<ScreenView>)> {
        let Some(w) = self.workers.get(&key) else { return Vec::new() };
        w.doc
            .items()
            .filter_map(|i| {
                let view = w.fresh_screens.get(&i.id).or_else(|| {
                    self.screens.get(&i.id).filter(|_| !w.stale_screens.contains(&i.id))
                })?;
                (view.read(cx).stream() == stream).then(|| (i.id, view.clone()))
            })
            .collect()
    }

    /// `view`, opened on the new link behind item `id`'s last picture, has something to show:
    /// it takes the stale picture's place.
    fn fresh_stream_shows(
        &mut self,
        key: WorkerKey,
        id: ItemId,
        view: &Entity<ScreenView>,
        cx: &mut Context<Self>,
    ) {
        let Some(w) = self.workers.get_mut(&key) else { return };
        if w.fresh_screens.get(&id).is_none_or(|fresh| fresh != view) {
            return;
        }
        w.fresh_screens.remove(&id);
        w.stale_screens.remove(&id);
        self.screens.insert(id, view.clone());
        cx.notify();
    }
}

/// The round trip a readout shows for a link measured at `live`: `pinned` once there is one.
fn shown_rtt(pinned: Option<Duration>, live: Option<Duration>) -> Option<Duration> {
    live.map(|live| pinned.unwrap_or(live))
}

/// Requested quality for a new stream: full scale at the main screen's refresh. The view then
/// asks for a scale matching the width it paints at, and for the refresh of the screen it is
/// drawn on.
fn quality_for() -> Quality {
    crate::screen::quality_of(1.0, crate::screen::main_refresh_hz())
}

/// Whether `sharing` shares the clipboard with the worker behind `key`, by its worker id.
pub(super) fn shares_clipboard(
    sharing: &slopty_settings::ClipboardSettings,
    key: WorkerKey,
) -> bool {
    super::keys::worker_id(key).is_none_or(|id| sharing.shared_with(&id.to_string()))
}

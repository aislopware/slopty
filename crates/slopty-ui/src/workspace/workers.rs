//! Workers coming and going, and what they say: the registry sync that places tiles, the
//! sessions, the streams and the files behind the tiles.

use std::time::Duration;

use gpui::{App, AppContext as _, Context, Entity, Window};
use slopty_client::ItemChange;
use slopty_client::layout::{TileRef, WorkerKey};
use slopty_core::{ItemId, SessionId, StreamId};
use slopty_proto::ClientMsg;
use slopty_proto::file::{FileRead, WriteResult};
use slopty_proto::handshake::HelloAck;
use slopty_proto::items::{ItemKind, ItemSync};
use slopty_proto::screen::{
    CaptureTarget, OpenAsk, Quality, ScreenEvent, ScreenFailure, ScreenRequest,
};
use slopty_proto::server::WorkerCaps;
use slopty_proto::tailnet::LinkPath;
use slopty_proto::terminal::{SessionState, SessionSummary, TermEvent, TermRequest, TermSize};
use slopty_proto::thread::ThreadId;

use super::attention::About;
use super::{Finished, Worker, WorkerLink, WorkerStatus, WorkspaceEvent, WorkspaceView, desktop};
use crate::file::{FileView, FileViewEvent};
use crate::screen::ScreenView;
use crate::terminal::{AttachProbe, TerminalView, TerminalViewEvent};

/// How long a quote waits for the composer it is for to be made, its face brought up or its
/// tile opened for it (an opened tile comes back from the worker first).
const QUOTE_WAIT: Duration = Duration::from_secs(5);

/// A stream the worker ends unasked again within this of the last is not asked for again: its
/// tile says it stopped and offers to reopen, rather than flicker between a picture and none.
pub(super) const STOP_AGAIN: Duration = Duration::from_secs(10);

/// The way to have a worker see a Screen Recording grant made at its desk: a running process
/// never does, so its daemon exits for its service manager to start it again.
pub const RESTART_WORKER: &str = "Restart Slopty there";

/// A remote tile whose stream the worker ended unasked ([`Worker::stopped`]).
#[derive(Debug)]
pub(super) struct Stopped {
    /// When it last ended.
    at: std::time::Instant,
    /// Set once it ended twice within [`STOP_AGAIN`]: the worker's words, shown in the pane,
    /// and the tile is not asked for again until the person reopens it or the link comes back.
    why: Option<String>,
}

impl Stopped {
    /// Why it stopped, once it is shown as stopped.
    pub(super) fn why(&self) -> Option<&str> {
        self.why.as_deref()
    }
}

impl WorkspaceView {
    /// "Stop sharing the clipboard with …" or "Share the clipboard with …": applied at once
    /// and said, and handed to the app to keep in the settings by the machine's name.
    pub(super) fn share_clipboard(
        &mut self,
        share: &super::actions::ShareClipboard,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(name) = self.workers.get(&share.worker).map(|w| w.name.clone()) else { return };
        let mut sharing = self.clip_sharing.clone();
        sharing.workers.insert(name.clone(), share.share);
        self.set_clipboard_sharing(sharing, cx);
        let said = if share.share {
            format!("The clipboard is shared with {name}")
        } else {
            format!("The clipboard is no longer shared with {name}")
        };
        self.show_notice(said, cx);
        cx.emit(WorkspaceEvent::ClipboardShared { worker: share.worker, share: share.share });
    }

    /// Whether the clipboard is shared with `key`, as the settings say by its name.
    #[must_use]
    pub fn clipboard_shared(&self, key: WorkerKey) -> bool {
        self.workers.get(&key).is_none_or(|w| self.clip_sharing.shared_with(&w.name))
    }

    /// "Stop sharing the clipboard with …" for each machine it is shared with, and "Share the
    /// clipboard with …" for each it is not.
    pub(super) fn clipboard_lines(&self) -> Vec<crate::palette::PaletteItem> {
        self.workers
            .iter()
            .map(|(key, w)| {
                let shared = self.clip_sharing.shared_with(&w.name);
                let label = if shared {
                    format!("Stop sharing the clipboard with {}", w.name)
                } else {
                    format!("Share the clipboard with {}", w.name)
                };
                let action = super::actions::ShareClipboard { worker: *key, share: !shared };
                crate::palette::PaletteItem::new(&label, Box::new(action), &[])
            })
            .collect()
    }

    /// The tailnet policy grant that lets this device in, which the away pill copies for a
    /// worker whose policy turns it away.
    pub fn set_tailnet_grant(&mut self, grant: Option<String>) {
        self.tailnet_grant = grant.map(gpui::SharedString::from);
    }

    /// The thread a command block from `session`'s terminal goes to as context: that of the
    /// agent in that terminal, else that of the agent tile last focused on the same worker,
    /// whatever the agent and whichever tile holds it (a terminal on its thread, or a thread's
    /// own tile). Only a thread whose agent is live takes one. `None` hides the offer to attach.
    pub(super) fn block_target(&self, session: SessionId) -> Option<ThreadId> {
        let agent_thread = |s: SessionId| self.agent_state(s).and_then(|_| self.session_thread(s));
        if let Some(thread) = agent_thread(session) {
            return Some(thread);
        }
        let worker = self.worker_of_session(session)?;
        let live = |t: &ThreadId| self.thread_stand(*t).is_some_and(|s| !s.exited);
        self.recency.iter().rev().find_map(|id| {
            let tile = self.tile_of(*id)?;
            if tile.worker != worker {
                return None;
            }
            match self.item(tile)?.kind {
                ItemKind::Terminal { session: other } => agent_thread(other),
                ItemKind::Thread { thread } => Some(thread).filter(live),
                _ => None,
            }
        })
    }

    /// A command block from `from`'s terminal, as Markdown, into the draft of the thread
    /// [`Self::block_target`] picks ([`Self::quote_to_thread`]).
    fn attach_block(&mut self, from: SessionId, text: String, cx: &mut Context<Self>) {
        let Some(thread) = self.block_target(from) else {
            self.show_notice("No agent to attach the block to".to_owned(), cx);
            return;
        };
        self.quote_to_thread(thread, text, cx, |_taken, _cx| {});
    }

    /// `text` at the end of `thread`'s draft, whatever its agent and wherever it shows: its
    /// terminal's tile is turned to its thread, its own tile brought up, or one opened for it;
    /// the text lands once a composer of the thread is there, with the keyboard. `done` hears
    /// whether one took it, so what the text came from goes only then.
    pub(super) fn quote_to_thread(
        &mut self,
        thread: ThreadId,
        text: String,
        cx: &mut Context<Self>,
        done: impl FnOnce(bool, &mut App) + 'static,
    ) {
        if let Some((_, session)) = self.live_terminal(thread) {
            self.show_face(session, true, cx);
        }
        if let Some(tile) = self.tile_of_thread(thread) {
            self.go_to(tile.item, cx);
        } else if let Some(key) = self.worker_of_thread(thread, cx) {
            self.open_thread(key, thread, cx);
        } else {
            self.show_notice("That thread is on a machine not connected here".to_owned(), cx);
            done(false, cx);
            return;
        }
        self.quote_into(thread, text, Box::new(done), cx);
    }

    /// Put `text` in the composer it is for once that is made: the next frames look for it
    /// ([`Self::settle_quotes`]), and after [`QUOTE_WAIT`] it is given up, the person told.
    fn quote_into(&mut self, to: ThreadId, text: String, done: Done, cx: &mut Context<Self>) {
        let id = self.quotes.next;
        self.quotes.next = id.wrapping_add(1);
        self.quotes.waiting.push(Quote { id, to, text, done });
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(QUOTE_WAIT).await;
            let _gone = this.update(cx, |w, cx| w.quote_lapsed(id, cx));
        })
        .detach();
        cx.notify();
    }

    /// Each frame, after the views are made: every waiting quote whose composer is there goes
    /// into its draft, with the keyboard, in the window its tile is in (a popped-out tile's own).
    pub(super) fn settle_quotes(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.quotes.waiting.is_empty() {
            return;
        }
        let here = window.window_handle();
        for quote in std::mem::take(&mut self.quotes.waiting) {
            let Some(composer) = self.composer_of_thread(quote.to, cx) else {
                self.quotes.waiting.push(quote);
                continue;
            };
            let popped = self.tile_of_thread(quote.to).and_then(|t| self.popouts.window(t.item));
            let into = popped.unwrap_or(here);
            let Quote { text, done, .. } = quote;
            // Outside this draw, so the composer redraws with the words in it.
            cx.defer(move |cx| {
                let landed = into.update(cx, |_, window, cx| composer.quote(&text, window, cx));
                done(landed.is_ok(), cx);
            });
        }
    }

    /// Quote `id` found no composer in time: what it came from keeps it, and the person is told.
    fn quote_lapsed(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(at) = self.quotes.waiting.iter().position(|q| q.id == id) else { return };
        let quote = self.quotes.waiting.remove(at);
        tracing::warn!("no composer showed; the text was not added");
        self.show_failure("The agent's composer did not open".to_owned(), cx);
        (quote.done)(false, cx);
    }

    /// The clock a worker's time out of reach is told by, with the tick that keeps its tiles'
    /// "Reconnecting for …" current started, once, while any worker is away.
    fn away_clock(&mut self, cx: &Context<Self>) -> Duration {
        if !self.away_ticking {
            self.away_ticking = true;
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(super::tile::AWAY_STEP).await;
                    let away = this.update(cx, |this, cx| {
                        let away = this.workers.values().any(|w| w.away_since.is_some());
                        if away {
                            cx.notify();
                        } else {
                            this.away_ticking = false;
                        }
                        away
                    });
                    if !matches!(away, Ok(true)) {
                        break;
                    }
                }
            })
            .detach();
        }
        self.now()
    }

    /// A worker this client has added, before its first connection: its tiles (from the
    /// saved layout) stay where they were, drawn from its items as this device last saw them
    /// until its first snapshot.
    pub fn add_worker(&mut self, key: WorkerKey, name: String, cx: &mut Context<Self>) {
        if let Some(w) = self.workers.get_mut(&key) {
            w.name = name;
        } else {
            let mut w = Worker::new(name);
            w.away_since = Some(self.away_clock(cx));
            if let Some(doc) = self.kept_doc(key) {
                w.doc = doc;
                self.items_dirty = true;
            }
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
        self.desktop.relinked(key);
        self.clip_sharing_changed();
        let Some(w) = self.workers.get_mut(&key) else { return };
        w.outbox =
            Some(crate::outbox::Outbox::new(link.out.clone(), crate::outbox::hold_control, cx));
        w.link = Some(link);
        w.links = w.links.saturating_add(1);
        // The worker hands pages and files only to a client that said it takes them.
        self.declare_handoffs(key);
        self.curtain_relinked(key);
        // A board's tasks on it are no longer marked away.
        self.projects.dirty = true;
        self.focus_link_reset(key);
        let Some(w) = self.workers.get_mut(&key) else { return };
        w.home = (!home.is_empty()).then_some(home);
        w.settings = (!settings.is_empty()).then_some(settings);
        w.caps = Some(caps);
        w.load = Some(load);
        w.relay.reset();
        w.relay_due = None;
        w.awaiting_snapshot = true;
        w.titles_requested = false;
        w.watched.clear();
        w.watched_folders.clear();
        w.pending_opens.clear();
        w.openings.clear();
        w.dropped.clear();
        w.failed_opens.clear();
        w.stopped.clear();
        if let Some(sized) = w.sized.as_mut() {
            sized.lost();
        }
        w.sessions = sessions.into_iter().map(|s| (s.id, s)).collect();
        self.items_dirty = true;
        self.reset_remote(key, &known, cx);
        self.relink_terminals(key, &known, cx);
        // The disk may have moved on while the worker was away: every file tile of it reads
        // again, and one holding an edit weighs it against what is there now.
        let files: Vec<ItemId> = self
            .workers
            .get(&key)
            .map(|w| w.doc.items().map(|i| i.id).filter(|id| self.files.contains_key(id)).collect())
            .unwrap_or_default();
        for id in files {
            self.request_file(id);
        }
        self.hub_agents(key, cx);
        cx.notify();
    }

    /// The link to `key` dropped or never came up, whatever dropped it: a silence, the worker
    /// restarting, a probe unanswered after a resume, the server saying the worker went away.
    /// Its tiles stay where they are and keep what they showed, set back under a pill saying
    /// the worker is away: a shell its last rows, a remote window its last picture, a face its
    /// draft, a note or a file its text. The next link takes them up in place
    /// ([`Self::connect_worker`]): the shells attach again from the worker's checkpoint, the
    /// faces follow again, and the streams open again behind their last pictures.
    pub fn disconnect_worker(
        &mut self,
        key: WorkerKey,
        status: WorkerStatus,
        cx: &mut Context<Self>,
    ) {
        let now = self.away_clock(cx);
        // An ask the link took with it is never answered: the next one goes.
        self.listing.retain(|(worker, _)| *worker != key);
        self.clone_lost(key, cx);
        self.run_lost(key, cx);
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
        w.relay_due = None;
        w.pending_opens.clear();
        // A shell asked for on it never comes, nor a tile opened by a drop.
        w.openings.clear();
        w.dropped.clear();
        // A stream opened on the link that dropped has nothing more coming.
        w.fresh_screens.clear();
        if let Some(sized) = w.sized.as_mut() {
            sized.lost();
        }
        w.picker_wanted = false;
        w.display_wanted = false;
        let sessions: Vec<SessionId> = w.sessions.keys().copied().collect();
        let items: Vec<ItemId> = w.doc.items().map(|i| i.id).collect();
        let screens = &self.screens;
        w.stale_screens.extend(items.iter().copied().filter(|id| screens.contains_key(id)));
        self.probes.retain(|(k, ..)| *k != key);
        // A board marks the tasks that run on it as away.
        self.projects.dirty = true;
        self.reset_remote(key, &sessions, cx);
        // The worker's own word on its agents went with the link; the server's ladder stands
        // in ([`Self::agent_state`]).
        for session in &sessions {
            self.handoff.forget_session(*session);
        }
        for item in &items {
            // A save sent on the link that dropped has no answer coming.
            if let Some(view) = self.files.get(item) {
                view.update(cx, FileView::link_lost);
            }
            // Asked again on the next link: what is in it may have changed meanwhile.
            if let Some(view) = self.folders.get(item) {
                view.update(cx, |v, _| v.refresh());
            }
        }
        for closed in self.closed.iter().filter(|c| c.tile.worker == key) {
            if let Some(view) = &closed.file {
                view.update(cx, FileView::link_lost);
            }
        }
        // The folder's next listing shows whether each was done.
        for op in &unanswered {
            self.folders_heard(key, op, true, cx);
        }
        for line in said {
            self.show_notice(line, cx);
        }
        self.items_dirty = true;
        if self.picker.as_ref().is_some_and(|(k, _)| *k == key) {
            self.let_picker_leave(cx);
            self.pending_return = true;
        }
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

    /// Whether `tile`'s body may show what is no longer so, and is drawn set back: its worker
    /// is away or its link in doubt ([`WorkerStatus::in_doubt`]), or its picture came over a
    /// link that has gone.
    pub(super) fn set_back(&self, tile: TileRef) -> bool {
        let Some(w) = self.workers.get(&tile.worker) else { return false };
        w.stale_screens.contains(&tile.item) || w.status.in_doubt() || w.link.is_none()
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

    /// The human forgot a worker: its tiles leave the layout with it.
    pub fn remove_worker(&mut self, key: WorkerKey, cx: &mut Context<Self>) {
        self.disconnect_worker(key, WorkerStatus::Connecting, cx);
        let Some(w) = self.workers.remove(&key) else { return };
        self.faces.threads.forget_meters(key);
        self.past_places.remove(&key);
        for item in w.doc.items() {
            self.drop_item_views(item.id, cx);
            if let ItemKind::Terminal { session } = item.kind {
                self.finished.remove(&About::Session(session));
                self.terminals.remove(&session);
            }
        }
        for session in w.sessions.keys() {
            self.finished.remove(&About::Session(*session));
            self.terminals.remove(session);
        }
        let (gone, closed): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.closed).into_iter().partition(|c| c.tile.worker == key);
        self.closed = closed;
        for view in gone.iter().filter_map(|c| c.file.as_ref()) {
            self.file_tile_gone(view, cx);
        }
        let workers = &self.workers;
        self.recency.retain(|id| workers.values().any(|w| w.doc.get(*id).is_some()));
        // Added again, it is a new worker: an empty registry is given its shell again.
        self.given_shell.remove(&key);
        self.given_pending.remove(&key);
        self.nav.folded.remove(&slopty_client::layout::GroupKey::machine(key));
        self.items_dirty = true;
        self.layout.retain_worker(key, |_| false);
        self.layout_touched(cx);
        self.after_focus_moved(cx);
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
        let label = super::navigator::rtt_label;
        let changed = w.rtt.map(label) != rtt.map(label);
        let was = std::mem::replace(&mut w.rtt, rtt);
        let sessions: Vec<SessionId> = w.sessions.keys().copied().collect();
        for session in sessions {
            if let Some(view) = self.terminals.get(&session) {
                view.update(cx, |v, _| v.set_rtt(rtt));
            }
        }
        self.tell_screens_rtt(key, cx);
        // The bars and the navigator show it: repaint only when what they print moved.
        if changed && self.rtt_shown(was, rtt) {
            cx.notify();
        }
    }

    /// How the tailnet carries the link to `key`, as its worker last said; the link that
    /// brings it is the one it describes, so it goes with the link. A DERP relay is said once
    /// it has held ([`slopty_client::relay::RelayWatch`]), so the chrome draws again then.
    pub fn set_link_path(&mut self, key: WorkerKey, path: LinkPath, cx: &mut Context<Self>) {
        let now = cx.background_executor().now();
        let Some(w) = self.workers.get_mut(&key).filter(|w| w.link.is_some()) else { return };
        let changed = w.relay.path() != Some(&path);
        w.relay.observe(path, now);
        w.relay_due = w.relay.due(now).map(|at| {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(at.saturating_duration_since(now)).await;
                let _gone = this.update(cx, |_this, cx| cx.notify());
            })
        });
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

    /// An open this client asked of `key` failed: the tile it would have made never comes, so
    /// the person hears why.
    pub fn open_failed(
        &mut self,
        key: WorkerKey,
        request: slopty_proto::RequestId,
        message: &str,
        cx: &mut Context<Self>,
    ) {
        self.opening_failed(key, request);
        let name = self.workers.get(&key).map_or("the machine", |w| w.name.as_str());
        let text = format!("Could not open a terminal on {name}: {message}");
        self.show_notice(text, cx);
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
        if snapshot {
            self.first_snapshot(key, cx);
            self.reopen_kept(key, cx);
            self.run_parked_tap(key, cx);
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
        match change {
            ItemChange::Reset => {
                let Some(w) = self.workers.get(&key) else { return };
                let present: std::collections::HashSet<ItemId> =
                    w.doc.items().map(|i| i.id).collect();
                // A tile whose item the worker no longer has leaves; the rest stay where this
                // device put them, and anything new is a background tab in its project.
                let starting = &self.starting;
                self.layout.retain_worker(key, |id| present.contains(&id) || starting.has(id));
                let new: Vec<ItemId> = w
                    .doc
                    .items()
                    .map(|i| i.id)
                    .filter(|id| !self.layout.contains(TileRef { worker: key, item: *id }))
                    .collect();
                let tiles: Vec<TileRef> =
                    new.iter().map(|&item| TileRef { worker: key, item }).collect();
                self.place_from_elsewhere(&tiles);
                for item in new {
                    self.note_recent(item);
                }
                let kept = self.recency.iter().copied().filter(|id| self.tile_of(*id).is_some());
                self.recency = kept.collect();
                let gone: Vec<ItemId> = self
                    .files
                    .keys()
                    .chain(self.folders.keys())
                    .chain(self.screens.keys())
                    .copied()
                    .filter(|id| self.tile_of(*id).is_none())
                    .collect();
                for id in gone {
                    self.drop_item_views(id, cx);
                }
            }
            ItemChange::Added { id, by_me } => {
                let tile = TileRef { worker: key, item: id };
                if by_me {
                    let opening = self.opening_of(key, id);
                    self.open_as(tile, opening);
                } else {
                    self.place_from_elsewhere(&[tile]);
                }
                self.note_recent(id);
                // A worker's given shell opens beside the rest and leaves the focus where it
                // was: a worker coming up must not take the keys someone is typing elsewhere.
                match (by_me, self.given_pending.remove(&key)) {
                    (true, Some(Some(before))) => self.layout.focus(before),
                    (true, _) => self.after_focus_moved(cx),
                    (false, _) => {}
                }
            }
            // A thread tile becoming its terminal's keeps its place and its focus: its id comes
            // straight back as the terminal's item.
            ItemChange::Removed(id) if self.retiling(id) => self.drop_item_views(id, cx),
            ItemChange::Removed(id) => {
                self.layout.remove(TileRef { worker: key, item: id });
                self.recency.retain(|r| *r != id);
                self.drop_item_views(id, cx);
                self.after_focus_moved(cx);
            }
            ItemChange::Changed(_) | ItemChange::Echo => {}
        }
        if change != ItemChange::Echo {
            self.keep_items(key, cx);
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

    fn drop_item_views(&mut self, id: ItemId, cx: &mut Context<Self>) {
        if let Some(view) = self.files.remove(&id) {
            self.file_tile_gone(&view, cx);
        }
        self.folders.remove(&id);
        // The page goes with its tile, and the worker's store with its last page.
        self.browsers.remove(&id);
        self.browser_links.remove(&id);
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
        let session = summary.id;
        let before = self
            .workers
            .get_mut(&key)
            .and_then(|w| w.sessions.insert(session, summary))
            .map(|s| s.program)
            .unwrap_or_default();
        self.program_moved(session, &before, cx);
        self.reconcile(cx);
        // A thread tile waiting on this session as its agent's terminal becomes its tile.
        self.settle_thread_tiles(cx);
        cx.notify();
    }

    /// A session is gone.
    pub fn session_closed(&mut self, session: SessionId, cx: &mut Context<Self>) {
        for w in self.workers.values_mut() {
            w.sessions.remove(&session);
        }
        self.handoff.forget_session(session);
        // Its "finished" badge has no tile to clear it by looking: the bell must not keep it.
        self.finished.remove(&About::Session(session));
        self.program_seen.remove(&session);
        self.program_sounded.remove(&session);
        self.update_awake(cx);
        self.reconcile(cx);
        self.agents_moved(cx);
        cx.notify();
    }

    /// A session's program exited: the tile stays with its last screen and says so, until the
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
    pub fn term_event(&mut self, session: SessionId, event: TermEvent, cx: &mut Context<Self>) {
        if let TermEvent::Matches { needle, total, .. } = &event {
            self.tile_answered(session, needle, *total, cx);
        }
        // A refusal the person caused (typing into a program that stopped reading, a request
        // of a terminal that ended) is theirs to see, not only the log's.
        if let TermEvent::Error(error) = &event {
            tracing::warn!(%session, %error, "worker");
            self.show_failure(error.to_string(), cx);
        }
        if let Some(view) = self.terminals.get(&session) {
            view.update(cx, |v, cx| v.apply(event, cx));
        }
    }

    /// Views for every tile whose content is alive, none for the rest: terminals attached,
    /// streams open for the remote tiles that are on screen (or were, within the grace).
    pub(super) fn reconcile(&mut self, cx: &mut Context<Self>) {
        let workers = &self.workers;
        // A closed tile whose session ended elsewhere: a plain shell stays on the list, to come
        // back as a new shell; anything else ran what cannot come back.
        self.closed.retain_mut(|c| {
            if c.session.is_some_and(|s| !workers.values().any(|w| w.sessions.contains_key(&s))) {
                c.session = None;
                return c.shell.is_some();
            }
            true
        });
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
            .chain(self.closed.iter().filter_map(|c| c.session.map(|s| (c.tile.worker, s))))
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
            .flat_map(|(key, w)| {
                w.doc
                    .items()
                    .filter_map(|i| match i.kind {
                        ItemKind::Terminal { session } => Some(session),
                        _ => None,
                    })
                    .chain(
                        self.closed
                            .iter()
                            .filter(move |c| c.tile.worker == *key)
                            .filter_map(|c| c.session),
                    )
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
        let clip = self.clip_hook(key);
        let workspace = cx.entity().downgrade();
        let probe: AttachProbe = std::rc::Rc::new(move |cx: &App| {
            workspace.upgrade().is_some_and(|w| w.read(cx).block_target(session).is_some())
        });
        let view = cx.new(|cx| {
            let mut view = TerminalView::new(session, size, link.out.clone(), theme, cx);
            if let Some(clip) = clip {
                view.set_clip_hook(clip);
            }
            view.set_attach_probe(probe);
            view
        });
        let sid = session;
        cx.subscribe(&view, move |this, _view, event, cx| match event {
            TerminalViewEvent::Bell => cx.emit(WorkspaceEvent::Bell(sid)),
            // The Dock bounce; the banner while the app is away is the app's
            // (`attention::Attention::program`).
            TerminalViewEvent::Notification { .. } => cx.emit(WorkspaceEvent::Program(sid)),
            TerminalViewEvent::Exited(status) => this.session_exited(sid, *status, cx),
            TerminalViewEvent::CloseConfirmed => this.close_shell(sid, cx),
            TerminalViewEvent::Title(_) => this.terminal_changed(sid, cx),
            // The header draws the report, over its pane or in the bar.
            TerminalViewEvent::Progress => this.progress_changed(sid, cx),
            TerminalViewEvent::Cwd { path, repo, branch } => {
                this.session_moved(sid, path, repo.as_deref(), branch.as_deref());
                cx.notify();
            }
            TerminalViewEvent::Notice(text) => this.show_notice(text.clone(), cx),
            TerminalViewEvent::CommandFinished { command, exit, elapsed } => {
                let done = Finished {
                    command: command.clone(),
                    exit: *exit,
                    elapsed: *elapsed,
                    turn: None,
                };
                this.command_finished(sid, done, cx);
                // The row's last command and the tile's state changed with the new prompt.
                this.chrome.notify(cx);
            }
            TerminalViewEvent::Attach(text) => this.attach_block(sid, text.clone(), cx),
            TerminalViewEvent::ViewFile { path, line } => {
                let path = this.absolute_in_session(sid, path);
                if let Some(worker) = this.worker_of_session(sid) {
                    this.open_path_on(worker, &path, *line, cx);
                }
            }
            TerminalViewEvent::OpenPage { url } => {
                let worker = this.worker_of_session(sid);
                this.open_browser(worker, url, cx);
            }
            TerminalViewEvent::DragOut { path } => {
                let path = this.absolute_in_session(sid, path);
                if let Some(worker) = this.worker_of_session(sid) {
                    this.drag_out(worker, &path, cx);
                }
            }
            TerminalViewEvent::PasteFiles(files) => {
                this.paste_files_in_shell(sid, files.clone(), cx);
            }
        })
        .detach();
        // Every change of the terminal's: a frame is coming for it, which a working mark's step
        // held for a typed key's echo rides; and the navigator hears of a command started.
        cx.observe(&view, move |this, _view, cx| {
            crate::icons::release_steps(cx);
            this.terminal_changed(sid, cx);
        })
        .detach();
        w.send(ClientMsg::Term { session, req: TermRequest::Attach { size } });
        // What this client paints with, so the driver's colours answer colour queries.
        w.send(ClientMsg::Term { session, req: TermRequest::Colors(self.theme.terminal.wire()) });
        if let Some(rtt) = w.rtt {
            view.update(cx, |v, _| v.set_rtt(Some(rtt)));
        }
        self.terminals.insert(session, view);
        // As it starts: a shell attached mid-command is drawn running from its first frame.
        let _news = self.copy_shell(session, cx);
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
        let quality = quality_for();
        let key = self.desktop_key();
        let awaiting = self.awaiting_sized();
        let mut keep: Vec<ItemId> = Vec::new();
        for w in self.workers.values_mut() {
            if w.link.is_none() {
                // Away, its tiles keep their last pictures for the next link to replace.
                let screens = &self.screens;
                keep.extend(w.doc.items().map(|i| i.id).filter(|id| screens.contains_key(id)));
                continue;
            }
            // A display tile streamed from a display made for this device opens its own way;
            // one gone from the registry goes back to nothing.
            let sized = w.sized.as_ref().map(desktop::Sized::item);
            if let Some(id) = sized {
                let item = w.doc.items().find(|i| i.id == id);
                if item.is_none() || key.is_none() {
                    w.sized = None;
                } else if self.parked.contains(&id) {
                    if let Some(s) = w.sized.as_mut() {
                        s.lost();
                    }
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
                .filter(|i| {
                    !self.parked.contains(&i.id) && Some(i.id) != sized && !awaiting.contains(&i.id)
                })
                .filter_map(|i| match i.kind {
                    ItemKind::Window { window } => Some((i.id, CaptureTarget::Window(window))),
                    ItemKind::Display { display } => Some((i.id, CaptureTarget::Display(display))),
                    ItemKind::Terminal { .. }
                    | ItemKind::File { .. }
                    | ItemKind::Folder { .. }
                    | ItemKind::Browser { .. }
                    | ItemKind::Review { .. }
                    | ItemKind::Changes { .. }
                    | ItemKind::Thread { .. } => None,
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
                self.fill_titles(key, &windows);
                let Some(w) = self.workers.get_mut(&key) else { return };
                w.titles_requested = false;
                if std::mem::take(&mut w.display_wanted)
                    && let Some(d) = displays.first()
                {
                    self.add_screen_item(
                        key,
                        CaptureTarget::Display(d.id),
                        format!("Display {}", d.id.0),
                        cx,
                    );
                }
                let Some(w) = self.workers.get_mut(&key) else { return };
                if std::mem::take(&mut w.picker_wanted) {
                    self.show_picker(key, windows, displays, cx);
                }
            }
            ScreenEvent::Display { stream, key: display_key, display } => {
                self.display_told(key, stream, display_key, display, cx);
            }
            ScreenEvent::Opened { stream, target, codec, width, height, .. } => {
                let theme = self.theme.clone();
                let quality = quality_for();
                let (show_stats, pinned) = (self.show_stats, self.pinned_rtt);
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
                view.update(cx, |v, cx| {
                    v.set_rtt(rtt, shown, cx);
                    if show_stats {
                        v.set_hud(true, cx);
                    }
                });
                let tile = TileRef { worker: key, item: id };
                let behind_stale = self.screens.contains_key(&id)
                    && self.workers.get(&key).is_some_and(|w| w.stale_screens.contains(&id));
                cx.subscribe(&view, move |this, view, event, cx| match event {
                    crate::screen::ScreenViewEvent::Pressed => this.focus_tile(tile, cx),
                    crate::screen::ScreenViewEvent::Ready => {
                        this.fresh_stream_shows(key, id, &view, cx);
                        cx.notify();
                    }
                    crate::screen::ScreenViewEvent::Health => cx.notify(),
                    crate::screen::ScreenViewEvent::PasteFiles(files) => {
                        let view = view.downgrade();
                        this.paste_files_in_window(tile, &view, files.clone(), cx);
                    }
                    #[cfg(target_os = "macos")]
                    crate::screen::ScreenViewEvent::DragOut(shared) => {
                        if !this.drag_out_of(tile.worker, shared, cx) {
                            this.show_notice("The drag could not go on here".to_owned(), cx);
                        }
                    }
                    #[cfg(not(target_os = "macos"))]
                    crate::screen::ScreenViewEvent::DragOut(_) => {}
                    crate::screen::ScreenViewEvent::CancelPaste => this.cancel_paste_of(&view, cx),
                    crate::screen::ScreenViewEvent::DragOutFailed(why) => {
                        this.show_failure(format!("The drag did not come out: {why}"), cx);
                    }
                })
                .detach();
                // Its facts, copied whenever it changes: its first frame, its sound.
                cx.observe(&view, move |this, _view, cx| this.stream_changed(id, cx)).detach();
                if behind_stale {
                    if let Some(w) = self.workers.get_mut(&key) {
                        w.fresh_screens.insert(id, view);
                    }
                } else {
                    self.screens.insert(id, view);
                    self.stream_changed(id, cx);
                    self.restore_popout(id, cx);
                }
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
            ScreenEvent::Curtain(state) => self.curtain_heard(key, state, cx),
            ScreenEvent::ListFailed { why } => {
                let Some(w) = self.workers.get_mut(&key) else { return };
                w.titles_requested = false;
                w.display_wanted = false;
                if std::mem::take(&mut w.picker_wanted) {
                    let text = crate::screen::failure_text(&why, &w.name);
                    if why == ScreenFailure::NotPermitted {
                        self.show_grant(key, text, cx);
                    } else {
                        self.show_notice(format!("No windows to list: {text}"), cx);
                    }
                }
            }
            ScreenEvent::Drag { stream, event } => {
                for (id, view) in self.stream_views(key, stream, cx) {
                    if let Some((drag, outcome)) = view.update(cx, |v, cx| v.drag_heard(&event, cx))
                    {
                        self.drag_ended(TileRef { worker: key, item: id }, drag, outcome, cx);
                    }
                }
            }
        }
        cx.notify();
    }

    /// "Reopen" on a tile shown as stopped: its stream is asked for again.
    pub(super) fn reopen_screen(&mut self, tile: TileRef, cx: &mut Context<Self>) {
        if let Some(w) = self.workers.get_mut(&tile.worker) {
            w.stopped.remove(&tile.item);
        }
        self.reconcile_screens();
        cx.notify();
    }

    /// An open the worker refused: the tile waiting on it stops asking until the next link,
    /// and says why in its pane. A display made for this device that could not be made goes
    /// back to the physical one, as a closed one does, and a notice says why.
    fn screen_refused(
        &mut self,
        key: WorkerKey,
        asked: OpenAsk,
        why: ScreenFailure,
        cx: &mut Context<Self>,
    ) {
        let Some(w) = self.workers.get_mut(&key) else { return };
        let id = match asked {
            OpenAsk::Target(target) => w.pending_opens.remove(&target),
            OpenAsk::Made(_) => {
                // Not made: not asked again unasked on this link.
                self.desktop.declined(key);
                w.sized.take().map(|s| s.item())
            }
        };
        let Some(id) = id else {
            tracing::debug!(?asked, ?why, "a refused open nobody waits on");
            return;
        };
        tracing::info!(%id, ?asked, ?why, "screen open refused");
        self.items_dirty = true;
        // A tile that asked for a target says why in its own pane, where it waited.
        if matches!(asked, OpenAsk::Target(_)) {
            w.failed_opens.insert(id, why);
            return;
        }
        let text = crate::screen::failure_text(&why, &w.name);
        if why == ScreenFailure::NotPermitted {
            self.show_grant(key, text, cx);
            return;
        }
        let item = w.doc.get(id).cloned();
        let title = item
            .map_or_else(String::new, |i| i.name.clone().unwrap_or_else(|| self.derived_title(&i)));
        let tile = TileRef { worker: key, item: id };
        self.show_failure_at(tile, format!("{title} did not open. {text}"), cx);
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
    /// it takes the tile's place, and the stale picture goes.
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
        self.stream_changed(id, cx);
        cx.notify();
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
                self.titles_dirty = true;
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

    /// Views for file items; the ones whose items are gone go. Needs the window (a file's
    /// editor does), so it runs from `render`, and only on a frame after a registry or a link
    /// changed ([`WorkspaceView::items_dirty`]).
    pub(super) fn reconcile_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let files: Vec<(WorkerKey, ItemId, &str)> = self
            .items()
            .filter_map(|(key, item)| match &item.kind {
                ItemKind::File { path } => Some((key, item.id, path.as_str())),
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
        // Each worker watches the set behind its file tiles and re-reads one that changes on disk.
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
        for (key, id, path) in new_files {
            self.make_file(key, id, path, window, cx);
        }
        let gone: Vec<ItemId> =
            self.files.keys().filter(|id| !file_ids.contains(id)).copied().collect();
        for id in gone {
            if let Some(view) = self.files.remove(&id) {
                self.file_tile_gone(&view, cx);
            }
        }
        self.reconcile_folders(cx);
    }

    /// The view of file item `id` at `path` on `worker`, which it asks for the text.
    fn make_file(
        &mut self,
        worker: WorkerKey,
        id: ItemId,
        path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let theme = self.theme.clone();
        let view = cx.new(|cx| FileView::new(id, worker, &path, theme, window, cx));
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
                            this.workers.get(&worker).map_or("The machine", |w| w.name.as_str());
                        let error = format!("{name} is out of reach");
                        view.update(cx, |v, cx| v.written(WriteResult::Failed { error }, cx));
                    }
                }
                FileViewEvent::Reload => this.request_file(id),
                FileViewEvent::Run(line) => {
                    let dir = file.rsplit_once('/').map(|(dir, _)| dir.to_owned());
                    let command = crate::file::terminal_command(line);
                    this.open_session_on(worker, dir.filter(|d| !d.is_empty()), command, None, cx);
                }
                FileViewEvent::RunBlock(code) => this.run_in_shell(code.clone(), cx),
                FileViewEvent::Edited { id, outcome } => this.file_edited(worker, *id, *outcome),
                FileViewEvent::OpenThread(opens) => this.open_thread_at(*opens, cx),
                FileViewEvent::Stamped => this.author_file(&view, cx),
            }
            cx.notify();
        })
        .detach();
        // Most of the tile's redraws are its caret blinking: its backup is looked at only when
        // where its edit stands has moved.
        let mut marked = None;
        cx.observe(&view, move |this, view, cx| {
            this.file_changed(id, cx);
            let mark = view.read(cx).backup_mark();
            if mark != marked {
                marked = mark;
                this.keep_unsaved(cx);
            }
        })
        .detach();
        if let Some(line) = self.file_focus.remove(&id) {
            view.update(cx, |v, cx| v.focus_line(Some(line), cx));
        }
        if let Some(kept) = self.take_kept(worker, id) {
            view.update(cx, |v, cx| v.restore(kept, cx));
        }
        if let Some(waits) = self.file_wait(worker, &path) {
            view.update(cx, |v, cx| v.set_waiting(Some(waits), cx));
        }
        let can = self.run_target().is_some();
        view.update(cx, |v, cx| v.set_can_run(can, cx));
        self.files.insert(id, view);
        if let Some(w) = self.workers.get(&worker) {
            w.send(ClientMsg::ReadFile { path });
        }
    }

    /// Whether [`Self::note_visible`] has anything to do for `visible`: a remote tile that
    /// came on screen or went off it, a stream due to be let go, or one waiting with no timer
    /// out for it.
    pub(super) fn visibility_due(&self, visible: &[ItemId]) -> bool {
        let now = std::time::Instant::now();
        let remote = self
            .items()
            .filter(|(_, i)| matches!(i.kind, ItemKind::Window { .. } | ItemKind::Display { .. }));
        let mut waiting = false;
        for (_, item) in remote {
            let id = item.id;
            let seen = visible.contains(&id) || self.popouts.holds(id);
            match (seen, self.unseen.get(&id)) {
                (true, Some(_)) | (false, None) => return true,
                (true, None) => {}
                (false, Some(since)) => {
                    let parked = self.parked.contains(&id);
                    if !parked && now.saturating_duration_since(*since) >= self.stream_grace {
                        return true;
                    }
                    waiting |= !parked;
                }
            }
        }
        waiting && !self.park_pending
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
            // A tile shown in a window of its own is on screen there.
            if visible.contains(&id) || self.popouts.holds(id) {
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

impl WorkspaceView {
    /// What to say of `key`'s link having stayed on a DERP relay, once it has held.
    #[must_use]
    pub(super) fn relay_notice(
        &self,
        key: WorkerKey,
        cx: &App,
    ) -> Option<slopty_client::relay::RelayNotice> {
        self.workers.get(&key)?.relay.notice(cx.background_executor().now())
    }
}

/// The round trip a readout shows for a link measured at `live`: `pinned` once there is one.
fn shown_rtt(pinned: Option<Duration>, live: Option<Duration>) -> Option<Duration> {
    live.map(|live| pinned.unwrap_or(live))
}

/// Whether a composer took a quote.
type Done = Box<dyn FnOnce(bool, &mut App)>;

/// A quote on its way to a composer that may not be made yet.
struct Quote {
    /// Which, to find it again when its time is up.
    id: u64,
    /// The thread it is for, in a composer of it wherever that shows.
    to: ThreadId,
    text: String,
    done: Done,
}

/// The quotes waiting for their composer.
#[derive(Default)]
pub(super) struct Quotes {
    waiting: Vec<Quote>,
    next: u64,
}

impl WorkspaceView {
    /// Have `key`'s worker exit for its service manager to start it again
    /// ([`slopty_proto::orchestration::Verb::RestartWorker`]), on the person's press: a grant
    /// made at its desk is seen once it is back. Its shells stay with ptyd.
    pub(super) fn restart_worker(&mut self, key: WorkerKey, cx: &mut Context<Self>) {
        let name = self.worker_name(key);
        let Some(worker) = super::projects::worker_id(key) else {
            self.show_failure(format!("Couldn\u{2019}t restart Slopty on {name}"), cx);
            return;
        };
        let verb = slopty_proto::orchestration::Verb::RestartWorker { worker };
        let said = format!("Restarting Slopty on {name}\u{2026}");
        self.send_to_server(verb, move |this, cx| this.show_notice(said, cx), cx);
    }
}

/// Requested quality for a new stream: full scale at the main screen's refresh. The view then
/// asks for a scale matching the width it paints at, and for the refresh of the screen it is
/// drawn on.
fn quality_for() -> Quality {
    crate::screen::quality_of(1.0, crate::screen::main_refresh_hz())
}

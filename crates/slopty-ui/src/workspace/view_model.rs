//! The shell fed from the workspace ([`crate::view_model`]): its lists published as the workers and
//! their thread tables move, the transcripts it watches followed, and its calls answered
//! ([`crate::view_model`]).
//!
//! A chat is its thread, by the thread's id. Nothing here speaks to an agent: the person types
//! to it in its own TUI, in its terminal (`docs/decisions/agents.md`, "TUI-first"). What the
//! shell asks of a thread is the person's own marks on it (seen, named, archived here) and the
//! sessions it starts or takes up again ([`past`]). Nothing deletes one: a session's record is
//! its agent's own.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use gpui::{Context, Subscription, Task};
use slopty_client::WorkerKey;
use slopty_proto::ClientMsg;
use slopty_proto::thread::wire::{Intent, IntentDone, ThreadRow};
use slopty_proto::thread::{IntentId, Liveness, Phase, ThreadId};
use slopty_view_rpc::{RpcError, RpcReply, methods};
use tokio::sync::mpsc;

use super::WorkspaceView;
use crate::view_model::{Ask, Feed, Transcript, rows, transcript};

mod blobs;
#[cfg(test)]
mod link_tests;
#[cfg(target_vendor = "apple")]
mod pages;
// Claude Code's past sessions, listed, searched and taken up again.
mod past;
mod surfaces;
mod work;

pub use surfaces::{
    AddAnswer, AddChoice, AddDevice, AddFailure, AddFix, AddKind, AddMark, AddProgress, AddStep,
    Found, FoundNode, Look,
};

/// When this client heard a thread's lease `lease` move, by its own monotonic clock: as `kept`
/// (the lease last heard and when) says while it has not moved, else `at`, now. The worker's
/// clock never counts: a lease is only compared with the one before it.
fn lease_heard(
    kept: Option<(slopty_core::WallMs, Instant)>,
    lease: slopty_core::WallMs,
    at: Instant,
) -> Instant {
    match kept {
        Some((had, got)) if had == lease => got,
        _ => at,
    }
}

/// How long changes gather before the lists go out: a frame.
const GATHER: Duration = Duration::from_millis(16);

/// How long the chats and projects wait for every machine's threads before they go out with
/// those that came: a machine out of reach must not hold the sidebar back for good.
const TABLES_WITHIN: Duration = Duration::from_secs(5);

/// How many older turns one page asks for: a followed thread's whole history comes in a few.
const PAGE_TURNS: u32 = 200;

/// A project folder as the threads and the person name it.
#[derive(Debug, Default)]
struct Folder {
    /// It is in a git repository.
    git: bool,
    /// The repository, as the shell groups clones and worktrees by it.
    repository: Option<String>,
    /// When its first chat began or the person added it, in ms since the epoch.
    created: Option<i64>,
}

impl Folder {
    /// Something in it began at `ms`.
    fn began(&mut self, ms: i64) {
        if ms > 0 {
            self.created = Some(self.created.map_or(ms, |was| was.min(ms)));
        }
    }
}

/// What the workspace keeps for the shell.
#[derive(Default)]
pub(super) struct Bridge {
    feed: Option<Arc<Feed>>,
    /// Answers the interface's asks.
    pump: Option<Task<()>>,
    /// The lists going out at the end of this frame.
    gathering: Option<Task<()>>,
    /// Each thread's lease as last heard, and when this client heard it move, by its own
    /// monotonic clock: a turn under way stands as of then ([`rows::session`]), whatever the
    /// worker's clock says.
    leases: HashMap<ThreadId, (slopty_core::WallMs, Instant)>,
    /// The workspace and each worker's thread table, heard.
    hearing: Vec<Subscription>,
    heard_hubs: BTreeSet<WorkerKey>,
    /// Chats followed now, by the thread each shows.
    followed: HashMap<String, (WorkerKey, ThreadId)>,
    /// What the view model sent a worker itself (a session's start) and the worker has not
    /// answered, by its intent: sent again, under the same id, when the link comes back.
    unanswered: HashMap<IntentId, (WorkerKey, ClientMsg)>,
    /// When the app began to serve the interface: the lists wait [`TABLES_WITHIN`] at most for
    /// every machine's threads.
    serving_since: Option<Instant>,
    /// What the right-pane surfaces wait on from the workers ([`surfaces`]).
    surfaces: surfaces::Surfaces,
    /// The pages agents show inline, hosted per window ([`pages`]).
    #[cfg(target_vendor = "apple")]
    pages: pages::Pages,
    /// What Claude Code's past sessions wait on from the machines ([`past`]).
    past: past::Past,
}

impl std::fmt::Debug for Bridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bridge").field("followed", &self.followed.len()).finish_non_exhaustive()
    }
}

/// When intent `id` was made, in ms since the epoch: its id is time-ordered (`UUIDv7`).
fn refused_at(id: IntentId) -> Option<u64> {
    let (secs, nanos) = id.as_uuid().get_timestamp()?.to_unix();
    Some(secs.saturating_mul(1000).saturating_add(u64::from(nanos / 1_000_000)))
}

/// How many bytes of words an excerpt of a search keeps on each side of its match.
const EXCERPT_AROUND: usize = 60;

/// The first place `query` (lowercase) is in `text`, case aside where lowercasing keeps its
/// bytes where they were, else as written: an excerpt of one line around it, and where the
/// match starts and ends in the excerpt, in bytes.
fn excerpt(text: &str, query: &str) -> Option<(String, usize, usize)> {
    let lower = text.to_lowercase();
    let at = if lower.len() == text.len() { lower.find(query) } else { text.find(query) }?;
    let end = at.saturating_add(query.len());
    let mut from = at.saturating_sub(EXCERPT_AROUND);
    while !text.is_char_boundary(from) {
        from = from.saturating_sub(1);
    }
    let mut to = end.saturating_add(EXCERPT_AROUND).min(text.len());
    while !text.is_char_boundary(to) {
        to = to.saturating_add(1);
    }
    let excerpt = text.get(from..to)?.replace(['\n', '\r', '\t'], " ");
    Some((excerpt, at.saturating_sub(from), end.saturating_sub(from)))
}

/// A string parameter.
fn param<'a>(params: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    params.get(key).and_then(serde_json::Value::as_str).filter(|s| !s.is_empty())
}

impl WorkspaceView {
    /// Serve the shell: publish into `feed` from now on and answer what it asks.
    pub fn serve_interface(
        &mut self,
        feed: Arc<Feed>,
        mut asks: mpsc::UnboundedReceiver<Ask>,
        cx: &mut Context<Self>,
    ) {
        self.view_model.feed = Some(feed);
        self.view_model.serving_since.get_or_insert_with(Instant::now);
        self.view_model.pump = Some(cx.spawn(async move |this, cx| {
            while let Some(ask) = asks.recv().await {
                if this.update(cx, |view, cx| view.view_ask(ask, cx)).is_err() {
                    return;
                }
            }
        }));
        let this = cx.entity();
        self.view_model.hearing.push(cx.observe(&this, |view, _, cx| view.view_changed(cx)));
        // The lists go out once every machine's threads came, or once waiting has gone on long
        // enough.
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TABLES_WITHIN).await;
            let _gone = this.update(cx, |view, cx| view.view_changed(cx));
        })
        .detach();
        self.view_changed(cx);
    }

    /// Something the interface shows may have moved: publish at the end of the frame.
    fn view_changed(&mut self, cx: &Context<Self>) {
        if self.view_model.feed.is_none() || self.view_model.gathering.is_some() {
            return;
        }
        self.view_model.gathering = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(GATHER).await;
            let _gone = this.update(cx, |view, cx| {
                view.view_model.gathering = None;
                view.view_publish(cx);
            });
        }));
    }

    /// Hear each worker's thread table once it is made.
    fn view_hear_hubs(&mut self, cx: &mut Context<Self>) {
        let hubs: Vec<_> = self
            .hubs()
            .filter(|(key, _)| !self.view_model.heard_hubs.contains(key))
            .map(|(key, hub)| (key, hub.clone()))
            .collect();
        for (key, hub) in hubs {
            self.view_model.heard_hubs.insert(key);
            self.view_model.hearing.push(cx.observe(&hub, |view, _, cx| view.view_changed(cx)));
        }
    }

    /// The thread a chat of the interface shows, and its worker.
    fn chat_thread(&self, chat: &str, cx: &gpui::App) -> Option<(WorkerKey, ThreadId)> {
        let thread: ThreadId = chat.parse().ok()?;
        let worker = self.worker_of_thread(thread, cx)?;
        Some((worker, thread))
    }

    /// Whether every machine's threads have come, or the lists waited [`TABLES_WITHIN`] for
    /// them: until then the chats and projects are not said.
    fn view_tables_known(&self, cx: &gpui::App) -> bool {
        let waited =
            self.view_model.serving_since.is_some_and(|since| since.elapsed() >= TABLES_WITHIN);
        waited
            || self.workers.keys().all(|key| {
                self.held_hub(*key).is_some_and(|hub| hub.read(cx).threads().heard_table())
            })
    }

    /// The home of the machine `device` names, when its link said it.
    fn view_home(&self, device: &str) -> Option<&str> {
        self.workers.get(&self.view_worker(device)?)?.home.as_deref()
    }

    /// The project folder a chat working in `cwd` (in the repository at `repo`) on `device`
    /// belongs to; `None` for one with no project ([`rows::place`]).
    fn view_place(&self, device: &str, cwd: Option<&str>, repo: Option<&str>) -> Option<String> {
        let added: Vec<(&str, &str)> = self
            .added_projects()
            .iter()
            .filter(|project| project.device == device)
            .map(|project| {
                (project.path.as_str(), project.real.as_deref().unwrap_or(&project.path))
            })
            .collect();
        rows::place(cwd, repo, self.view_home(device), &added)
    }

    /// Publish everything the interface watches.
    fn view_publish(&mut self, cx: &mut Context<Self>) {
        self.view_hear_hubs(cx);
        self.live_adopt_terminals(cx); // an agent's terminal is a session of its own
        self.probe_projects(cx);
        self.view_fetch_followed(cx);
        let Some(feed) = self.view_model.feed.clone() else { return };
        let now = Utc::now();
        let devices =
            self.workers.keys().filter_map(|key| self.surface_device_row(*key, now)).collect();
        let mut chats = Vec::new();
        let mut sessions = Vec::new();
        let mut folders: BTreeMap<(String, String), Folder> = BTreeMap::new();
        let mut leases = std::mem::take(&mut self.view_model.leases);
        let mut heard_leases = HashMap::new();
        // Every worker's rows at once: an agent may start its subagents on other machines.
        let all: Vec<(WorkerKey, String, &ThreadRow)> = self
            .hubs()
            .flat_map(|(key, hub)| {
                let device = key.to_string();
                hub.read(cx)
                    .threads()
                    .rows()
                    .rows
                    .values()
                    .map(move |row| (key, device.clone(), row))
            })
            .collect();
        let exited: BTreeSet<ThreadId> = all
            .iter()
            .filter(|(_, _, row)| matches!(row.status.liveness, Liveness::Exited { .. }))
            .map(|(_, _, row)| row.id)
            .collect();
        for (key, device, row) in &all {
            // A subagent of an agent that exited stopped with it.
            let standing = rows::standing(row, &exited);
            let row = &*standing;
            let id = row.id.to_string();
            let mut chat = rows::chat(row, device);
            // The person's look here stands at once: a seen mark on its way to the worker reads
            // as seen until the row says it back, so no publish in between shows it unread.
            if let Some(ended) = row.ended.filter(|e| e.turn <= self.seen_through(row.id, row.seen))
            {
                chat.last_seen_at = Some(rows::time(ended.at_ms));
            }
            let place = self.view_place(device, row.cwd.as_deref(), row.repo.as_deref());
            chat.space_id = place.as_deref().map(|path| self.view_space_id(device, path));
            chat.config = Some(Self::chat_config(row));
            chat.archived = self.surface_archived(&id);
            chat.id.clone_from(&id);
            if let Some(parent) = rows::parent(row) {
                chat.parent_chat_id = Some(parent.to_string());
            }
            // A folder is a project while a session of its own, not archived, is there (or the
            // person added it): archiving its last session, or removing it, takes it away.
            if let Some(path) = place.filter(|_| !chat.archived && rows::parent(row).is_none()) {
                let folder = folders.entry((device.clone(), path)).or_default();
                folder.git |= row.repo.is_some();
                if folder.repository.is_none() {
                    folder.repository = rows::repository_id(row);
                }
                folder.began(chat.created_at.timestamp_millis());
            }
            let running =
                rows::running_children(row.id, all.iter().map(|(_, _, row)| *row), &exited);
            // A machine out of reach says nothing new of its threads: their status stands as of
            // their last change, so the shell lets a "working" go stale rather than spin on.
            let linked = self.held_hub(*key).is_some_and(|hub| hub.read(cx).threads().linked());
            let heard = if linked { now } else { rows::time(row.updated_ms) };
            // A lease stands as of when it was heard to move here, which this publish follows
            // within a frame.
            let leased = row.lease_ms.map(|lease| {
                let at = Instant::now();
                let got = lease_heard(leases.remove(&row.id), lease, at);
                heard_leases.insert(row.id, (lease, got));
                let ago = chrono::TimeDelta::from_std(at.saturating_duration_since(got));
                now.checked_sub_signed(ago.unwrap_or_default()).unwrap_or(now)
            });
            let mut session = rows::session(row, device, running, heard, leased);
            session.chat_id = id;
            chats.push(chat);
            sessions.push(session);
        }
        // The plain terminals opened as sessions of their own stand beside the agents' sessions.
        for (chat, session) in self.live_terminal_rows(now, cx) {
            if let Some(space) = &chat.space_id
                && let Some((_, path)) = self.view_space(space)
            {
                let folder = folders.entry((chat.device_id.clone(), path)).or_default();
                folder.began(chat.created_at.timestamp_millis());
            }
            chats.push(chat);
            sessions.push(session);
        }
        for project in self.added_projects() {
            let path = project.path.trim_end_matches('/').to_owned();
            let folder = folders.entry((project.device.clone(), path)).or_default();
            folder.git |= project.git;
            if let Some(created) = project.created_ms {
                folder.began(created);
            }
        }
        let spaces = folders
            .into_iter()
            .map(|((device, path), folder)| {
                let created = folder
                    .created
                    .and_then(DateTime::from_timestamp_millis)
                    .unwrap_or(DateTime::UNIX_EPOCH);
                let mut space = rows::space(&device, &path, folder.git, created);
                space.repository_id = folder.repository;
                if let Some(project) = self.added_project_at(&device, &path) {
                    space.id.clone_from(&project.id);
                    space.name.clone_from(&project.name);
                }
                space
            })
            .collect();
        feed.devices(devices);
        // The chats and projects wait for every machine's threads: a list without them would
        // read as theirs gone.
        if self.view_tables_known(cx) {
            feed.spaces(spaces);
            feed.chats(chats);
        }
        feed.sessions(sessions);
        feed.terminals(self.live_session_terminals(cx));
        self.view_model.leases = heard_leases;
        let workers_up = (!self.workers.is_empty())
            .then(|| self.workers.values().any(|worker| worker.status.is_up()));
        feed.links(workers_up);
        // A thread whose state has not come says nothing yet: an empty transcript would stand as
        // its whole history, and the real one, once it comes, as live arrivals. A chat with no
        // thread had its empty transcript when followed.
        for chat in feed.followed() {
            // Its tray, dock and rules beside it.
            if let Some(views) = self.view_chat_views(&chat, cx) {
                feed.chat_views(&chat, views);
            }
            if let Some(said) = self.view_transcript(&chat, cx) {
                feed.transcript(&chat, said);
            } else if self.view_unreachable(&chat, cx) {
                // Its machine is out of reach and nothing of the thread is kept here: the chat
                // shows it has nothing rather than waiting for good.
                feed.transcript(&chat, Transcript::of(Vec::new()));
            }
        }
    }

    /// Whether the chat `chat` follows a thread whose machine is out of reach, with nothing of
    /// the thread kept here.
    fn view_unreachable(&self, chat: &str, cx: &Context<Self>) -> bool {
        let Some((key, _)) = self.view_model.followed.get(chat) else { return false };
        self.held_hub(*key).is_none_or(|hub| !hub.read(cx).threads().linked())
    }

    /// The transcript of the chat `chat` as its thread stands here, and whether older turns
    /// are still on their way.
    fn view_transcript(&self, chat: &str, cx: &gpui::App) -> Option<Transcript> {
        let (key, thread) = self.view_model.followed.get(chat).copied()?;
        let hub = self.held_hub(key)?.read(cx);
        let state = hub.threads().mirror(thread)?.state()?;
        let table = &hub.threads().rows().rows;
        let device = key.to_string();
        // A subagent may run on any machine.
        let running = |child| {
            self.hubs()
                .find_map(|(_, hub)| hub.read(cx).threads().rows().rows.get(&child))
                .map(|row| transcript::ChildRun::of(row, child.to_string()))
        };
        let host = self.workers.get(&key).map(|worker| worker.name.clone()).unwrap_or_default();
        let look =
            transcript::Look::new(&device).host(&host).children(running).whole(|content| match hub
                .whole(content)?
                .as_ref()
            {
                slopty_proto::thread::wire::Expanded::Text(text) => Some(text.clone()),
                _ => None,
            });
        let mut entries = transcript::entries(state, &look);
        // The table is the worker's word on whether the agent works now; a mirror drawn from the
        // cache may still hold a turn that ended since.
        let live = table
            .get(&thread)
            .is_some_and(|row| rows::running(row) || row.status.phase == Phase::NeedsYou);
        transcript::settle(&mut entries, live);
        // What the worker last turned down, until the thread moves past it.
        let last = state.items.last().map_or(0, |item| item.at_ms.as_millis());
        // The mark the app keeps by itself (seen) is no act of the person's.
        let acted = |r: &&crate::conversation::thread::hub::Refusal| {
            !matches!(r.intent, Intent::Seen { .. })
        };
        if let Some(refusal) = hub.refusals(thread).rfind(acted)
            && let Some(at) = refused_at(refusal.id).filter(|at| *at > last)
        {
            let at = i64::try_from(at).unwrap_or(i64::MAX);
            transcript::refused(&refusal.words, at, &device, &mut entries);
        }
        let history_pending = state.older && hub.linked();
        Some(Transcript { entries, history_pending })
    }

    /// Ask for what the followed threads' transcripts still lack: their older turns, and the
    /// wholes of the texts their workers cut.
    fn view_fetch_followed(&self, cx: &mut Context<Self>) {
        let followed: Vec<(WorkerKey, ThreadId)> =
            self.view_model.followed.values().copied().collect();
        for (key, thread) in followed {
            let Some(hub) = self.held_hub(key).cloned() else { continue };
            let cut: Vec<slopty_proto::thread::ContentRef> = {
                let held = hub.read(cx);
                let Some(state) = held.threads().mirror(thread).and_then(|m| m.state()) else {
                    continue;
                };
                transcript::cut(state).filter(|c| held.whole(c).is_none()).cloned().collect()
            };
            hub.update(cx, |hub, cx| {
                hub.page(thread, PAGE_TURNS, cx);
                for content in &cut {
                    hub.expand(thread, content, cx);
                }
            });
        }
    }

    /// Answer one of the interface's asks.
    fn view_ask(&mut self, ask: Ask, cx: &mut Context<Self>) {
        match ask {
            Ask::Follow(chat) => self.view_follow(chat, cx),
            Ask::Republish => self.view_changed(cx),
            Ask::Unfollow(chat) => {
                if let Some((key, thread)) = self.view_model.followed.remove(&chat)
                    && let Some(hub) = self.held_hub(key).cloned()
                {
                    hub.update(cx, |hub, cx| hub.close(thread, cx));
                }
            }
            Ask::Call { method, params, reply } => {
                let answer = if method == methods::FETCH_TOOL_BLOB {
                    Some(self.view_tool_blob(&params, cx))
                } else if let Some(answer) = self.view_page_call(&method, &params, cx) {
                    Some(answer)
                } else {
                    self.view_surface_call(&method, &params, cx)
                };
                if let Some(answer) = answer {
                    cx.spawn(async move |_, _| {
                        let _gone = reply.send(answer.await);
                    })
                    .detach();
                    return;
                }
                let answer = self.view_call(&method, &params, cx).map(RpcReply::Value);
                let _gone = reply.send(answer);
            }
        }
    }

    /// A call on an inline page ([`pages`]).
    #[cfg(target_vendor = "apple")]
    fn view_page_call(
        &mut self,
        method: &str,
        params: &serde_json::Value,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<RpcReply, RpcError>>> {
        self.page_call(method, params, cx)
    }

    /// No inline pages here: no call on one is answered.
    #[cfg(not(target_vendor = "apple"))]
    #[expect(clippy::unused_self, reason = "pages are hosted on Apple platforms only")]
    fn view_page_call(
        &mut self,
        _method: &str,
        _params: &serde_json::Value,
        _cx: &mut Context<Self>,
    ) -> Option<Task<Result<RpcReply, RpcError>>> {
        None
    }

    /// Follow the thread `chat` shows; a chat with no thread here shows it has nothing.
    fn view_follow(&mut self, chat: String, cx: &mut Context<Self>) {
        let Some((key, thread)) = self.chat_thread(&chat, cx) else {
            if let Some(feed) = &self.view_model.feed {
                feed.transcript(&chat, Transcript::of(Vec::new()));
            }
            return;
        };
        self.view_model.followed.insert(chat, (key, thread));
        let hub = self.thread_hub(key, cx);
        hub.update(cx, |hub, cx| hub.open(thread, cx));
        self.view_changed(cx);
    }

    /// One of the interface's calls.
    fn view_call(
        &mut self,
        method: &str,
        params: &serde_json::Value,
        cx: &mut Context<Self>,
    ) -> Result<serde_json::Value, RpcError> {
        match method {
            methods::MUTATE => self.view_mutate(params, cx),
            methods::SEARCH_CHATS => Ok(self.view_search_chats(params, cx)),
            _ => Err(RpcError::UnknownMethod(method.to_owned())),
        }
    }

    /// `Mutate`: the chat operations Slopty's threads have.
    fn view_mutate(
        &mut self,
        params: &serde_json::Value,
        cx: &mut Context<Self>,
    ) -> Result<serde_json::Value, RpcError> {
        let op = param(params, "op").unwrap_or_default();
        let chat = param(params, "chatId");
        match (op, chat) {
            // The person looked at the chat: its thread is seen through its latest turn, on
            // its worker, which tells every other device.
            // The latest turn is the row's word, whoever speaks for the thread's state: a
            // Claude Code chat's terminal does while its agent lives.
            ("markChatSeen", Some(chat)) => {
                let ended = self.chat_thread(chat, cx).and_then(|(key, thread)| {
                    let row = self.held_hub(key)?.read(cx).threads().rows().rows.get(&thread)?;
                    let ended = row.ended.filter(|ended| ended.turn > row.seen)?;
                    Some((key, thread, ended.turn))
                });
                if let Some((key, thread, turn)) = ended {
                    self.mark_seen(key, thread, turn, cx);
                }
                Ok(serde_json::json!({}))
            }
            // The person's own name for the chat, kept on its worker for every device.
            ("renameChat", Some(chat)) => {
                let title = param(params, "title").unwrap_or_default().to_owned();
                let (key, thread) = self
                    .chat_thread(chat, cx)
                    .ok_or_else(|| RpcError::Failed("Name the chat once it has started".into()))?;
                let hub = self.thread_hub(key, cx);
                hub.update(cx, |hub, cx| hub.intent(thread, Intent::Name { title }, cx));
                Ok(serde_json::json!({}))
            }
            // A project removed moves its sessions to Archived, as the shell says it does: a
            // session is its agent's own record, which Slopty never deletes.
            ("deleteSpace", _) => {
                if let Some(space) = param(params, "spaceId") {
                    self.view_archive_space(space, cx);
                }
                self.project_mutate(op, params, cx).unwrap_or_else(|| Ok(serde_json::json!({})))
            }
            _ => self
                .surface_mutate(op, params, cx)
                .or_else(|| self.project_mutate(op, params, cx))
                .unwrap_or_else(|| {
                    Err(RpcError::Failed(format!("{op} is not something Slopty does to a thread")))
                }),
        }
    }

    /// Archive every session of the space `space`: those its sidebar lists, not their
    /// subagents, which go with them.
    fn view_archive_space(&mut self, space: &str, cx: &Context<Self>) {
        let mut archived = Vec::new();
        for (key, hub) in self.hubs() {
            let device = key.to_string();
            for row in hub.read(cx).threads().rows().rows.values() {
                let place = self.view_place(&device, row.cwd.as_deref(), row.repo.as_deref());
                if rows::parent(row).is_none()
                    && place.is_some_and(|path| self.view_space_id(&device, &path) == space)
                {
                    archived.push(row.id.to_string());
                }
            }
        }
        self.surface_archive(archived, cx);
    }

    /// The worker a device id of the interface names.
    fn view_worker(&self, device: &str) -> Option<WorkerKey> {
        self.workers.keys().find(|key| key.to_string() == device).copied()
    }

    /// The id of the space of the folder `path` on `device`: the one the person added it by,
    /// else the folder's own ([`rows::space_id`]).
    fn view_space_id(&self, device: &str, path: &str) -> String {
        self.added_project_at(device, path)
            .map_or_else(|| rows::space_id(device, path), |project| project.id.clone())
    }

    /// The worker and folder a space id of the interface names ([`rows::space_id`]).
    fn view_space(&self, space: &str) -> Option<(WorkerKey, String)> {
        if let Some(project) = self.added_project(space) {
            return Some((self.view_worker(&project.device)?, project.path.clone()));
        }
        let (device, path) = space.split_once(':')?;
        Some((self.view_worker(device)?, path.to_owned()))
    }

    /// Send `msg` for intent `id` to `key`, kept until the worker answers it.
    pub(super) fn view_send_kept(&mut self, key: WorkerKey, id: IntentId, msg: ClientMsg) {
        self.send(key, msg.clone());
        self.view_model.unanswered.insert(id, (key, msg));
    }

    /// Send again what the view model sent `key` and `key` has not answered: a session's
    /// start, taken up where it stands.
    pub(in crate::workspace) fn view_resend(&self, key: WorkerKey) {
        for (at, msg) in self.view_model.unanswered.values() {
            if *at == key {
                self.send(key, msg.clone());
            }
        }
    }

    /// `SearchChats`: the chats whose words hold `query` (case aside), as far as this client
    /// holds them: the person's messages and the agents' answers of the threads it has, and
    /// every thread's title and last line. At most `limit`, each with the first match in an
    /// excerpt (byte offsets into it).
    fn view_search_chats(&self, params: &serde_json::Value, cx: &gpui::App) -> serde_json::Value {
        let query = param(params, "query").unwrap_or_default().trim().to_lowercase();
        let limit = params
            .get("limit")
            .and_then(serde_json::Value::as_u64)
            .and_then(|limit| usize::try_from(limit).ok())
            .unwrap_or(50);
        if query.chars().count() < 2 {
            return serde_json::json!([]);
        }
        let mut found = Vec::new();
        for (_, hub) in self.hubs() {
            let hub = hub.read(cx);
            for row in hub.threads().rows().rows.values() {
                if found.len() >= limit {
                    break;
                }
                let state =
                    hub.threads().mirror(row.id).and_then(slopty_client::threads::Mirror::state);
                let said = state.into_iter().flat_map(|state| {
                    state.items.iter().rev().filter_map(|item| match &item.body {
                        slopty_proto::thread::ItemBody::User(message) => {
                            Some(("user", message.text.text.as_str()))
                        }
                        slopty_proto::thread::ItemBody::Text(text) => {
                            Some(("agent", text.text.as_str()))
                        }
                        _ => None,
                    })
                });
                let lines = row.last_line.as_deref().map(|line| ("agent", line));
                let hit = said
                    .chain(lines)
                    .chain(std::iter::once(("title", row.title.as_str())))
                    .find_map(|(role, text)| excerpt(text, &query).map(|hit| (role, hit)));
                if let Some((role, (excerpt, start, end))) = hit {
                    found.push(serde_json::json!({
                        "chatId": row.id.to_string(),
                        "role": if role == "title" { "agent" } else { role },
                        "excerpt": excerpt,
                        "matchStart": start,
                        "matchEnd": end,
                    }));
                }
            }
        }
        serde_json::Value::Array(found)
    }

    /// `key`'s answer to any intent: a session being started hears how it went, and what was
    /// kept to be sent again is let go.
    pub(super) fn view_intent_done(&mut self, done: &IntentDone) {
        self.past_done(done); // a session started or taken up again
        self.view_model.unanswered.remove(&done.id);
    }
}

#[cfg(test)]
mod tests {
    use gpui::AppContext as _;
    use slopty_core::WallMs;
    use slopty_proto::ClientMsg;
    use slopty_proto::thread::wire::{
        Intent, TableFrame, ThreadFrame, ThreadRequest, ThreadRow, TurnEnded,
    };
    use slopty_proto::thread::{Cursor, Liveness, ThreadState, TurnId};
    use slopty_view_rpc::methods;
    use tokio::sync::mpsc::Receiver;

    use crate::conversation::thread::fixtures;
    use crate::view_model::transcript::shown_call;
    use crate::workspace::WorkspaceView;
    use crate::workspace::rig::{KEY, link, mac};

    /// A call's value.
    fn reply(
        answer: Result<slopty_view_rpc::RpcReply, slopty_view_rpc::RpcError>,
    ) -> serde_json::Value {
        match answer {
            Ok(slopty_view_rpc::RpcReply::Value(value)) => value,
            Ok(slopty_view_rpc::RpcReply::Stream(_)) => panic!("a stream, not a value"),
            Err(e) => panic!("refused: {e:?}"),
        }
    }

    /// A workspace linked to one worker whose table holds `state`'s thread, followed so its
    /// state is here, and what the worker has been sent since.
    fn holding<'a>(
        state: &ThreadState,
        cx: &'a mut gpui::TestAppContext,
    ) -> (gpui::Entity<WorkspaceView>, &'a mut gpui::VisualTestContext, Receiver<ClientMsg>) {
        let workspace = cx.new(|_| WorkspaceView::new(slopty_theme::Theme::default()));
        let cx = cx.add_empty_window();
        workspace.update(cx, |v, cx| v.add_worker(KEY, "studio".to_owned(), cx));
        let mut rx = link(&workspace, cx, mac(|_| {}), Vec::new());
        let cursor = Cursor { epoch: 1, seq: 1 };
        let thread = state.meta.id;
        let rows = vec![state.row(WallMs::ZERO)];
        workspace.update(cx, |v, cx| {
            v.threads_linked(KEY, cx);
            v.thread_table(KEY, &TableFrame::Snapshot { cursor, rows }, cx);
            v.thread_hub(KEY, cx).update(cx, |hub, cx| hub.open(thread, cx));
            let frame = ThreadFrame::Snapshot { cursor, state: Box::new(state.clone()) };
            v.thread_frame(KEY, thread, frame, cx);
        });
        cx.run_until_parked();
        while rx.try_recv().is_ok() {}
        (workspace, cx, rx)
    }

    /// A followed chat whose thread's state has not come says no transcript, not an empty one:
    /// the shell takes the first it hears as the whole history, so its history arrives as one.
    #[gpui::test]
    fn a_transcript_waits_for_its_threads_state(cx: &mut gpui::TestAppContext) {
        let state = fixtures::thread("tools");
        let thread = state.meta.id;
        let chat = thread.to_string();
        let workspace = cx.new(|_| WorkspaceView::new(slopty_theme::Theme::default()));
        let cx = cx.add_empty_window();
        workspace.update(cx, |v, cx| v.add_worker(KEY, "studio".to_owned(), cx));
        let _rx = link(&workspace, cx, mac(|_| {}), Vec::new());
        let (_, feed, _asks) = crate::view_model::ViewModel::new("me".to_owned());
        let (said, _) = feed.watch_transcript(&chat);
        let cursor = Cursor { epoch: 1, seq: 1 };
        workspace.update(cx, |v, cx| {
            v.view_model.feed = Some(std::sync::Arc::clone(&feed));
            v.threads_linked(KEY, cx);
            let rows = vec![state.row(WallMs::ZERO)];
            v.thread_table(KEY, &TableFrame::Snapshot { cursor, rows }, cx);
            v.view_follow(chat.clone(), cx);
            v.view_publish(cx);
        });
        assert!(said.borrow().is_none(), "nothing is said before the thread's state comes");
        workspace.update(cx, |v, cx| {
            let frame = ThreadFrame::Snapshot { cursor, state: Box::new(state.clone()) };
            v.thread_frame(KEY, thread, frame, cx);
            v.view_publish(cx);
        });
        let history = said.borrow().clone().expect("the transcript once the state has come");
        assert!(!history.entries.is_empty(), "the whole history, at once");
    }

    /// A followed chat on a machine out of reach, with nothing of its thread kept here, shows
    /// it has nothing instead of loading for good.
    #[gpui::test]
    fn a_chat_out_of_reach_shows_it_has_nothing(cx: &mut gpui::TestAppContext) {
        let state = fixtures::thread("tools");
        let thread = state.meta.id;
        let chat = thread.to_string();
        let workspace = cx.new(|_| WorkspaceView::new(slopty_theme::Theme::default()));
        let cx = cx.add_empty_window();
        workspace.update(cx, |v, cx| v.add_worker(KEY, "studio".to_owned(), cx));
        let _rx = link(&workspace, cx, mac(|_| {}), Vec::new());
        let (_, feed, _asks) = crate::view_model::ViewModel::new("me".to_owned());
        let (said, _) = feed.watch_transcript(&chat);
        let cursor = Cursor { epoch: 1, seq: 1 };
        workspace.update(cx, |v, cx| {
            v.view_model.feed = Some(std::sync::Arc::clone(&feed));
            v.threads_linked(KEY, cx);
            let rows = vec![state.row(WallMs::ZERO)];
            v.thread_table(KEY, &TableFrame::Snapshot { cursor, rows }, cx);
            v.view_follow(chat.clone(), cx);
            v.threads_unlinked(KEY, cx);
            v.view_publish(cx);
        });
        let said = said.borrow().clone();
        assert!(said.is_some_and(|said| said.entries.is_empty()), "nothing, said");
    }

    /// A working thread on a machine out of reach stands as of its last change, so the shell
    /// lets it go stale rather than spin on; one in reach is current.
    #[gpui::test]
    fn a_working_thread_out_of_reach_goes_stale(cx: &mut gpui::TestAppContext) {
        use slopty_proto::thread::Phase;
        let mut state = fixtures::thread("tools");
        state.status.phase = Phase::Working;
        let changed = 1_760_000_000_000_u64;
        let workspace = cx.new(|_| WorkspaceView::new(slopty_theme::Theme::default()));
        let cx = cx.add_empty_window();
        workspace.update(cx, |v, cx| v.add_worker(KEY, "studio".to_owned(), cx));
        let _rx = link(&workspace, cx, mac(|_| {}), Vec::new());
        let feed = serve(&workspace, cx);
        let cursor = Cursor { epoch: 1, seq: 1 };
        let heard = |linked: bool, cx: &mut gpui::VisualTestContext| {
            workspace.update(cx, |v, cx| {
                v.threads_linked(KEY, cx);
                let rows = vec![state.row(WallMs::from_millis(changed))];
                v.thread_table(KEY, &TableFrame::Snapshot { cursor, rows }, cx);
                if !linked {
                    v.threads_unlinked(KEY, cx);
                }
                v.view_publish(cx);
            });
            let sessions = feed.said_sessions();
            let session = sessions.iter().find(|s| s.chat_id == state.meta.id.to_string());
            session.expect("its session").updated_at.timestamp_millis()
        };
        assert!(heard(true, cx) > i64::try_from(changed).expect("a time"), "in reach: now");
        assert_eq!(heard(false, cx), i64::try_from(changed).expect("a time"), "out of reach");
    }

    /// A turn under way stands as of when this client heard its lease move, by its own clock:
    /// a worker whose clock is ten minutes off keeps it live, a lease that does not move keeps
    /// the time it was heard (so it goes stale), and one that moves is heard again.
    #[gpui::test]
    fn a_lease_stands_as_of_its_arrival_whatever_the_workers_clock(cx: &mut gpui::TestAppContext) {
        use slopty_proto::thread::Phase;
        use slopty_view_proto::view::{Indicator, effective_indicator};
        let mut state = fixtures::thread("tools");
        state.status.phase = Phase::Working;
        let workspace = cx.new(|_| WorkspaceView::new(slopty_theme::Theme::default()));
        let cx = cx.add_empty_window();
        workspace.update(cx, |v, cx| v.add_worker(KEY, "studio".to_owned(), cx));
        let _rx = link(&workspace, cx, mac(|_| {}), Vec::new());
        let feed = serve(&workspace, cx);
        let ten_minutes_behind = |at: chrono::DateTime<chrono::Utc>| {
            let behind = at.checked_sub_signed(chrono::TimeDelta::minutes(10)).expect("a time");
            WallMs::from_millis(u64::try_from(behind.timestamp_millis()).expect("a time"))
        };
        let lease = ten_minutes_behind(chrono::Utc::now());
        let said = |lease: WallMs, seq: u64, cx: &mut gpui::VisualTestContext| {
            workspace.update(cx, |v, cx| {
                v.threads_linked(KEY, cx);
                let mut row = state.row(WallMs::ZERO);
                row.lease_ms = Some(lease);
                let cursor = Cursor { epoch: 1, seq };
                v.thread_table(KEY, &TableFrame::Snapshot { cursor, rows: vec![row] }, cx);
                v.view_publish(cx);
            });
            let sessions = feed.said_sessions();
            sessions
                .iter()
                .find(|s| s.chat_id == state.meta.id.to_string())
                .cloned()
                .expect("its session")
        };
        let first = said(lease, 1, cx);
        let now = chrono::Utc::now();
        let off = now.signed_duration_since(first.updated_at).num_seconds().abs();
        assert!(off < 5, "as of its arrival here, {off} s off");
        assert_eq!(effective_indicator(Some(&first), now), Indicator::Working, "live");
        let later = now.checked_add_signed(chrono::TimeDelta::seconds(50)).expect("a time");
        assert_eq!(effective_indicator(Some(&first), later), Indicator::None, "unmoved: stale");
    }

    /// A lease that does not move keeps the time it was heard, so its turn goes stale; one that
    /// moves, however the worker's clock runs, is heard as of now.
    #[test]
    fn a_lease_is_heard_when_it_moves() {
        let start = std::time::Instant::now();
        let later = start.checked_add(std::time::Duration::from_secs(30)).expect("a time");
        let lease = WallMs::from_millis(5_000);
        assert_eq!(super::lease_heard(None, lease, start), start, "first heard");
        assert_eq!(super::lease_heard(Some((lease, start)), lease, later), start, "unmoved");
        let back = WallMs::from_millis(1_000);
        assert_eq!(super::lease_heard(Some((lease, start)), back, later), later, "moved back");
    }

    /// A tool call's whole output is fetched as its thread holds it.
    #[gpui::test]
    fn a_tool_calls_whole_output_is_fetched(cx: &mut gpui::TestAppContext) {
        let state = fixtures::thread("tools");
        let (item, output) = state
            .items
            .iter()
            .find_map(|item| match &item.body {
                slopty_proto::thread::ItemBody::Tool(call) => {
                    Some((item.id.0.clone(), call.output.clone()?.text))
                }
                _ => None,
            })
            .expect("a tool call with output");
        let (workspace, cx, _rx) = holding(&state, cx);
        let params = serde_json::json!({ "blobRef": format!("{}/{item}", state.meta.id) });
        let answer = workspace.update(cx, |v, cx| v.view_tool_blob(&params, cx));
        cx.run_until_parked();
        let answer = futures::FutureExt::now_or_never(answer).expect("answered at once");
        assert_eq!(reply(answer)["text"], output);
    }

    /// What the worker was sent about its threads.
    fn sent_threads(rx: &mut Receiver<ClientMsg>) -> Vec<ThreadRequest> {
        std::iter::from_fn(|| rx.try_recv().ok())
            .filter_map(|msg| match msg {
                ClientMsg::Thread(request) => Some(request),
                _ => None,
            })
            .collect()
    }

    /// The workspace serves a feed the test reads.
    fn serve(
        workspace: &gpui::Entity<WorkspaceView>,
        cx: &mut gpui::VisualTestContext,
    ) -> std::sync::Arc<crate::view_model::Feed> {
        let (_, feed, _asks) = crate::view_model::ViewModel::new("me".to_owned());
        workspace.update(cx, |v, _| v.view_model.feed = Some(std::sync::Arc::clone(&feed)));
        feed
    }

    /// `state`'s chat is followed, its transcript watched.
    fn follow(
        workspace: &gpui::Entity<WorkspaceView>,
        cx: &mut gpui::VisualTestContext,
        state: &ThreadState,
    ) -> String {
        let chat = state.meta.id.to_string();
        let thread = state.meta.id;
        workspace.update(cx, |v, _| {
            v.view_model.followed.insert(chat.clone(), (KEY, thread));
        });
        chat
    }

    /// A followed chat whose thread holds older turns than it was sent asks for them.
    #[gpui::test]
    fn older_turns_are_asked_while_a_chat_is_followed(cx: &mut gpui::TestAppContext) {
        let mut state = fixtures::thread("tools");
        state.older = true;
        let thread = state.meta.id;
        let (workspace, cx, mut rx) = holding(&state, cx);
        follow(&workspace, cx, &state);
        workspace.update(cx, |v, cx| v.view_fetch_followed(cx));
        cx.run_until_parked();
        let paged = sent_threads(&mut rx)
            .into_iter()
            .any(|request| matches!(request, ThreadRequest::Page { thread: t, .. } if t == thread));
        assert!(paged, "its older turns are asked for");
    }

    /// "Show full output" fetches what the worker cut, whole.
    #[gpui::test]
    fn a_cut_output_is_fetched_whole(cx: &mut gpui::TestAppContext) {
        use slopty_proto::thread::detail::{Clip, Clipped};
        use slopty_proto::thread::wire::Expanded;
        use slopty_proto::thread::{ContentRef, ItemBody};
        let mut state = fixtures::thread("tools");
        let whole = "out\n".repeat(1000);
        let full = ContentRef("t1:out".to_owned());
        let item = state.items.iter_mut().find(|item| shown_call(&item.body)).expect("a tool call");
        let id = item.id.0.clone();
        if let ItemBody::Tool(call) = &mut item.body {
            let clip = Clip { lines: 40, chars: 32_000 };
            call.output = Some(Clipped::tail(&whole, clip, Some(full.clone())));
        }
        let thread = state.meta.id;
        let (workspace, cx, mut rx) = holding(&state, cx);
        let params = serde_json::json!({ "blobRef": format!("{thread}/{id}") });
        let answer = workspace.update(cx, |v, cx| v.view_tool_blob(&params, cx));
        cx.run_until_parked();
        let asked = sent_threads(&mut rx).into_iter().any(
            |request| matches!(request, ThreadRequest::Expand { content, .. } if content == full),
        );
        assert!(asked, "the whole is asked of the worker");
        let frame = ThreadFrame::Expanded { content: full, body: Expanded::Text(whole.clone()) };
        workspace.update(cx, |v, cx| v.thread_frame(KEY, thread, frame, cx));
        cx.executor().advance_clock(std::time::Duration::from_millis(100));
        cx.run_until_parked();
        let answer = futures::FutureExt::now_or_never(answer).expect("answered");
        assert_eq!(reply(answer)["text"], whole);
    }

    /// A picture of a thread is read back through the thread once its worker sent it.
    #[gpui::test]
    fn a_threads_picture_is_read_back(cx: &mut gpui::TestAppContext) {
        use slopty_proto::thread::wire::Expanded;
        use slopty_proto::thread::{ContentRef, Image, ItemBody};
        let mut state = fixtures::thread("tools");
        let at = ContentRef("img:1".to_owned());
        let image = Image {
            digest: "ab".repeat(32),
            media_type: "image/png".to_owned(),
            bytes: 3,
            width: 1,
            height: 1,
            at: at.clone(),
        };
        let item = state.items.iter_mut().find(|item| shown_call(&item.body)).expect("a tool call");
        let id = item.id.0.clone();
        if let ItemBody::Tool(call) = &mut item.body {
            call.images = vec![image.clone()];
        }
        let thread = state.meta.id;
        let (workspace, cx, _rx) = holding(&state, cx);
        let path = crate::view_model::transcript::image_path(thread, &id, 0, &image);
        let answer = workspace.update(cx, |v, cx| v.view_image_chunk(&path, 0, cx));
        let frame = ThreadFrame::Expanded { content: at, body: Expanded::Bytes(vec![1, 2, 3]) };
        workspace.update(cx, |v, cx| v.thread_frame(KEY, thread, frame, cx));
        cx.executor().advance_clock(std::time::Duration::from_millis(100));
        cx.run_until_parked();
        let chunk = reply(futures::FutureExt::now_or_never(answer).expect("answered"));
        assert_eq!(chunk["mimeType"], "image/png");
        assert_eq!(chunk["data"], "AQID");
        assert_eq!(chunk["done"], true);
    }

    /// A working turn's time runs from when it began, through the approvals within it.
    #[gpui::test]
    fn a_turns_time_runs_from_when_it_began(cx: &mut gpui::TestAppContext) {
        use slopty_proto::thread::Phase;
        let mut state = fixtures::thread("tools");
        let (workspace, cx, _rx) = holding(&state, cx);
        let feed = serve(&workspace, cx);
        let thread = state.meta.id;
        let began = 1_760_000_000_000_u64;
        let turn = state.turns.last_mut().expect("a turn");
        turn.state = slopty_proto::thread::TurnState::Active;
        turn.started_ms = WallMs::from_millis(began);
        for ((phase, since), seq) in [
            (Phase::Working, began),
            (Phase::NeedsYou, began + 5_000),
            (Phase::Working, began + 9_000),
        ]
        .into_iter()
        .zip(2_u64..)
        {
            state.status.phase = phase;
            state.status.since_ms = WallMs::from_millis(since);
            let cursor = Cursor { epoch: 1, seq };
            let rows = vec![state.row(WallMs::ZERO)];
            workspace.update(cx, |v, cx| {
                v.thread_table(KEY, &TableFrame::Snapshot { cursor, rows }, cx);
                v.view_publish(cx);
            });
        }
        let sessions = feed.said_sessions();
        let session =
            sessions.iter().find(|s| s.chat_id == thread.to_string()).expect("its session");
        let started = session.started_at.map(|at| at.timestamp_millis());
        assert_eq!(started, Some(i64::try_from(began).expect("a time")));
    }

    /// A subagent running on another machine shows as running on its spawn.
    #[gpui::test]
    fn a_subagent_on_another_machine_runs(cx: &mut gpui::TestAppContext) {
        use slopty_proto::thread::detail::{AgentDetail, Clipped};
        use slopty_proto::thread::{ItemBody, Phase, ToolDetail};
        let mut state = fixtures::thread("tools");
        let mut child = fixtures::empty();
        child.status.phase = Phase::Working;
        let item = state.items.iter_mut().find(|item| shown_call(&item.body)).expect("a tool call");
        if let ItemBody::Tool(call) = &mut item.body {
            call.detail = Some(ToolDetail::Agent(AgentDetail {
                agent_type: None,
                description: Some("look around".to_owned()),
                prompt: Clipped::whole("look"),
                background: false,
                report: None,
                tokens: None,
                tool_uses: None,
                duration_ms: None,
                model: None,
                tally: std::collections::BTreeMap::new(),
                changed: slopty_proto::thread::Changed::default(),
            }));
            call.child = Some(child.meta.id);
        }
        let (workspace, cx, _rx) = holding(&state, cx);
        let chat = follow(&workspace, cx, &state);
        let other = slopty_client::WorkerKey::new(2);
        let rows = vec![child.row(WallMs::ZERO)];
        let cursor = Cursor { epoch: 1, seq: 1 };
        workspace
            .update(cx, |v, cx| v.thread_table(other, &TableFrame::Snapshot { cursor, rows }, cx));
        let said = workspace.read_with(cx, |v, cx| v.view_transcript(&chat, cx)).expect("said");
        let status = said.entries.iter().flat_map(|e| &e.parts).find_map(|part| match part {
            slopty_view_doc::MessagePart::Tool { subagent_status, .. } => *subagent_status,
            _ => None,
        });
        assert_eq!(status, Some(slopty_view_doc::SubagentStatus::Running));
    }

    /// A chat opened is marked seen through its latest turn.
    #[gpui::test]
    fn a_chat_opened_is_seen_through_its_latest_turn(cx: &mut gpui::TestAppContext) {
        let mut state = fixtures::thread("tools");
        state.status.liveness = Liveness::Live;
        let ended = state.row(WallMs::ZERO).ended.expect("a turn that ended").turn;
        let thread = state.meta.id;
        let (workspace, cx, mut rx) = holding(&state, cx);
        let params = serde_json::json!({ "op": "markChatSeen", "chatId": thread.to_string() });
        let answer = workspace.update(cx, |v, cx| v.view_call(methods::MUTATE, &params, cx));
        assert!(answer.is_ok(), "{answer:?}");
        cx.run_until_parked();
        let seen = sent_threads(&mut rx).into_iter().any(|request| {
            matches!(request, ThreadRequest::Intent { intent: Intent::Seen { turn }, .. }
                if turn == ended)
        });
        assert!(seen, "seen through its latest turn");
    }

    /// A chat opened reads seen at once, and stays so until its worker says the mark back: a
    /// publish meanwhile, its row still unseen, shows it read. A turn that ends after the look
    /// is unread.
    #[gpui::test]
    fn a_chat_opened_reads_seen_before_its_worker_says_so(cx: &mut gpui::TestAppContext) {
        let mut state = fixtures::thread("tools");
        state.status.liveness = Liveness::Live;
        let row = state.row(WallMs::ZERO);
        let ended = row.ended.expect("a turn that ended");
        let thread = state.meta.id;
        let (workspace, cx, _rx) = holding(&state, cx);
        let feed = serve(&workspace, cx);
        let unseen = |workspace: &gpui::Entity<WorkspaceView>, cx: &mut gpui::VisualTestContext| {
            workspace.update(cx, WorkspaceView::view_publish);
            let chats = feed.said_chats().expect("the chats");
            chats.iter().find(|c| c.id == thread.to_string()).expect("its chat").unseen()
        };
        let mut seq = 1_u64;
        let mut table = |ended: TurnEnded| {
            seq = seq.saturating_add(1);
            let rows = vec![ThreadRow { ended: Some(ended), ..row.clone() }];
            TableFrame::Snapshot { cursor: Cursor { epoch: 1, seq }, rows }
        };
        assert!(unseen(&workspace, cx), "unread before the look");

        let params = serde_json::json!({ "op": "markChatSeen", "chatId": thread.to_string() });
        let answer = workspace.update(cx, |v, cx| v.view_call(methods::MUTATE, &params, cx));
        assert!(answer.is_ok(), "{answer:?}");
        cx.run_until_parked();
        assert!(!unseen(&workspace, cx), "read at once");
        // The worker's table moves on before the mark comes back.
        workspace.update(cx, |v, cx| v.thread_table(KEY, &table(ended), cx));
        cx.run_until_parked();
        assert!(!unseen(&workspace, cx), "read while the mark is on its way");

        let next = TurnEnded { turn: TurnId(ended.turn.0.saturating_add(1)), ..ended };
        workspace.update(cx, |v, cx| v.thread_table(KEY, &table(next), cx));
        cx.run_until_parked();
        assert!(unseen(&workspace, cx), "a turn ended after the look is unread");
    }

    /// The chats wait for every machine's threads: an empty first list would read as every
    /// chat gone.
    #[gpui::test]
    fn the_chats_wait_for_every_machines_threads(cx: &mut gpui::TestAppContext) {
        let state = fixtures::thread("tools");
        let workspace = cx.new(|_| WorkspaceView::new(slopty_theme::Theme::default()));
        let cx = cx.add_empty_window();
        workspace.update(cx, |v, cx| v.add_worker(KEY, "studio".to_owned(), cx));
        let _rx = link(&workspace, cx, mac(|_| {}), Vec::new());
        let feed = serve(&workspace, cx);
        workspace.update(cx, WorkspaceView::view_publish);
        assert!(feed.said_chats().is_none(), "nothing before the machine's threads");
        let cursor = Cursor { epoch: 1, seq: 1 };
        workspace.update(cx, |v, cx| {
            v.threads_linked(KEY, cx);
            let rows = vec![state.row(WallMs::ZERO)];
            v.thread_table(KEY, &TableFrame::Snapshot { cursor, rows }, cx);
            v.view_publish(cx);
        });
        let chats = feed.said_chats().expect("said once they came");
        assert_eq!(chats.len(), 1);
    }

    /// A tapped note opens the chat it is about.
    #[test]
    fn a_tapped_note_names_its_chat() {
        use slopty_platform::notify::info::{THREAD, WORKER};
        let state = fixtures::thread("tools");
        let tap = slopty_platform::notify::Tap {
            id: "n".to_owned(),
            info: std::collections::BTreeMap::from([
                (WORKER.to_owned(), KEY.value().to_string()),
                (THREAD.to_owned(), state.meta.id.to_string()),
            ]),
        };
        let chat = WorkspaceView::tapped_chat(&tap);
        assert_eq!(chat, Some(state.meta.id.to_string()));
    }
}

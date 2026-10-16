//! The shell fed from the workspace ([`crate::view_model`]): its lists published as the workers and
//! their thread tables move, the transcripts it watches followed, and its calls answered
//! ([`crate::view_model`]).
//!
//! A chat the interface mints before its first message is held here until that message
//! starts its thread; the thread then goes by the chat's id, so the interface never sees the
//! switch.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use gpui::{AppContext as _, Context, Subscription, Task};
use slopty_client::WorkerKey;
use slopty_proto::ClientMsg;
use slopty_proto::thread::wire::{
    Intent, IntentDone, NewWorktree, Outcome, Start, ThreadRequest, ThreadRow,
};
use slopty_proto::thread::{
    AgentId, Delivery, IntentId, Liveness, PendingState, Phase, ThreadId, ThreadState, TurnState,
    Wait,
};
use slopty_view_doc::SessionCommandPayload;
use slopty_view_proto::HarnessId;
use slopty_view_rpc::{RpcError, RpcReply, methods};
use tokio::sync::mpsc;

use super::WorkspaceView;
use crate::view_model::{Ask, Feed, Pull, Transcript, rows, transcript};

mod blobs;
#[cfg(test)]
mod delivery_tests;
#[cfg(target_vendor = "apple")]
mod pages;
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

/// How long a Stop waits for its worker's word before the shell is told it failed: the
/// worker's own ladder ends a turn well within it.
const INTERRUPT_WITHIN: Duration = Duration::from_secs(15);

/// How long the chats and projects wait for every machine's threads before they go out with
/// those that came: a machine out of reach must not hold the sidebar back for good.
const TABLES_WITHIN: Duration = Duration::from_secs(5);

/// How many older turns one page asks for: a followed thread's whole history comes in a few.
const PAGE_TURNS: u32 = 200;

/// A chat the interface made that has no thread yet.
#[derive(Debug, Clone)]
struct Minted {
    worker: WorkerKey,
    cwd: String,
    created: DateTime<Utc>,
    /// The chat it is a side chat of: its first message asks that chat's thread aside.
    parent: Option<String>,
    /// The branch the person picked for it.
    branch: Option<String>,
}

/// A message written to a chat whose thread is still starting: sent to the thread once it has
/// started, after the chat's first words.
#[derive(Debug, Clone)]
struct HeldSend {
    /// The words as the person wrote them.
    text: String,
    attachments: Vec<String>,
    /// Held for the turn's end, as the shell asked.
    hold: bool,
    /// The id the shell gave it: its queue row's, or its message's.
    name: String,
    /// It is a queue row (`QueueMessage`), not a message sent at once.
    queued: bool,
}

/// A chat's start the worker turned down, kept to say why and to be tried again.
#[derive(Debug, Clone)]
struct RefusedStart {
    worker: WorkerKey,
    /// What was sent: the start, or the aside a side chat starts with.
    msg: ClientMsg,
    /// The side chat's first message, which goes once its aside has started.
    first_words: Option<FirstWords>,
    /// The id the shell gave the first message, and its words as the person wrote them.
    named: Option<(String, String)>,
    /// Why, as the person reads it.
    words: String,
    /// When, in ms since the epoch.
    at: i64,
}

/// A side chat's first message, sent to its aside once it started.
#[derive(Debug, Clone)]
struct FirstWords {
    /// In the agent's words.
    text: String,
    attachments: Vec<String>,
    /// The id the shell gave it.
    name: String,
    /// As the person wrote it.
    said: String,
}

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
    /// Threads that go by the id of the chat that started them.
    aliases: HashMap<ThreadId, String>,
    /// Chats made in the interface with no thread yet.
    minted: HashMap<String, Minted>,
    /// Starts sent for minted chats, by their intent.
    starts: HashMap<IntentId, String>,
    /// What the view model sent a worker itself (a minted chat's start, a side chat's aside) and
    /// the worker has not answered, by its intent: sent again, under the same id, when the link
    /// comes back and when the person retries.
    unanswered: HashMap<IntentId, (WorkerKey, ClientMsg)>,
    /// The first message of a side chat, by the aside that starts it: sent to the aside once
    /// it has started, with the id the shell gave it.
    first_words: HashMap<IntentId, FirstWords>,
    /// The id the shell gave each message it sent, by the intent that sent it: its entry in
    /// the transcript goes by it, so the shell's own echo of the message gives way to it.
    named: HashMap<IntentId, String>,
    /// The words of each message the shell sent, by its intent, as the person wrote them: the
    /// agent was given them in its own words, and the chat shows the person's.
    said: HashMap<IntentId, String>,
    /// When the app began to serve the interface: the lists wait [`TABLES_WITHIN`] at most for
    /// every machine's threads.
    serving_since: Option<Instant>,
    /// What the right-pane surfaces wait on from the workers ([`surfaces`]).
    surfaces: surfaces::Surfaces,
    /// The messages the shell queued (`QueueMessage`), by their intent: the id of the queue
    /// row each is. A message sent at once is the shell's own echo until it goes, never a row.
    queue_rows: HashMap<IntentId, String>,
    /// Messages written to a chat while its thread starts, by chat.
    held: HashMap<String, Vec<HeldSend>>,
    /// Starts the worker turned down, by chat.
    refused: HashMap<String, RefusedStart>,
    /// Chats deleted while their start was on its way: their thread is closed once it starts.
    doomed: BTreeSet<String>,
    /// Stops waiting for their worker's word, by intent.
    interrupts: HashMap<IntentId, tokio::sync::oneshot::Sender<Result<RpcReply, RpcError>>>,
    /// Whether the aliases were read back from where they are kept.
    aliases_read: bool,
    /// slopty: the threads of chats started under a followed chat, held open while they ask
    /// the person something, so it is asked in the followed chat ([`work`]).
    asking: BTreeSet<(WorkerKey, ThreadId)>,
    /// slopty: the pages agents show inline, hosted per window ([`pages`]).
    #[cfg(target_vendor = "apple")]
    pages: pages::Pages,
}

impl std::fmt::Debug for Bridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bridge")
            .field("followed", &self.followed.len())
            .field("minted", &self.minted.len())
            .finish_non_exhaustive()
    }
}

/// The harness the shell knows agent `agent` by: the inverse of [`agent_of`].
fn harness_of(agent: &AgentId) -> HarnessId {
    agent.acp_name().map_or_else(|| HarnessId::named(&agent.0), HarnessId::named)
}

/// `text` from the composer as agent `harness` reads it: the shell's chips (a file, a folder, a
/// command, a skill, a chat, an attachment) put in the agent's own words (zeron's own decoder,
/// [`slopty_view_proto::invocation::harness_prompt`]). Codex's skills too: Slopty gives Codex
/// its prompt as text.
fn agent_prompt(text: &str, harness: HarnessId) -> String {
    let prompt = slopty_view_proto::invocation::harness_prompt(text, harness);
    if harness == HarnessId::CODEX {
        return slopty_view_proto::invocation::invocation_prompt(&codex_skills(&prompt));
    }
    prompt
}

/// The skills Codex lists, picked by `$` (the `harness-skill:` ones `ListSkills` gives), as
/// Codex's own `$name`: its adapter sends each with the skill it names.
fn codex_skills(text: &str) -> String {
    use slopty_view_proto::invocation::{Invocation, invocation_links, native_skill_identity};
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for (range, invocation) in invocation_links(text) {
        let Invocation::Skill { name, path, .. } = &invocation else { continue };
        if !native_skill_identity(path) {
            continue;
        }
        out.push_str(text.get(at..range.start).unwrap_or_default());
        out.push('$');
        out.push_str(name);
        at = range.end;
    }
    out.push_str(text.get(at..).unwrap_or_default());
    out
}

/// The agent a shell harness names: Claude Code, Codex and pi by their own ids, any other an
/// ACP agent by its registry's name; Claude Code when the shell names none.
fn agent_of(harness: Option<HarnessId>) -> AgentId {
    match harness {
        None | Some(HarnessId::CLAUDE_CODE | HarnessId::MOCK) => {
            AgentId::named(AgentId::CLAUDE_CODE)
        }
        Some(HarnessId::CODEX) => AgentId::named(AgentId::CODEX),
        Some(HarnessId::PI) => AgentId::named(AgentId::PI),
        Some(other) => AgentId::acp(other.as_str()),
    }
}

/// The name of a worktree a chat starts in: the first words of its first message, then a few
/// letters of the start's own id so no two chats share one.
fn worktree_name(prompt: &str, id: IntentId) -> String {
    let words: Vec<String> = prompt
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .take(4)
        .map(str::to_ascii_lowercase)
        .collect();
    let mut name = words.join("-");
    name.truncate(32);
    let name = name.trim_end_matches('-');
    let tag: String = id.to_string().chars().rev().take(4).collect();
    if name.is_empty() { format!("chat-{tag}") } else { format!("{name}-{tag}") }
}

/// When intent `id` was made, in ms since the epoch: its id is time-ordered (`UUIDv7`).
fn refused_at(id: IntentId) -> Option<u64> {
    let (secs, nanos) = id.as_uuid().get_timestamp()?.to_unix();
    Some(secs.saturating_mul(1000).saturating_add(u64::from(nanos / 1_000_000)))
}

/// Where the ids the chats started here go by are kept, so a chat keeps its id (and its pin,
/// its section, its tab) when the app comes back.
fn aliases_path() -> std::path::PathBuf {
    slopty_platform::dirs::data_dir().join("interface").join("aliases.json")
}

/// The aliases as they are kept: thread ids to chat ids.
fn aliases_text(aliases: &HashMap<ThreadId, String>) -> Vec<u8> {
    let kept: BTreeMap<String, &String> =
        aliases.iter().map(|(thread, chat)| (thread.to_string(), chat)).collect();
    serde_json::to_vec(&kept).unwrap_or_default()
}

/// The aliases `text` keeps, by thread.
fn aliases_of(text: &[u8]) -> HashMap<ThreadId, String> {
    serde_json::from_slice::<BTreeMap<String, String>>(text)
        .inspect_err(|e| tracing::warn!(%e, "the aliases do not read"))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(thread, chat)| Some((thread.parse().ok()?, chat)))
        .collect()
}

/// The aliases kept, by thread.
fn read_aliases() -> HashMap<ThreadId, String> {
    std::fs::read(aliases_path()).map(|text| aliases_of(&text)).unwrap_or_default()
}

/// Why a message the worker holds does not go, as its queue row says; nothing while it waits.
fn held_gate(state: &PendingState) -> Option<slopty_view_doc::QueueDeliveryGate> {
    let reason = match state {
        PendingState::Waiting => return None,
        PendingState::Held { reason } => reason.clone(),
        PendingState::Sending => "Going to the agent now".to_owned(),
    };
    Some(slopty_view_doc::QueueDeliveryGate::Held { reason })
}

/// When the usage limit holding `state`'s agent lifts, while one holds it: its failed last turn's
/// own word, else the reset of its most used rate window.
fn limit_resets(state: &ThreadState) -> Option<DateTime<Utc>> {
    let limited = state.status.wait.as_ref().is_some_and(|wait| wait.kind == Wait::LIMIT);
    let failed_until = state.turns.last().and_then(|turn| match turn.state {
        TurnState::Failed { until_ms, .. } => until_ms,
        _ => None,
    });
    if let Some(until) = failed_until.filter(|_| limited || state.status.phase == Phase::Failed) {
        return Some(rows::time(until));
    }
    if !limited {
        return None;
    }
    state
        .meters
        .limits
        .iter()
        .filter(|limit| limit.used_bp >= 10_000)
        .filter_map(|limit| limit.resets_ms)
        .max()
        .map(rows::time)
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

/// Whether `params` is a `QueueCommand` that stops the agent.
fn is_interrupt(params: &serde_json::Value) -> bool {
    params.get("command").and_then(|c| c.get("kind")).and_then(serde_json::Value::as_str)
        == Some("interrupt")
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
        if !self.view_model.aliases_read {
            self.view_model.aliases_read = true;
            for (thread, chat) in read_aliases() {
                self.view_model.aliases.entry(thread).or_insert(chat);
            }
        }
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

    /// Keep the aliases where they outlive the app, once the app serves the interface.
    fn keep_aliases(&self, cx: &Context<Self>) {
        if !self.view_model.aliases_read {
            return;
        }
        let text = aliases_text(&self.view_model.aliases);
        cx.background_spawn(async move {
            let path = aliases_path();
            let written = path
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::write(&path, text));
            if let Err(e) = written {
                tracing::warn!(path = %path.display(), %e, "the aliases were not kept");
            }
        })
        .detach();
    }

    /// The id the interface knows `thread` by.
    pub(super) fn chat_id(&self, thread: ThreadId) -> String {
        self.view_model.aliases.get(&thread).cloned().unwrap_or_else(|| thread.to_string())
    }

    /// The thread a chat of the interface shows, and its worker.
    fn chat_thread(&self, chat: &str, cx: &gpui::App) -> Option<(WorkerKey, ThreadId)> {
        let thread = self
            .view_model
            .aliases
            .iter()
            .find(|(_, alias)| alias.as_str() == chat)
            .map(|(thread, _)| *thread)
            .or_else(|| chat.parse().ok())?;
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
        self.settle_configs(cx);
        self.probe_projects(cx);
        self.view_fetch_followed(cx);
        self.view_open_asking(cx);
        let Some(feed) = self.view_model.feed.clone() else { return };
        let now = Utc::now();
        let devices =
            self.workers.keys().filter_map(|key| self.surface_device_row(*key, now)).collect();
        let mut chats = Vec::new();
        let mut sessions = Vec::new();
        let mut pulls = Vec::new();
        let mut folders: BTreeMap<(String, String), Folder> = BTreeMap::new();
        let mut links = Vec::new();
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
            let id = self.chat_id(row.id);
            let mut chat = rows::chat(row, device);
            let place = self.view_place(device, row.cwd.as_deref(), row.repo.as_deref());
            chat.space_id = place.as_deref().map(|path| self.view_space_id(device, path));
            chat.config = Some(self.chat_config(&id, device, row));
            if let Some(summary) = rows::pull(row, chat.branch.as_deref()) {
                let folders = [row.cwd.clone(), row.repo.clone()].into_iter().flatten().collect();
                let branch = chat.branch.clone();
                pulls.push(Pull { device: device.clone(), folders, branch, summary });
            }
            chat.archived = self.surface_archived(&id);
            chat.id.clone_from(&id);
            if let Some(parent) = rows::parent(row) {
                chat.parent_chat_id = Some(self.chat_id(parent));
            }
            if let Some(path) = place {
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
            let unsent = self.held_hub(*key).map_or(0, |hub| {
                hub.read(cx).threads().outbox().of(row.id).filter(|s| s.outcome.is_none()).count()
            });
            links.push(self.view_chat_link(&id, *key, unsent));
            session.chat_id = id;
            chats.push(chat);
            sessions.push(session);
        }
        for (id, minted) in &self.view_model.minted {
            let device = minted.worker.to_string();
            let held = self.view_model.held.get(id).map_or(0, Vec::len);
            links.push(self.view_chat_link(id, minted.worker, held));
            let place = self.view_place(&device, Some(&minted.cwd), None);
            if let Some(path) = &place {
                let folder = folders.entry((device.clone(), path.clone())).or_default();
                folder.began(minted.created.timestamp_millis());
            }
            chats.push(slopty_view_proto::Chat {
                id: id.clone(),
                device_id: device.clone(),
                title: None,
                archived: self.surface_archived(id),
                cwd: Some(minted.cwd.clone()),
                branch: minted.branch.clone(),
                checkout_id: None,
                source_context: None,
                config: self.picked_config(id),
                last_message_preview: None,
                last_message_at: None,
                created_at: minted.created,
                harness_session_id: None,
                harness_session_cwd: None,
                space_id: place.map(|path| self.view_space_id(&device, &path)),
                last_seen_at: None,
                room_gen: None,
                parent_chat_id: minted.parent.clone(),
                subagent: false,
                in_parent: false,
                draft: None, // slopty: new field.
            });
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
        self.view_model.leases = heard_leases;
        let workers_up = (!self.workers.is_empty())
            .then(|| self.workers.values().any(|worker| worker.status.is_up()));
        feed.links(workers_up, links);
        feed.transfers(self.attaching());
        feed.pulls(pulls);
        feed.sidebar(self.sidebar_state());
        // A thread whose state has not come says nothing yet: an empty transcript would stand as
        // its whole history, and the real one, once it comes, as live arrivals. A chat with no
        // thread had its empty transcript when followed.
        for chat in feed.followed() {
            // slopty: its tray, dock and rules beside it.
            if let Some(views) = self.view_chat_views(&chat, cx) {
                feed.chat_views(&chat, views);
            }
            if let Some(said) = self.view_transcript(&chat, cx) {
                feed.transcript(&chat, said);
            } else if let Some(refused) = self.view_model.refused.get(&chat) {
                let mut entries = Vec::new();
                let device = refused.worker.to_string();
                transcript::refused(&refused.words, refused.at, &device, &mut entries);
                feed.transcript(&chat, Transcript::of(entries));
            } else if self.view_unreachable(&chat, cx) {
                // Its machine is out of reach and nothing of the thread is kept here: the chat
                // shows it has nothing rather than waiting for good.
                feed.transcript(&chat, Transcript::of(Vec::new()));
            }
        }
        for chat in feed.queued() {
            feed.queue(&chat, self.view_queue(&chat, cx));
        }
    }

    /// The messages waiting to go to the chat `chat`'s agent, as its thread holds them.
    /// A chat still starting shows what it holds for its thread. A message reads as the person
    /// wrote it, its chips as chips.
    fn view_queue(&self, chat: &str, cx: &Context<Self>) -> Vec<slopty_view_doc::QueuedMessage> {
        if let Some(minted) = self.view_model.minted.get(chat) {
            let held = self.view_model.held.get(chat).map(Vec::as_slice).unwrap_or_default();
            return held
                .iter()
                .filter(|held| held.queued)
                .map(|held| slopty_view_doc::QueuedMessage {
                    id: held.name.clone(),
                    text: held.text.clone(),
                    attachments: held.attachments.clone(),
                    hold_for_turn_end: held.hold,
                    issued_by: minted.worker.to_string(),
                    issued_at: 0,
                    edited_at: None,
                    delivery_gate: None,
                })
                .collect();
        }
        let Some((key, thread)) = self.chat_thread(chat, cx) else { return Vec::new() };
        let Some(hub) = self.held_hub(key) else { return Vec::new() };
        let hub = hub.read(cx);
        let Some(state) =
            hub.threads().mirror(thread).and_then(slopty_client::threads::Mirror::state)
        else {
            return Vec::new();
        };
        let rows = &self.view_model.queue_rows;
        let named = &self.view_model.named;
        let said = &self.view_model.said;
        state
            .pending
            .iter()
            // A message the shell sent at once is its own echo until it goes: not a queue row.
            .filter(|pending| {
                rows.contains_key(&pending.intent) || !named.contains_key(&pending.intent)
            })
            .map(|pending| slopty_view_doc::QueuedMessage {
                id: rows
                    .get(&pending.intent)
                    .cloned()
                    .unwrap_or_else(|| pending.intent.to_string()),
                text: said
                    .get(&pending.intent)
                    .or_else(|| state.said.get(&pending.intent))
                    .cloned()
                    .unwrap_or_else(|| pending.text.clone()),
                attachments: pending.attachments.clone(),
                hold_for_turn_end: pending.delivery == Delivery::Queue,
                issued_by: key.to_string(),
                issued_at: 0,
                edited_at: None,
                delivery_gate: held_gate(&pending.state),
            })
            .collect()
    }

    /// The intent the queue row `row` of the chat's thread is.
    pub(super) fn queued_intent(&self, row: &str) -> Option<IntentId> {
        self.view_model
            .queue_rows
            .iter()
            .find(|(_, id)| id.as_str() == row)
            .map(|(intent, _)| *intent)
            .or_else(|| row.parse().ok())
    }

    /// Whether the chat `chat` follows a thread whose machine is out of reach, with nothing of
    /// the thread kept here.
    fn view_unreachable(&self, chat: &str, cx: &Context<Self>) -> bool {
        let Some((key, _)) = self.view_model.followed.get(chat) else { return false };
        self.held_hub(*key).is_none_or(|hub| !hub.read(cx).threads().linked())
    }

    /// How the chat `chat` on `key` reaches its machine, with `unsent` of its asks not answered
    /// yet: delivered while its worker's link is up, on its way back while it is being dialled,
    /// offline while nothing answers there.
    fn view_chat_link(
        &self,
        chat: &str,
        key: WorkerKey,
        unsent: usize,
    ) -> slopty_view_proto::ChatConnectivity {
        use slopty_view_proto::ChatSyncState;

        use crate::workspace::WorkerStatus as S;
        let status = self.workers.get(&key).map(|worker| &worker.status);
        let sync_state = match status {
            Some(S::Connected | S::Checking) => ChatSyncState::Synced,
            Some(S::Connecting | S::Silent(_) | S::Relinking | S::Reconnecting(_)) => {
                ChatSyncState::Connecting
            }
            _ => ChatSyncState::Offline,
        };
        let up = status.is_some_and(crate::workspace::WorkerStatus::is_up);
        slopty_view_proto::ChatConnectivity {
            chat_id: chat.to_owned(),
            sync_state,
            connected: up,
            delivery_live: up,
            pending_pushes: u64::try_from(unsent).unwrap_or(u64::MAX),
        }
    }

    /// The transcript of the chat `chat` as its thread stands here, with how full its agent's
    /// context is and whether older turns are still on their way.
    fn view_transcript(&self, chat: &str, cx: &gpui::App) -> Option<Transcript> {
        let (key, thread) = self.view_model.followed.get(chat).copied()?;
        let hub = self.held_hub(key)?.read(cx);
        let state = hub.threads().mirror(thread)?.state()?;
        let table = &hub.threads().rows().rows;
        let device = key.to_string();
        let (named, said) = (&self.view_model.named, &self.view_model.said);
        // A subagent may run on any machine.
        let running = |child| {
            self.hubs()
                .find_map(|(_, hub)| hub.read(cx).threads().rows().rows.get(&child))
                .map(|row| transcript::ChildRun::of(row, self.chat_id(child)))
        };
        let host = self.workers.get(&key).map(|worker| worker.name.clone()).unwrap_or_default();
        // The chat a forked thread came from, for its seam: its id and its title.
        let source = |thread: ThreadId| {
            let title = self
                .hubs()
                .find_map(|(_, hub)| hub.read(cx).threads().rows().rows.get(&thread).cloned())
                .map(|row| row.title)
                .unwrap_or_default();
            Some((self.chat_id(thread), title))
        };
        let look = transcript::Look::new(&device)
            .host(&host)
            .source(source)
            .children(running)
            .chats(|thread| self.chat_id(thread))
            .named(|intent| named.get(&intent).cloned())
            .said(|intent| said.get(&intent).or_else(|| state.said.get(&intent)).cloned())
            .whole(|content| match hub.whole(content)?.as_ref() {
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
        transcript::asks(state, &device, &mut entries);
        // slopty: what the chats an agent started under this one ask, asked here.
        transcript::child_asks(&self.view_asking_children(thread, cx), &device, &mut entries);
        // What the worker last turned down, until the thread moves past it.
        let last = state.items.last().map_or(0, |item| item.at_ms.as_millis());
        // The marks the app keeps by itself (seen, a draft) are no act of the person's.
        let acted = |r: &&crate::conversation::thread::hub::Refusal| {
            !matches!(r.intent, Some(Intent::Seen { .. } | Intent::Draft { .. }))
        };
        if let Some(refusal) = hub.refusals(thread).rfind(acted)
            && let Some(at) = refused_at(refusal.id).filter(|at| *at > last)
        {
            let at = i64::try_from(at).unwrap_or(i64::MAX);
            transcript::refused(&refusal.words, at, &device, &mut entries);
        }
        let meters = &state.meters;
        let context_usage = (meters.context_tokens.is_some() || meters.context_window.is_some())
            .then_some(slopty_view_proto::ContextUsage {
                tokens: meters.context_tokens,
                window: meters.context_window,
            });
        let history_pending = state.older && hub.linked();
        Some(Transcript {
            entries,
            context_usage,
            history_pending,
            limits: rows::limits(&meters.limits),
            limit_resets_at: limit_resets(state),
        })
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
                let answer = if method == methods::QUEUE_COMMAND && is_interrupt(&params) {
                    Some(self.view_interrupt(&params, cx))
                } else if method == methods::FETCH_TOOL_BLOB {
                    Some(self.view_tool_blob(&params, cx))
                } else if let Some(answer) = self.view_work_call(&method, &params, cx) {
                    Some(answer)
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

    /// slopty: a call on an inline page ([`pages`]).
    #[cfg(target_vendor = "apple")]
    fn view_page_call(
        &mut self,
        method: &str,
        params: &serde_json::Value,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<RpcReply, RpcError>>> {
        self.page_call(method, params, cx)
    }

    /// slopty: no inline pages here: no call on one is answered.
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

    /// Follow the thread `chat` shows; a minted chat shows nothing until it has one.
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
            methods::QUEUE_COMMAND => self.view_command(params, cx),
            methods::QUEUE_MESSAGE => self.view_queue_message(params, cx),
            methods::RETRY_TURN => self.view_retry_turn(params, cx),
            methods::SAVE_DRAFT => self.view_save_draft(params, cx),
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
            ("createChat", Some(chat)) => {
                let space = param(params, "spaceId").and_then(|id| self.view_space(id));
                let (worker, cwd) = if let Some(found) = space {
                    found
                } else {
                    // A device that runs no worker (this Mac, without one) starts it on the
                    // machine in front, as every other call that names none does.
                    let worker = param(params, "deviceId")
                        .and_then(|device| self.view_worker(device))
                        .or_else(|| self.context_worker())
                        .ok_or_else(|| {
                            RpcError::Failed("Connect a machine to start a chat on".to_owned())
                        })?;
                    (worker, "~".to_owned())
                };
                let cwd = param(params, "cwd").map_or(cwd, str::to_owned);
                let parent = param(params, "parentChatId").map(str::to_owned);
                let branch = param(params, "branch").map(str::to_owned);
                // The model and effort picked on the new chat are its own from the first.
                if let Some(config) =
                    params.get("config").cloned().and_then(|c| serde_json::from_value(c).ok())
                {
                    self.pick_config(chat, config);
                }
                let created = Utc::now();
                let minted = Minted { worker, cwd, created, parent, branch };
                self.view_model.minted.insert(chat.to_owned(), minted);
                self.view_changed(cx);
                Ok(serde_json::json!({}))
            }
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
            // Deleted for good: a chat not started is forgotten; a thread is closed on its worker,
            // its agent ended and the worker's record of it gone, and every device stops showing
            // it. The agent's own session stays wherever the agent keeps it.
            ("deleteChat", Some(chat)) => {
                self.view_model.held.remove(chat);
                self.view_model.refused.remove(chat);
                if self.view_model.minted.remove(chat).is_some() {
                    // A start on its way is not sent again, and its thread is closed once it
                    // starts.
                    let starting: Vec<IntentId> = self
                        .view_model
                        .starts
                        .iter()
                        .filter(|(_, c)| c.as_str() == chat)
                        .map(|(id, _)| *id)
                        .collect();
                    for id in &starting {
                        self.view_model.unanswered.remove(id);
                    }
                    if !starting.is_empty() {
                        self.view_model.doomed.insert(chat.to_owned());
                    }
                } else {
                    let (key, thread) = self
                        .chat_thread(chat, cx)
                        .ok_or_else(|| RpcError::Failed(format!("no thread {chat}")))?;
                    let hub = self.thread_hub(key, cx);
                    hub.update(cx, |hub, cx| hub.intent(thread, Intent::Discard, cx));
                    self.view_model.followed.remove(chat);
                    if self.view_model.aliases.remove(&thread).is_some() {
                        self.keep_aliases(cx);
                    }
                }
                self.view_changed(cx);
                Ok(serde_json::json!({}))
            }
            // A project removed takes its sessions with it, as the shell says it does.
            ("deleteSpace", _) => {
                if let Some(space) = param(params, "spaceId") {
                    self.view_discard_space(space, cx);
                }
                self.project_mutate(op, params, cx).unwrap_or_else(|| Ok(serde_json::json!({})))
            }
            _ => self
                .surface_mutate(op, params, cx)
                .or_else(|| self.project_mutate(op, params, cx))
                .or_else(|| self.config_mutate(op, params, cx))
                .or_else(|| self.sidebar_mutate(op, params, cx))
                .unwrap_or_else(|| {
                    Err(RpcError::Failed(format!("{op} is not something Slopty does to a thread")))
                }),
        }
    }

    /// Close for good every thread in the space `space`, and forget the chats made in it and not
    /// started.
    fn view_discard_space(&mut self, space: &str, cx: &mut Context<Self>) {
        let mut doomed = Vec::new();
        for (key, hub) in self.hubs() {
            let device = key.to_string();
            for row in hub.read(cx).threads().rows().rows.values() {
                if rows::folder(row).is_some_and(|path| self.view_space_id(&device, path) == space)
                {
                    doomed.push((key, row.id));
                }
            }
        }
        for (key, thread) in doomed {
            let hub = self.thread_hub(key, cx);
            hub.update(cx, |hub, cx| hub.intent(thread, Intent::Discard, cx));
        }
        let unstarted: Vec<String> = self
            .view_model
            .minted
            .iter()
            .filter(|(_, m)| self.view_space_id(&m.worker.to_string(), &m.cwd) == space)
            .map(|(chat, _)| chat.clone())
            .collect();
        for chat in unstarted {
            self.view_model.minted.remove(&chat);
        }
        self.view_changed(cx);
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

    /// `QueueMessage`: a message written while the agent works. Held for the turn's end, it
    /// waits in the worker's queue for the thread where the agent has one (`Cap::QUEUE`); else
    /// it goes to the agent at once, which queues or steers it as it does.
    fn view_queue_message(
        &mut self,
        params: &serde_json::Value,
        cx: &mut Context<Self>,
    ) -> Result<serde_json::Value, RpcError> {
        let chat = param(params, "chatId")
            .ok_or_else(|| RpcError::BadParams("chatId is required".to_owned()))?;
        let attachments: Vec<String> = params
            .get("attachments")
            .cloned()
            .and_then(|a| serde_json::from_value(a).ok())
            .unwrap_or_default();
        // Held for the turn's end where the agent can hold it, as the shell asks.
        let hold = params.get("holdForTurnEnd").and_then(serde_json::Value::as_bool) == Some(true);
        let text = param(params, "text").unwrap_or_default().to_owned();
        // A chat whose thread is starting keeps it until the thread is there.
        if self.view_model.minted.contains_key(chat) {
            let name = IntentId::new().to_string();
            let held = HeldSend { text, attachments, hold, name: name.clone(), queued: true };
            self.view_model.held.entry(chat.to_owned()).or_default().push(held);
            return Ok(serde_json::json!({ "id": name }));
        }
        let (key, thread) = self
            .chat_thread(chat, cx)
            .ok_or_else(|| RpcError::Failed(format!("no thread {chat}")))?;
        let held = HeldSend { text, attachments, hold, name: String::new(), queued: true };
        let id = self.view_send_to(key, thread, held, cx);
        self.view_sent_draft(key, thread, cx);
        Ok(serde_json::json!({ "id": id.to_string() }))
    }

    /// Send the message `held` to `thread` on `key`: queued for the turn's end where it asks
    /// and the agent can hold it, else as the agent takes a message now. A queue row goes by
    /// its own id, or the intent's when it has none yet, and so does the message once it went.
    fn view_send_to(
        &mut self,
        key: WorkerKey,
        thread: ThreadId,
        held: HeldSend,
        cx: &mut Context<Self>,
    ) -> IntentId {
        let hub = self.thread_hub(key, cx);
        let harness = self.thread_harness(key, thread, cx);
        let text = agent_prompt(&held.text, harness);
        let delivery = hub
            .read(cx)
            .threads()
            .mirror(thread)
            .and_then(slopty_client::threads::Mirror::state)
            .map_or(Delivery::Steer, |state| {
                if held.hold && state.meta.can(slopty_proto::thread::Cap::QUEUE) {
                    Delivery::Queue
                } else {
                    state.meta.delivery_now()
                }
            });
        let intent = Intent::Send { text: text.clone(), delivery, attachments: held.attachments };
        let id = hub.update(cx, |hub, cx| hub.intent(thread, intent, cx));
        self.keep_said(id, held.text, &text, Some((key, thread)), cx);
        let name = if held.name.is_empty() { id.to_string() } else { held.name };
        if held.queued {
            self.view_model.queue_rows.insert(id, name.clone());
        }
        self.view_model.named.insert(id, name);
        id
    }

    /// Keep `words`, as the person wrote the message `id` the agent is given as `agent`, where
    /// they differ (a chip spelled out): the chat shows them at once, and every device once
    /// `to`'s thread holds them too ([`Intent::Said`]).
    fn keep_said(
        &mut self,
        id: IntentId,
        words: String,
        agent: &str,
        to: Option<(WorkerKey, ThreadId)>,
        cx: &mut Context<Self>,
    ) {
        if words == agent {
            return;
        }
        if let Some((key, thread)) = to {
            let said = Intent::Said { message: id, words: words.clone() };
            let hub = self.thread_hub(key, cx);
            let _said = hub.update(cx, |hub, cx| hub.intent(thread, said, cx));
        }
        self.view_model.said.insert(id, words);
    }

    /// `QueueCommand`: a message to a chat's thread, or the start of a minted chat's.
    fn view_command(
        &mut self,
        params: &serde_json::Value,
        cx: &mut Context<Self>,
    ) -> Result<serde_json::Value, RpcError> {
        let chat = param(params, "chatId")
            .ok_or_else(|| RpcError::BadParams("chatId is required".to_owned()))?
            .to_owned();
        let command: SessionCommandPayload = params
            .get("command")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| RpcError::BadParams(e.to_string()))?
            .ok_or_else(|| RpcError::BadParams("command is required".to_owned()))?;
        if let Some(minted) = self.view_model.minted.get(&chat).cloned() {
            let SessionCommandPayload::Run { request, message_id } = command else {
                return Err(RpcError::Failed("the thread has not started yet".to_owned()));
            };
            // A chat already starting is never started twice: the message waits for its thread.
            if self.view_model.starts.values().any(|c| *c == chat) {
                let held = HeldSend {
                    text: request.prompt,
                    attachments: request.attachments,
                    hold: false,
                    name: message_id,
                    queued: false,
                };
                self.view_model.held.entry(chat).or_default().push(held);
                return Ok(serde_json::json!({ "commandId": IntentId::new().to_string() }));
            }
            self.view_model.refused.remove(&chat);
            let id = IntentId::new();
            // A side chat's first message asks its chat's thread aside, and goes to the aside.
            if let Some(parent) = minted.parent.as_deref() {
                let (key, thread) = self
                    .chat_thread(parent, cx)
                    .ok_or_else(|| RpcError::Failed("Its chat has not started yet".to_owned()))?;
                let harness = self.thread_harness(key, thread, cx);
                let first = FirstWords {
                    text: agent_prompt(&request.prompt, harness),
                    attachments: request.attachments,
                    name: message_id,
                    said: request.prompt,
                };
                self.view_model.first_words.insert(id, first);
                self.view_model.starts.insert(id, chat);
                let msg =
                    ClientMsg::Thread(ThreadRequest::Intent { id, thread, intent: Intent::Aside });
                self.view_send_kept(key, id, msg);
                return Ok(serde_json::json!({ "commandId": id.to_string() }));
            }
            if request.worktree.is_some() {
                self.project_setup_for(&chat, minted.worker, &minted.cwd);
            }
            let worktree = request.worktree.as_ref().map(|spec| NewWorktree {
                name: worktree_name(&request.prompt, id),
                base: Some(spec.base.clone()).filter(|base| base != "HEAD"),
                merge_base: None,
                pull: None,
                setup: true,
            });
            let agent = agent_of(request.harness);
            let prompt = agent_prompt(&request.prompt, harness_of(&agent));
            if prompt != request.prompt {
                self.view_model.said.insert(id, request.prompt.clone());
            }
            self.keep_options(&chat, &request.model_options);
            let scratch = slopty_view_proto::projectless::is_scratch_cwd(&minted.cwd)
                .then(|| minted.cwd.clone());
            let start = Start {
                agent,
                cwd: minted.cwd,
                drive: None,
                prompt: Some(prompt),
                model: request.model,
                mode: Self::start_mode(&request.model_options),
                effort: request
                    .reasoning
                    .and_then(|level| serde_json::to_value(level).ok())
                    .and_then(|level| level.as_str().map(str::to_owned)),
                attachments: request.attachments,
                args: Vec::new(),
                options: Self::start_options(&request.model_options),
                worktree,
            };
            self.view_model.starts.insert(id, chat);
            self.view_model.named.insert(id, message_id);
            let msg = ClientMsg::Thread(ThreadRequest::Start { id, start: Box::new(start) });
            match scratch {
                // A chat with no project starts in a folder of its own, made first.
                Some(cwd) => {
                    let worker = minted.worker;
                    Self::make_scratch(
                        worker,
                        &cwd,
                        move |view, _| {
                            // Deleted while its folder was made: never started.
                            if view.view_model.starts.contains_key(&id) {
                                view.view_send_kept(worker, id, msg);
                            }
                        },
                        cx,
                    );
                }
                None => self.view_send_kept(minted.worker, id, msg),
            }
            return Ok(serde_json::json!({ "commandId": id.to_string() }));
        }
        let (key, thread) = self
            .chat_thread(&chat, cx)
            .ok_or_else(|| RpcError::Failed(format!("no thread {chat}")))?;
        let hub = self.thread_hub(key, cx);
        let delivery_now = |hub: &crate::conversation::thread::hub::ThreadHub| {
            hub.threads()
                .mirror(thread)
                .and_then(slopty_client::threads::Mirror::state)
                .map_or(Delivery::Steer, |state| state.meta.delivery_now())
        };
        let harness = self.thread_harness(key, thread, cx);
        // A message to a thread whose agent exited takes its session up again, with the message
        // as its first words.
        if let SessionCommandPayload::Run { request, message_id } = &command {
            let resume = hub.read(cx).resume_start(thread).map_err(RpcError::Failed)?;
            if let Some(mut start) = resume {
                if hub.read(cx).resuming(thread) {
                    return Err(RpcError::Failed("It is starting again: one moment".to_owned()));
                }
                let agent = agent_prompt(&request.prompt, harness);
                start.prompt = Some(agent.clone());
                start.attachments.clone_from(&request.attachments);
                if request.model.is_some() {
                    start.model.clone_from(&request.model);
                }
                let id = hub.update(cx, |hub, cx| hub.resume(thread, start, cx));
                self.view_model.named.insert(id, message_id.clone());
                self.keep_said(id, request.prompt.clone(), &agent, Some((key, thread)), cx);
                return Ok(serde_json::json!({ "commandId": id.to_string() }));
            }
        }
        let (intent, message_id, said) = match command {
            SessionCommandPayload::Run { request, message_id } => (
                Intent::Send {
                    text: agent_prompt(&request.prompt, harness),
                    delivery: delivery_now(hub.read(cx)),
                    attachments: request.attachments,
                },
                Some(message_id),
                Some(request.prompt),
            ),
            SessionCommandPayload::Steer { prompt, message_id } => (
                Intent::Send {
                    text: agent_prompt(&prompt, harness),
                    delivery: Delivery::Steer,
                    attachments: Vec::new(),
                },
                message_id,
                Some(prompt),
            ),
            SessionCommandPayload::Interrupt {} => (Intent::Interrupt, None, None),
            SessionCommandPayload::RespondInput { request_id, answers } => {
                let state = hub
                    .read(cx)
                    .threads()
                    .mirror(thread)
                    .and_then(slopty_client::threads::Mirror::state);
                let request = state
                    .and_then(|state| state.requests.iter().find(|r| r.id.0 == request_id))
                    .filter(|request| request.is_open());
                // slopty: a request of a chat started under this one goes back to that chat.
                let Some(request) = request else {
                    return self
                        .view_child_answer(thread, &request_id, &answers, cx)
                        .unwrap_or_else(|| {
                            Err(RpcError::Failed("It no longer waits on an answer".to_owned()))
                        });
                };
                let (choice, message) =
                    transcript::answer(request, &answers).map_err(RpcError::Failed)?;
                (Intent::Answer { ask: request.id.clone(), choice, message }, None, None)
            }
        };
        let agent = match &intent {
            Intent::Send { text, .. } => Some(text.clone()),
            _ => None,
        };
        let id = hub.update(cx, |hub, cx| hub.intent(thread, intent, cx));
        if let Some(message_id) = message_id {
            self.view_model.named.insert(id, message_id);
        }
        if let (Some(said), Some(agent)) = (said, agent) {
            self.keep_said(id, said, &agent, Some((key, thread)), cx);
            self.view_sent_draft(key, thread, cx);
        }
        Ok(serde_json::json!({ "commandId": id.to_string() }))
    }

    /// A message went to the thread `thread` on `key`: what the person kept writing to it on
    /// any device is sent, so it is cleared there too.
    fn view_sent_draft(&mut self, key: WorkerKey, thread: ThreadId, cx: &mut Context<Self>) {
        let kept = self.held_hub(key).is_some_and(|hub| {
            let hub = hub.read(cx);
            let row = hub.threads().rows().rows.get(&thread);
            row.and_then(|row| row.draft.as_ref()).is_some_and(|draft| !draft.text.is_empty())
        });
        if kept {
            let hub = self.thread_hub(key, cx);
            let _cleared = hub.update(cx, |hub, cx| {
                hub.intent(thread, Intent::Draft { text: String::new() }, cx)
            });
        }
    }

    /// The harness the shell knows `thread`'s agent by.
    fn thread_harness(&self, key: WorkerKey, thread: ThreadId, cx: &Context<Self>) -> HarnessId {
        self.held_hub(key)
            .and_then(|hub| {
                hub.read(cx).threads().rows().rows.get(&thread).map(|r| harness_of(&r.agent))
            })
            .unwrap_or(HarnessId::CLAUDE_CODE)
    }

    /// Send `msg` for intent `id` to `key`, kept until the worker answers it.
    pub(super) fn view_send_kept(&mut self, key: WorkerKey, id: IntentId, msg: ClientMsg) {
        self.send(key, msg.clone());
        self.view_model.unanswered.insert(id, (key, msg));
    }

    /// Send again what the view model sent `key` and `key` has not answered; for the chat
    /// `chat` alone when one is named. How many went.
    pub(in crate::workspace) fn view_resend(&self, key: WorkerKey, chat: Option<&str>) -> usize {
        let kept = self.view_model.unanswered.iter().filter(|(id, (at, _))| {
            *at == key
                && chat
                    .is_none_or(|chat| self.view_model.starts.get(*id).is_some_and(|c| c == chat))
        });
        let mut sent = 0_usize;
        for (_, (_, msg)) in kept {
            self.send(key, msg.clone());
            sent = sent.saturating_add(1);
        }
        sent
    }

    /// A start sent for a minted chat was answered: its thread goes by the chat's id from now
    /// on, and the messages written meanwhile go to it; a start turned down is kept to say why
    /// and to be tried again. Whether the answer was one of these.
    pub(super) fn view_start_done(
        &mut self,
        key: WorkerKey,
        done: &IntentDone,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(chat) = self.view_model.starts.remove(&done.id) else { return false };
        let sent = self.view_model.unanswered.remove(&done.id);
        match &done.outcome {
            Outcome::Started { thread } => {
                // Deleted while it started: closed at once.
                if self.view_model.doomed.remove(&chat) {
                    self.view_model.first_words.remove(&done.id);
                    let hub = self.thread_hub(key, cx);
                    hub.update(cx, |hub, cx| hub.intent(*thread, Intent::Discard, cx));
                    self.view_changed(cx);
                    return true;
                }
                self.view_model.minted.remove(&chat);
                self.view_model.refused.remove(&chat);
                if let Some(words) = self.view_model.said.get(&done.id).cloned() {
                    let said = Intent::Said { message: done.id, words };
                    let hub = self.thread_hub(key, cx);
                    let _said = hub.update(cx, |hub, cx| hub.intent(*thread, said, cx));
                }
                self.view_model.aliases.insert(*thread, chat.clone());
                self.keep_aliases(cx);
                self.surface_rekey_archived(&chat, thread.to_string(), cx);
                if let Some(first) = self.view_model.first_words.remove(&done.id) {
                    let hub = self.thread_hub(key, cx);
                    let (text, attachments) = (first.text, first.attachments);
                    let send =
                        Intent::Send { text: text.clone(), delivery: Delivery::Steer, attachments };
                    let sent = hub.update(cx, |hub, cx| hub.intent(*thread, send, cx));
                    self.view_model.named.insert(sent, first.name);
                    self.keep_said(sent, first.said, &text, Some((key, *thread)), cx);
                }
                for held in self.view_model.held.remove(&chat).unwrap_or_default() {
                    self.view_send_to(key, *thread, held, cx);
                }
                if self.view_model.feed.as_ref().is_some_and(|feed| feed.followed().contains(&chat))
                {
                    self.view_model.followed.insert(chat, (key, *thread));
                    let hub = self.thread_hub(key, cx);
                    hub.update(cx, |hub, cx| hub.open(*thread, cx));
                }
            }
            outcome => {
                tracing::warn!(?outcome, %chat, "a thread the interface asked for did not start");
                let first_words = self.view_model.first_words.remove(&done.id);
                if self.view_model.doomed.remove(&chat) {
                    self.view_changed(cx);
                    return true;
                }
                if let Some((worker, msg)) = sent {
                    let words = match outcome {
                        Outcome::Refused { reason } => format!("It did not start: {reason}"),
                        Outcome::Unsupported { cap } => {
                            format!("It did not start: its agent cannot {}", cap.0)
                        }
                        Outcome::SetupFailed { code, .. } => match code {
                            Some(code) => {
                                format!("It did not start: the worktree's setup failed ({code})")
                            }
                            None => "It did not start: the worktree's setup was stopped".to_owned(),
                        },
                        _ => "It did not start".to_owned(),
                    };
                    let refused = RefusedStart {
                        worker,
                        msg,
                        first_words,
                        named: self.view_model.named.remove(&done.id).map(|name| {
                            (name, self.view_model.said.remove(&done.id).unwrap_or_default())
                        }),
                        words,
                        at: Utc::now().timestamp_millis(),
                    };
                    self.view_model.refused.insert(chat, refused);
                }
            }
        }
        self.view_changed(cx);
        true
    }

    /// Try again the start of the chat `chat` its worker turned down, under a new id. Whether
    /// there was one.
    pub(super) fn view_retry_start(&mut self, chat: &str, cx: &Context<Self>) -> bool {
        let Some(refused) = self.view_model.refused.remove(chat) else { return false };
        let id = IntentId::new();
        let msg = match refused.msg {
            ClientMsg::Thread(ThreadRequest::Start { start, .. }) => {
                ClientMsg::Thread(ThreadRequest::Start { id, start })
            }
            ClientMsg::Thread(ThreadRequest::Intent { thread, intent, .. }) => {
                ClientMsg::Thread(ThreadRequest::Intent { id, thread, intent })
            }
            other => other,
        };
        if let Some(first_words) = refused.first_words {
            self.view_model.first_words.insert(id, first_words);
        }
        if let Some((name, said)) = refused.named {
            self.view_model.named.insert(id, name);
            self.view_model.said.insert(id, said);
        }
        self.view_model.starts.insert(id, chat.to_owned());
        self.view_send_kept(refused.worker, id, msg);
        self.view_changed(cx);
        true
    }

    /// `RetryTurn`: the chat's failed last turn tried again, with the words that started it, as
    /// a new message. Refused when its last turn did not fail.
    fn view_retry_turn(
        &mut self,
        params: &serde_json::Value,
        cx: &mut Context<Self>,
    ) -> Result<serde_json::Value, RpcError> {
        let chat = param(params, "chatId")
            .ok_or_else(|| RpcError::BadParams("chatId is required".to_owned()))?;
        let (key, thread) = self
            .chat_thread(chat, cx)
            .ok_or_else(|| RpcError::Failed(format!("no thread {chat}")))?;
        let hub = self.thread_hub(key, cx);
        let (agent, said, attachments, delivery) = {
            let held = hub.read(cx);
            let state = held
                .threads()
                .mirror(thread)
                .and_then(slopty_client::threads::Mirror::state)
                .ok_or_else(|| RpcError::Failed("The chat has not come yet".to_owned()))?;
            let failed = state
                .turns
                .last()
                .filter(|turn| matches!(turn.state, TurnState::Failed { .. }))
                .ok_or_else(|| RpcError::Failed("Its last turn did not fail".to_owned()))?;
            let message = failed
                .input
                .as_ref()
                .and_then(|input| state.items.iter().find(|item| item.id == *input))
                .and_then(|item| match &item.body {
                    slopty_proto::thread::ItemBody::User(message) => Some(message),
                    _ => None,
                })
                .ok_or_else(|| RpcError::Failed("Its turn began with no message".to_owned()))?;
            let agent = message
                .text
                .full
                .as_ref()
                .and_then(|full| match held.whole(full)?.as_ref() {
                    slopty_proto::thread::wire::Expanded::Text(text) => Some(text.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| message.text.text.clone());
            if message.text.full.is_some() && agent == message.text.text {
                return Err(RpcError::Failed("Its words are still on their way".to_owned()));
            }
            let said = message.intent.and_then(|intent| {
                self.view_model.said.get(&intent).or_else(|| state.said.get(&intent)).cloned()
            });
            (agent, said, Vec::new(), state.meta.delivery_now())
        };
        let send = Intent::Send { text: agent.clone(), delivery, attachments };
        let id = hub.update(cx, |hub, cx| hub.intent(thread, send, cx));
        if let Some(said) = said {
            self.keep_said(id, said, &agent, Some((key, thread)), cx);
        }
        Ok(serde_json::json!({ "commandId": id.to_string() }))
    }

    /// `SaveDraft`: what the person is writing to a chat and has not sent, kept on its machine
    /// for every device ([`Intent::Draft`]); empty words clear it. A chat not started yet keeps
    /// it here alone.
    fn view_save_draft(
        &mut self,
        params: &serde_json::Value,
        cx: &mut Context<Self>,
    ) -> Result<serde_json::Value, RpcError> {
        let chat = param(params, "chatId")
            .ok_or_else(|| RpcError::BadParams("chatId is required".to_owned()))?;
        let text = params.get("text").and_then(serde_json::Value::as_str).unwrap_or_default();
        if text.len() > slopty_proto::thread::wire::Draft::MAX_BYTES {
            return Err(RpcError::Failed("The draft is too long to keep on its machine".into()));
        }
        let Some((key, thread)) = self.chat_thread(chat, cx) else {
            return Ok(serde_json::json!({}));
        };
        let kept = self.held_hub(key).and_then(|hub| {
            let hub = hub.read(cx);
            let row = hub.threads().rows().rows.get(&thread)?;
            Some(row.draft.as_ref().map_or_else(String::new, |draft| draft.text.clone()))
        });
        if kept.as_deref() != Some(text) {
            let hub = self.thread_hub(key, cx);
            let draft = Intent::Draft { text: text.to_owned() };
            let _kept = hub.update(cx, |hub, cx| hub.intent(thread, draft, cx));
        }
        Ok(serde_json::json!({}))
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
                        "chatId": self.chat_id(row.id),
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

    /// `key`'s answer to any intent: a Stop waiting for it hears how it went, and what was kept
    /// to be sent again is let go.
    pub(super) fn view_intent_done(&mut self, done: &IntentDone) {
        if !self.view_model.starts.contains_key(&done.id) {
            self.view_model.unanswered.remove(&done.id);
        }
        let Some(reply) = self.view_model.interrupts.remove(&done.id) else { return };
        let answer = match &done.outcome {
            Outcome::Refused { reason } => Err(RpcError::Failed(reason.clone())),
            Outcome::Unsupported { cap } => {
                Err(RpcError::Failed(format!("Its agent cannot {}", cap.0)))
            }
            // slopty: a branched thread is said as the chat the shell knows it by.
            Outcome::Started { thread } => Ok(RpcReply::Value(serde_json::json!({
                "commandId": done.id.to_string(),
                "chatId": self.chat_id(*thread),
            }))),
            _ => Ok(RpcReply::Value(serde_json::json!({ "commandId": done.id.to_string() }))),
        };
        let _gone = reply.send(answer);
    }

    /// A Stop: answered once the worker said how it went, so a Stop turned down says why and
    /// the next one can be asked; one the worker leaves unanswered for [`INTERRUPT_WITHIN`]
    /// failed, which lets the shell ask again too.
    fn view_interrupt(
        &mut self,
        params: &serde_json::Value,
        cx: &mut Context<Self>,
    ) -> Task<Result<RpcReply, RpcError>> {
        let Some(chat) = param(params, "chatId") else {
            return Task::ready(Err(RpcError::BadParams("chatId is required".to_owned())));
        };
        let Some((key, thread)) = self.chat_thread(chat, cx) else {
            return Task::ready(Err(RpcError::Failed(format!("no thread {chat}"))));
        };
        let hub = self.thread_hub(key, cx);
        let id = hub.update(cx, |hub, cx| hub.intent(thread, Intent::Interrupt, cx));
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.view_model.interrupts.insert(id, reply);
        cx.spawn(async move |this, cx| {
            use futures::FutureExt as _;
            let timer = cx.background_executor().timer(INTERRUPT_WITHIN);
            let unanswered = || Err(RpcError::Failed("The agent did not stop in time".to_owned()));
            let answer = futures::select_biased! {
                answer = answer.fuse() => answer.unwrap_or_else(|_| unanswered()),
                () = timer.fuse() => unanswered(),
            };
            let _gone = this.update(cx, |view, _| view.view_model.interrupts.remove(&id));
            answer
        })
    }
}

#[cfg(test)]
mod tests {
    use gpui::AppContext as _;
    use slopty_core::WallMs;
    use slopty_proto::ClientMsg;
    use slopty_proto::thread::wire::{
        Intent, IntentDone, Outcome, TableFrame, ThreadFrame, ThreadRequest,
    };
    use slopty_proto::thread::{
        AgentId, Cursor, Delivery, IntentId, Liveness, Pending, PendingState, ThreadState,
    };
    use slopty_view_doc::SessionCommandPayload;
    use slopty_view_proto::HarnessId;
    use slopty_view_rpc::methods;
    use tokio::sync::mpsc::Receiver;

    use super::agent_of;
    use crate::conversation::thread::fixtures;
    use crate::view_model::transcript::shown_call;
    use crate::workspace::WorkspaceView;
    use crate::workspace::rig::{KEY, link, mac};

    /// A chat starts the agent its harness names: Claude Code, Codex and pi by their own ids,
    /// any other harness an ACP agent by its registry's name, and Claude Code when none is named.
    #[test]
    fn a_harness_starts_its_own_agent() {
        assert!(agent_of(None).is(AgentId::CLAUDE_CODE));
        assert!(agent_of(Some(HarnessId::CLAUDE_CODE)).is(AgentId::CLAUDE_CODE));
        assert!(agent_of(Some(HarnessId::CODEX)).is(AgentId::CODEX));
        assert!(agent_of(Some(HarnessId::PI)).is(AgentId::PI));
        assert_eq!(agent_of(Some(HarnessId::OPENCODE)).acp_name(), Some("opencode"));
        let gemini = HarnessId::named("gemini");
        assert_eq!(agent_of(Some(gemini)).acp_name(), Some("gemini"));
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

    /// A message the shell sent at once is its own echo until it goes, never a queue row; one
    /// the shell queued is.
    #[gpui::test]
    fn a_sent_message_is_no_queue_row(cx: &mut gpui::TestAppContext) {
        let (state, first, second) = queued();
        let (workspace, cx, _rx) = holding(&state, cx);
        let rows = workspace.update(cx, |v, cx| {
            v.view_model.named.insert(first, "m-1".to_owned());
            v.view_model.named.insert(second, second.to_string());
            v.view_model.queue_rows.insert(second, second.to_string());
            v.view_queue(&state.meta.id.to_string(), cx)
        });
        let ids: Vec<_> = rows.iter().map(|row| row.id.clone()).collect();
        assert_eq!(ids, [second.to_string()]);
    }

    /// A message the worker holds says why on its row; one being typed into the agent is not
    /// taken back, and says so.
    #[gpui::test]
    fn a_held_message_says_why(cx: &mut gpui::TestAppContext) {
        let (mut state, first, second) = queued();
        state.pending[0].state = PendingState::Sending;
        state.pending[1].state = PendingState::Held { reason: "Your draft is in the way".into() };
        let (workspace, cx, _rx) = holding(&state, cx);
        let chat = state.meta.id.to_string();
        let rows = workspace.update(cx, |v, cx| v.view_queue(&chat, cx));
        let gates: Vec<_> = rows.into_iter().map(|row| row.delivery_gate).collect();
        let held = |reason: &str| {
            Some(slopty_view_doc::QueueDeliveryGate::Held { reason: reason.to_owned() })
        };
        assert_eq!(gates, [held("Going to the agent now"), held("Your draft is in the way")]);
        let params = serde_json::json!({ "chatId": chat, "id": first.to_string() });
        let removed =
            workspace.update(cx, |v, cx| v.queue_call(methods::REMOVE_QUEUED_MESSAGE, &params, cx));
        assert!(removed.is_err(), "being typed: not taken back");
        let params = serde_json::json!({ "chatId": chat, "id": second.to_string() });
        let removed =
            workspace.update(cx, |v, cx| v.queue_call(methods::REMOVE_QUEUED_MESSAGE, &params, cx));
        assert_eq!(reply(removed)["removed"], true);
    }

    /// "Send now" stops the turn, then sends the message next.
    #[gpui::test]
    fn send_now_stops_the_turn_first(cx: &mut gpui::TestAppContext) {
        let (state, first, _) = queued();
        let (workspace, cx, mut rx) = holding(&state, cx);
        let params =
            serde_json::json!({ "chatId": state.meta.id.to_string(), "id": first.to_string() });
        let sent = workspace
            .update(cx, |v, cx| v.queue_call(methods::SEND_QUEUED_MESSAGE_NOW, &params, cx));
        assert_eq!(reply(sent)["sent"], true);
        cx.run_until_parked();
        let intents: Vec<Intent> = sent_threads(&mut rx)
            .into_iter()
            .filter_map(|request| match request {
                ThreadRequest::Intent { intent, .. } => Some(intent),
                _ => None,
            })
            .collect();
        assert_eq!(intents, [Intent::Interrupt, Intent::Promote { pending: first }]);
    }

    /// A Stop the worker turns down says why, so the shell lets the next one be asked.
    #[gpui::test]
    fn a_refused_stop_says_why(cx: &mut gpui::TestAppContext) {
        use futures::FutureExt as _;
        let state = fixtures::thread("tools");
        let (workspace, cx, mut rx) = holding(&state, cx);
        let params = serde_json::json!({
            "chatId": state.meta.id.to_string(), "command": { "kind": "interrupt" },
        });
        let answer = workspace.update(cx, |v, cx| v.view_interrupt(&params, cx));
        cx.run_until_parked();
        let id = sent_threads(&mut rx)
            .into_iter()
            .find_map(|request| match request {
                ThreadRequest::Intent { id, intent: Intent::Interrupt, .. } => Some(id),
                _ => None,
            })
            .expect("the stop");
        let done = IntentDone { id, outcome: Outcome::Refused { reason: "Nothing runs".into() } };
        workspace.update(cx, |v, cx| v.thread_done(KEY, &done, cx));
        cx.run_until_parked();
        let answer = answer.now_or_never().expect("answered");
        assert!(
            matches!(answer, Err(slopty_view_rpc::RpcError::Failed(why)) if why == "Nothing runs")
        );
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

    /// A chat made in the interface, with the worker `rx` goes to, and the start its first
    /// message sent.
    fn starting(
        workspace: &gpui::Entity<WorkspaceView>,
        cx: &mut gpui::VisualTestContext,
        rx: &mut Receiver<ClientMsg>,
    ) -> IntentId {
        let create = serde_json::json!({
            "op": "createChat", "chatId": "m", "deviceId": KEY.to_string(), "cwd": "/w",
            "branch": "fix", "config": { "harness": "claude-code", "model": "opus", "sandbox": "workspace-write" },
        });
        let made = workspace.update(cx, |v, cx| v.view_call(methods::MUTATE, &create, cx));
        assert!(made.is_ok(), "{made:?}");
        let request: slopty_view_proto::RunRequest = serde_json::from_value(serde_json::json!({
            "prompt": "go", "model": null, "reasoning": null, "cwd": "/w",
            "sandbox": "workspace-write", "resume": null,
        }))
        .expect("a run");
        let command = SessionCommandPayload::Run { request, message_id: "m-1".to_owned() };
        let run = serde_json::json!({ "chatId": "m", "command": command });
        let ran = workspace.update(cx, |v, cx| v.view_call(methods::QUEUE_COMMAND, &run, cx));
        assert!(ran.is_ok(), "{ran:?}");
        cx.run_until_parked();
        sent_threads(rx)
            .into_iter()
            .find_map(|request| match request {
                ThreadRequest::Start { id, .. } => Some(id),
                _ => None,
            })
            .expect("a start")
    }

    /// The model, effort and branch picked on a new chat are its own from the first.
    #[gpui::test]
    fn a_new_chat_keeps_what_was_picked(cx: &mut gpui::TestAppContext) {
        let (workspace, cx, mut rx) = holding(&fixtures::thread("tools"), cx);
        starting(&workspace, cx, &mut rx);
        let (model, branch) = workspace.read_with(cx, |v, _| {
            let model = v.picked_config("m").and_then(|config| config.model);
            (model, v.view_model.minted.get("m").and_then(|m| m.branch.clone()))
        });
        assert_eq!(model.as_deref(), Some("opus"));
        assert_eq!(branch.as_deref(), Some("fix"));
    }

    /// A message written while the chat starts waits for its thread, and the chat is never
    /// started twice.
    #[gpui::test]
    fn a_message_while_a_chat_starts_waits_for_its_thread(cx: &mut gpui::TestAppContext) {
        let (workspace, cx, mut rx) = holding(&fixtures::thread("tools"), cx);
        let start = starting(&workspace, cx, &mut rx);
        let more = serde_json::json!({ "chatId": "m", "text": "and then", "holdForTurnEnd": true });
        let queued = workspace.update(cx, |v, cx| v.view_call(methods::QUEUE_MESSAGE, &more, cx));
        let row = queued.expect("held")["id"].as_str().map(str::to_owned).expect("its row");
        cx.run_until_parked();
        assert!(sent_threads(&mut rx).is_empty(), "nothing goes before the thread");
        let thread = slopty_proto::thread::ThreadId::new();
        let done = IntentDone { id: start, outcome: Outcome::Started { thread } };
        workspace.update(cx, |v, cx| v.thread_done(KEY, &done, cx));
        cx.run_until_parked();
        let sends: Vec<IntentId> = sent_threads(&mut rx)
            .into_iter()
            .filter_map(|request| match request {
                ThreadRequest::Intent { id, thread: t, intent: Intent::Send { .. } }
                    if t == thread =>
                {
                    Some(id)
                }
                ThreadRequest::Start { .. } => panic!("started twice"),
                _ => None,
            })
            .collect();
        assert_eq!(sends.len(), 1);
        let named = workspace.read_with(cx, |v, _| v.view_model.queue_rows.get(&sends[0]).cloned());
        assert_eq!(named, Some(row), "its row keeps its id");
    }

    /// A start the worker turns down says why, and goes again under a new id on Retry.
    #[gpui::test]
    fn a_refused_start_says_why_and_goes_again(cx: &mut gpui::TestAppContext) {
        let (workspace, cx, mut rx) = holding(&fixtures::thread("tools"), cx);
        let start = starting(&workspace, cx, &mut rx);
        let done =
            IntentDone { id: start, outcome: Outcome::Refused { reason: "no claude".into() } };
        workspace.update(cx, |v, cx| v.thread_done(KEY, &done, cx));
        let words =
            workspace.read_with(cx, |v, _| v.view_model.refused.get("m").map(|r| r.words.clone()));
        assert_eq!(words.as_deref(), Some("It did not start: no claude"));
        assert!(workspace.update(cx, |v, cx| v.view_retry_start("m", cx)));
        cx.run_until_parked();
        let again = sent_threads(&mut rx).into_iter().find_map(|request| match request {
            ThreadRequest::Start { id, .. } => Some(id),
            _ => None,
        });
        assert!(again.is_some_and(|id| id != start), "again, under a new id");
    }

    /// A chat deleted while it starts is closed once its thread is there, not brought back.
    #[gpui::test]
    fn a_chat_deleted_while_it_starts_stays_gone(cx: &mut gpui::TestAppContext) {
        let (workspace, cx, mut rx) = holding(&fixtures::thread("tools"), cx);
        let start = starting(&workspace, cx, &mut rx);
        let delete = serde_json::json!({ "op": "deleteChat", "chatId": "m" });
        let deleted = workspace.update(cx, |v, cx| v.view_call(methods::MUTATE, &delete, cx));
        assert!(deleted.is_ok(), "{deleted:?}");
        let thread = slopty_proto::thread::ThreadId::new();
        let done = IntentDone { id: start, outcome: Outcome::Started { thread } };
        workspace.update(cx, |v, cx| v.thread_done(KEY, &done, cx));
        cx.run_until_parked();
        let closed = sent_threads(&mut rx).into_iter().any(|request| {
            matches!(request, ThreadRequest::Intent { thread: t, intent: Intent::Discard, .. }
                if t == thread)
        });
        assert!(closed, "its thread is closed");
        let aliased = workspace.read_with(cx, |v, _| v.view_model.aliases.contains_key(&thread));
        assert!(!aliased, "and never shown as the chat");
    }

    /// The ids chats started here go by read back as they were kept.
    #[test]
    fn the_aliases_read_back() {
        let thread = slopty_proto::thread::ThreadId::new();
        let kept = std::collections::HashMap::from([(thread, "m".to_owned())]);
        let text = super::aliases_text(&kept);
        assert_eq!(super::aliases_of(&text), kept);
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

    /// Deleting a chat whose thread started closes the thread for good on its worker: its
    /// agent ends and every device stops showing it, as the shell's dialog says.
    #[gpui::test]
    fn deleting_a_started_chat_closes_its_thread(cx: &mut gpui::TestAppContext) {
        let state = fixtures::thread("tools");
        let thread = state.meta.id;
        let (workspace, cx, mut rx) = holding(&state, cx);
        let params = serde_json::json!({ "op": "deleteChat", "chatId": thread.to_string() });
        let answer = workspace.update(cx, |v, cx| v.view_call(methods::MUTATE, &params, cx));
        assert!(answer.is_ok(), "{answer:?}");
        cx.run_until_parked();
        let closed = sent_threads(&mut rx).into_iter().any(|request| {
            matches!(request, ThreadRequest::Intent { thread: t, intent: Intent::Discard, .. }
                if t == thread)
        });
        assert!(closed, "the thread is closed on its worker");
    }

    /// A message to the exited agent of thread `state`, as the composer sends one, and what
    /// its worker was sent for it.
    fn run_exited(state: &ThreadState, cx: &mut gpui::TestAppContext) -> Vec<ThreadRequest> {
        let (workspace, cx, mut rx) = holding(state, cx);
        let request: slopty_view_proto::RunRequest = serde_json::from_value(serde_json::json!({
            "prompt": "go on",
            "model": null,
            "reasoning": null,
            "cwd": "/w",
            "sandbox": "workspace-write",
            "resume": null,
        }))
        .expect("a run");
        let command = SessionCommandPayload::Run { request, message_id: "m-1".to_owned() };
        let params = serde_json::json!({ "chatId": state.meta.id.to_string(), "command": command });
        let answer = workspace.update(cx, |v, cx| v.view_call(methods::QUEUE_COMMAND, &params, cx));
        assert!(answer.is_ok(), "{answer:?}");
        cx.run_until_parked();
        sent_threads(&mut rx)
    }

    /// A message to a chat whose Claude Code exited goes to it as a message, which its worker
    /// takes the session up again with: no start of its own.
    #[gpui::test]
    fn a_message_to_an_exited_claude_goes_as_a_message(cx: &mut gpui::TestAppContext) {
        let mut state = fixtures::thread("tools");
        state.status.liveness = Liveness::Exited { resumable: true };
        let sent = run_exited(&state, cx);
        let said = sent.iter().any(|r| {
            matches!(r, ThreadRequest::Intent { intent: Intent::Send { text, .. }, .. }
                if text == "go on")
        });
        assert!(said, "the message goes to Claude Code: {sent:?}");
        assert!(!sent.iter().any(|r| matches!(r, ThreadRequest::Start { .. })), "no start");
    }

    /// A shared Codex that exited is taken up again by a start of its own, with the message as
    /// its first words.
    #[gpui::test]
    fn a_message_to_an_exited_codex_takes_its_session_up_again(cx: &mut gpui::TestAppContext) {
        use slopty_proto::thread::Drive;
        let mut state = fixtures::thread("tools");
        state.meta.agent = AgentId::named(AgentId::CODEX);
        state.meta.drive = Drive::named(Drive::SHARED);
        state.status.liveness = Liveness::Exited { resumable: true };
        let native = state.meta.native.clone();
        let start = run_exited(&state, cx)
            .into_iter()
            .find_map(|request| match request {
                ThreadRequest::Start { start, .. } => Some(start),
                _ => None,
            })
            .expect("a start that takes the session up again");
        assert_eq!(start.args, ["resume".to_owned(), native]);
        assert_eq!(start.prompt.as_deref(), Some("go on"));
    }

    /// The thread `state` holding two queued messages, the first with a file.
    fn queued() -> (ThreadState, IntentId, IntentId) {
        let mut state = fixtures::thread("tools");
        let (first, second) = (IntentId::new(), IntentId::new());
        let waiting = |intent, text: &str, attachments: Vec<String>| Pending {
            intent,
            text: text.to_owned(),
            attachments,
            delivery: Delivery::Queue,
            state: PendingState::Waiting,
        };
        state.pending = vec![
            waiting(first, "then the docs", vec!["/drop/shot.png".to_owned()]),
            waiting(second, "and the tests", Vec::new()),
        ];
        (state, first, second)
    }

    fn reply(
        answer: Result<slopty_view_rpc::RpcReply, slopty_view_rpc::RpcError>,
    ) -> serde_json::Value {
        match answer {
            Ok(slopty_view_rpc::RpcReply::Value(value)) => value,
            Ok(slopty_view_rpc::RpcReply::Stream(_)) => panic!("a stream, not a value"),
            Err(e) => panic!("refused: {e:?}"),
        }
    }

    /// A queued message edited in the composer is leased with what it says, and committed with
    /// the files the composer holds now; a commit over a message changed meanwhile conflicts and
    /// changes nothing.
    #[gpui::test]
    fn a_queued_edit_is_leased_and_committed_with_its_files(cx: &mut gpui::TestAppContext) {
        let (state, first, _) = queued();
        let thread = state.meta.id;
        let (workspace, cx, mut rx) = holding(&state, cx);
        let chat = thread.to_string();
        let begin = serde_json::json!({ "chatId": chat, "id": first.to_string() });
        let lease = reply(
            workspace
                .update(cx, |v, cx| v.queue_call(methods::BEGIN_QUEUED_MESSAGE_EDIT, &begin, cx)),
        );
        assert_eq!(lease["outcome"], "acquired", "{lease}");
        assert_eq!(lease["text"], "then the docs");
        assert_eq!(lease["attachments"], serde_json::json!(["/drop/shot.png"]));
        let hash = lease["baseTextHash"].as_str().expect("a hash").to_owned();

        let stale = serde_json::json!({
            "chatId": chat, "id": first.to_string(), "action": "commit",
            "text": "never", "expectedTextHash": "0", "attachments": [],
        });
        let conflict = reply(
            workspace
                .update(cx, |v, cx| v.queue_call(methods::FINISH_QUEUED_MESSAGE_EDIT, &stale, cx)),
        );
        assert_eq!(conflict["outcome"], "conflict");

        let commit = serde_json::json!({
            "chatId": chat, "id": first.to_string(), "action": "commit",
            "text": "then the docs, briefly", "expectedTextHash": hash,
            "attachments": ["/drop/other.png"],
        });
        let done =
            reply(workspace.update(cx, |v, cx| {
                v.queue_call(methods::FINISH_QUEUED_MESSAGE_EDIT, &commit, cx)
            }));
        assert_eq!(done["outcome"], "committed");
        cx.run_until_parked();
        let edits: Vec<Intent> = sent_threads(&mut rx)
            .into_iter()
            .filter_map(|request| match request {
                ThreadRequest::Intent { intent: intent @ Intent::Edit { .. }, .. } => Some(intent),
                _ => None,
            })
            .collect();
        assert_eq!(
            edits,
            [Intent::Edit {
                pending: first,
                text: "then the docs, briefly".to_owned(),
                attachments: vec!["/drop/other.png".to_owned()],
            }],
            "only the commit over the message as leased goes"
        );
    }

    /// A queued message dragged to the top goes just before the one that was first.
    #[gpui::test]
    fn a_queued_message_moved_to_the_top_goes_before_the_first(cx: &mut gpui::TestAppContext) {
        let (state, first, second) = queued();
        let (workspace, cx, mut rx) = holding(&state, cx);
        let params = serde_json::json!({
            "chatId": state.meta.id.to_string(), "id": second.to_string(), "toIndex": 0,
        });
        let moved = reply(
            workspace.update(cx, |v, cx| v.queue_call(methods::MOVE_QUEUED_MESSAGE, &params, cx)),
        );
        assert_eq!(moved["changed"], true);
        cx.run_until_parked();
        let sent = sent_threads(&mut rx).into_iter().any(|request| {
            matches!(request, ThreadRequest::Intent { intent: Intent::Move { pending, before }, .. }
                if pending == second && before == Some(first))
        });
        assert!(sent, "the move names the message it goes before");
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

    /// A transcript says how full its agent's context is.
    #[gpui::test]
    fn a_transcript_says_how_full_its_context_is(cx: &mut gpui::TestAppContext) {
        let mut state = fixtures::thread("tools");
        state.meters.context_tokens = Some(50_000);
        state.meters.context_window = Some(200_000);
        let (workspace, cx, _rx) = holding(&state, cx);
        let chat = follow(&workspace, cx, &state);
        let said = workspace.read_with(cx, |v, cx| v.view_transcript(&chat, cx)).expect("said");
        let usage = said.context_usage.expect("its context");
        assert_eq!((usage.tokens, usage.window), (Some(50_000), Some(200_000)));
    }

    /// A queued message changed in the composer goes to the agent in its own words, and the
    /// queue shows it as the person wrote it.
    #[gpui::test]
    fn a_queued_edit_goes_in_the_agents_words(cx: &mut gpui::TestAppContext) {
        let (state, first, _) = queued();
        let (workspace, cx, mut rx) = holding(&state, cx);
        let chat = state.meta.id.to_string();
        let begin = serde_json::json!({ "chatId": chat, "id": first.to_string() });
        let lease = reply(
            workspace
                .update(cx, |v, cx| v.queue_call(methods::BEGIN_QUEUED_MESSAGE_EDIT, &begin, cx)),
        );
        let chip = slopty_view_proto::file_mentions::local_file_link("src/a.rs", false);
        let written = format!("Check {chip}");
        let commit = serde_json::json!({
            "chatId": chat, "id": first.to_string(), "action": "commit", "text": written,
            "expectedTextHash": lease["baseTextHash"], "attachments": [],
        });
        let done =
            reply(workspace.update(cx, |v, cx| {
                v.queue_call(methods::FINISH_QUEUED_MESSAGE_EDIT, &commit, cx)
            }));
        assert_eq!(done["outcome"], "committed");
        cx.run_until_parked();
        let sent = sent_threads(&mut rx);
        let edited = sent.iter().find_map(|request| match request {
            ThreadRequest::Intent { intent: Intent::Edit { text, .. }, .. } => Some(text.clone()),
            _ => None,
        });
        let agent = super::agent_prompt(&written, HarnessId::CLAUDE_CODE);
        assert_ne!(agent, written, "the chip is put in the agent's words");
        assert_eq!(edited, Some(agent));
        let said = sent.iter().find_map(|request| match request {
            ThreadRequest::Intent { intent: Intent::Said { message, words }, .. } => {
                Some((*message, words.clone()))
            }
            _ => None,
        });
        assert_eq!(said, Some((first, written.clone())), "the thread keeps the person's words");
        let shown = workspace.update(cx, |v, cx| v.view_queue(&chat, cx));
        assert_eq!(shown.first().map(|m| m.text.clone()), Some(written), "as the person wrote it");
    }

    /// A message's own words the thread keeps show on any device, after any restart: the
    /// queue row and the transcript read them, not the agent's.
    #[gpui::test]
    fn the_threads_own_words_show_where_this_app_kept_none(cx: &mut gpui::TestAppContext) {
        let (mut state, first, _) = queued();
        let written = "Check @src/a.rs".to_owned();
        state.apply(&slopty_proto::thread::Action::Said { message: first, words: written.clone() });
        let (workspace, cx, _rx) = holding(&state, cx);
        let chat = state.meta.id.to_string();
        let shown = workspace.update(cx, |v, cx| v.view_queue(&chat, cx));
        assert_eq!(shown.first().map(|m| m.text.clone()), Some(written));
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

    /// A chat with no project on a device that runs no worker starts on the machine in front.
    #[gpui::test]
    fn a_projectless_chat_starts_on_the_machine_in_front(cx: &mut gpui::TestAppContext) {
        let (workspace, cx, _rx) = holding(&fixtures::thread("tools"), cx);
        let create = serde_json::json!({ "op": "createChat", "chatId": "m", "deviceId": "me" });
        let made = workspace.update(cx, |v, cx| v.view_call(methods::MUTATE, &create, cx));
        assert!(made.is_ok(), "{made:?}");
        let worker = workspace.read_with(cx, |v, _| v.view_model.minted.get("m").map(|m| m.worker));
        assert_eq!(worker, Some(KEY));
    }

    /// A message queued while its chat starts shows in the queue and can be taken back.
    #[gpui::test]
    fn a_message_queued_while_a_chat_starts_shows_and_goes(cx: &mut gpui::TestAppContext) {
        let (workspace, cx, mut rx) = holding(&fixtures::thread("tools"), cx);
        starting(&workspace, cx, &mut rx);
        let more = serde_json::json!({ "chatId": "m", "text": "and then", "holdForTurnEnd": true });
        let queued = workspace.update(cx, |v, cx| v.view_call(methods::QUEUE_MESSAGE, &more, cx));
        let row = queued.expect("held")["id"].as_str().map(str::to_owned).expect("its row");
        let shown = workspace.update(cx, |v, cx| v.view_queue("m", cx));
        let texts: Vec<_> = shown.iter().map(|m| (m.id.clone(), m.text.clone())).collect();
        assert_eq!(texts, [(row.clone(), "and then".to_owned())]);
        let params = serde_json::json!({ "chatId": "m", "id": row });
        let removed =
            workspace.update(cx, |v, cx| v.queue_call(methods::REMOVE_QUEUED_MESSAGE, &params, cx));
        assert_eq!(reply(removed)["removed"], true);
        assert_eq!(workspace.update(cx, |v, cx| v.view_queue("m", cx)), Vec::new());
    }

    /// A tapped note opens the chat it is about.
    #[gpui::test]
    fn a_tapped_note_names_its_chat(cx: &mut gpui::TestAppContext) {
        use slopty_platform::notify::info::{THREAD, WORKER};
        let state = fixtures::thread("tools");
        let (workspace, cx, _rx) = holding(&state, cx);
        let tap = slopty_platform::notify::Tap {
            id: "n".to_owned(),
            info: std::collections::BTreeMap::from([
                (WORKER.to_owned(), KEY.value().to_string()),
                (THREAD.to_owned(), state.meta.id.to_string()),
            ]),
            action: None,
            text: None,
        };
        let chat = workspace.read_with(cx, |v, _| v.tapped_chat(&tap));
        assert_eq!(chat, Some(state.meta.id.to_string()));
    }
}

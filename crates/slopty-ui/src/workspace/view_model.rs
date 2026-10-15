//! The shell fed from the workspace ([`crate::view_model`]): its lists published as the workers and
//! their thread tables move, the transcripts it watches followed, and its calls answered
//! ([`crate::view_model`]).
//!
//! A chat the interface mints before its first message is held here until that message
//! starts its thread; the thread then goes by the chat's id, so the interface never sees the
//! switch.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use gpui::{Context, Subscription, Task};
use slopty_client::WorkerKey;
use slopty_proto::ClientMsg;
use slopty_proto::thread::wire::{
    Intent, IntentDone, NewWorktree, Outcome, Start, ThreadRequest, ThreadRow,
};
use slopty_proto::thread::{AgentId, Delivery, IntentId, ThreadId};
use slopty_view_doc::SessionCommandPayload;
use slopty_view_proto::HarnessId;
use slopty_view_rpc::{RpcError, RpcReply, methods};
use tokio::sync::mpsc;

use super::WorkspaceView;
use crate::view_model::{Ask, Feed, Pull, rows, transcript};

mod surfaces;

pub use surfaces::{
    AddAnswer, AddChoice, AddDevice, AddFailure, AddFix, AddKind, AddMark, AddProgress, AddStep,
    Found, FoundNode, Look,
};

/// How long changes gather before the lists go out: a frame.
const GATHER: Duration = Duration::from_millis(16);

/// How often live sessions are said again, inside the shell's staleness window (45 s).
const HEARTBEAT: Duration = Duration::from_secs(15);

/// A chat the interface made that has no thread yet.
#[derive(Debug, Clone)]
struct Minted {
    worker: WorkerKey,
    cwd: String,
    created: DateTime<Utc>,
    /// The chat it is a side chat of: its first message asks that chat's thread aside.
    parent: Option<String>,
}

/// What the workspace keeps for the shell.
#[derive(Default)]
pub(super) struct Bridge {
    feed: Option<Arc<Feed>>,
    /// Answers the interface's asks.
    pump: Option<Task<()>>,
    /// The lists going out at the end of this frame.
    gathering: Option<Task<()>>,
    /// Says the live sessions again before they go stale.
    heartbeat: Option<Task<()>>,
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
    first_words: HashMap<IntentId, (String, Vec<String>, String)>,
    /// The id the shell gave each message it sent, by the intent that sent it: its entry in
    /// the transcript goes by it, so the shell's own echo of the message gives way to it.
    named: HashMap<IntentId, String>,
    /// What the right-pane surfaces wait on from the workers ([`surfaces`]).
    surfaces: surfaces::Surfaces,
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
        self.view_model.pump = Some(cx.spawn(async move |this, cx| {
            while let Some(ask) = asks.recv().await {
                if this.update(cx, |view, cx| view.view_ask(ask, cx)).is_err() {
                    return;
                }
            }
        }));
        let this = cx.entity();
        self.view_model.hearing.push(cx.observe(&this, |view, _, cx| view.view_changed(cx)));
        self.view_model.heartbeat = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(HEARTBEAT).await;
                if this.update(cx, Self::view_publish).is_err() {
                    return;
                }
            }
        }));
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

    /// The id the interface knows `thread` by.
    fn chat_id(&self, thread: ThreadId) -> String {
        self.view_model.aliases.get(&thread).cloned().unwrap_or_else(|| thread.to_string())
    }

    /// The thread a chat of the interface shows, and its worker.
    fn chat_thread(&self, chat: &str, cx: &Context<Self>) -> Option<(WorkerKey, ThreadId)> {
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

    /// Publish everything the interface watches.
    fn view_publish(&mut self, cx: &mut Context<Self>) {
        self.view_hear_hubs(cx);
        self.settle_configs(cx);
        let Some(feed) = self.view_model.feed.clone() else { return };
        let now = Utc::now();
        let devices =
            self.workers.keys().filter_map(|key| self.surface_device_row(*key, now)).collect();
        let mut chats = Vec::new();
        let mut sessions = Vec::new();
        let mut pulls = Vec::new();
        // Each folder, and whether a thread in it named it as its repository.
        let mut folders: BTreeMap<(String, String), bool> = BTreeMap::new();
        // Every worker's rows at once: an agent may start its subagents on other machines.
        let all: Vec<(String, &ThreadRow)> = self
            .hubs()
            .flat_map(|(key, hub)| {
                let device = key.to_string();
                hub.read(cx).threads().rows().rows.values().map(move |row| (device.clone(), row))
            })
            .collect();
        for (device, row) in &all {
            let row = *row;
            let id = self.chat_id(row.id);
            let mut chat = rows::chat(row, device);
            chat.space_id = rows::folder(row).map(|path| self.view_space_id(device, path));
            chat.config = Some(self.chat_config(&id, device, row));
            if chat.branch.is_none() {
                chat.branch = self.view_branch(device, row);
            }
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
            if let Some(folder) = rows::folder(row) {
                let git = row.repo.as_deref() == Some(folder);
                *folders.entry((device.clone(), folder.to_owned())).or_default() |= git;
            }
            let running = rows::running_children(row.id, all.iter().map(|(_, row)| *row));
            let mut session = rows::session(row, device, running, now);
            session.chat_id = id;
            chats.push(chat);
            sessions.push(session);
        }
        for (id, minted) in &self.view_model.minted {
            let device = minted.worker.to_string();
            folders.entry((device.clone(), minted.cwd.clone())).or_default();
            chats.push(slopty_view_proto::Chat {
                id: id.clone(),
                device_id: device.clone(),
                title: None,
                archived: self.surface_archived(id),
                cwd: Some(minted.cwd.clone()),
                branch: None,
                checkout_id: None,
                source_context: None,
                config: None,
                last_message_preview: None,
                last_message_at: None,
                created_at: minted.created,
                harness_session_id: None,
                harness_session_cwd: None,
                space_id: Some(self.view_space_id(&device, &minted.cwd)),
                last_seen_at: None,
                room_gen: None,
                parent_chat_id: minted.parent.clone(),
            });
        }
        for project in self.added_projects() {
            folders.entry((project.device.clone(), project.path.clone())).or_default();
        }
        let spaces = folders
            .into_iter()
            .map(|((device, path), git)| {
                let mut space = rows::space(&device, &path, git, now);
                if let Some(project) = self.added_project_at(&device, &path) {
                    space.id.clone_from(&project.id);
                    space.name.clone_from(&project.name);
                }
                space
            })
            .collect();
        feed.devices(devices);
        feed.spaces(spaces);
        feed.chats(chats);
        feed.sessions(sessions);
        feed.pulls(pulls);
        feed.sidebar(self.sidebar_state());
        for chat in feed.followed() {
            let entries = self.view_transcript(&chat, cx).unwrap_or_default();
            feed.transcript(&chat, entries);
        }
        for chat in feed.queued() {
            feed.queue(&chat, self.view_queue(&chat, cx));
        }
        // An agent taken up again for its terminal shows in the table, not the workspace.
        self.live_settle_resumes(cx);
    }

    /// The messages waiting to go to the chat `chat`'s agent, as its thread holds them.
    fn view_queue(&self, chat: &str, cx: &Context<Self>) -> Vec<slopty_view_doc::QueuedMessage> {
        let Some((key, thread)) = self.chat_thread(chat, cx) else { return Vec::new() };
        let Some(hub) = self.held_hub(key) else { return Vec::new() };
        let hub = hub.read(cx);
        let Some(state) =
            hub.threads().mirror(thread).and_then(slopty_client::threads::Mirror::state)
        else {
            return Vec::new();
        };
        state
            .pending
            .iter()
            .map(|pending| slopty_view_doc::QueuedMessage {
                id: pending.intent.to_string(),
                text: pending.text.clone(),
                attachments: pending.attachments.clone(),
                hold_for_turn_end: pending.delivery == Delivery::Queue,
                issued_by: key.to_string(),
                issued_at: 0,
                edited_at: None,
                delivery_gate: None,
            })
            .collect()
    }

    /// The transcript of the chat `chat` as its thread stands here.
    fn view_transcript(
        &self,
        chat: &str,
        cx: &Context<Self>,
    ) -> Option<Vec<slopty_view_doc::SessionMessageEntry>> {
        let (key, thread) = self.view_model.followed.get(chat).copied()?;
        let hub = self.held_hub(key)?.read(cx);
        let state = hub.threads().mirror(thread)?.state()?;
        let table = &hub.threads().rows().rows;
        let device = key.to_string();
        let named = &self.view_model.named;
        let mut entries = transcript::entries(
            state,
            &device,
            |child| table.get(&child).map(rows::running),
            |intent| named.get(&intent).cloned(),
        );
        // The table is the worker's word on whether the agent works now; a mirror drawn from the
        // cache may still hold a turn that ended since.
        let live = table.get(&thread).is_some_and(|row| {
            rows::running(row) || row.status.phase == slopty_proto::thread::Phase::NeedsYou
        });
        transcript::settle(&mut entries, live);
        transcript::asks(state, &device, &mut entries);
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
        Some(entries)
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
                if let Some(answer) = self.view_surface_call(&method, &params, cx) {
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

    /// Follow the thread `chat` shows; a minted chat shows nothing until it has one.
    fn view_follow(&mut self, chat: String, cx: &mut Context<Self>) {
        let Some((key, thread)) = self.chat_thread(&chat, cx) else {
            if let Some(feed) = &self.view_model.feed {
                feed.transcript(&chat, Vec::new());
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
                    let device = param(params, "deviceId")
                        .ok_or_else(|| RpcError::BadParams("a space or device".to_owned()))?;
                    let worker = self
                        .view_worker(device)
                        .ok_or_else(|| RpcError::Failed(format!("no machine {device}")))?;
                    (worker, "~".to_owned())
                };
                let cwd = param(params, "cwd").map_or(cwd, str::to_owned);
                let parent = param(params, "parentChatId").map(str::to_owned);
                self.view_model
                    .minted
                    .insert(chat.to_owned(), Minted { worker, cwd, created: Utc::now(), parent });
                self.view_changed(cx);
                Ok(serde_json::json!({}))
            }
            // The person looked at the chat: its thread is seen through its latest turn, on
            // its worker, which tells every other device.
            ("markChatSeen", Some(chat)) => {
                if let Some((key, thread)) = self.chat_thread(chat, cx)
                    && let Some(ended) = self.thread_stand(thread).and_then(|stand| stand.ended)
                {
                    self.mark_seen(key, thread, ended.turn, cx);
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
                if self.view_model.minted.remove(chat).is_none() {
                    let (key, thread) = self
                        .chat_thread(chat, cx)
                        .ok_or_else(|| RpcError::Failed(format!("no thread {chat}")))?;
                    let hub = self.thread_hub(key, cx);
                    hub.update(cx, |hub, cx| hub.intent(thread, Intent::Discard, cx));
                    self.view_model.followed.remove(chat);
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

    /// The branch the thread `row` on `device` works on where its agent names none: the one its
    /// terminal's checkout has, as the worker reads it at each prompt.
    fn view_branch(&self, device: &str, row: &ThreadRow) -> Option<String> {
        let worker = self.workers.get(&self.view_worker(device)?)?;
        worker.sessions.get(&row.terminal?)?.branch.clone()
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
        let (key, thread) = self
            .chat_thread(chat, cx)
            .ok_or_else(|| RpcError::Failed(format!("no thread {chat}")))?;
        let hub = self.thread_hub(key, cx);
        let harness = self.thread_harness(key, thread, cx);
        let text = agent_prompt(param(params, "text").unwrap_or_default(), harness);
        // Held for the turn's end where the agent can hold it, as the shell asks.
        let hold = params.get("holdForTurnEnd").and_then(serde_json::Value::as_bool) == Some(true);
        let delivery = hub
            .read(cx)
            .threads()
            .mirror(thread)
            .and_then(slopty_client::threads::Mirror::state)
            .map_or(Delivery::Steer, |state| {
                if hold && state.meta.can(slopty_proto::thread::Cap::QUEUE) {
                    Delivery::Queue
                } else {
                    state.meta.delivery_now()
                }
            });
        let intent = Intent::Send { text, delivery, attachments };
        let id = hub.update(cx, |hub, cx| hub.intent(thread, intent, cx));
        // The queue's row goes by the intent's id, and so does the message once it went.
        self.view_model.named.insert(id, id.to_string());
        Ok(serde_json::json!({ "id": id.to_string() }))
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
            let id = IntentId::new();
            // A side chat's first message asks its chat's thread aside, and goes to the aside.
            if let Some(parent) = minted.parent.as_deref() {
                let (key, thread) = self
                    .chat_thread(parent, cx)
                    .ok_or_else(|| RpcError::Failed("Its chat has not started yet".to_owned()))?;
                let harness = self.thread_harness(key, thread, cx);
                let text = agent_prompt(&request.prompt, harness);
                self.view_model.first_words.insert(id, (text, request.attachments, message_id));
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
            let start = Start {
                agent,
                cwd: minted.cwd,
                drive: None,
                prompt: Some(prompt),
                model: request.model,
                mode: None,
                effort: request
                    .reasoning
                    .and_then(|level| serde_json::to_value(level).ok())
                    .and_then(|level| level.as_str().map(str::to_owned)),
                attachments: request.attachments,
                args: Vec::new(),
                worktree,
            };
            self.view_model.starts.insert(id, chat);
            self.view_model.named.insert(id, message_id);
            let msg = ClientMsg::Thread(ThreadRequest::Start { id, start: Box::new(start) });
            self.view_send_kept(minted.worker, id, msg);
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
                start.prompt = Some(agent_prompt(&request.prompt, harness));
                start.attachments.clone_from(&request.attachments);
                if request.model.is_some() {
                    start.model.clone_from(&request.model);
                }
                let id = hub.update(cx, |hub, cx| hub.resume(thread, start, cx));
                self.view_model.named.insert(id, message_id.clone());
                return Ok(serde_json::json!({ "commandId": id.to_string() }));
            }
        }
        let (intent, message_id) = match command {
            SessionCommandPayload::Run { request, message_id } => (
                Intent::Send {
                    text: agent_prompt(&request.prompt, harness),
                    delivery: delivery_now(hub.read(cx)),
                    attachments: request.attachments,
                },
                Some(message_id),
            ),
            SessionCommandPayload::Steer { prompt, message_id } => (
                Intent::Send {
                    text: agent_prompt(&prompt, harness),
                    delivery: Delivery::Steer,
                    attachments: Vec::new(),
                },
                message_id,
            ),
            SessionCommandPayload::Interrupt {} => (Intent::Interrupt, None),
            SessionCommandPayload::RespondInput { request_id, answers } => {
                let state = hub
                    .read(cx)
                    .threads()
                    .mirror(thread)
                    .and_then(slopty_client::threads::Mirror::state);
                let request = state
                    .and_then(|state| state.requests.iter().find(|r| r.id.0 == request_id))
                    .filter(|request| request.is_open())
                    .ok_or_else(|| {
                        RpcError::Failed("It no longer waits on an answer".to_owned())
                    })?;
                let (choice, message) =
                    transcript::answer(request, &answers).map_err(RpcError::Failed)?;
                (Intent::Answer { ask: request.id.clone(), choice, message }, None)
            }
        };
        let id = hub.update(cx, |hub, cx| hub.intent(thread, intent, cx));
        if let Some(message_id) = message_id {
            self.view_model.named.insert(id, message_id);
        }
        Ok(serde_json::json!({ "commandId": id.to_string() }))
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
    /// on. Whether the answer was one of these.
    pub(super) fn view_start_done(
        &mut self,
        key: WorkerKey,
        done: &IntentDone,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(chat) = self.view_model.starts.remove(&done.id) else { return false };
        self.view_model.unanswered.remove(&done.id);
        match &done.outcome {
            Outcome::Started { thread } => {
                self.view_model.minted.remove(&chat);
                self.view_model.aliases.insert(*thread, chat.clone());
                self.surface_rekey_archived(&chat, thread.to_string(), cx);
                if let Some((text, attachments, message_id)) =
                    self.view_model.first_words.remove(&done.id)
                {
                    let hub = self.thread_hub(key, cx);
                    let send = Intent::Send { text, delivery: Delivery::Steer, attachments };
                    let sent = hub.update(cx, |hub, cx| hub.intent(*thread, send, cx));
                    self.view_model.named.insert(sent, message_id);
                }
                if self.view_model.feed.as_ref().is_some_and(|feed| feed.followed().contains(&chat))
                {
                    self.view_model.followed.insert(chat, (key, *thread));
                    let hub = self.thread_hub(key, cx);
                    hub.update(cx, |hub, cx| hub.open(*thread, cx));
                }
            }
            outcome => {
                self.view_model.first_words.remove(&done.id);
                tracing::warn!(?outcome, %chat, "a thread the interface asked for did not start");
            }
        }
        self.view_changed(cx);
        true
    }
}

#[cfg(test)]
mod tests {
    use gpui::AppContext as _;
    use slopty_core::WallMs;
    use slopty_proto::ClientMsg;
    use slopty_proto::thread::wire::{Intent, TableFrame, ThreadFrame, ThreadRequest};
    use slopty_proto::thread::{
        AgentId, Cursor, Delivery, IntentId, Liveness, Pending, PendingState, ThreadState,
    };
    use slopty_view_doc::SessionCommandPayload;
    use slopty_view_proto::HarnessId;
    use slopty_view_rpc::methods;
    use tokio::sync::mpsc::Receiver;

    use super::agent_of;
    use crate::conversation::thread::fixtures;
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

    /// A message to a chat whose Claude Code exited takes its session up again, with the message
    /// as its first words, instead of going to an agent that is not there.
    #[gpui::test]
    fn a_message_to_an_exited_agent_takes_its_session_up_again(cx: &mut gpui::TestAppContext) {
        let mut state = fixtures::thread("tools");
        state.status.liveness = Liveness::Exited { resumable: true };
        let thread = state.meta.id;
        let native = state.meta.native.clone();
        let (workspace, cx, mut rx) = holding(&state, cx);
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
        let params = serde_json::json!({ "chatId": thread.to_string(), "command": command });
        let answer = workspace.update(cx, |v, cx| v.view_call(methods::QUEUE_COMMAND, &params, cx));
        assert!(answer.is_ok(), "{answer:?}");
        cx.run_until_parked();
        let started = sent_threads(&mut rx).into_iter().find_map(|request| match request {
            ThreadRequest::Start { start, .. } => Some(start),
            _ => None,
        });
        let start = started.expect("a start that takes the session up again");
        assert_eq!(start.args, ["--resume".to_owned(), native]);
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
}

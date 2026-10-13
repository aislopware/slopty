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
use slopty_proto::thread::wire::{Intent, IntentDone, Outcome, Start, ThreadRequest, ThreadRow};
use slopty_proto::thread::{AgentId, Delivery, IntentId, ThreadId};
use slopty_view_doc::SessionCommandPayload;
use slopty_view_proto::HarnessId;
use slopty_view_rpc::{RpcError, RpcReply, methods};
use tokio::sync::mpsc;

use super::WorkspaceView;
use crate::view_model::{Ask, Feed, rows, transcript};

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
        let Some(feed) = self.view_model.feed.clone() else { return };
        let now = Utc::now();
        let devices =
            self.workers.keys().filter_map(|key| self.surface_device_row(*key, now)).collect();
        let mut chats = Vec::new();
        let mut sessions = Vec::new();
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
                space_id: Some(rows::space_id(&device, &minted.cwd)),
                last_seen_at: None,
                room_gen: None,
                parent_chat_id: None,
            });
        }
        let spaces = folders
            .into_iter()
            .map(|((device, path), git)| rows::space(&device, &path, git, now))
            .collect();
        feed.devices(devices);
        feed.spaces(spaces);
        feed.chats(chats);
        feed.sessions(sessions);
        for chat in feed.followed() {
            let entries = self.view_transcript(&chat, cx).unwrap_or_default();
            feed.transcript(&chat, entries);
        }
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
        Some(transcript::entries(state, &device, |child| table.get(&child).map(rows::running)))
    }

    /// Answer one of the interface's asks.
    fn view_ask(&mut self, ask: Ask, cx: &mut Context<Self>) {
        match ask {
            Ask::Follow(chat) => self.view_follow(chat, cx),
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
                self.view_model
                    .minted
                    .insert(chat.to_owned(), Minted { worker, cwd, created: Utc::now() });
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
            ("deleteChat", Some(chat)) => {
                self.view_model.minted.remove(chat);
                self.view_changed(cx);
                Ok(serde_json::json!({}))
            }
            _ => self.surface_mutate(op, params, cx).unwrap_or_else(|| {
                Err(RpcError::Failed(format!("{op} is not something Slopty does to a thread")))
            }),
        }
    }

    /// The worker a device id of the interface names.
    fn view_worker(&self, device: &str) -> Option<WorkerKey> {
        self.workers.keys().find(|key| key.to_string() == device).copied()
    }

    /// The worker and folder a space id of the interface names ([`rows::space_id`]).
    fn view_space(&self, space: &str) -> Option<(WorkerKey, String)> {
        let (device, path) = space.split_once(':')?;
        Some((self.view_worker(device)?, path.to_owned()))
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
            let SessionCommandPayload::Run { request, .. } = command else {
                return Err(RpcError::Failed("the thread has not started yet".to_owned()));
            };
            let id = IntentId::new();
            let start = Start {
                agent: agent_of(request.harness),
                cwd: minted.cwd,
                drive: None,
                prompt: Some(request.prompt),
                model: request.model,
                mode: None,
                effort: None,
                attachments: request.attachments,
                args: Vec::new(),
                worktree: None,
            };
            self.view_model.starts.insert(id, chat);
            self.send(
                minted.worker,
                ClientMsg::Thread(ThreadRequest::Start { id, start: Box::new(start) }),
            );
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
        let intent = match command {
            SessionCommandPayload::Run { request, .. } => Intent::Send {
                text: request.prompt,
                delivery: delivery_now(hub.read(cx)),
                attachments: request.attachments,
            },
            SessionCommandPayload::Steer { prompt, .. } => {
                Intent::Send { text: prompt, delivery: Delivery::Steer, attachments: Vec::new() }
            }
            SessionCommandPayload::Interrupt {} => Intent::Interrupt,
            SessionCommandPayload::RespondInput { .. } => {
                return Err(RpcError::Failed("answers go through the request card".to_owned()));
            }
        };
        let id = hub.update(cx, |hub, cx| hub.intent(thread, intent, cx));
        Ok(serde_json::json!({ "commandId": id.to_string() }))
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
        match &done.outcome {
            Outcome::Started { thread } => {
                self.view_model.minted.remove(&chat);
                self.view_model.aliases.insert(*thread, chat.clone());
                if self.view_model.feed.as_ref().is_some_and(|feed| feed.followed().contains(&chat))
                {
                    self.view_model.followed.insert(chat, (key, *thread));
                    let hub = self.thread_hub(key, cx);
                    hub.update(cx, |hub, cx| hub.open(*thread, cx));
                }
            }
            outcome => {
                tracing::warn!(?outcome, %chat, "a thread the interface asked for did not start");
            }
        }
        self.view_changed(cx);
        true
    }
}

#[cfg(test)]
mod tests {
    use slopty_proto::thread::AgentId;
    use slopty_view_proto::HarnessId;

    use super::agent_of;

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
}

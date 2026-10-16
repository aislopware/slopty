//! Slopty's view model: what feeds the shell (`slopty-shell`) from Slopty's server and workers
//! (`docs/decisions/ui.md`).
//!
//! [`ViewModel`] is one RPC service. The shell's state calls it in process, with the view types'
//! envelopes, through `slopty_view_rpc::memory_client`. Its standing lists are published here by
//! the workspace ([`Feed`]) and served as watches. A transcript is followed while a watch is open
//! on it. Every other call goes to the workspace on the UI thread ([`Ask::Call`]), which answers it
//! or says the method is unknown.
//!
//! The model it presents, in the shell's words:
//! - a **device** is a worker;
//! - a **space** is a folder on a worker;
//! - a **chat** is a thread, and its **session** is the thread's live status;
//! - a **subagent** is a thread's child, nested under its parent with the running count.

pub mod facts;
pub mod folds;
pub mod live;
pub mod rows;
pub mod text;
pub mod transcript;
pub mod work;

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt as _;
use futures::stream::BoxStream;
use parking_lot::Mutex;
use serde::Serialize;
use slopty_view_doc::{
    ChatDock, PermissionRules, QueuedMessage, SessionMessageEntry, TranscriptBaseline,
    TranscriptFrame, TranscriptUpdate, WorkTray, calls,
};
use slopty_view_proto::{
    AgentUsageWindow, Chat, ChatConnectivity, Connectivity, ConnectivityState, Device, EngineInfo,
    ServerPresence, Session, SidebarPreferencesState, Space, TransferProgress, WorkspaceScope,
};
use slopty_view_rpc::{RpcError, RpcReply, RpcService, methods};
use tokio::sync::{mpsc, oneshot, watch};

/// What the view model asks of the workspace on the UI thread.
pub enum Ask {
    /// A watch opened on this chat's transcript: follow its thread and publish it
    /// ([`Feed::transcript`]).
    Follow(String),
    /// The last watch on this chat's transcript closed.
    Unfollow(String),
    /// A watch opened on what the workspace publishes: publish it all again now.
    Republish,
    /// Any other call: answer it on `reply`, a value or a watch's stream, or say the method is
    /// unknown.
    Call {
        /// The method's name ([`slopty_view_rpc::methods`]).
        method: String,
        /// Its parameters.
        params: serde_json::Value,
        /// Where the answer goes.
        reply: oneshot::Sender<Result<RpcReply, RpcError>>,
    },
}

impl std::fmt::Debug for Ask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Follow(chat) => f.debug_tuple("Follow").field(chat).finish(),
            Self::Unfollow(chat) => f.debug_tuple("Unfollow").field(chat).finish(),
            Self::Republish => f.write_str("Republish"),
            Self::Call { method, params, .. } => f
                .debug_struct("Call")
                .field("method", method)
                .field("params", params)
                .finish_non_exhaustive(),
        }
    }
}

/// The lists the interface watches, as the workspace last published them.
#[derive(Debug)]
pub struct Feed {
    devices: watch::Sender<Arc<[Device]>>,
    /// `None` until the workspace has heard every machine's threads, or given up waiting: an
    /// empty or partial first list would read as chats and projects gone, and the shell would
    /// let go of their pins and filter.
    spaces: watch::Sender<Option<Arc<[Space]>>>,
    chats: watch::Sender<Option<Arc<[Chat]>>>,
    sessions: watch::Sender<Arc<[Session]>>,
    /// The link to the server, for the sidebar's foot, and each chat's link to its worker.
    connectivity: watch::Sender<Connectivity>,
    /// Whether any worker is linked now, `None` with none at all: the whole's state where no
    /// server is followed.
    workers_up: Mutex<Option<bool>>,
    /// The files on their way to a machine to go with a message, by the upload id the composer
    /// gave each.
    transfers: watch::Sender<Arc<[TransferProgress]>>,
    /// This machine's own worker as a device id, once the app knows whether it has one.
    local: watch::Sender<Local>,
    /// Each followed chat's transcript, `None` until its thread has come.
    transcripts: Mutex<HashMap<String, Followed>>,
    /// The messages waiting to go to each chat's agent, for the chats whose queue is watched.
    queues: Mutex<HashMap<String, watch::Sender<Arc<[QueuedMessage]>>>>,
    /// The pull requests the threads' branches have, as their rows show them.
    pulls: watch::Sender<Arc<[Pull]>>,
    /// The sidebar's pins and sections, `None` until the workspace says them.
    sidebar: watch::Sender<Option<SidebarPreferencesState>>,
}

/// A pull request a thread's branch has, where the thread works.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pull {
    /// The machine, as a device id.
    pub device: String,
    /// The folders the thread works in: where it runs and its repository's root.
    pub folders: Vec<String>,
    /// The branch, when known.
    pub branch: Option<String>,
    /// The pull request.
    pub summary: slopty_view_proto::ChangeRequestSummary,
}

/// This machine's own worker, as the app last said.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Local {
    /// The app has not said yet.
    Unknown,
    /// Its device id, or `None` when this machine runs no worker (a phone, a Mac without one).
    Known(Option<String>),
}

/// One followed chat's transcript, the rest of what its thread says (slopty: its tray, dock
/// and rules), and how many watches are open on any of it: the thread stays open while one is.
#[derive(Debug)]
struct Followed {
    said: watch::Sender<Option<Arc<Transcript>>>,
    work: watch::Sender<Option<Arc<WorkTray>>>,
    dock: watch::Sender<Option<Arc<ChatDock>>>,
    rules: watch::Sender<Option<Arc<PermissionRules>>>,
    watches: usize,
}

impl Followed {
    fn new() -> Self {
        Self {
            said: watch::Sender::new(None),
            work: watch::Sender::new(None),
            dock: watch::Sender::new(None),
            rules: watch::Sender::new(None),
            watches: 0,
        }
    }
}

/// slopty: what a followed chat's thread says beside its transcript, as the workspace publishes
/// it ([`Feed::chat_views`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChatViews {
    /// Its background work.
    pub work: WorkTray,
    /// Its dock: activity, goal, notes, the status line, the mode, the context card.
    pub dock: ChatDock,
    /// Its agent's permission rules.
    pub rules: PermissionRules,
}

/// Replace a followed chat's value, waking its watches only when it changed.
fn publish_value<T: PartialEq>(sender: &watch::Sender<Option<Arc<T>>>, value: T) {
    sender.send_if_modified(|now| {
        if now.as_deref() == Some(&value) {
            return false;
        }
        *now = Some(Arc::new(value));
        true
    });
}

/// A chat's transcript as the workspace says it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Transcript {
    /// Its entries.
    pub entries: Vec<SessionMessageEntry>,
    /// How full the agent's context is, for the shell's ring.
    pub context_usage: Option<slopty_view_proto::ContextUsage>,
    /// Older turns are still on their way: the entries are the latest ones, which the shell
    /// shows as it waits for the whole history (zeron's opening tail).
    pub history_pending: bool,
    /// The plan's rate windows as the agent last said them, the most used first, for the
    /// shell's usage ring.
    pub limits: Vec<AgentUsageWindow>,
    /// When the usage limit holding the agent lifts, while one does.
    pub limit_resets_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl Transcript {
    /// The whole of a transcript of `entries`, with nothing more to say.
    #[must_use]
    pub const fn of(entries: Vec<SessionMessageEntry>) -> Self {
        Self {
            entries,
            context_usage: None,
            history_pending: false,
            limits: Vec::new(),
            limit_resets_at: None,
        }
    }
}

impl Feed {
    fn new() -> Self {
        Self {
            devices: watch::Sender::new(Arc::default()),
            spaces: watch::Sender::new(None),
            chats: watch::Sender::new(None),
            sessions: watch::Sender::new(Arc::default()),
            connectivity: watch::Sender::new(Connectivity::default()),
            workers_up: Mutex::new(None),
            transfers: watch::Sender::new(Arc::default()),
            local: watch::Sender::new(Local::Unknown),
            transcripts: Mutex::new(HashMap::new()),
            queues: Mutex::new(HashMap::new()),
            pulls: watch::Sender::new(Arc::default()),
            sidebar: watch::Sender::new(None),
        }
    }

    /// This machine's own worker, as a device id, or that it has none: the interface's "This
    /// device".
    pub fn local_device(&self, device: Option<String>) {
        self.local.send_if_modified(|now| {
            let known = Local::Known(device);
            let changed = *now != known;
            *now = known;
            changed
        });
    }

    /// This machine's own worker, as a device id, once the app has said it has one.
    #[must_use]
    pub fn local_worker(&self) -> Option<String> {
        match &*self.local.borrow() {
            Local::Known(device) => device.clone(),
            Local::Unknown => None,
        }
    }

    /// The workers, as devices. Unchanged lists wake no watch.
    pub fn devices(&self, devices: Vec<Device>) {
        publish(&self.devices, devices);
    }

    /// The folders on the workers, as spaces.
    pub fn spaces(&self, spaces: Vec<Space>) {
        publish_once_known(&self.spaces, spaces);
    }

    /// The threads, as chats.
    pub fn chats(&self, chats: Vec<Chat>) {
        publish_once_known(&self.chats, chats);
    }

    /// The chats as last said, `None` before the first.
    #[cfg(test)]
    pub(crate) fn said_chats(&self) -> Option<Arc<[Chat]>> {
        self.chats.borrow().clone()
    }

    /// The spaces as last said, `None` before the first.
    #[cfg(test)]
    pub(crate) fn said_spaces(&self) -> Option<Arc<[Space]>> {
        self.spaces.borrow().clone()
    }

    /// The sessions as last said.
    #[cfg(test)]
    pub(crate) fn said_sessions(&self) -> Arc<[Session]> {
        self.sessions.borrow().clone()
    }

    /// The sidebar's pins and sections.
    pub fn sidebar(&self, state: SidebarPreferencesState) {
        self.sidebar.send_if_modified(|now| {
            let changed = now.as_ref() != Some(&state);
            *now = Some(state);
            changed
        });
    }

    /// The pull requests the threads' branches have.
    pub fn pulls(&self, pulls: Vec<Pull>) {
        publish(&self.pulls, pulls);
    }

    /// The threads' live status, as sessions.
    pub fn sessions(&self, sessions: Vec<Session>) {
        publish(&self.sessions, sessions);
    }

    /// The server this client follows, and whether it is linked now; `None` without one.
    pub fn server(&self, server: Option<ServerPresence>) {
        let workers = *self.workers_up.lock();
        self.connectivity.send_if_modified(|now| {
            let state = edge(server.as_ref(), workers);
            if now.server == server && now.state == state {
                return false;
            }
            now.server = server;
            now.state = state;
            true
        });
    }

    /// How this client reaches the workers: whether any is linked now (`None` with none at
    /// all), which stands for the whole where no server is followed, and each chat by its
    /// worker's link (`chats`). Unchanged, it wakes no watch.
    pub fn links(&self, workers: Option<bool>, chats: Vec<ChatConnectivity>) {
        *self.workers_up.lock() = workers;
        self.connectivity.send_if_modified(|now| {
            let state = edge(now.server.as_ref(), workers);
            if now.state == state && now.chats == chats {
                return false;
            }
            now.state = state;
            now.chats = chats;
            true
        });
    }

    /// The files on their way to a machine with a message, each by its upload id.
    pub fn transfers(&self, transfers: Vec<TransferProgress>) {
        publish(&self.transfers, transfers);
    }

    /// The connectivity as last said.
    #[cfg(test)]
    pub(crate) fn said_connectivity(&self) -> Connectivity {
        self.connectivity.borrow().clone()
    }

    /// The chats followed now: the workspace keeps their threads open and publishes them.
    #[must_use]
    pub fn followed(&self) -> Vec<String> {
        self.transcripts.lock().keys().cloned().collect()
    }

    /// `chat`'s transcript as its thread stands now; nothing while no watch is open on it.
    pub fn transcript(&self, chat: &str, said: Transcript) {
        let sender = self.transcripts.lock().get(chat).map(|t| t.said.clone());
        let Some(sender) = sender else { return };
        sender.send_if_modified(|now| {
            if now.as_deref() == Some(&said) {
                return false;
            }
            *now = Some(Arc::new(said));
            true
        });
    }

    /// slopty: what `chat`'s thread says beside its transcript; nothing while no watch is open
    /// on the chat.
    pub fn chat_views(&self, chat: &str, views: ChatViews) {
        let senders = self
            .transcripts
            .lock()
            .get(chat)
            .map(|f| (f.work.clone(), f.dock.clone(), f.rules.clone()));
        let Some((work, dock, rules)) = senders else { return };
        publish_value(&work, views.work);
        publish_value(&dock, views.dock);
        publish_value(&rules, views.rules);
    }

    /// The chats whose queue is watched: the workspace publishes their waiting messages.
    #[must_use]
    pub fn queued(&self) -> Vec<String> {
        self.queues.lock().keys().cloned().collect()
    }

    /// The messages waiting to go to `chat`'s agent; nothing while no watch is open on it.
    pub fn queue(&self, chat: &str, items: Vec<QueuedMessage>) {
        let sender = self.queues.lock().get(chat).cloned();
        if let Some(sender) = sender {
            publish(&sender, items);
        }
    }

    /// A watch opened on `chat`'s queue.
    fn watch_queue(&self, chat: &str) -> watch::Receiver<Arc<[QueuedMessage]>> {
        let mut queues = self.queues.lock();
        queues
            .entry(chat.to_owned())
            .or_insert_with(|| watch::Sender::new(Arc::default()))
            .subscribe()
    }

    /// A watch opened on `chat`: the receiver it reads, and whether it is the first.
    pub(crate) fn watch_transcript(
        &self,
        chat: &str,
    ) -> (watch::Receiver<Option<Arc<Transcript>>>, bool) {
        self.follow(chat, |followed| followed.said.subscribe())
    }

    /// A watch opened on something of `chat`'s, read by `pick`: what it reads, and whether it is
    /// the chat's first watch.
    fn follow<R>(&self, chat: &str, pick: impl FnOnce(&Followed) -> R) -> (R, bool) {
        let mut transcripts = self.transcripts.lock();
        let followed = transcripts.entry(chat.to_owned()).or_insert_with(Followed::new);
        followed.watches = followed.watches.saturating_add(1);
        let watching = (pick(followed), followed.watches == 1);
        drop(transcripts);
        watching
    }

    /// A watch on `chat` closed: whether it was the last.
    fn unwatch_transcript(&self, chat: &str) -> bool {
        let mut transcripts = self.transcripts.lock();
        let Some(transcript) = transcripts.get_mut(chat) else { return false };
        transcript.watches = transcript.watches.saturating_sub(1);
        if transcript.watches > 0 {
            return false;
        }
        transcripts.remove(chat);
        true
    }
}

/// The whole's link, as the shell's pill and composer read it: the server's where one is
/// followed, else whether any worker is linked; nothing to show with neither.
const fn edge(server: Option<&ServerPresence>, workers: Option<bool>) -> ConnectivityState {
    match (server, workers) {
        (Some(server), _) if server.online => ConnectivityState::Connected,
        (Some(_), _) | (None, Some(false)) => ConnectivityState::Reconnecting,
        (None, Some(true)) => ConnectivityState::Connected,
        (None, None) => ConnectivityState::Disabled,
    }
}

/// Replace a list, waking its watches only when it changed.
fn publish<T: PartialEq>(sender: &watch::Sender<Arc<[T]>>, list: Vec<T>) {
    sender.send_if_modified(|now| {
        if **now == *list {
            return false;
        }
        *now = list.into();
        true
    });
}

/// Replace a list that waits to be known, waking its watches when it is first said and when it
/// changed.
fn publish_once_known<T: PartialEq>(sender: &watch::Sender<Option<Arc<[T]>>>, list: Vec<T>) {
    sender.send_if_modified(|now| {
        if now.as_deref() == Some(list.as_slice()) {
            return false;
        }
        *now = Some(list.into());
        true
    });
}

/// The view model: Slopty's server and workers, as the shell reads them.
#[derive(Debug)]
pub struct ViewModel {
    info: EngineInfo,
    feed: Arc<Feed>,
    asks: mpsc::UnboundedSender<Ask>,
}

/// What the shell may ask of Slopty, on this device and on every worker.
///
/// Its composer's chips, put in each agent's words on the way; the thread's queue of messages
/// sent while the agent works, whose files stay chips beside the words; and a chat with no
/// project in a dated scratch folder of its own, which the view model makes on its machine
/// before the agent starts there. Updating an agent Slopty does not do.
#[must_use]
pub fn capabilities() -> Vec<String> {
    use slopty_view_proto::capabilities as can;
    [
        can::COMPOSER_REFERENCES_V1,
        can::MESSAGE_QUEUE_V1,
        can::MESSAGE_QUEUE_ACTIONS_V1,
        can::MESSAGE_QUEUE_ATTACHMENTS_V1,
        can::MESSAGE_QUEUE_CLEAN_ATTACHMENT_TEXT_V1,
        can::MESSAGE_QUEUE_EDIT_LEASE_V1,
        can::PROJECTLESS_SCRATCH_V1,
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

impl ViewModel {
    /// A view model for the client `device`, with the feed the workspace publishes into and the
    /// asks it answers.
    #[must_use]
    pub fn new(device: String) -> (Arc<Self>, Arc<Feed>, mpsc::UnboundedReceiver<Ask>) {
        let feed = Arc::new(Feed::new());
        let (asks, asked) = mpsc::unbounded_channel();
        let info = EngineInfo {
            device_id: device,
            // Slopty has no sign-in of its own: the tailnet decides who reaches what.
            workspace_scope: WorkspaceScope::Local,
            cursor_sdk_version: None,
            capabilities: capabilities(),
        };
        (Arc::new(Self { info, feed: Arc::clone(&feed), asks }), feed, asked)
    }

    /// A call the workspace answers.
    async fn ask(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        let (reply, answer) = oneshot::channel();
        self.asks.send(Ask::Call { method: method.to_owned(), params, reply }).map_err(closed)?;
        answer.await.map_err(closed)?
    }

    /// The transcript watch on `params.chatId`.
    fn transcript(&self, params: &serde_json::Value) -> Result<RpcReply, RpcError> {
        let chat = params
            .get("chatId")
            .and_then(serde_json::Value::as_str)
            .filter(|chat| !chat.is_empty())
            .ok_or_else(|| RpcError::BadParams("chatId is required".to_owned()))?
            .to_owned();
        let (entries, first) = self.feed.watch_transcript(&chat);
        if first {
            let _sent = self.asks.send(Ask::Follow(chat.clone()));
        }
        let closing = Closing { chat, feed: Arc::clone(&self.feed), asks: self.asks.clone() };
        Ok(RpcReply::Stream(transcript_frames(entries, closing)))
    }

    /// slopty: a watch of what `chat`'s thread says beside its transcript, read by `pick`: the
    /// thread is followed while it is open, as a transcript's watch follows it.
    fn chat_watch<T>(
        &self,
        chat: String,
        pick: impl FnOnce(&Followed) -> watch::Receiver<Option<Arc<T>>>,
    ) -> RpcReply
    where
        T: Serialize + Send + Sync + 'static,
    {
        let (rx, first) = self.feed.follow(&chat, pick);
        let ask = if first { Ask::Follow(chat.clone()) } else { Ask::Republish };
        let _sent = self.asks.send(ask);
        let closing = Closing { chat, feed: Arc::clone(&self.feed), asks: self.asks.clone() };
        let frames =
            futures::stream::unfold((rx, closing, true), async |(mut rx, closing, first)| {
                if !first {
                    rx.changed().await.ok()?;
                }
                let value = rx.wait_for(Option::is_some).await.ok()?.clone()?;
                Some((serde_json::to_value(&*value).ok()?, (rx, closing, false)))
            });
        RpcReply::Stream(frames.boxed())
    }

    /// slopty: `WatchRules`: a chat's rules, or those of the newest thread on a machine (in a
    /// folder) whose agent reads them, which the workspace picks.
    async fn rules(&self, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        let chat = if let Some(chat) = chat_param(&params) {
            chat
        } else {
            let picked = if let RpcReply::Value(value) = self.ask(RULES_CHAT, params).await? {
                value
            } else {
                serde_json::Value::Null
            };
            chat_param(&picked).ok_or_else(|| {
                RpcError::Failed("No chat on that machine reads permission rules".to_owned())
            })?
        };
        Ok(self.chat_watch(chat, |followed| followed.rules.subscribe()))
    }
}

/// slopty: the workspace's own call naming the chat whose rules `WatchRules` reads for
/// `{ deviceId, cwd? }`: `{ chatId }`.
pub(crate) const RULES_CHAT: &str = "slopty.rulesChat";

/// The `chatId` a call names.
fn chat_param(params: &serde_json::Value) -> Option<String> {
    params
        .get("chatId")
        .and_then(serde_json::Value::as_str)
        .filter(|chat| !chat.is_empty())
        .map(str::to_owned)
}

/// The workspace is gone: the app is closing.
fn closed(error: impl std::fmt::Display) -> RpcError {
    tracing::debug!(%error, "the workspace no longer answers the interface");
    RpcError::Closed
}

/// Unfollows a chat once its last transcript watch is dropped.
struct Closing {
    chat: String,
    feed: Arc<Feed>,
    asks: mpsc::UnboundedSender<Ask>,
}

impl Drop for Closing {
    fn drop(&mut self) {
        if self.feed.unwatch_transcript(&self.chat) {
            let _sent = self.asks.send(Ask::Unfollow(std::mem::take(&mut self.chat)));
        }
    }
}

/// A transcript as the shell's frames: the whole of it first, as history, then what changed.
///
/// While older turns are on their way the latest ones go as zeron's opening tail
/// (`historyPending`), and the whole history, once here, as one reset the shell takes as
/// history: older turns put in front never arrive as live ones. How full the context is, the
/// plan's rate windows and when a usage limit lifts ride every frame.
fn transcript_frames(
    said: watch::Receiver<Option<Arc<Transcript>>>,
    closing: Closing,
) -> BoxStream<'static, serde_json::Value> {
    let start = (said, None::<Arc<Transcript>>, closing, true);
    futures::stream::unfold(start, async |(mut said, last, closing, mut first)| {
        loop {
            if !first && said.changed().await.is_err() {
                return None;
            }
            first = false;
            let Some(next) = said.borrow_and_update().clone() else { continue };
            let whole = last.as_ref().is_none_or(|last| {
                last.history_pending || next.history_pending || put_in_front(last, &next)
            });
            let update = if whole {
                TranscriptUpdate {
                    replay_baseline: Some(TranscriptBaseline::capture(&next.entries)),
                    frame: TranscriptFrame::reset(&next.entries),
                    context_usage: next.context_usage,
                    limits: next.limits.clone(),
                    limit_resets_at: next.limit_resets_at,
                }
            } else {
                let last = last.as_ref()?;
                let frame = slopty_view_doc::diff_transcript(&last.entries, &next.entries);
                if frame.is_empty_delta()
                    && last.context_usage == next.context_usage
                    && last.limits == next.limits
                    && last.limit_resets_at == next.limit_resets_at
                {
                    continue;
                }
                TranscriptUpdate {
                    replay_baseline: None,
                    frame,
                    context_usage: next.context_usage,
                    limits: next.limits.clone(),
                    limit_resets_at: next.limit_resets_at,
                }
            };
            let mut value = serde_json::to_value(&update).ok()?;
            if next.history_pending
                && let Some(fields) = value.as_object_mut()
            {
                fields.insert("historyPending".to_owned(), serde_json::Value::Bool(true));
            }
            return Some((value, (said, Some(next), closing, false)));
        }
    })
    .boxed()
}

/// Whether `next` holds entries in front of everything `last` held: older turns came.
fn put_in_front(last: &Transcript, next: &Transcript) -> bool {
    let Some(first) = last.entries.first() else { return false };
    next.entries.iter().position(|entry| entry.id == first.id).is_some_and(|at| at > 0)
}

/// A list's watch: the list now, then each time it changes.
fn list_frames<T>(rx: watch::Receiver<Arc<[T]>>) -> BoxStream<'static, serde_json::Value>
where
    T: Serialize + Send + Sync + 'static,
{
    futures::stream::unfold((rx, true), async |(mut rx, first)| {
        if !first && rx.changed().await.is_err() {
            return None;
        }
        let value = serde_json::to_value(&**rx.borrow_and_update()).ok()?;
        Some((value, (rx, false)))
    })
    .boxed()
}

/// A list's watch once the list is known: the list then, then each time it changes.
fn known_list_frames<T>(
    rx: watch::Receiver<Option<Arc<[T]>>>,
) -> BoxStream<'static, serde_json::Value>
where
    T: Serialize + Send + Sync + 'static,
{
    futures::stream::unfold((rx, true), async |(mut rx, first)| {
        if !first {
            rx.changed().await.ok()?;
        }
        let list = rx.wait_for(Option::is_some).await.ok()?.clone()?;
        let value = serde_json::to_value(&*list).ok()?;
        Some((value, (rx, false)))
    })
    .boxed()
}

/// A watch of one value, a frame each time it changes.
fn value_frames<T>(rx: watch::Receiver<T>) -> BoxStream<'static, serde_json::Value>
where
    T: Serialize + Send + Sync + 'static,
{
    futures::stream::unfold((rx, true), async |(mut rx, first)| {
        if !first && rx.changed().await.is_err() {
            return None;
        }
        let value = serde_json::to_value(&*rx.borrow_and_update()).ok()?;
        Some((value, (rx, false)))
    })
    .boxed()
}

/// A watch whose one value never changes, held open so the interface does not ask again.
fn still<T: Serialize>(value: &T) -> Result<RpcReply, RpcError> {
    let value = serde_json::to_value(value)
        .map_err(|e| RpcError::Failed(format!("serialize response: {e}")))?;
    Ok(RpcReply::Stream(
        futures::stream::once(async move { value }).chain(futures::stream::pending()).boxed(),
    ))
}

/// The pull request of the checkout `cwd` on `device` with `branch` checked out, among `pulls`.
fn checkout_pull<'a>(pulls: &'a [Pull], device: &str, cwd: &str, branch: &str) -> Option<&'a Pull> {
    let cwd = cwd.trim_end_matches('/');
    pulls.iter().find(|pull| {
        pull.device == device
            && pull.folders.iter().any(|folder| folder.trim_end_matches('/') == cwd)
            && (branch.is_empty() || pull.branch.as_deref().is_none_or(|b| b == branch))
    })
}

impl ViewModel {
    /// `WatchCheckoutChangeRequest`: the pull request of a checkout's branch, as its threads'
    /// rows show it, said again each time it changes.
    fn change_request(&self, params: &serde_json::Value) -> RpcReply {
        let text = |key: &str| params.get(key).and_then(serde_json::Value::as_str).unwrap_or("");
        let device = Some(text("targetDeviceId"))
            .filter(|device| !device.is_empty())
            .map(str::to_owned)
            .or_else(|| self.feed.local_worker())
            .unwrap_or_default();
        let (cwd, branch) = (text("cwd").to_owned(), text("branch").to_owned());
        let pulls = self.feed.pulls.subscribe();
        let frames = futures::stream::unfold(
            (pulls, None::<Option<slopty_view_proto::ChangeRequestSummary>>),
            move |(mut pulls, last)| {
                let (device, cwd, branch) = (device.clone(), cwd.clone(), branch.clone());
                async move {
                    loop {
                        if last.is_some() {
                            pulls.changed().await.ok()?;
                        }
                        let now = checkout_pull(&pulls.borrow_and_update(), &device, &cwd, &branch)
                            .map(|pull| pull.summary.clone());
                        if last.as_ref() == Some(&now) {
                            continue;
                        }
                        let status = slopty_view_proto::CheckoutChangeRequestStatus {
                            checkout_id: cwd.clone(),
                            device_id: device.clone(),
                            cwd: cwd.clone(),
                            branch: branch.clone(),
                            change_request: now.clone(),
                            updated_at: chrono::Utc::now(),
                        };
                        let value = serde_json::to_value(&status).ok()?;
                        return Some((value, (pulls, Some(now))));
                    }
                }
            },
        );
        RpcReply::Stream(frames.boxed())
    }
}

#[async_trait]
impl RpcService for ViewModel {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        match method {
            methods::ENGINE_INFO => RpcReply::value(&self.info),
            methods::ENGINE_READY => RpcReply::value(&serde_json::json!({ "ready": true })),
            // This machine's worker, once the app knows whether there is one; the client's own
            // id when there is none.
            methods::LOCAL_DEVICE => {
                let mut local = self.feed.local.subscribe();
                let known =
                    local.wait_for(|local| *local != Local::Unknown).await.map(|l| l.clone());
                let device = match known {
                    Ok(Local::Known(Some(device))) => device,
                    _ => self.info.device_id.clone(),
                };
                RpcReply::value(&serde_json::json!({ "deviceId": device }))
            }
            methods::WATCH_DEVICES => {
                Ok(RpcReply::Stream(list_frames(self.feed.devices.subscribe())))
            }
            methods::WATCH_SPACES => {
                Ok(RpcReply::Stream(known_list_frames(self.feed.spaces.subscribe())))
            }
            methods::WATCH_CHATS => {
                Ok(RpcReply::Stream(known_list_frames(self.feed.chats.subscribe())))
            }
            methods::WATCH_SESSIONS => {
                Ok(RpcReply::Stream(list_frames(self.feed.sessions.subscribe())))
            }
            methods::WATCH_DOC_MESSAGES => self.transcript(&params),
            methods::WATCH_QUEUE => {
                let chat = params.get("chatId").and_then(serde_json::Value::as_str).unwrap_or("");
                let queue = self.feed.watch_queue(chat);
                // A queue the workspace has not published yet still reads from the start.
                let _sent = self.asks.send(Ask::Republish);
                let frames = futures::stream::unfold((queue, true), async |(mut queue, first)| {
                    if !first {
                        queue.changed().await.ok()?;
                    }
                    let items = queue.borrow_and_update().clone();
                    Some((serde_json::json!({ "items": &*items }), (queue, false)))
                });
                Ok(RpcReply::Stream(frames.boxed()))
            }
            methods::WATCH_CHECKOUT_CHANGE_REQUEST => Ok(self.change_request(&params)),
            // Pins and sections are this client's own, kept by the workspace.
            methods::WATCH_SIDEBAR_PREFERENCES => {
                let sidebar = self.feed.sidebar.subscribe();
                let _sent = self.asks.send(Ask::Republish);
                let frames = futures::stream::unfold((sidebar, true), async |(mut rx, first)| {
                    if !first {
                        rx.changed().await.ok()?;
                    }
                    let state = rx.wait_for(Option::is_some).await.ok()?.clone()?;
                    Some((serde_json::to_value(&state).ok()?, (rx, false)))
                });
                Ok(RpcReply::Stream(frames.boxed()))
            }
            methods::WATCH_CONNECTIVITY => {
                Ok(RpcReply::Stream(value_frames(self.feed.connectivity.subscribe())))
            }
            methods::WATCH_TRANSFERS => {
                Ok(RpcReply::Stream(list_frames(self.feed.transfers.subscribe())))
            }
            methods::AUTH_STATUS => still(&serde_json::json!({ "_tag": "SignedOut" })),
            // slopty: a chat's tray, dock and rules, followed as its transcript is.
            calls::WATCH_WORK | calls::WATCH_DOCK => {
                let chat = chat_param(&params)
                    .ok_or_else(|| RpcError::BadParams("chatId is required".to_owned()))?;
                Ok(if method == calls::WATCH_WORK {
                    self.chat_watch(chat, |followed| followed.work.subscribe())
                } else {
                    self.chat_watch(chat, |followed| followed.dock.subscribe())
                })
            }
            calls::WATCH_RULES => self.rules(params).await,
            _ => self.ask(method, params).await,
        }
    }
}

#[cfg(test)]
mod tests;

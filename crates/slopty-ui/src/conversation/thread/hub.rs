//! One worker's threads on this client, as an entity every thread view of that worker reads.
//!
//! It holds [`slopty_client::threads::Threads`] fed by the link, keeps it on disk off the UI
//! thread, and asks the workspace to send what it hands back.
//!
//! The workspace makes one hub per worker, feeds it what the link brings ([`ThreadHub::table`],
//! [`ThreadHub::frame`], [`ThreadHub::done`], [`ThreadHub::connected`],
//! [`ThreadHub::disconnected`]) and sends what [`HubEvent::Send`] carries. A view calls the
//! rest, and what a person does shows in the frame they did it: the hub notifies before it asks
//! for anything to be sent.
//!
//! An intent the worker turns down is said in words where its thread shows. A message and a
//! change to one hold the person's words and stand on their own lines; every other refusal
//! (an answer, a stop, a model, a keep) is kept here as a [`Refusal`] until the person lets it
//! go, since the thread's own state would otherwise show nothing happened. An agent that
//! exited is taken up again in a terminal of its own (`docs/decisions/agents.md`), never here.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use gpui::{Context, EventEmitter, Task};
use slopty_client::threads::{Cache, Cached, Changed, Outbox, Threads};
use slopty_proto::git::GitOutcome;
use slopty_proto::thread::wire::{
    Intent, IntentDone, Outcome, Review, ReviewScope, TableFrame, ThreadFrame,
};
use slopty_proto::thread::{AgentId, ContentRef, IntentId, ThreadId};
use slopty_proto::{ClientMsg, RequestId};

use super::git::GitBook;

/// How long a thread rests before what it is now is kept on disk: a busy one is written once
/// it settles, not on every word.
const KEEP_AFTER: Duration = Duration::from_secs(2);

/// Refusals kept at most, the oldest let go first: a person reads the last few.
const REFUSALS: usize = 32;

/// An intent the worker turned down whose thread would show nothing of it: why, in words,
/// until the person lets it go.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Refusal {
    /// The intent.
    pub id: IntentId,
    /// Its thread.
    pub thread: ThreadId,
    /// What was asked.
    pub intent: Intent,
    /// What was turned down and why, as the person reads it.
    pub words: String,
}

/// What a hub tells the workspace and its views.
#[derive(Clone, Debug, PartialEq)]
pub enum HubEvent {
    /// Send these to the worker, in order.
    Send(Vec<ClientMsg>),
    /// The table changed.
    Table,
}

/// Writes waiting for the disk, the newest of each.
#[derive(Default)]
struct Writes {
    threads: HashMap<ThreadId, Cached>,
    outbox: Option<Outbox>,
}

impl Writes {
    fn is_empty(&self) -> bool {
        self.threads.is_empty() && self.outbox.is_none()
    }
}

/// One worker's threads.
pub struct ThreadHub {
    worker: String,
    threads: Threads,
    cache: Option<Cache>,
    /// Views open on each thread: it is followed while one is.
    views: HashMap<ThreadId, usize>,
    /// Threads waiting out [`KEEP_AFTER`] before they are kept.
    resting: HashMap<ThreadId, Task<()>>,
    writes: Writes,
    /// The task writing, while one is: writes go one after another, so an older one never
    /// lands over a newer.
    writing: Option<Task<()>>,
    /// Intents turned down that their threads would not show, oldest first.
    refusals: Vec<Refusal>,
    /// The git ops asked of this worker's repositories, and what each last said.
    git: GitBook,
    /// The agents this worker can start a thread of, as its link says.
    agents: Vec<AgentId>,
}

impl std::fmt::Debug for ThreadHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ThreadHub")
            .field("worker", &self.worker)
            .field("open", &self.views)
            .finish_non_exhaustive()
    }
}

impl EventEmitter<HubEvent> for ThreadHub {}

impl ThreadHub {
    /// The threads of the worker named `worker`, kept in `cache`; what the cache kept of the
    /// outbox goes again once linked.
    #[must_use]
    pub fn new(worker: String, cache: Option<Cache>) -> Self {
        let outbox = cache.as_ref().map(Cache::outbox).unwrap_or_default();
        Self {
            worker,
            threads: Threads::new(outbox),
            cache,
            views: HashMap::new(),
            resting: HashMap::new(),
            writes: Writes::default(),
            writing: None,
            refusals: Vec::new(),
            git: GitBook::default(),
            agents: Vec::new(),
        }
    }

    /// The worker's link says which agents it can start.
    pub fn set_agents(&mut self, agents: Vec<AgentId>, cx: &mut Context<Self>) {
        if self.agents != agents {
            self.agents = agents;
            cx.notify();
        }
    }

    /// The threads.
    #[must_use]
    pub const fn threads(&self) -> &Threads {
        &self.threads
    }

    // ----- what the link brings ----------------------------------------------------------

    /// The link came up.
    pub fn connected(&mut self, cx: &mut Context<Self>) {
        let out = self.threads.connected();
        cx.notify();
        cx.emit(HubEvent::Send(out));
    }

    /// The link went.
    pub fn disconnected(&mut self, cx: &mut Context<Self>) {
        self.threads.disconnected();
        self.git.lost();
        cx.notify();
    }

    /// A frame of the table.
    pub fn table(&mut self, frame: &TableFrame, cx: &mut Context<Self>) {
        self.threads.table(frame);
        self.keep_outbox(cx);
        cx.emit(HubEvent::Table);
        cx.notify();
    }

    /// A frame of `thread`'s stream.
    pub fn frame(&mut self, thread: ThreadId, frame: ThreadFrame, cx: &mut Context<Self>) {
        let (changed, out) = self.threads.frame(thread, frame);
        match changed {
            Changed::Nothing => {}
            Changed::Thread => {
                self.rest(thread, cx);
                self.keep_outbox(cx);
                cx.notify();
            }
            Changed::Expanded(_) | Changed::Review => cx.notify(),
        }
        if !out.is_empty() {
            cx.emit(HubEvent::Send(out));
        }
    }

    /// The worker's answer to an intent. One it turned down that the thread would not show is
    /// kept, in words, as a [`Refusal`].
    pub fn done(&mut self, done: &IntentDone, cx: &mut Context<Self>) {
        let sent = self.threads.outbox().all().iter().find(|s| s.id == done.id).cloned();
        if self.threads.done(done) {
            self.keep_outbox(cx);
            if let Some(sent) = sent
                && let Some(why) =
                    turned_down(&done.outcome).filter(|_| !speaks_for_itself(&sent.intent))
            {
                let words = refused_words(&sent.intent, &why);
                self.refuse(Refusal {
                    id: sent.id,
                    thread: sent.thread,
                    intent: sent.intent,
                    words,
                });
            }
            cx.notify();
        }
    }

    fn refuse(&mut self, refusal: Refusal) {
        self.refusals.push(refusal);
        let over = self.refusals.len().saturating_sub(REFUSALS);
        self.refusals.drain(..over);
    }

    // ----- what views ask ----------------------------------------------------------------

    /// A view opened on `thread`: drawn from the cache in this frame when it kept it, and
    /// followed while any view is open on it.
    ///
    /// The cache is read here, on the UI thread: it is the one read that draws a thread's
    /// first frame, and waiting for it off the thread would draw an empty one first
    /// (`docs/MEASUREMENTS.md`, "a cached thread's first frame").
    pub fn open(&mut self, thread: ThreadId, cx: &mut Context<Self>) {
        let views = self.views.entry(thread).or_default();
        *views = views.saturating_add(1);
        if *views > 1 {
            return;
        }
        let cached = self.cache.as_ref().and_then(|c| c.thread(thread));
        let out = self.threads.open_thread(thread, cached);
        cx.notify();
        if let Some(msg) = out {
            cx.emit(HubEvent::Send(vec![msg]));
        }
    }

    /// A view on `thread` closed: the last one unfollows it, and what it was is kept.
    pub fn close(&mut self, thread: ThreadId, cx: &mut Context<Self>) {
        let Some(views) = self.views.get_mut(&thread) else { return };
        *views = views.saturating_sub(1);
        if *views > 0 {
            return;
        }
        self.views.remove(&thread);
        self.resting.remove(&thread);
        let (cached, out) = self.threads.close_thread(thread);
        if let Some(cached) = cached {
            self.writes.threads.insert(thread, cached);
            self.flush(cx);
        }
        if let Some(msg) = out {
            cx.emit(HubEvent::Send(vec![msg]));
        }
    }

    /// Ask `thread` to do `intent`. It shows at once; it goes now, or once the link is back.
    pub fn intent(&mut self, thread: ThreadId, intent: Intent, cx: &mut Context<Self>) -> IntentId {
        let id = IntentId::new();
        self.intent_as(id, thread, intent, cx);
        id
    }

    /// Ask `thread` to do `intent` under `id`, the one its origin gave it: as [`Self::intent`].
    pub fn intent_as(
        &mut self,
        id: IntentId,
        thread: ThreadId,
        intent: Intent,
        cx: &mut Context<Self>,
    ) {
        let out = self.threads.intent_as(id, thread, intent);
        self.keep_outbox(cx);
        cx.notify();
        if let Some(msg) = out {
            cx.emit(HubEvent::Send(vec![msg]));
        }
    }

    /// What `thread` was turned down that it would not show, oldest first.
    pub fn refusals(&self, thread: ThreadId) -> impl DoubleEndedIterator<Item = &Refusal> {
        self.refusals.iter().filter(move |r| r.thread == thread)
    }

    /// Whether anything was asked under `id` here.
    #[must_use]
    pub fn asked(&self, id: IntentId) -> bool {
        self.threads.asked(id)
    }

    /// Ask what `thread` changed over `scope`.
    pub fn ask_review(&mut self, thread: ThreadId, scope: ReviewScope, cx: &mut Context<Self>) {
        if let Some(msg) = self.threads.ask_review(thread, scope) {
            cx.emit(HubEvent::Send(vec![msg]));
        }
    }

    /// `thread`'s last review over `scope`.
    #[must_use]
    pub fn review(&self, thread: ThreadId, scope: &ReviewScope) -> Option<&Arc<Review>> {
        self.threads.review(thread, scope)
    }

    /// Ask for the whole of `content` in `thread`, unless it is held or on its way; the hub
    /// notifies once it came ([`Self::whole`]).
    pub fn expand(&mut self, thread: ThreadId, content: &ContentRef, cx: &mut Context<Self>) {
        if let Some(msg) = self.threads.expand(thread, content) {
            cx.emit(HubEvent::Send(vec![msg]));
        }
    }

    /// The whole of `content`, once it came.
    #[must_use]
    pub fn whole(
        &self,
        content: &ContentRef,
    ) -> Option<&Arc<slopty_proto::thread::wire::Expanded>> {
        self.threads.whole(content)
    }

    /// Ask for `turns` of `thread`'s turns before the first one held, unless it holds them all
    /// or that page is on its way.
    pub fn page(&mut self, thread: ThreadId, turns: u32, cx: &mut Context<Self>) {
        if let Some(msg) = self.threads.page(thread, turns) {
            cx.emit(HubEvent::Send(vec![msg]));
        }
    }

    // ----- git ---------------------------------------------------------------------------

    /// The worker answered git op `request`: what the repository said shows, and what goes
    /// after it (a push, a fresh status) is sent.
    pub fn git_done(&mut self, request: RequestId, outcome: GitOutcome, cx: &mut Context<Self>) {
        if let Some((_repo, then)) = self.git.answer(request, outcome) {
            self.git_sent(then, cx);
        }
    }

    fn git_sent(&self, msgs: Vec<ClientMsg>, cx: &mut Context<Self>) {
        cx.notify();
        if self.threads.linked() && !msgs.is_empty() {
            cx.emit(HubEvent::Send(msgs));
        }
    }

    // ----- the cache -------------------------------------------------------------------

    /// Keep `thread` once it has rested [`KEEP_AFTER`].
    fn rest(&mut self, thread: ThreadId, cx: &Context<Self>) {
        if self.cache.is_none() {
            return;
        }
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(KEEP_AFTER).await;
            let _gone = this.update(cx, |this, cx| {
                this.resting.remove(&thread);
                if let Some(cached) = this.threads.to_cache(thread) {
                    this.writes.threads.insert(thread, cached);
                    this.flush(cx);
                }
            });
        });
        self.resting.insert(thread, task);
    }

    fn keep_outbox(&mut self, cx: &Context<Self>) {
        if self.threads.take_outbox_changed() && self.cache.is_some() {
            self.writes.outbox = Some(self.threads.outbox().clone());
            self.flush(cx);
        }
    }

    /// Write what waits, off the UI thread, one batch after another.
    fn flush(&mut self, cx: &Context<Self>) {
        if self.writing.is_some() || self.writes.is_empty() {
            return;
        }
        let Some(cache) = self.cache.clone() else {
            self.writes = Writes::default();
            return;
        };
        self.writing = Some(cx.spawn(async move |this, cx| {
            loop {
                let taken = this.update(cx, |this, _cx| {
                    let batch = std::mem::take(&mut this.writes);
                    // Done in the same update that found nothing left, so a write asked for
                    // after it starts a new task rather than waiting on this one.
                    if batch.is_empty() {
                        this.writing = None;
                    }
                    batch
                });
                let Ok(batch) = taken else { return };
                if batch.is_empty() {
                    return;
                }
                let cache = cache.clone();
                cx.background_executor().spawn(async move { write(&cache, batch) }).await;
            }
        }));
    }

    /// The worker's name for people.
    #[must_use]
    pub fn worker(&self) -> &str {
        &self.worker
    }

    /// Whether the link to the worker is up, so what is asked goes now.
    #[must_use]
    pub const fn linked(&self) -> bool {
        self.threads.linked()
    }

    /// The git ops asked of this worker's repositories, and what each last said.
    #[must_use]
    pub const fn git(&self) -> &GitBook {
        &self.git
    }
}

/// Why the worker turned an intent down, when it did, in words.
fn turned_down(outcome: &Outcome) -> Option<String> {
    match outcome {
        Outcome::Refused { reason } => Some(reason.trim().trim_end_matches('.').to_owned()),
        Outcome::Unsupported { .. } => Some("the agent can't do that through Slopty".to_owned()),
        Outcome::SetupFailed { .. } => Some("the worktree's setup failed".to_owned()),
        Outcome::Done | Outcome::Accepted | Outcome::Started { .. } => None,
    }
}

/// Whether the thread view draws `intent`'s refusal on its own line: a message and a change to
/// one hold the person's words and stay until dismissed.
const fn speaks_for_itself(intent: &Intent) -> bool {
    matches!(intent, Intent::Send { .. } | Intent::Edit { .. })
}

/// What was turned down and why, as the person reads it: "Couldn't stop: the agent is not
/// working".
#[must_use]
pub fn refused_words(intent: &Intent, why: &str) -> String {
    let what = match intent {
        Intent::Send { .. } => "Not sent".to_owned(),
        Intent::Edit { .. } => "Not changed".to_owned(),
        Intent::Withdraw { .. } => "Couldn't take the message back".to_owned(),
        Intent::Promote { .. } => "Couldn't send the message now".to_owned(),
        Intent::Move { .. } => "Couldn't reorder the queue".to_owned(),
        Intent::Interrupt => "Couldn't stop".to_owned(),
        Intent::Answer { .. } => "The answer didn't go".to_owned(),
        Intent::SetModel { model } => format!("Couldn't switch to {model}"),
        Intent::SetMode { mode } => format!("Couldn't switch to {mode}"),
        Intent::Keep(pick) => format!("Couldn't keep {}", file_name(&pick.path)),
        Intent::Revert(pick) => format!("Couldn't revert {}", file_name(&pick.path)),
        Intent::Fork { .. } => "Couldn't fork the thread".to_owned(),
        Intent::SetEffort { effort } => format!("Couldn't set the effort to {effort}"),
        Intent::Aside => "Couldn't ask aside".to_owned(),
        Intent::Discard => "Couldn't close the thread".to_owned(),
        Intent::KeepAside => "Couldn't keep the aside".to_owned(),
        Intent::Review { .. } => "The review didn't start".to_owned(),
        Intent::Seen { .. } => "Couldn't mark it seen".to_owned(),
        Intent::Draft { .. } => "Couldn't keep the draft".to_owned(),
        Intent::Name { .. } => "Couldn't rename the chat".to_owned(),
        Intent::Said { .. } => "Couldn't keep the message's own words".to_owned(),
        Intent::End => "Couldn't end the agent".to_owned(),
        Intent::SetOption { option, choice } => format!("Couldn't set {option} to {choice}"),
        Intent::StopTask { .. } => "Couldn't stop it".to_owned(),
        Intent::Background { .. } => "Couldn't move it to the background".to_owned(),
        Intent::Shell { .. } => "The command didn't run".to_owned(),
        Intent::Rewind { .. } => "Couldn't put the files back".to_owned(),
        Intent::RewindConversation { .. } => "Couldn't take the conversation back".to_owned(),
        Intent::Unrewind { .. } => "Couldn't undo the rewind".to_owned(),
        Intent::Rule(_) => "Couldn't change the rule".to_owned(),
        Intent::Surface { .. } => "The mod didn't take it".to_owned(),
        Intent::Side { .. } => "Couldn't ask the side question".to_owned(),
        Intent::McpReconnect { server } => format!("Couldn't reconnect {server}"),
        Intent::McpEnable { server, enabled: true } => format!("Couldn't enable {server}"),
        Intent::McpEnable { server, enabled: false } => format!("Couldn't disable {server}"),
    };
    format!("{what}: {why}")
}

/// Whether `row` is an aside of another thread (`ThreadMeta::aside_of`): it shows only in its
/// thread's sheet, so the lists that show threads, what needs the person and the notes pass
/// over it.
#[must_use]
pub fn is_aside(row: &slopty_proto::thread::wire::ThreadRow) -> bool {
    row.facts.contains_key(slopty_proto::thread::ThreadMeta::ASIDE_FACT)
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().filter(|n| !n.is_empty()).unwrap_or(path)
}

fn write(cache: &Cache, batch: Writes) {
    for (thread, cached) in batch.threads {
        if let Err(error) = cache.keep_thread(thread, &cached) {
            tracing::warn!(%error, %thread, "thread not kept");
        }
    }
    if let Some(outbox) = batch.outbox
        && let Err(error) = cache.keep_outbox(&outbox)
    {
        tracing::warn!(%error, "outbox not kept");
    }
}

/// An agent's name as a menu lists it: Claude Code, Codex, pi, or another by its own name.
#[must_use]
pub fn agent_label(agent: &AgentId) -> String {
    match agent.0.as_str() {
        AgentId::CLAUDE_CODE => "Claude Code".to_owned(),
        AgentId::CODEX => "Codex".to_owned(),
        AgentId::PI => "pi".to_owned(),
        other => other
            .strip_prefix("acp:")
            .filter(|name| !name.is_empty())
            .unwrap_or("the agent")
            .to_owned(),
    }
}

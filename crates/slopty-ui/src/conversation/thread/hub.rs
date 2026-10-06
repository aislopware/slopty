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
//! go, since the thread's own state would otherwise show nothing happened. A thread's agent
//! that exited is taken up again from here too ([`ThreadHub::resume`]): a start of its own
//! session, answered as any start is.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use gpui::{Context, EventEmitter, Task};
use slopty_client::threads::{Cache, Cached, Changed, Outbox, Stamp, Threads};
use slopty_proto::git::{GitOp, GitOutcome};
use slopty_proto::thread::wire::{
    Authors, Expanded, Intent, IntentDone, Outcome, Review, ReviewScope, SEARCH_THREADS, Start,
    TableFrame, ThreadFrame, ThreadHits, ThreadRequest,
};
use slopty_proto::thread::{AgentId, ContentRef, IntentId, ThreadId};
use slopty_proto::{ClientMsg, RequestId};

use super::git::GitBook;

/// How long a thread rests before what it is now is kept on disk: a busy one is written once
/// it settles, not on every word.
const KEEP_AFTER: Duration = Duration::from_secs(2);

/// Turns asked for by a page of older ones.
const PAGE_TURNS: u32 = 10;

/// Refusals kept at most, the oldest let go first: a person reads the last few.
const REFUSALS: usize = 32;

/// An intent the worker turned down whose thread would show nothing of it: why, in words,
/// until the person lets it go.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Refusal {
    /// The intent, or the start that took the thread up again.
    pub id: IntentId,
    /// Its thread.
    pub thread: ThreadId,
    /// What was asked; `None` for a start.
    pub intent: Option<Intent>,
    /// What was turned down and why, as the person reads it.
    pub words: String,
}

/// What a hub tells the workspace and its views.
#[derive(Clone, Debug, PartialEq)]
pub enum HubEvent {
    /// Send these to the worker, in order.
    Send(Vec<ClientMsg>),
    /// `thread` moved on, or this client's intents for it did.
    Thread(ThreadId),
    /// The table changed.
    Table,
    /// The whole of some clipped content came.
    Expanded(ContentRef),
    /// `thread`'s review came.
    Review(ThreadId),
    /// A git op on the repository holding this folder was asked or answered.
    Git(String),
    /// What the worker found in its threads for the words last asked
    /// ([`ThreadHub::search`]).
    Hits,
    /// The worker said who wrote the lines of a file ([`ThreadHub::authors`]).
    Authors,
    /// An intent on `from` started `thread`: a fork, a go in another agent, an edit from a
    /// turn. The workspace opens it, unless it is an aside, which its view shows.
    Started {
        /// The thread the person acted on.
        from: ThreadId,
        /// The thread it started.
        thread: ThreadId,
        /// The intent that started it.
        intent: IntentId,
        /// It is an aside (`Intent::Aside`), shown in a sheet over `from`'s view.
        aside: bool,
    },
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
    /// Starts that take an exited thread's session up again, not yet answered: each goes again
    /// under its id when the link comes back.
    resuming: HashMap<IntentId, (ThreadId, Start)>,
    /// Threads to take up again as soon as they are known here and their agent has exited:
    /// a closed agent tile reopened after its session ended.
    waking: HashSet<ThreadId>,
    /// The git ops asked of this worker's repositories, and what each last said.
    git: GitBook,
    /// The agents this worker can start a thread of, as its link says.
    agents: Vec<AgentId>,
    /// The words last searched for in this worker's threads, and what it found for them once
    /// it answered.
    searched: Option<(String, Option<ThreadHits>)>,
    /// The words a new thread's composer opens with, by the intent that starts it.
    seeds: HashMap<IntentId, String>,
    /// The same, by the thread it started, until a view takes them.
    seeded: HashMap<ThreadId, String>,
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
            resuming: HashMap::new(),
            waking: HashSet::new(),
            git: GitBook::default(),
            agents: Vec::new(),
            searched: None,
            seeds: HashMap::new(),
            seeded: HashMap::new(),
        }
    }

    /// The worker's name for people.
    #[must_use]
    pub fn worker(&self) -> &str {
        &self.worker
    }

    /// The agents this worker can start a thread of: what "Continue in…" offers.
    #[must_use]
    pub fn agents(&self) -> &[AgentId] {
        &self.agents
    }

    /// The worker's link says which agents it can start.
    pub fn set_agents(&mut self, agents: Vec<AgentId>, cx: &mut Context<Self>) {
        if self.agents != agents {
            self.agents = agents;
            cx.notify();
        }
    }

    /// Ask the worker what was said in its threads with every word of `query`; the answer
    /// comes back to [`Self::thread_hits`]. The same words again, or none, ask nothing.
    pub fn search(&mut self, query: &str, cx: &mut Context<Self>) {
        let query = query.trim();
        if query.is_empty() {
            self.searched = None;
            return;
        }
        if self.searched.as_ref().is_some_and(|(asked, _)| asked == query) || !self.threads.linked()
        {
            return;
        }
        self.searched = Some((query.to_owned(), None));
        let limit = SEARCH_THREADS;
        let ask = ThreadRequest::Search { query: query.to_owned(), limit };
        cx.emit(HubEvent::Send(vec![ClientMsg::Thread(ask)]));
    }

    /// What the worker found; an answer for words no longer asked is dropped.
    pub fn thread_hits(&mut self, hits: ThreadHits, cx: &mut Context<Self>) {
        let Some((asked, found)) = &mut self.searched else { return };
        if *asked != hits.query {
            return;
        }
        *found = Some(hits);
        cx.emit(HubEvent::Hits);
        cx.notify();
    }

    /// Who wrote the lines of `path` (absolute, or in `thread`'s repository) as `stamp` read it,
    /// once the worker has said; until then it is asked, once, and [`HubEvent::Authors`] says
    /// when it came.
    pub fn authors(
        &mut self,
        thread: Option<ThreadId>,
        path: &str,
        stamp: &Stamp,
        cx: &mut Context<Self>,
    ) -> Option<Arc<Authors>> {
        let held = self.threads.authors(thread, path, stamp);
        if held.is_none()
            && let Some(msg) = self.threads.ask_authors(thread, path, stamp)
        {
            cx.emit(HubEvent::Send(vec![msg]));
        }
        held
    }

    /// The worker said who wrote the lines of a file.
    pub fn heard_authors(&mut self, authors: Authors, cx: &mut Context<Self>) {
        self.threads.heard_authors(authors);
        cx.emit(HubEvent::Authors);
        cx.notify();
    }

    /// What the worker found for `query`, once it answered.
    #[must_use]
    pub fn hits(&self, query: &str) -> Option<&ThreadHits> {
        let (asked, found) = self.searched.as_ref()?;
        (asked == query.trim()).then_some(found.as_ref()?)
    }

    /// The threads.
    #[must_use]
    pub const fn threads(&self) -> &Threads {
        &self.threads
    }

    // ----- what the link brings ----------------------------------------------------------

    /// The link came up.
    pub fn connected(&mut self, cx: &mut Context<Self>) {
        let mut out = self.threads.connected();
        out.extend(self.resuming.iter().map(|(id, (_, start))| start_msg(*id, start)));
        cx.notify();
        cx.emit(HubEvent::Send(out));
    }

    /// The link went.
    pub fn disconnected(&mut self, cx: &mut Context<Self>) {
        self.threads.disconnected();
        self.git.lost();
        self.searched = None;
        for thread in self.threads.open() {
            cx.emit(HubEvent::Thread(thread));
        }
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
                self.wake(thread, cx);
                self.rest(thread, cx);
                self.keep_outbox(cx);
                cx.emit(HubEvent::Thread(thread));
                cx.notify();
            }
            Changed::Expanded(content) => {
                cx.emit(HubEvent::Expanded(content));
                cx.notify();
            }
            Changed::Review => {
                cx.emit(HubEvent::Review(thread));
                cx.notify();
            }
        }
        if !out.is_empty() {
            cx.emit(HubEvent::Send(out));
        }
    }

    /// The worker's answer to an intent, or to a start that takes a thread up again. One it
    /// turned down that the thread would not show is kept, in words, as a [`Refusal`].
    pub fn done(&mut self, done: &IntentDone, cx: &mut Context<Self>) {
        if let Some((thread, _start)) = self.resuming.remove(&done.id) {
            if let Some(why) = turned_down(&done.outcome) {
                let words = format!("Couldn't resume: {why}");
                self.refuse(Refusal { id: done.id, thread, intent: None, words });
            }
            cx.emit(HubEvent::Thread(thread));
            cx.notify();
            return;
        }
        let sent = self.threads.outbox().all().iter().find(|s| s.id == done.id).cloned();
        if self.threads.done(done) {
            self.keep_outbox(cx);
            let seed = self.seeds.remove(&done.id);
            if let Some(sent) = sent {
                let why = turned_down(&done.outcome).filter(|_| !speaks_for_itself(&sent.intent));
                if let Outcome::Started { thread } = done.outcome {
                    if let Some(seed) = seed {
                        self.seeded.insert(thread, seed);
                    }
                    let aside = matches!(sent.intent, Intent::Aside);
                    cx.emit(HubEvent::Started {
                        from: sent.thread,
                        thread,
                        intent: sent.id,
                        aside,
                    });
                }
                // An aside kept is a thread of its own now: the workspace opens it as it opens
                // a fork.
                if matches!(sent.intent, Intent::KeepAside) && done.outcome == Outcome::Done {
                    let thread = sent.thread;
                    cx.emit(HubEvent::Started {
                        from: thread,
                        thread,
                        intent: sent.id,
                        aside: false,
                    });
                }
                if let Some(why) = why {
                    let words = refused_words(&sent.intent, &why);
                    self.refuse(Refusal {
                        id: sent.id,
                        thread: sent.thread,
                        intent: Some(sent.intent),
                        words,
                    });
                }
                cx.emit(HubEvent::Thread(sent.thread));
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
        cx.emit(HubEvent::Thread(thread));
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
        let (id, out) = self.threads.intent(thread, intent);
        self.keep_outbox(cx);
        cx.emit(HubEvent::Thread(thread));
        cx.notify();
        if let Some(msg) = out {
            cx.emit(HubEvent::Send(vec![msg]));
        }
        id
    }

    /// [`ThreadHub::intent`] for an intent that starts a thread, whose composer then opens
    /// with `seed`.
    pub fn intent_seeded(
        &mut self,
        thread: ThreadId,
        intent: Intent,
        seed: String,
        cx: &mut Context<Self>,
    ) -> IntentId {
        let id = self.intent(thread, intent, cx);
        self.seeds.insert(id, seed);
        id
    }

    /// The words `thread`'s composer opens with, given once to the first view of it.
    pub fn take_seed(&mut self, thread: ThreadId) -> Option<String> {
        self.seeded.remove(&thread)
    }

    /// Let a failed intent go once the person has read why: one the thread draws itself, or
    /// a [`Refusal`].
    pub fn dismiss(&mut self, id: IntentId, cx: &mut Context<Self>) {
        if let Some(gone) = self.threads.dismiss(id) {
            self.keep_outbox(cx);
            cx.emit(HubEvent::Thread(gone.thread));
            cx.notify();
        }
        if let Some(at) = self.refusals.iter().position(|r| r.id == id) {
            let gone = self.refusals.remove(at);
            cx.emit(HubEvent::Thread(gone.thread));
            cx.notify();
        }
    }

    /// What `thread` was turned down that it would not show, oldest first.
    pub fn refusals(&self, thread: ThreadId) -> impl DoubleEndedIterator<Item = &Refusal> {
        self.refusals.iter().filter(move |r| r.thread == thread)
    }

    /// Take `thread`'s exited agent up again on its own session, as `start` says: it shows
    /// as resuming at once, and goes now or once the link is back. A refusal is kept in words.
    pub fn resume(&mut self, thread: ThreadId, start: Start, cx: &mut Context<Self>) -> IntentId {
        let id = IntentId::new();
        let msg = self.threads.linked().then(|| start_msg(id, &start));
        self.resuming.insert(id, (thread, start));
        cx.emit(HubEvent::Thread(thread));
        cx.notify();
        if let Some(msg) = msg {
            cx.emit(HubEvent::Send(vec![msg]));
        }
        id
    }

    /// Take `thread` up again once it is known here, by its agent's own door
    /// ([`crate::conversation::thread::view::exited::gone`]): a start of its session, where its
    /// agent takes one. One whose next message starts it, one that cannot go on, and one still
    /// running are left as they are.
    pub fn resume_when_known(&mut self, thread: ThreadId, cx: &mut Context<Self>) {
        self.waking.insert(thread);
        self.wake(thread, cx);
    }

    /// `thread` is known now: take it up again if it was asked to be.
    fn wake(&mut self, thread: ThreadId, cx: &mut Context<Self>) {
        if !self.waking.contains(&thread) {
            return;
        }
        let Some(state) = self.threads.mirror(thread).and_then(|m| m.state()) else { return };
        self.waking.remove(&thread);
        let gone = crate::conversation::thread::view::exited::gone(state);
        if let Some(crate::conversation::thread::view::exited::Gone::Resume(start)) = gone
            && !self.resuming(thread)
        {
            let _id = self.resume(thread, *start, cx);
        }
    }

    /// Whether a start taking `thread` up again is on its way.
    #[must_use]
    pub fn resuming(&self, thread: ThreadId) -> bool {
        self.resuming.values().any(|(t, _)| *t == thread)
    }

    /// The whole of `content`, when it came; asked for once, when it did not.
    pub fn expanded(
        &mut self,
        thread: ThreadId,
        content: &ContentRef,
        cx: &mut Context<Self>,
    ) -> Option<Arc<Expanded>> {
        let held = self.threads.expanded(content);
        if held.is_none()
            && let Some(msg) = self.threads.expand(thread, content)
        {
            cx.emit(HubEvent::Send(vec![msg]));
        }
        held
    }

    /// Ask for older turns of `thread`.
    pub fn page(&self, thread: ThreadId, cx: &mut Context<Self>) {
        if let Some(msg) = self.threads.page(thread, PAGE_TURNS) {
            cx.emit(HubEvent::Send(vec![msg]));
        }
    }

    /// Ask what `thread` changed over `scope`.
    pub fn ask_review(&self, thread: ThreadId, scope: ReviewScope, cx: &mut Context<Self>) {
        if let Some(msg) = self.threads.ask_review(thread, scope) {
            cx.emit(HubEvent::Send(vec![msg]));
        }
    }

    /// `thread`'s last review.
    #[must_use]
    pub fn review(&self, thread: ThreadId) -> Option<&Arc<Review>> {
        self.threads.review(thread)
    }

    // ----- git ---------------------------------------------------------------------------

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

    /// Ask `op` of the repository holding the folder `repo`. Out of reach, nothing is asked
    /// and the repository says so: an op is never held for later, since what it would do may
    /// no longer be what the person meant.
    pub fn git_op(&mut self, repo: &str, op: GitOp, cx: &mut Context<Self>) -> Option<RequestId> {
        if !self.threads.linked() {
            self.git.offline(repo, &op);
            self.git_sent(repo, Vec::new(), cx);
            return None;
        }
        let (request, msg) = self.git.ask(repo, op);
        self.git_sent(repo, vec![msg], cx);
        Some(request)
    }

    /// Commit `paths` of the repository holding `repo` with the person's `message`, then push
    /// once the commit is made.
    pub fn commit_and_push(
        &mut self,
        repo: &str,
        paths: Vec<String>,
        message: String,
        cx: &mut Context<Self>,
    ) -> Option<RequestId> {
        if !self.threads.linked() {
            self.git.offline(repo, &GitOp::Push);
            self.git_sent(repo, Vec::new(), cx);
            return None;
        }
        let (request, msg) = self.git.commit_and_push(repo, paths, message);
        self.git_sent(repo, vec![msg], cx);
        Some(request)
    }

    /// The worker answered git op `request`: what the repository said shows, and what goes
    /// after it (a push, a fresh status) is sent.
    pub fn git_done(&mut self, request: RequestId, outcome: GitOutcome, cx: &mut Context<Self>) {
        if let Some((repo, then)) = self.git.answer(request, outcome) {
            self.git_sent(&repo, then, cx);
        }
    }

    fn git_sent(&self, repo: &str, msgs: Vec<ClientMsg>, cx: &mut Context<Self>) {
        cx.emit(HubEvent::Git(repo.to_owned()));
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
}

fn start_msg(id: IntentId, start: &Start) -> ClientMsg {
    ClientMsg::Thread(ThreadRequest::Start { id, start: Box::new(start.clone()) })
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
        Intent::Interrupt => "Couldn't stop".to_owned(),
        Intent::Answer { .. } => "The answer didn't go".to_owned(),
        Intent::Release { .. } => "Couldn't hand the request back".to_owned(),
        Intent::SetModel { model } => format!("Couldn't switch to {model}"),
        Intent::SetMode { mode } => format!("Couldn't switch to {mode}"),
        Intent::Compact => "Couldn't compact".to_owned(),
        Intent::Keep(pick) => format!("Couldn't keep {}", file_name(&pick.path)),
        Intent::Revert(pick) => format!("Couldn't revert {}", file_name(&pick.path)),
        Intent::Handoff => "Couldn't hand over to the terminal".to_owned(),
        Intent::TakeBack => "Couldn't take the session back".to_owned(),
        Intent::Fork { .. } => "Couldn't fork the thread".to_owned(),
        Intent::Continue { .. } => "Couldn't go on in a new thread".to_owned(),
        Intent::Rewind { .. } => "Couldn't go back to that turn".to_owned(),
        Intent::SetEffort { effort } => format!("Couldn't set the effort to {effort}"),
        Intent::Aside => "Couldn't ask aside".to_owned(),
        Intent::Discard => "Couldn't close the aside".to_owned(),
        Intent::KeepAside => "Couldn't keep the aside".to_owned(),
        Intent::Review { .. } => "The review didn't start".to_owned(),
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

//! Following agents' conversations: who follows which session, the permission prompts held for
//! them, and what the hooks tell a follower between two reads of the transcript.
//!
//! **Held prompts.** Claude Code runs the `PermissionRequest` hook synchronously, and the relay
//! asks the worker for a decision (`CtlRequest::Permission`). [`Holds`] decides what becomes of
//! the question:
//!
//! - nobody can answer it: it is handed back at once, undecided, and the TUI shows its own dialog
//!   as if there were no hook;
//! - someone follows the session: it is held under a new id and shown to the followers; the first
//!   answer from one of them takes it, and anything after that finds nothing to take;
//! - nobody follows, but a client answers approvals (from a notification or the inbox) and the
//!   prompt is a yes or no ([`Held::approvable`]): it is held the same way for the approvers, for a
//!   shorter time the caller sets ([`Reach::Approvers`]);
//! - the last follower unfollows (the person went back to the TUI), nobody who could answer it is
//!   connected any more, a client hands it to the TUI, the worker has held it as long as it may, or
//!   the relay went away: it is released, undecided. A release and an answer race for the same
//!   entry, and whichever comes first takes it.
//!
//! `R` is what the caller keeps with a held prompt (the reply channel, the prompt as shown);
//! the machine only ever hands it back once.
//!
//! Orchestration follows as [`ORCHESTRATION`], a link no connection has: from the first verb
//! that reads a session's conversation or starts its agent until the session ends
//! ([`Holds::forget`]).
//!
//! **What followers watch.** [`Board`] keeps each session's [`Seen`] behind a watch: the hooks
//! heard, the latest meters, the subagent files named, and the blocks Slopty's Claude Code mod
//! reports as the model writes them. The mod is heard only after its `hello` passed
//! `slopty_agent::live::gate`; a session whose mod was refused says so in the log once and is
//! followed from the transcript alone.
//!
//! **One read for every follower.** The board also keeps each followed session's [`Reader`]:
//! the session's transcripts, decoded once however many clients follow, each read's changes
//! broadcast to all of them, and the conversation as it stands handed to one that joins.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::Instant;

use slopty_agent::Hook;
use slopty_agent::conversation::Transcripts;
use slopty_agent::conversation::output::Outputs;
use slopty_agent::live::{self, ModEvent};
use slopty_core::SessionId;
use slopty_proto::conversation::{Change, Meters, Output};
use tokio::sync::{broadcast, watch};

use crate::clip::Link;

/// The follower orchestration's verbs are: no connection's link, which is the address of its
/// state and never zero.
pub const ORCHESTRATION: Link = 0;

/// Who a prompt is held for, as it is asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reach {
    /// The session's followers (and the approvers too, for a yes or no).
    Followers,
    /// Nobody follows the session: the clients that answer approvals, for a bounded time.
    Approvers,
}

/// A prompt being held, and for which session.
#[derive(Debug, PartialEq, Eq)]
pub struct Held<R> {
    /// The session whose agent asks.
    pub session: SessionId,
    /// A yes or no, which an approver may answer: not a question or a plan to read.
    pub approvable: bool,
    /// What the caller keeps with it.
    pub reply: R,
}

/// Who follows which session, who answers approvals, and the permission prompts held for them.
#[derive(Debug)]
pub struct Holds<R> {
    /// The last id given; ids start at 1.
    last: u64,
    followers: HashMap<SessionId, BTreeSet<Link>>,
    approvers: BTreeSet<Link>,
    held: BTreeMap<u64, Held<R>>,
}

impl<R> Default for Holds<R> {
    fn default() -> Self {
        Self {
            last: 0,
            followers: HashMap::new(),
            approvers: BTreeSet::new(),
            held: BTreeMap::new(),
        }
    }
}

impl<R> Holds<R> {
    /// `link` follows `session`. Returns the ids of the prompts already held for it, which the
    /// new follower is to be shown.
    pub fn follow(&mut self, session: SessionId, link: Link) -> Vec<u64> {
        self.followers.entry(session).or_default().insert(link);
        self.held.iter().filter(|(_id, held)| held.session == session).map(|(id, _)| *id).collect()
    }

    /// `link` stops following `session`. When no follower is left, every prompt held for the
    /// session is released and handed back, to be answered undecided: the person went back to
    /// the TUI, whoever else answers approvals.
    pub fn unfollow(&mut self, session: SessionId, link: Link) -> Vec<(u64, Held<R>)> {
        let Some(links) = self.followers.get_mut(&session) else { return Vec::new() };
        links.remove(&link);
        if !links.is_empty() {
            return Vec::new();
        }
        self.followers.remove(&session);
        self.held.extract_if(.., |_id, held| held.session == session).collect()
    }

    /// `link` answers approvals from now on. Returns the ids of the yes-or-no prompts held that
    /// it is to be shown, those it already sees as a follower left out.
    pub fn approve(&mut self, link: Link) -> Vec<u64> {
        self.approvers.insert(link);
        self.held
            .iter()
            .filter(|(_id, held)| held.approvable && !self.follows(link, held.session))
            .map(|(id, _)| *id)
            .collect()
    }

    /// `link` stops answering approvals. Every prompt nobody else can answer now is released
    /// and handed back.
    pub fn stop_approving(&mut self, link: Link) -> Vec<(u64, Held<R>)> {
        self.approvers.remove(&link);
        self.orphans()
    }

    /// `link`'s connection ended: it stops following everything and answering approvals, and
    /// every prompt nobody else can answer is handed back.
    pub fn leave(&mut self, link: Link) -> Vec<(u64, Held<R>)> {
        self.followers.retain(|_session, links| {
            links.remove(&link);
            !links.is_empty()
        });
        self.approvers.remove(&link);
        self.orphans()
    }

    /// `session` ended: nobody follows it any more, and every prompt held for it is handed back.
    pub fn forget(&mut self, session: SessionId) -> Vec<(u64, Held<R>)> {
        self.followers.remove(&session);
        self.held.extract_if(.., |_id, held| held.session == session).collect()
    }

    /// Whether `link` follows `session`.
    #[must_use]
    pub fn follows(&self, link: Link, session: SessionId) -> bool {
        self.followers.get(&session).is_some_and(|links| links.contains(&link))
    }

    /// Whether `link` answers approvals.
    #[must_use]
    pub fn approves(&self, link: Link) -> bool {
        self.approvers.contains(&link)
    }

    /// Who a prompt Claude Code asks in `session` would be held for; `None` when nobody could
    /// answer it and it is to be handed back at once. A prompt that is not `approvable` waits
    /// only for followers.
    #[must_use]
    pub fn reach(&self, session: SessionId, approvable: bool) -> Option<Reach> {
        if self.followers.contains_key(&session) {
            Some(Reach::Followers)
        } else if approvable && !self.approvers.is_empty() {
            Some(Reach::Approvers)
        } else {
            None
        }
    }

    /// Claude Code asks for a permission in `session`: held under a new id, what `make` builds
    /// from it kept with it, while someone can answer it ([`Self::reach`]). `None` when nobody
    /// can and the question is to be answered undecided at once; `make` is not called then.
    pub fn ask(
        &mut self,
        session: SessionId,
        approvable: bool,
        make: impl FnOnce(u64) -> R,
    ) -> Option<u64> {
        self.reach(session, approvable)?;
        self.last = self.last.saturating_add(1);
        self.held.insert(self.last, Held { session, approvable, reply: make(self.last) });
        Some(self.last)
    }

    /// A client answers prompt `ask` of `session`, or hands it back to the TUI: it is taken
    /// when it is still held there and was shown to `link` (a follower of the session, or an
    /// approver for a yes or no). `None` for a second answer, one after a release, one for
    /// another session, and one from a client it was not shown to.
    pub fn answer(&mut self, link: Link, session: SessionId, ask: u64) -> Option<Held<R>> {
        let held = self.held.get(&ask)?;
        if held.session != session || !self.shown(link, held) {
            return None;
        }
        self.held.remove(&ask)
    }

    /// Stop holding `ask` (the wait is over, or the relay went away); `None` when an answer or
    /// a release took it first.
    pub fn release(&mut self, ask: u64) -> Option<Held<R>> {
        self.held.remove(&ask)
    }

    /// The prompts held for `session`, oldest first.
    #[cfg(test)]
    fn held_for(&self, session: SessionId) -> impl Iterator<Item = (u64, &R)> {
        self.held
            .iter()
            .filter(move |(_id, held)| held.session == session)
            .map(|(id, held)| (*id, &held.reply))
    }

    /// The prompts shown to `link`, oldest first: what it is to be shown again when it missed
    /// the broadcast.
    pub fn shown_to(&self, link: Link) -> impl Iterator<Item = (u64, &R)> {
        self.held
            .iter()
            .filter(move |(_id, held)| self.shown(link, held))
            .map(|(id, held)| (*id, &held.reply))
    }

    /// Whether news of prompt `ask` of `session` goes to `link`: while it is held, whether it
    /// is shown to it; once settled, whether it may have been (the client drops news of a
    /// prompt it never had).
    #[must_use]
    pub fn tells(&self, link: Link, session: SessionId, ask: u64) -> bool {
        match self.held.get(&ask) {
            Some(held) => self.shown(link, held),
            None => self.follows(link, session) || self.approves(link),
        }
    }

    /// A held prompt.
    #[must_use]
    pub fn get(&self, ask: u64) -> Option<&Held<R>> {
        self.held.get(&ask)
    }

    fn shown(&self, link: Link, held: &Held<R>) -> bool {
        self.follows(link, held.session) || (held.approvable && self.approves(link))
    }

    /// Hand back every prompt nobody can answer any more.
    fn orphans(&mut self) -> Vec<(u64, Held<R>)> {
        let (followers, approvers) = (&self.followers, !self.approvers.is_empty());
        self.held
            .extract_if(.., |_id, held| {
                !(followers.contains_key(&held.session) || held.approvable && approvers)
            })
            .collect()
    }
}

/// What the hooks said of one session since following began, as a follow task watches it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Seen {
    /// Hooks heard: the transcript has likely grown, so a follower reads it now rather than
    /// at its next tick.
    pub hooks: u64,
    /// The status line's latest meters.
    pub meters: Option<Meters>,
    /// The agent's own working directory, as its latest hook said it.
    pub cwd: Option<String>,
    /// Subagent transcripts the hooks named (`SubagentStop`'s `agent_transcript_path`).
    pub subagents: BTreeSet<PathBuf>,
    /// The blocks the mod reports, once it is trusted.
    pub live: live::Board,
}

/// Whether a session's mod is heard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trust {
    /// It passed the gate.
    Trusted,
    /// It was refused, and the log said why.
    Refused,
}

/// Reads a follower may fall behind by before it is sent the conversation whole again.
const BEHIND: usize = 64;

/// What one read of a session's transcripts changed, as every follower is sent it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Read {
    /// The conversation's changes, in order.
    pub changes: Vec<Change>,
    /// What the background commands printed since the last read.
    pub outputs: Vec<Output>,
}

/// A session's transcripts, read once for every follower of the session.
///
/// Each read's [`Read`] goes out on a broadcast; a follower that joins is handed the
/// conversation as it stands and the reads after it ([`Self::join`]). Kept on the [`Board`]
/// while anyone follows, behind an async lock that a read holds on the blocking pool, so a
/// join lands between two reads and misses none.
#[derive(Debug)]
pub struct Reader {
    transcripts: Transcripts,
    sent: broadcast::Sender<Arc<Read>>,
    reads: u64,
}

/// A [`Reader`] as its followers hold it.
pub type SharedReader = Arc<tokio::sync::Mutex<Reader>>;

impl Default for Reader {
    fn default() -> Self {
        Self { transcripts: Transcripts::default(), sent: broadcast::channel(BEHIND).0, reads: 0 }
    }
}

impl Reader {
    /// Read what the session's files gained (`Transcripts::read`, the main transcript `main`
    /// and the subagent files `known`) and send it to every follower. Blocking.
    pub fn read(&mut self, main: &Path, known: &[PathBuf]) {
        self.reads = self.reads.wrapping_add(1);
        let changes = self.transcripts.read(main, known);
        let outputs = self.transcripts.outputs();
        if !changes.is_empty() || !outputs.is_empty() {
            // Nobody listening is no error: a joiner catching up before it subscribes.
            let _heard = self.sent.send(Arc::new(Read { changes, outputs }));
        }
    }

    /// A follower starts: the reads to come, and the conversation as it stands. That is a
    /// reset of every thread, then each thread's entries, task list and turns, then the end of
    /// every background command's output, as a first read would send them. Blocking: the
    /// outputs are read from their files.
    #[must_use]
    pub fn join(&self) -> (broadcast::Receiver<Arc<Read>>, Read) {
        let conversation = self.transcripts.conversation();
        let mut changes = vec![Change::Reset { thread: None }];
        for thread in conversation.threads() {
            changes.extend(
                conversation
                    .entries(thread)
                    .iter()
                    .map(|entry| Change::Upsert { thread: thread.clone(), entry: entry.clone() }),
            );
            let tasks = conversation.tasks(thread);
            if !tasks.is_empty() {
                changes.push(Change::Tasks { thread: thread.clone(), tasks: tasks.to_vec() });
            }
            changes.extend(
                conversation
                    .turns(thread)
                    .iter()
                    .map(|turn| Change::Turn { thread: thread.clone(), turn: turn.clone() }),
            );
        }
        let outputs = Outputs::default().read(conversation);
        (self.sent.subscribe(), Read { changes, outputs })
    }

    /// The transcripts as read so far, for a follower that asks for a whole text or a picture.
    #[must_use]
    pub const fn transcripts(&self) -> &Transcripts {
        &self.transcripts
    }

    /// How many times the files were read.
    #[must_use]
    pub const fn reads(&self) -> u64 {
        self.reads
    }
}

/// Every session's [`Seen`], each behind a watch its followers wait on.
#[derive(Debug, Default)]
pub struct Board {
    sessions: HashMap<SessionId, watch::Sender<Seen>>,
    /// Each session's mod, once it said hello.
    mods: HashMap<SessionId, Trust>,
    /// Each followed session's reader, alive while a follower holds it.
    readers: HashMap<SessionId, Weak<tokio::sync::Mutex<Reader>>>,
}

impl Board {
    /// A hook fired in `session`.
    pub fn heard(&mut self, session: SessionId, hook: &Hook) {
        let seen = self.seen(session);
        seen.send_modify(|seen| {
            seen.hooks = seen.hooks.wrapping_add(1);
            if let Some(meters) = &hook.meters {
                seen.meters = Some(meters.clone());
            }
            if let Some(cwd) = hook.cwd.as_ref().filter(|c| !c.is_empty()) {
                seen.cwd = Some(cwd.clone());
            }
            // Claude Code's own agents (a compaction's summary, a prompt suggestion) are not
            // the model's subagents, and get no thread.
            if let Some(path) =
                hook.agent_transcript_path.as_ref().filter(|_| !hook.is_internal_subagent())
            {
                seen.subagents.insert(PathBuf::from(path));
            }
        });
    }

    /// The mod in `session` reported `events`, at `now`. A `hello` decides whether it is heard
    /// ([`live::gate`]); nothing else counts until one passed. Followers wake only when the
    /// blocks or the meters changed.
    pub fn reported(&mut self, session: SessionId, events: &[ModEvent], now: Instant) {
        let mut trust = self.mods.get(&session).copied();
        let seen =
            self.sessions.entry(session).or_insert_with(|| watch::channel(Seen::default()).0);
        seen.send_if_modified(|seen| {
            let mut changed = false;
            for event in events {
                match event {
                    ModEvent::Hello(hello) => {
                        trust = Some(match live::gate(hello) {
                            Ok(()) => Trust::Trusted,
                            Err(refusal) if trust == Some(Trust::Refused) => {
                                tracing::debug!(%session, %refusal, "the mod is still refused");
                                Trust::Refused
                            }
                            Err(refusal) => {
                                tracing::warn!(%session, %refusal, "the Claude Code mod is not heard; following the transcript");
                                Trust::Refused
                            }
                        });
                    }
                    _ if trust != Some(Trust::Trusted) => {}
                    ModEvent::Measure(measure) => {
                        let meters = measure.onto(seen.meters.take());
                        seen.meters = Some(meters);
                        changed = true;
                    }
                    event => changed |= seen.live.apply(event, now),
                }
            }
            changed
        });
        if let Some(trust) = trust {
            self.mods.insert(session, trust);
        }
    }

    /// What has been seen of `session` so far, without watching it.
    #[must_use]
    pub fn current(&self, session: SessionId) -> Seen {
        self.sessions.get(&session).map(|seen| seen.borrow().clone()).unwrap_or_default()
    }

    /// Watch `session`: the receiver holds what has been seen so far and wakes on each hook.
    pub fn watch(&mut self, session: SessionId) -> watch::Receiver<Seen> {
        self.seen(session).subscribe()
    }

    /// The reader of `session`'s transcripts, and whether it is new: a new one has read nothing
    /// yet, and nothing reads it on a tick until its taker sees to that. It lasts while a
    /// follower holds it.
    pub fn reader(&mut self, session: SessionId) -> (SharedReader, bool) {
        if let Some(reader) = self.readers.get(&session).and_then(Weak::upgrade) {
            return (reader, false);
        }
        let reader = SharedReader::default();
        self.readers.insert(session, Arc::downgrade(&reader));
        (reader, true)
    }

    /// The session is gone; its watchers see the sender close.
    pub fn forget(&mut self, session: SessionId) {
        self.sessions.remove(&session);
        self.mods.remove(&session);
        self.readers.remove(&session);
    }

    fn seen(&mut self, session: SessionId) -> &watch::Sender<Seen> {
        self.sessions.entry(session).or_insert_with(|| watch::channel(Seen::default()).0)
    }
}

#[cfg(test)]
mod tests {
    use slopty_proto::conversation::{Entry, Task, ThreadId, Turn};

    use super::*;

    const A: Link = 1;
    const B: Link = 2;

    fn session(n: u8) -> SessionId {
        format!("00000000-0000-4000-8000-{n:012}").parse().expect("a session id")
    }

    /// Nobody follows: the question goes straight back, and nothing is held.
    #[test]
    fn with_no_follower_a_prompt_passes_at_once() {
        let mut holds = Holds::default();
        assert_eq!(holds.ask(session(1), false, |_| "reply"), None);
        holds.follow(session(2), A);
        assert_eq!(
            holds.ask(session(1), false, |_| "other"),
            None,
            "a follower of another session"
        );
        assert!(holds.held_for(session(1)).next().is_none());
    }

    /// Held for a follower; the first answer takes it, a second finds nothing.
    #[test]
    fn the_first_answer_wins() {
        let mut holds = Holds::default();
        let s = session(1);
        holds.follow(s, A);
        holds.follow(s, B);
        let ask = holds.ask(s, false, |_| "reply").expect("held");
        assert_eq!(holds.held_for(s).collect::<Vec<_>>(), [(ask, &"reply")]);
        assert_eq!(
            holds.answer(B, s, ask),
            Some(Held { session: s, approvable: false, reply: "reply" })
        );
        assert_eq!(holds.answer(A, s, ask), None, "two answers");
        assert_eq!(holds.release(ask), None, "nothing left to release");
    }

    /// An answer that comes after the prompt was released finds nothing, and so does one for
    /// the wrong session or from a client that does not follow.
    #[test]
    fn a_late_or_stray_answer_is_dropped() {
        let mut holds = Holds::default();
        let s = session(1);
        holds.follow(s, A);
        let ask = holds.ask(s, false, |_| 1).expect("held");
        assert_eq!(holds.answer(B, s, ask), None, "not a follower");
        assert_eq!(holds.answer(A, session(2), ask), None, "another session");
        assert_eq!(
            holds.release(ask),
            Some(Held { session: s, approvable: false, reply: 1 }),
            "timed out"
        );
        assert_eq!(holds.answer(A, s, ask), None, "answer after release");
    }

    /// The last follower leaving releases what was held; one of two leaving does not.
    #[test]
    fn the_last_follower_leaving_mid_hold_releases_it() {
        let mut holds = Holds::default();
        let (s, t) = (session(1), session(2));
        holds.follow(s, A);
        holds.follow(s, B);
        holds.follow(t, B);
        let first = holds.ask(s, false, |_| "s").expect("held");
        let other = holds.ask(t, false, |_| "t").expect("held");
        assert!(holds.unfollow(s, A).is_empty(), "B still follows");
        assert_eq!(holds.get(first).map(|h| h.reply), Some("s"));
        assert_eq!(
            holds.unfollow(s, B),
            [(first, Held { session: s, approvable: false, reply: "s" })]
        );
        assert!(!holds.follows(B, s));
        assert_eq!(holds.answer(B, t, other).map(|h| h.reply), Some("t"), "t is unaffected");
        assert!(holds.unfollow(s, B).is_empty(), "unfollowing twice");
    }

    /// A connection that ends leaves every session it followed, releasing where it was the
    /// last.
    #[test]
    fn a_disconnect_releases_where_it_was_the_last_follower() {
        let mut holds = Holds::default();
        let (s, t) = (session(1), session(2));
        holds.follow(s, A);
        holds.follow(t, A);
        holds.follow(t, B);
        let on_s = holds.ask(s, false, |_| "s").expect("held");
        let on_t = holds.ask(t, false, |_| "t").expect("held");
        assert_eq!(holds.leave(A), [(on_s, Held { session: s, approvable: false, reply: "s" })]);
        assert!(holds.get(on_t).is_some(), "B still follows t");
        assert_eq!(holds.ask(s, false, |_| "again"), None, "nobody follows s now");
    }

    /// A client that starts following while a prompt is held is shown it; ids are never reused.
    #[test]
    fn a_new_follower_is_shown_what_is_waiting() {
        let mut holds = Holds::default();
        let s = session(1);
        assert!(holds.follow(s, A).is_empty());
        let one = holds.ask(s, false, |_| ()).expect("held");
        let two = holds.ask(s, false, |_| ()).expect("held");
        assert_eq!(holds.follow(s, B), [one, two]);
        assert_eq!(holds.follow(s, B), [one, two], "following twice is following once");
        let shown: Vec<u64> = holds.shown_to(B).map(|(id, _reply)| id).collect();
        assert_eq!(shown, [one, two], "what a follower that missed the broadcast is sent again");
        assert_eq!(holds.shown_to(3).count(), 0, "nothing for a client that follows nothing");
        assert!(holds.release(one).is_some());
        holds.unfollow(s, A);
        holds.unfollow(s, B);
        holds.follow(s, A);
        assert!(holds.ask(s, false, |_| ()).is_some_and(|three| three > two), "a fresh id");
    }

    /// Orchestration follows like a connection: it holds a prompt, answers it, and a client
    /// that follows too sees what is held; the session's end lets both go and releases the rest.
    #[test]
    fn orchestration_follows_until_the_session_ends() {
        let mut holds = Holds::default();
        let (s, t) = (session(1), session(2));
        assert!(holds.follow(s, ORCHESTRATION).is_empty());
        let first = holds.ask(s, false, |_| "first").expect("held for orchestration");
        assert_eq!(holds.follow(s, A), [first], "a client that follows later is shown it");
        assert_eq!(holds.answer(ORCHESTRATION, s, first).map(|h| h.reply), Some("first"));
        let second = holds.ask(s, false, |_| "second").expect("held");
        holds.follow(t, ORCHESTRATION);
        assert_eq!(
            holds.forget(s),
            [(second, Held { session: s, approvable: false, reply: "second" })]
        );
        assert!(!holds.follows(ORCHESTRATION, s) && !holds.follows(A, s), "nobody follows s");
        assert!(holds.follows(ORCHESTRATION, t), "t is unaffected");
        assert!(holds.unfollow(s, A).is_empty(), "the connection's own unfollow finds nothing");
    }

    /// A client that answers approvals has a yes or no held for it with nobody following, and
    /// is shown what already waits; a question waits only for followers. It answers or hands a
    /// prompt back as a follower does, and the last approver going releases what only it could
    /// answer, while a follower's unfollow still releases the session's prompts to the TUI.
    #[test]
    fn an_approver_is_held_for_without_following() {
        let mut holds = Holds::default();
        let (s, t) = (session(1), session(2));
        assert_eq!(holds.reach(s, true), None, "nobody to answer");
        holds.follow(t, B);
        let question = holds.ask(t, false, |_| "question").expect("held for the follower");
        let waiting = holds.ask(t, true, |_| "waiting").expect("held for the follower");
        assert_eq!(
            holds.approve(A),
            [waiting],
            "shown the yes or no already held, not the question"
        );
        assert_eq!(holds.approve(B), Vec::<u64>::new(), "a follower already sees them");
        assert!(holds.stop_approving(B).is_empty(), "A still answers");
        assert_eq!(holds.reach(s, true), Some(Reach::Approvers));
        assert_eq!(holds.ask(s, false, |_| "question"), None, "a question needs a follower");
        let yes = holds.ask(s, true, |_| "yes").expect("held for the approvers");
        assert!(holds.tells(A, s, yes) && !holds.tells(A, t, question), "only the yes or no");
        assert_eq!(holds.shown_to(A).map(|(id, _)| id).collect::<Vec<_>>(), [waiting, yes]);
        assert_eq!(holds.answer(A, s, yes).map(|h| h.reply), Some("yes"), "an approver answers");
        let no = holds.ask(s, true, |_| "no").expect("held");
        let released = holds.stop_approving(A);
        assert_eq!(released, [(no, Held { session: s, approvable: true, reply: "no" })]);
        assert!(holds.get(waiting).is_some(), "B still follows t");
        holds.approve(A);
        let kept = holds.ask(s, true, |_| "kept").expect("held");
        holds.follow(s, B);
        let gone: Vec<u64> = holds.leave(B).into_iter().map(|(id, _)| id).collect();
        assert_eq!(gone, [question], "the question only B could answer");
        assert!(holds.get(waiting).is_some() && holds.get(kept).is_some(), "A answers the rest");
        holds.follow(s, B);
        assert_eq!(holds.unfollow(s, B).len(), 1, "the person went back to the TUI");
    }

    /// The board keeps the latest meters and the named subagent files, and wakes a watcher on
    /// every hook.
    #[test]
    fn the_board_wakes_followers_on_each_hook() {
        let mut board = Board::default();
        let s = session(1);
        let mut seen = board.watch(s);
        assert!(!seen.has_changed().unwrap_or(true));
        let stop = Hook::parse(
            r#"{"hook_event_name":"SubagentStop","agent_id":"a1","agent_transcript_path":"/t/s/subagents/agent-a1.jsonl"}"#,
        )
        .expect("hook");
        board.heard(s, &stop);
        assert!(seen.has_changed().unwrap_or(false));
        let meters = Meters { model: Some("Opus".to_owned()), ..Meters::default() };
        board.heard(s, &Hook { meters: Some(meters.clone()), ..Hook::default() });
        let now = seen.borrow_and_update().clone();
        assert_eq!(now.hooks, 2);
        assert_eq!(now.meters, Some(meters));
        assert_eq!(
            now.subagents.into_iter().collect::<Vec<_>>(),
            [PathBuf::from("/t/s/subagents/agent-a1.jsonl")]
        );
        let internal = Hook::parse(
            r#"{"hook_event_name":"SubagentStop","agent_id":"a2","agent_type":"","agent_transcript_path":"/t/s/subagents/agent-a2.jsonl"}"#,
        )
        .expect("hook");
        board.heard(s, &internal);
        assert_eq!(seen.borrow_and_update().subagents.len(), 1, "a compaction gets no thread");
        board.forget(s);
        assert!(seen.has_changed().is_err(), "the watch closes with the session");
    }

    fn append(path: &Path, text: &str) {
        use std::io::Write as _;
        let mut file =
            std::fs::OpenOptions::new().create(true).append(true).open(path).expect("open");
        file.write_all(text.as_bytes()).expect("write");
    }

    fn prompt(uuid: &str, parent: Option<&str>, text: &str) -> String {
        let record = serde_json::json!({
            "type": "user", "uuid": uuid, "parentUuid": parent,
            "timestamp": "2026-09-27T03:15:25.849Z",
            "message": { "role": "user", "content": text },
        });
        format!("{record}\n")
    }

    /// The entries' ids, in order, and the resets.
    fn ids(changes: &[Change]) -> Vec<String> {
        changes
            .iter()
            .filter_map(|change| match change {
                Change::Upsert { thread, entry } => Some(format!("{thread:?} {}", entry.id)),
                Change::Reset { thread } => Some(format!("reset {thread:?}")),
                _ => None,
            })
            .collect()
    }

    type Model = BTreeMap<ThreadId, (Vec<Entry>, Vec<Task>, BTreeMap<String, Turn>)>;

    /// The conversation a client holds once it applied `changes`.
    fn applied(changes: &[Change]) -> Model {
        let mut model = Model::new();
        for change in changes.iter().cloned() {
            match change {
                Change::Upsert { thread, entry } => {
                    let entries = &mut model.entry(thread).or_default().0;
                    match entries.iter_mut().find(|e| e.id == entry.id) {
                        Some(old) => *old = entry,
                        None => entries.push(entry),
                    }
                }
                Change::Remove { thread, id } => {
                    model.entry(thread).or_default().0.retain(|e| e.id != id);
                }
                Change::Tasks { thread, tasks } => model.entry(thread).or_default().1 = tasks,
                Change::Turn { thread, turn } => {
                    model.entry(thread).or_default().2.insert(turn.prompt.clone(), turn);
                }
                Change::Reset { thread: Some(thread) } => {
                    model.remove(&thread);
                }
                Change::Reset { thread: None } => model.clear(),
            }
        }
        model
    }

    /// Two followers of a session are sent the same changes from one read of its files, a read
    /// that finds nothing sends nothing, and a follower that comes later is handed the
    /// conversation as a first read of the files would give it. The reader lasts while a
    /// follower holds it.
    #[test]
    fn followers_share_one_read_and_a_late_one_gets_it_whole() {
        let dir = tempfile::tempdir().expect("tempdir");
        let main = dir.path().join("s1.jsonl");
        append(&main, &prompt("u1", None, "hello"));
        let mut board = Board::default();
        let s = session(1);
        let (reader, fresh) = board.reader(s);
        assert!(fresh, "nobody followed yet");
        let mut read = reader.try_lock().expect("unlocked");
        read.read(&main, &[]);
        let (mut a, first) = read.join();
        assert_eq!(ids(&first.changes), ["reset None", "Main u1"]);
        let (again, fresh) = board.reader(s);
        assert!(!fresh && Arc::ptr_eq(&again, &reader), "the second follower shares it");
        let (mut b, _now) = read.join();

        append(&main, &prompt("u2", Some("u1"), "and then"));
        read.read(&main, &[]);
        let (to_a, to_b) = (a.try_recv().expect("A is sent it"), b.try_recv().expect("B too"));
        assert!(Arc::ptr_eq(&to_a, &to_b), "one read for both");
        assert_eq!(ids(&to_a.changes), ["Main u2"]);
        read.read(&main, &[]);
        assert!(a.try_recv().is_err(), "a read that finds nothing sends nothing");
        assert_eq!(read.reads(), 3);

        let (_c, late) = read.join();
        assert_eq!(ids(&late.changes), ["reset None", "Main u1", "Main u2"]);
        let whole = Transcripts::default().read(&main, &[]);
        assert_eq!(applied(&late.changes), applied(&whole), "as a first read gives it");
        assert!(applied(&late.changes).values().all(|(_, _, turns)| !turns.is_empty()));

        drop(read);
        drop((again, reader));
        assert!(board.reader(s).1, "the last follower gone, the next starts afresh");
        board.forget(s);
        assert!(board.readers.is_empty());
    }

    /// What following one session costs per tick with `FOLLOWERS` clients: each decoding the
    /// transcripts on its own (the path this replaced, `Transcripts` per follower) against one
    /// shared read sent to all. On the captured `tools` session with its subagent: a tick that
    /// finds nothing new, and the second half of the session appended a record per tick.
    #[test]
    #[ignore = "measurement"]
    fn follower_read_cost() {
        const FOLLOWERS: usize = 4;
        const IDLE_TICKS: u32 = 2_000;
        const REPLAYS: u32 = 20;
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../slopty-agent/tests/fixtures/conversation/tools");
        let captured = std::fs::read_to_string(fixture.join("transcript.jsonl")).expect("fixture");
        let lines: Vec<&str> = captured.lines().collect();
        let (first, rest) = lines.split_at(lines.len() / 2);
        let dir = tempfile::tempdir().expect("tempdir");
        let main = dir.path().join("s1.jsonl");
        let subagents = slopty_agent::conversation::subagents_dir(&main);
        std::fs::create_dir_all(&subagents).expect("mkdir");
        for entry in std::fs::read_dir(fixture.join("subagents")).expect("subagents") {
            let path = entry.expect("entry").path();
            std::fs::copy(&path, subagents.join(path.file_name().expect("name"))).expect("copy");
        }
        let start = || std::fs::write(&main, format!("{}\n", first.join("\n"))).expect("write");
        let micros = |d: std::time::Duration| d.as_secs_f64() * 1e6;

        for round in 1..=3 {
            start();
            let mut own: Vec<Transcripts> =
                std::iter::repeat_with(Transcripts::default).take(FOLLOWERS).collect();
            for t in &mut own {
                let _all = (t.read(&main, &[]), t.outputs());
            }
            let clock = Instant::now();
            for _ in 0..IDLE_TICKS {
                for t in &mut own {
                    let _nothing = (t.read(&main, &[]), t.outputs());
                }
            }
            let idle_own = micros(clock.elapsed()) / f64::from(IDLE_TICKS);

            let mut reader = Reader::default();
            reader.read(&main, &[]);
            let _listening: Vec<_> =
                std::iter::repeat_with(|| reader.join().0).take(FOLLOWERS).collect();
            let clock = Instant::now();
            for _ in 0..IDLE_TICKS {
                reader.read(&main, &[]);
            }
            let idle_shared = micros(clock.elapsed()) / f64::from(IDLE_TICKS);

            let (mut grow_own, mut grow_shared) = (0.0, 0.0);
            for _ in 0..REPLAYS {
                start();
                let mut own: Vec<Transcripts> =
                    std::iter::repeat_with(Transcripts::default).take(FOLLOWERS).collect();
                for t in &mut own {
                    let _all = (t.read(&main, &[]), t.outputs());
                }
                let clock = Instant::now();
                for line in rest {
                    append(&main, &format!("{line}\n"));
                    for t in &mut own {
                        let _grown = (t.read(&main, &[]), t.outputs());
                    }
                }
                grow_own += micros(clock.elapsed());

                start();
                let mut reader = Reader::default();
                reader.read(&main, &[]);
                let mut followers: Vec<_> =
                    std::iter::repeat_with(|| reader.join().0).take(FOLLOWERS).collect();
                let clock = Instant::now();
                for line in rest {
                    append(&main, &format!("{line}\n"));
                    reader.read(&main, &[]);
                    for follower in &mut followers {
                        while let Ok(read) = follower.try_recv() {
                            let _sent = Arc::unwrap_or_clone(read);
                        }
                    }
                }
                grow_shared += micros(clock.elapsed());
            }
            let per_record =
                f64::from(REPLAYS) * f64::from(u32::try_from(rest.len()).expect("few"));
            eprintln!(
                "round {round}, {FOLLOWERS} followers: idle tick {idle_own:.1} -> {idle_shared:.1} µs, \
                 a record appended {:.1} -> {:.1} µs",
                grow_own / per_record,
                grow_shared / per_record,
            );
        }
    }

    fn mod_event(value: &serde_json::Value) -> ModEvent {
        ModEvent::decode(value)
    }

    fn hello(claude: &str) -> ModEvent {
        mod_event(&serde_json::json!({"kind": "hello", "protocol": 1, "claude": claude}))
    }

    fn piece(text: &str) -> ModEvent {
        mod_event(&serde_json::json!({
            "kind": "text", "turnId": "t", "step": 0, "block": 0, "text": text,
        }))
    }

    fn texts(seen: &watch::Receiver<Seen>) -> Vec<String> {
        seen.borrow().live.blocks().values().map(|b| b.text.clone()).collect()
    }

    /// The mod is heard only after a hello from a verified Claude Code: what comes before it,
    /// or after a refused one, wakes nobody and shows nothing.
    #[test]
    fn the_mod_is_heard_after_its_hello_passes() {
        let mut board = Board::default();
        let now = Instant::now();
        let (trusted, refused) = (session(1), session(2));
        let mut seen = board.watch(trusted);
        board.reported(trusted, &[piece("early")], now);
        assert!(!seen.has_changed().unwrap_or(true), "nothing before the hello");
        let verified = slopty_agent::claude_mod::MOD_CLAUDE_VERSIONS[0];
        board.reported(trusted, &[hello(verified), piece("Sun")], now);
        board.reported(trusted, &[piece("day")], now);
        assert!(seen.has_changed().unwrap_or(false));
        assert_eq!(texts(&seen), ["Sunday"]);
        let measure = serde_json::json!({
            "kind": "measure", "context": {"percent": 3.5, "tokens": 7000, "window": 200_000},
            "cost": {"usd": 0.25}, "rateLimits": [],
        });
        board.reported(trusted, &[mod_event(&measure)], now);
        let meters = seen.borrow_and_update().meters.clone().expect("meters");
        assert_eq!((meters.context_used_pct, meters.cost_usd), (Some(3.5), Some(0.25)));

        let other = board.watch(refused);
        board.reported(refused, &[hello("0.0.1"), piece("unheard")], now);
        board.reported(refused, &[hello("0.0.1"), mod_event(&measure)], now);
        assert!(!other.has_changed().unwrap_or(true), "a refused mod wakes nobody");
        assert!(texts(&other).is_empty() && other.borrow().meters.is_none());
        assert_eq!(board.mods.get(&refused), Some(&Trust::Refused));
        board.reported(refused, &[hello(verified), piece("heard")], now);
        assert_eq!(texts(&other), ["heard"], "a later hello that passes is heard");
        board.forget(refused);
        assert!(!board.mods.contains_key(&refused));
        assert!(other.has_changed().is_err(), "the watch closes with the session");
    }
}

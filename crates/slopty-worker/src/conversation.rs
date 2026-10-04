//! Claude Code's permission prompts held for the people who can answer them, and what the
//! hooks and the mod said of each session between two reads of its transcript.
//!
//! **Held prompts.** Claude Code runs the `PermissionRequest` hook synchronously, and the relay
//! asks the worker for a decision (`CtlRequest::Permission`). [`Holds`] decides what becomes of
//! the question:
//!
//! - nobody can answer it: it is handed back at once, undecided, and the TUI shows its own dialog
//!   as if there were no hook;
//! - a client follows the session's thread: it is held under a new id and shown on the thread as a
//!   request; the first answer from a follower takes it, and anything after that finds nothing to
//!   take;
//! - nobody follows, but a client keeps the thread table, where every thread's requests show (a
//!   notification's "Allow", the inbox), and the prompt is a yes or no ([`Held::approvable`]): it
//!   is held the same way for those approvers, for a shorter time the caller sets
//!   ([`Reach::Approvers`]);
//! - the last follower lets the thread go (the person went back to the TUI), nobody who could
//!   answer it is connected any more, a client hands it to the TUI, the worker has held it as long
//!   as it may, or the relay went away: it is released, undecided. A release and an answer race for
//!   the same entry, and whichever comes first takes it.
//!
//! `R` is what the caller keeps with a held prompt (the reply channel, the prompt as shown);
//! the machine only ever hands it back once.
//!
//! Orchestration follows as [`ORCHESTRATION`], a link no connection has: from the first verb
//! that reads a session's conversation or starts its agent until the session ends
//! ([`Holds::forget`]).
//!
//! **What the hooks said.** [`Board`] keeps each session's [`Seen`] behind a watch, which the
//! session's thread observer waits on: the hooks heard, the latest meters, the subagent files
//! named, and the blocks Slopty's Claude Code mod reports as the model writes them. The mod is
//! heard only after its `hello` passed `slopty_agent::live::gate`; a session whose mod was
//! refused says so in the log once and is observed from the transcript alone.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::time::Instant;

use slopty_agent::Hook;
use slopty_agent::live::{self, ModEvent};
use slopty_core::SessionId;
use slopty_proto::conversation::Meters;
use tokio::sync::watch;

use crate::clip::Link;

/// The follower orchestration's verbs are: no connection's link, which is the address of its
/// state and never zero.
pub const ORCHESTRATION: Link = 0;

/// Who a prompt is held for, as it is asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reach {
    /// The followers of the session's thread (and the approvers too, for a yes or no).
    Followers,
    /// Nobody follows the session's thread: the clients that keep the thread table, for a
    /// bounded time.
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

/// Who follows which session's thread, who keeps the thread table, and the permission prompts
/// held for them.
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
    /// `link` follows `session`'s thread, which shows the prompts already held for it.
    pub fn follow(&mut self, session: SessionId, link: Link) {
        self.followers.entry(session).or_default().insert(link);
    }

    /// `link` stops following `session`'s thread. When no follower is left, every prompt held
    /// for the session is released and handed back, to be answered undecided: the person went
    /// back to the TUI, whoever else answers approvals.
    pub fn unfollow(&mut self, session: SessionId, link: Link) -> Vec<(u64, Held<R>)> {
        let Some(links) = self.followers.get_mut(&session) else { return Vec::new() };
        links.remove(&link);
        if !links.is_empty() {
            return Vec::new();
        }
        self.followers.remove(&session);
        self.held.extract_if(.., |_id, held| held.session == session).collect()
    }

    /// `link` keeps the thread table, where every request shows: it answers approvals from now
    /// until its connection ends.
    pub fn approve(&mut self, link: Link) {
        self.approvers.insert(link);
    }

    /// `link`'s connection ended: it stops following everything and answering approvals, and
    /// every prompt nobody else can answer is handed back.
    pub fn leave(&mut self, link: Link) -> Vec<(u64, Held<R>)> {
        self.followers.retain(|_session, links| {
            links.remove(&link);
            !links.is_empty()
        });
        self.approvers.remove(&link);
        let (followers, approvers) = (&self.followers, !self.approvers.is_empty());
        self.held
            .extract_if(.., |_id, held| {
                !(followers.contains_key(&held.session) || held.approvable && approvers)
            })
            .collect()
    }

    /// `session` ended: nobody follows it any more, and every prompt held for it is handed back.
    pub fn forget(&mut self, session: SessionId) -> Vec<(u64, Held<R>)> {
        self.followers.remove(&session);
        self.held.extract_if(.., |_id, held| held.session == session).collect()
    }

    /// Whether `link` follows `session`'s thread.
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

    /// A held prompt.
    #[must_use]
    pub fn get(&self, ask: u64) -> Option<&Held<R>> {
        self.held.get(&ask)
    }

    fn shown(&self, link: Link, held: &Held<R>) -> bool {
        self.follows(link, held.session) || (held.approvable && self.approves(link))
    }
}

/// What the hooks and the mod said of one session, as its thread observer watches it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Seen {
    /// Hooks heard: the transcript has likely grown, so the observer reads it now rather than
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
    /// Claude Code's own command list and model aliases, as the trusted mod last listed them.
    pub catalog: Option<live::Catalog>,
}

/// Whether a session's mod is heard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trust {
    /// It passed the gate.
    Trusted,
    /// It was refused, and the log said why.
    Refused,
}

/// Every session's [`Seen`], each behind a watch its observer waits on.
#[derive(Debug, Default)]
pub struct Board {
    sessions: HashMap<SessionId, watch::Sender<Seen>>,
    /// Each session's mod, once it said hello.
    mods: HashMap<SessionId, Trust>,
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
    /// ([`live::gate`]); nothing else counts until one passed. Watchers wake only when the
    /// blocks, the meters or the catalog changed.
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
                    ModEvent::Catalog(catalog) => {
                        changed |= seen.catalog.as_ref() != Some(catalog);
                        seen.catalog = Some(catalog.clone());
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

    /// The session is gone; its watchers see the sender close.
    pub fn forget(&mut self, session: SessionId) {
        self.sessions.remove(&session);
        self.mods.remove(&session);
    }

    fn seen(&mut self, session: SessionId) -> &watch::Sender<Seen> {
        self.sessions.entry(session).or_insert_with(|| watch::channel(Seen::default()).0)
    }
}

#[cfg(test)]
mod tests {
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

    /// A client that starts following while a prompt is held can answer it; ids are never
    /// reused.
    #[test]
    fn a_new_follower_answers_what_is_waiting() {
        let mut holds = Holds::default();
        let s = session(1);
        holds.follow(s, A);
        let one = holds.ask(s, false, |_| ()).expect("held");
        let two = holds.ask(s, false, |_| ()).expect("held");
        holds.follow(s, B);
        holds.follow(s, B);
        assert!(holds.answer(B, s, one).is_some(), "a follower that came later answers");
        assert!(holds.unfollow(s, A).is_empty(), "B still follows");
        assert_eq!(holds.unfollow(s, B).len(), 1, "following twice is following once");
        holds.follow(s, A);
        assert!(holds.ask(s, false, |_| ()).is_some_and(|three| three > two), "a fresh id");
    }

    /// Orchestration follows like a connection: it holds a prompt, answers it, and a client
    /// that follows too sees what is held; the session's end lets both go and releases the rest.
    #[test]
    fn orchestration_follows_until_the_session_ends() {
        let mut holds = Holds::default();
        let (s, t) = (session(1), session(2));
        holds.follow(s, ORCHESTRATION);
        let first = holds.ask(s, false, |_| "first").expect("held for orchestration");
        holds.follow(s, A);
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

    /// A client that keeps the thread table has a yes or no held for it with nobody following;
    /// a question waits only for followers. It answers or hands a prompt back as a follower
    /// does, and the last approver leaving releases what only it could answer, while a
    /// follower's unfollow still releases the session's prompts to the TUI.
    #[test]
    fn an_approver_is_held_for_without_following() {
        let mut holds = Holds::default();
        let (s, t) = (session(1), session(2));
        assert_eq!(holds.reach(s, true), None, "nobody to answer");
        holds.follow(t, B);
        let question = holds.ask(t, false, |_| "question").expect("held for the follower");
        let waiting = holds.ask(t, true, |_| "waiting").expect("held for the follower");
        holds.approve(A);
        assert!(holds.approves(A) && !holds.approves(B));
        assert_eq!(holds.reach(s, true), Some(Reach::Approvers));
        assert_eq!(holds.ask(s, false, |_| "question"), None, "a question needs a follower");
        let yes = holds.ask(s, true, |_| "yes").expect("held for the approvers");
        assert_eq!(holds.answer(A, t, question), None, "the question is not shown to A");
        assert_eq!(holds.answer(A, s, yes).map(|h| h.reply), Some("yes"), "an approver answers");
        let kept = holds.ask(s, true, |_| "kept").expect("held");
        holds.follow(s, B);
        let gone: Vec<u64> = holds.leave(B).into_iter().map(|(id, _)| id).collect();
        assert_eq!(gone, [question], "the question only B could answer");
        assert!(holds.get(waiting).is_some() && holds.get(kept).is_some(), "A answers the rest");
        let orphaned: Vec<u64> = holds.leave(A).into_iter().map(|(id, _)| id).collect();
        assert_eq!(orphaned, [waiting, kept], "nobody is left to answer");
        holds.follow(s, B);
        let again = holds.ask(s, true, |_| "again").expect("held for the follower");
        assert_eq!(holds.unfollow(s, B).len(), 1, "the person went back to the TUI");
        assert!(holds.get(again).is_none());
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
            "rateLimits": [],
        });
        board.reported(trusted, &[mod_event(&measure)], now);
        let meters = seen.borrow_and_update().meters.clone().expect("meters");
        assert_eq!((meters.context_used_pct, meters.context_window), (Some(3.5), Some(200_000)));
        let catalog = serde_json::json!({
            "kind": "catalog", "models": ["default", "opus"], "model": "opus",
            "commands": [{"name": "model", "description": "Set the model", "source": "builtin"}],
        });
        board.reported(trusted, &[mod_event(&catalog)], now);
        let listed = seen.borrow_and_update().catalog.clone().expect("the catalog");
        assert_eq!((listed.models.len(), listed.commands.len()), (2, 1));
        board.reported(trusted, &[mod_event(&catalog)], now);
        assert!(!seen.has_changed().unwrap_or(true), "the same catalog wakes nobody");

        let other = board.watch(refused);
        board.reported(refused, &[hello("0.0.1"), piece("unheard")], now);
        board.reported(refused, &[hello("0.0.1"), mod_event(&measure), mod_event(&catalog)], now);
        assert!(!other.has_changed().unwrap_or(true), "a refused mod wakes nobody");
        assert!(texts(&other).is_empty() && other.borrow().meters.is_none());
        assert!(other.borrow().catalog.is_none(), "nor lists anything");
        assert_eq!(board.mods.get(&refused), Some(&Trust::Refused));
        board.reported(refused, &[hello(verified), piece("heard")], now);
        assert_eq!(texts(&other), ["heard"], "a later hello that passes is heard");
        board.forget(refused);
        assert!(!board.mods.contains_key(&refused));
        assert!(other.has_changed().is_err(), "the watch closes with the session");
    }
}

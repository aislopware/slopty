//! Following agents' conversations: who follows which session, the permission prompts held for
//! them, and what the hooks tell a follower between two reads of the transcript.
//!
//! **Held prompts.** Claude Code runs the `PermissionRequest` hook synchronously, and the relay
//! asks the worker for a decision (`CtlRequest::Permission`). [`Holds`] decides what becomes of
//! the question:
//!
//! - nobody follows the session: it is handed back at once, undecided, and the TUI shows its own
//!   dialog as if there were no hook;
//! - someone does: it is held under a new id and shown to the followers; the first answer from one
//!   of them takes it, and anything after that finds nothing to take;
//! - the last follower leaves (unfollows or disconnects), the worker has held it as long as it may,
//!   or the relay went away: it is released, undecided. A release and an answer race for the same
//!   entry, and whichever comes first takes it.
//!
//! `R` is what the caller keeps with a held prompt (the reply channel, the prompt as shown);
//! the machine only ever hands it back once.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;

use slopty_agent::Hook;
use slopty_core::SessionId;
use slopty_proto::conversation::Meters;
use tokio::sync::watch;

use crate::clip::Link;

/// A prompt being held, and for which session.
#[derive(Debug, PartialEq, Eq)]
pub struct Held<R> {
    /// The session whose agent asks.
    pub session: SessionId,
    /// What the caller keeps with it.
    pub reply: R,
}

/// Who follows which session, and the permission prompts held for them.
#[derive(Debug)]
pub struct Holds<R> {
    /// The last id given; ids start at 1.
    last: u64,
    followers: HashMap<SessionId, BTreeSet<Link>>,
    held: BTreeMap<u64, Held<R>>,
}

impl<R> Default for Holds<R> {
    fn default() -> Self {
        Self { last: 0, followers: HashMap::new(), held: BTreeMap::new() }
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
    /// session is released and handed back, to be answered undecided.
    pub fn unfollow(&mut self, session: SessionId, link: Link) -> Vec<(u64, Held<R>)> {
        let Some(links) = self.followers.get_mut(&session) else { return Vec::new() };
        links.remove(&link);
        if !links.is_empty() {
            return Vec::new();
        }
        self.followers.remove(&session);
        self.held.extract_if(.., |_id, held| held.session == session).collect()
    }

    /// `link`'s connection ended: it stops following everything, as [`Self::unfollow`].
    pub fn leave(&mut self, link: Link) -> Vec<(u64, Held<R>)> {
        let mut deserted = BTreeSet::new();
        self.followers.retain(|session, links| {
            links.remove(&link);
            if links.is_empty() {
                deserted.insert(*session);
            }
            !links.is_empty()
        });
        self.held.extract_if(.., |_id, held| deserted.contains(&held.session)).collect()
    }

    /// Whether `link` follows `session`.
    #[must_use]
    pub fn follows(&self, link: Link, session: SessionId) -> bool {
        self.followers.get(&session).is_some_and(|links| links.contains(&link))
    }

    /// Claude Code asks for a permission in `session`: held under a new id, what `make` builds
    /// from it kept with it, while someone follows the session. `None` when nobody does and
    /// the question is to be answered undecided at once; `make` is not called then.
    pub fn ask(&mut self, session: SessionId, make: impl FnOnce(u64) -> R) -> Option<u64> {
        if !self.followers.contains_key(&session) {
            return None;
        }
        self.last = self.last.saturating_add(1);
        self.held.insert(self.last, Held { session, reply: make(self.last) });
        Some(self.last)
    }

    /// A follower answers prompt `ask` of `session`: it is taken when it is still held there
    /// and `link` follows the session. `None` for a second answer, one after a release, one
    /// for another session, and one from a client that does not follow.
    pub fn answer(&mut self, link: Link, session: SessionId, ask: u64) -> Option<Held<R>> {
        let held = self.held.get(&ask)?;
        if held.session != session || !self.follows(link, session) {
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
    pub fn held_for(&self, session: SessionId) -> impl Iterator<Item = (u64, &R)> {
        self.held
            .iter()
            .filter(move |(_id, held)| held.session == session)
            .map(|(id, held)| (*id, &held.reply))
    }

    /// The prompts held for every session `link` follows, oldest first: what it is to be shown
    /// again when it missed the broadcast.
    pub fn shown_to(&self, link: Link) -> impl Iterator<Item = (u64, &R)> {
        self.held
            .iter()
            .filter(move |(_id, held)| self.follows(link, held.session))
            .map(|(id, held)| (*id, &held.reply))
    }

    /// A held prompt.
    #[must_use]
    pub fn get(&self, ask: u64) -> Option<&Held<R>> {
        self.held.get(&ask)
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
    /// Subagent transcripts the hooks named (`SubagentStop`'s `agent_transcript_path`).
    pub subagents: BTreeSet<PathBuf>,
}

/// Every session's [`Seen`], each behind a watch its followers wait on.
#[derive(Debug, Default)]
pub struct Board {
    sessions: HashMap<SessionId, watch::Sender<Seen>>,
}

impl Board {
    /// A hook fired in `session`.
    pub fn heard(&mut self, session: SessionId, hook: &Hook) {
        let seen =
            self.sessions.entry(session).or_insert_with(|| watch::channel(Seen::default()).0);
        seen.send_modify(|seen| {
            seen.hooks = seen.hooks.wrapping_add(1);
            if let Some(meters) = &hook.meters {
                seen.meters = Some(meters.clone());
            }
            if let Some(path) = &hook.agent_transcript_path {
                seen.subagents.insert(PathBuf::from(path));
            }
        });
    }

    /// Watch `session`: the receiver holds what has been seen so far and wakes on each hook.
    pub fn watch(&mut self, session: SessionId) -> watch::Receiver<Seen> {
        self.sessions
            .entry(session)
            .or_insert_with(|| watch::channel(Seen::default()).0)
            .subscribe()
    }

    /// The session is gone; its watchers see the sender close.
    pub fn forget(&mut self, session: SessionId) {
        self.sessions.remove(&session);
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
        assert_eq!(holds.ask(session(1), |_| "reply"), None);
        holds.follow(session(2), A);
        assert_eq!(holds.ask(session(1), |_| "other"), None, "a follower of another session");
        assert!(holds.held_for(session(1)).next().is_none());
    }

    /// Held for a follower; the first answer takes it, a second finds nothing.
    #[test]
    fn the_first_answer_wins() {
        let mut holds = Holds::default();
        let s = session(1);
        holds.follow(s, A);
        holds.follow(s, B);
        let ask = holds.ask(s, |_| "reply").expect("held");
        assert_eq!(holds.held_for(s).collect::<Vec<_>>(), [(ask, &"reply")]);
        assert_eq!(holds.answer(B, s, ask), Some(Held { session: s, reply: "reply" }));
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
        let ask = holds.ask(s, |_| 1).expect("held");
        assert_eq!(holds.answer(B, s, ask), None, "not a follower");
        assert_eq!(holds.answer(A, session(2), ask), None, "another session");
        assert_eq!(holds.release(ask), Some(Held { session: s, reply: 1 }), "timed out");
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
        let first = holds.ask(s, |_| "s").expect("held");
        let other = holds.ask(t, |_| "t").expect("held");
        assert!(holds.unfollow(s, A).is_empty(), "B still follows");
        assert_eq!(holds.get(first).map(|h| h.reply), Some("s"));
        assert_eq!(holds.unfollow(s, B), [(first, Held { session: s, reply: "s" })]);
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
        let on_s = holds.ask(s, |_| "s").expect("held");
        let on_t = holds.ask(t, |_| "t").expect("held");
        assert_eq!(holds.leave(A), [(on_s, Held { session: s, reply: "s" })]);
        assert!(holds.get(on_t).is_some(), "B still follows t");
        assert_eq!(holds.ask(s, |_| "again"), None, "nobody follows s now");
    }

    /// A client that starts following while a prompt is held is shown it; ids are never reused.
    #[test]
    fn a_new_follower_is_shown_what_is_waiting() {
        let mut holds = Holds::default();
        let s = session(1);
        assert!(holds.follow(s, A).is_empty());
        let one = holds.ask(s, |_| ()).expect("held");
        let two = holds.ask(s, |_| ()).expect("held");
        assert_eq!(holds.follow(s, B), [one, two]);
        assert_eq!(holds.follow(s, B), [one, two], "following twice is following once");
        let shown: Vec<u64> = holds.shown_to(B).map(|(id, _reply)| id).collect();
        assert_eq!(shown, [one, two], "what a follower that missed the broadcast is sent again");
        assert_eq!(holds.shown_to(3).count(), 0, "nothing for a client that follows nothing");
        assert!(holds.release(one).is_some());
        holds.unfollow(s, A);
        holds.unfollow(s, B);
        holds.follow(s, A);
        assert!(holds.ask(s, |_| ()).is_some_and(|three| three > two), "a fresh id");
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
        board.forget(s);
        assert!(seen.has_changed().is_err(), "the watch closes with the session");
    }
}

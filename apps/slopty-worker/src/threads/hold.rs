//! Claude Code's permission prompts, held on the daemon for whoever can answer them on the
//! session's thread.
//!
//! The relay's `CtlRequest::Permission`, once the control socket has taken its hook in, comes
//! to [`ask`]. The daemon's [`Follows::holds`] (`slopty_worker::conversation::Holds`) decides:
//! undecided at once when nobody can answer, else held and told to the session's thread
//! observer ([`slopty_worker::thread::claude::Driver::permission`]), which shows it as a request
//! on the thread. It waits there until a follower of the thread answers it (`Intent::Answer`,
//! [`answer`]) or hands it to the TUI (`Intent::Release`, [`hand_back`]), the last follower lets
//! the thread go, the wait runs out or the relay goes away ([`release`]).
//!
//! A prompt nobody follows is held for the clients that keep the thread table, where every
//! request shows (a notification's "Allow", the inbox, the stacked approval cards), for
//! [`APPROVAL_HOLD`] at most: a yes or no is answered there in place, a plan or a question in
//! the thread its card opens. Once someone follows the thread, the hold lasts as long as the
//! relay waits ([`held_on`]), so the person reading a plan is not cut off. While
//! the server says a pocketed phone can answer (`FromServer::Pushes`), any prompt nobody
//! follows is held for that phone as long as the relay waits instead: the person has to reach
//! the phone. A note's Allow reaches the server's link first, which answers it as
//! orchestration does; a plan to confirm or a question is answered in the thread the note
//! opens, which the phone then follows.
//!
//! A call auto mode declined (`PermissionDenied`) comes the same way. The thread is told of the
//! decline whoever follows it, and when the person may let the model try again
//! ([`permission::retryable`]) that yes or no is held as an approval is; an allow answers the
//! hook with its retry, and anything else lets the decline stand.
//!
//! **Orchestration** follows too, as `ORCHESTRATION` ([`Orchestrated`]): the sessions whose
//! conversation a verb read or whose agent a verb started, until they end.

use std::time::Duration;

use slopty_agent::{Hook, HookEvent, permission};
use slopty_core::{ClientId, SessionId, WallMs};
use slopty_proto::conversation::{PermissionEvent, PermissionPrompt, Settled, Verdict};
use slopty_proto::ctl::Decision;
use slopty_worker::clip::Link;
use slopty_worker::conversation::{Board, Held, Holds, ORCHESTRATION, Reach};
use slopty_worker::orchestrate::{Conversations, Sources};
use tokio::sync::oneshot;

use crate::Daemon;

/// How much sooner than the relay gives up the worker answers, so the answer reaches it.
const HOLD_MARGIN: Duration = Duration::from_secs(1);

/// The longest a prompt nobody follows is held for the approvers before the TUI asks.
///
/// Time to reach a phone and answer its notification, while a person at the terminal who never
/// looks at one is not kept from Claude Code's own dialog for long. A client showing the TUI in
/// front of the person hands it back sooner.
pub const APPROVAL_HOLD: Duration = Duration::from_secs(120);

/// Who follows which session's thread, the prompts held for them, and what the hooks said.
#[derive(Debug, Default)]
pub struct Follows {
    /// Followers, approvers and held prompts.
    pub holds: Holds<Pending>,
    /// Each session's latest meters, named subagent files and the mod's blocks, a wake-up per
    /// hook.
    pub board: Board,
}

/// What the worker keeps with a held prompt.
#[derive(Debug)]
pub struct Pending {
    /// As the thread shows it.
    pub prompt: PermissionPrompt,
    /// The hook as it asked: its suggestions go back on "allow always", its tool's input with
    /// an answer or a plan's approval.
    pub hook: Hook,
    /// Where the decision goes: the relay's connection.
    pub reply: oneshot::Sender<Decision>,
}

/// The decision the relay waiting on `session`'s permission prompt `hook`, for at most
/// `relay_wait`, gets.
///
/// Undecided at once when nobody can answer it. Held, it is decided by an answer, or released
/// undecided when the last follower leaves, nobody who could answer is left, a client hands it
/// to the TUI or the wait is up (the relay's, or [`APPROVAL_HOLD`] when it is held for the
/// approvers only); `relay_gone` finishing means Claude Code gave up on the hook, and the prompt
/// is withdrawn.
pub async fn ask(
    daemon: &Daemon,
    session: SessionId,
    hook: &Hook,
    relay_wait: Duration,
    relay_gone: impl Future<Output = ()>,
) -> Decision {
    if hook.event == HookEvent::PermissionDenied {
        tell(
            daemon,
            PermissionEvent::Declined(Box::new(permission::declined(session, hook, WallMs::now()))),
        );
        if !permission::retryable(hook) {
            return Decision::Pass;
        }
    }
    let (reply, decided) = oneshot::channel();
    let Some((prompt, reach, wait)) = hold(daemon, session, hook, reply, relay_wait) else {
        return Decision::Pass;
    };
    let id = prompt.ask;
    tracing::info!(%session, ask = id, tool = %prompt.tool, ?reach, "permission held");
    let held_at = tokio::time::Instant::now();
    let after = |wait| held_at.checked_add(wait).unwrap_or(held_at);
    let longest = after(hold_for(Reach::Followers, relay_wait));
    let mut until = after(wait);
    tokio::pin!(decided, relay_gone);
    let outcome = loop {
        tokio::select! {
            decision = &mut decided => return decision.unwrap_or(Decision::Pass),
            () = tokio::time::sleep_until(until) => {
                let now = daemon.follows.lock().holds.reach(session);
                match held_on(now, until, longest) {
                    Some(later) => until = later,
                    None => break Settled::Released,
                }
            }
            () = &mut relay_gone => break Settled::Withdrawn,
        }
    };
    let released = daemon.follows.lock().holds.release(id);
    match released {
        Some(held) => {
            tracing::info!(%session, ask = id, ?outcome, "permission no longer held");
            settle(daemon, id, held.session, outcome);
            Decision::Pass
        }
        // An answer or a release took it first and has sent its decision.
        None => decided.try_recv().unwrap_or(Decision::Pass),
    }
}

/// Hold a prompt for whoever can answer it in `session`, and tell the session's thread: the
/// prompt, who it is held for, and how long; `None` when nobody can answer.
///
/// It is told before the lock is let go, so it is ahead of its `Settled`: a prompt is settled
/// only once it is out of the holds, under the same lock.
fn hold(
    daemon: &Daemon,
    session: SessionId,
    hook: &Hook,
    reply: oneshot::Sender<Decision>,
    relay_wait: Duration,
) -> Option<(PermissionPrompt, Reach, Duration)> {
    let approvable = permission::approvable(hook);
    let follows = &mut *daemon.follows.lock();
    let call = follows.board.call_of(session, hook);
    let holds = &mut follows.holds;
    let reach = holds.reach(session)?;
    let wait = hold_for(reach, relay_wait);
    let asked_ms = WallMs::now();
    let until_ms = asked_ms.saturating_add(wait);
    let id = holds.ask(session, approvable, |id| Pending {
        prompt: permission::prompt(session, id, (hook, call), asked_ms, until_ms),
        hook: hook.clone(),
        reply,
    })?;
    let prompt = holds.get(id).map(|held| held.reply.prompt.clone())?;
    tell(daemon, PermissionEvent::Asked(Box::new(prompt.clone())));
    Some((prompt, reach, wait))
}

/// Whether a prompt whose hold ran out at `until` is held on, and until when: when someone
/// follows its thread now, or a pocketed phone can answer, it waits as long as the relay does
/// (`longest`), as if held for them from the start. A person who opened the thread from an
/// approval card is not cut off mid-answer by the approvers' shorter hold.
fn held_on(
    now: Option<Reach>,
    until: tokio::time::Instant,
    longest: tokio::time::Instant,
) -> Option<tokio::time::Instant> {
    match now {
        Some(Reach::Followers | Reach::Pushed) if longest > until => Some(longest),
        _ => None,
    }
}

/// How long a prompt held for `reach` waits when the relay waits `relay_wait`.
fn hold_for(reach: Reach, relay_wait: Duration) -> Duration {
    let wait = relay_wait.saturating_sub(HOLD_MARGIN);
    match reach {
        Reach::Followers | Reach::Pushed => wait,
        Reach::Approvers => wait.min(APPROVAL_HOLD),
    }
}

/// The server says whether a pocketed phone can answer; once none can, every
/// prompt nobody else can answer goes back to the TUI.
pub fn pushed(daemon: &Daemon, pushed: bool) {
    let released = daemon.follows.lock().holds.set_pushed(pushed);
    release(daemon, released);
}

/// A client hands prompt `ask` of `session` back to the TUI: the person answers there. Taken
/// as an answer is, so only from a client it was shown to; `false` when it was not taken.
pub fn hand_back(daemon: &Daemon, link: Link, by: ClientId, session: SessionId, ask: u64) -> bool {
    let taken = daemon.follows.lock().holds.answer(link, session, ask);
    let Some(held) = taken else {
        tracing::debug!(%session, ask, %by, "a hand-back of a prompt no longer held");
        return false;
    };
    tracing::info!(%session, ask, %by, "permission handed back to the TUI");
    release(daemon, vec![(ask, held)]);
    true
}

/// A client answers: the first answer to a prompt still held goes to its relay, and the thread
/// hears it was answered. Any other answer is dropped; `false` says so.
pub fn answer(
    daemon: &Daemon,
    link: Link,
    by: ClientId,
    session: SessionId,
    ask: u64,
    verdict: Verdict,
) -> bool {
    let taken = daemon.follows.lock().holds.answer(link, session, ask);
    let Some(held) = taken else {
        tracing::debug!(%session, ask, %by, "an answer to a prompt no longer held");
        return false;
    };
    tracing::info!(%session, ask, %by, ?verdict, "permission answered");
    let decision = permission::decision(&verdict, &held.reply.hook);
    let _gone = held.reply.reply.send(decision);
    settle(daemon, ask, session, Settled::Answered { verdict, by });
    true
}

/// The daemon's Claude Code sessions as orchestration's verbs reach them: orchestration
/// follows as [`ORCHESTRATION`], and answers through the session's thread
/// (`crate::threads::Reads`).
pub struct Orchestrated(pub Daemon);

impl std::fmt::Debug for Orchestrated {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Orchestrated").field("worker", &self.0.id).finish_non_exhaustive()
    }
}

impl Conversations for Orchestrated {
    fn follow(&self, session: SessionId) {
        // The prompts held already reach the session's thread as its requests.
        self.0.follows.lock().holds.follow(session, ORCHESTRATION);
    }

    fn sources(&self, session: SessionId) -> Sources {
        let main = self.0.agents.lock().transcript_path(session);
        let seen = self.0.follows.lock().board.current(session);
        Sources { main, subagents: seen.subagents.into_iter().collect(), meters: seen.meters }
    }

    fn forget(&self, session: SessionId) {
        let released = {
            let mut follows = self.0.follows.lock();
            follows.board.forget(session);
            follows.holds.forget(session)
        };
        release(&self.0, released);
    }
}

/// Hand back prompts nobody answered: each relay answers nothing, and Claude Code shows its own
/// dialog.
pub fn release(daemon: &Daemon, released: Vec<(u64, Held<Pending>)>) {
    for (id, held) in released {
        tracing::info!(session = %held.session, ask = id, "permission released to the TUI");
        settle(daemon, id, held.session, Settled::Released);
        let _gone = held.reply.reply.send(Decision::Pass);
    }
}

fn settle(daemon: &Daemon, ask: u64, session: SessionId, outcome: Settled) {
    tell(daemon, PermissionEvent::Settled { session, ask, outcome });
}

/// Tell the session's thread observer of a prompt held or settled; nothing observes where the
/// daemon keeps no threads, and nobody can answer there.
fn tell(daemon: &Daemon, event: PermissionEvent) {
    if let Some(threads) = &daemon.threads {
        threads.claude.permission(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A follower's hold lasts until a second before the relay gives up; an approver's is
    /// bounded by [`APPROVAL_HOLD`] too, and by the relay's wait when that is shorter.
    #[test]
    fn an_approvers_hold_is_bounded() {
        let relay = permission::WAIT;
        assert_eq!(hold_for(Reach::Followers, relay), relay.saturating_sub(HOLD_MARGIN));
        assert_eq!(hold_for(Reach::Approvers, relay), APPROVAL_HOLD);
        let short = Duration::from_secs(10);
        assert_eq!(hold_for(Reach::Approvers, short), Duration::from_secs(9));
        assert!(APPROVAL_HOLD < relay, "the TUI asks well before Claude Code gives up");
    }

    /// A prompt held for a pocketed phone (a yes or no, a plan, a question) waits as long as
    /// the relay does, less the margin its answer needs to reach the relay: the person has to
    /// reach the phone first.
    #[test]
    fn a_prompt_is_held_for_a_pushed_phone_up_to_the_relays_wait() {
        let relay = permission::WAIT;
        assert_eq!(hold_for(Reach::Pushed, relay), relay.saturating_sub(HOLD_MARGIN));
        assert!(hold_for(Reach::Pushed, relay) > APPROVAL_HOLD, "longer than a linked approver's");
        let short = Duration::from_secs(10);
        assert_eq!(hold_for(Reach::Pushed, short), Duration::from_secs(9), "never past the relay");
    }

    /// An approvers' hold that runs out is held on to the relay's wait once someone follows the
    /// thread (the person opened it from its card) or a phone can answer, and released when
    /// only approvers are left, or when it already waited that long.
    #[test]
    fn a_followed_thread_keeps_its_prompt_past_the_approvers_hold() {
        let start = tokio::time::Instant::now();
        let (until, longest) = (start + APPROVAL_HOLD, start + Duration::from_secs(599));
        assert_eq!(held_on(Some(Reach::Followers), until, longest), Some(longest));
        assert_eq!(held_on(Some(Reach::Pushed), until, longest), Some(longest));
        assert_eq!(held_on(Some(Reach::Approvers), until, longest), None, "nobody opened it");
        assert_eq!(held_on(None, until, longest), None);
        assert_eq!(held_on(Some(Reach::Followers), longest, longest), None, "as long as it may");
    }
}

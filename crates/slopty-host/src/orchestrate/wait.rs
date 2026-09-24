//! `WaitFor`: block until something happens in a session, woken by the session's own
//! [`Activity`] and the agent events, never by a timer that asks again.
//!
//! - `Output(regex)` reads as `expect` does: from the session's mark ([`SessionHandle::mark`]),
//!   which is where orchestration opened the terminal or first typed into it or waited on it, and
//!   which each match moves past the matching line. So output that came before the wait (a quick
//!   command's, typed by the verb before) still matches, and a second wait sees only what follows
//!   the first one's match. The mark's row is read from its column; every row below it whole. Each
//!   wake reads only from the row the cursor was on at the last read, so a long wait over a busy
//!   terminal formats each row about once.
//! - `Quiet { ms }` holds once no output arrived for that long; any output restarts it.
//! - `CommandDone` holds once a command has ended (`133;D`) at or after the session's mark: the one
//!   running now, or the one the verb's own input started, however soon it ended. Each
//!   `CommandDone` consumes the command it reports, so the next waits for a later one.
//! - `Exit` holds once the program is gone (its exit reported, its PTY closed, or the session
//!   closed).
//! - `AgentNeedsInput` holds at once when the agent is blocked on a human, else at the next report
//!   of it blocked, idle at its prompt or done with its turn. An agent already idle when the call
//!   starts counts only once it reports again: the caller has usually just typed to it, and its
//!   idle status is the one from before.
//!
//! Every condition but `Exit` ends as [`Waited::Closed`] when the program exits first.

use std::time::Duration;

use slopty_engine::ghostty::Position;
use slopty_proto::HostMsg;
use slopty_proto::agent::AgentStatus;
use slopty_proto::orchestration::{ErrorCode, Line, WaitUntil, Waited};
use tokio::sync::{broadcast, watch};
use tokio::time::Instant;

use super::Failure;
use crate::session::{Activity, Read, SessionHandle, Text};

/// The agent side of a wait: the status when the call started and the events after it.
#[derive(Debug)]
pub struct AgentFeed {
    /// The daemon's broadcast, subscribed when the call started.
    pub events: broadcast::Receiver<HostMsg>,
    /// The agent's status then, if one runs.
    pub now: Option<AgentStatus>,
}

/// What woke a waiter.
enum Wake {
    Changed,
    TimedOut,
    /// The session actor stopped.
    Gone,
}

async fn changed(activity: &mut watch::Receiver<Activity>, deadline: Instant) -> Wake {
    match tokio::time::timeout_at(deadline, activity.changed()).await {
        Ok(Ok(())) => Wake::Changed,
        Ok(Err(_closed)) => Wake::Gone,
        Err(_elapsed) => Wake::TimedOut,
    }
}

/// Wait until `until` holds in the session, or `timeout` passes.
///
/// # Errors
///
/// [`ErrorCode::Invalid`] for a pattern that does not compile or an agent wait without an
/// [`AgentFeed`]; [`ErrorCode::UnknownTerminal`] when the session is gone before the wait
/// starts.
pub async fn wait_for(
    handle: &SessionHandle,
    until: &WaitUntil,
    timeout: Duration,
    agent: Option<AgentFeed>,
) -> Result<Waited, Failure> {
    let now = Instant::now();
    let deadline = now.checked_add(timeout).unwrap_or(now);
    let mut activity = handle.activity();
    match until {
        WaitUntil::Output(pattern) => output(handle, pattern, &mut activity, deadline).await,
        WaitUntil::Quiet { ms } => {
            let quiet = Duration::from_millis(u64::from(*ms));
            loop {
                if activity.borrow_and_update().exited {
                    return Ok(Waited::Closed);
                }
                let at = Instant::now().checked_add(quiet).unwrap_or(deadline);
                if at > deadline {
                    // Quiet or not, the timeout comes first unless something ends the wait.
                    return Ok(match changed(&mut activity, deadline).await {
                        Wake::Gone => Waited::Closed,
                        Wake::Changed | Wake::TimedOut => Waited::TimedOut,
                    });
                }
                match changed(&mut activity, at).await {
                    Wake::Changed => {}
                    Wake::TimedOut => return Ok(Waited::Met { line: None }),
                    Wake::Gone => return Ok(Waited::Closed),
                }
            }
        }
        WaitUntil::CommandDone => command_done(handle, &mut activity, deadline).await,
        WaitUntil::Exit => loop {
            if activity.borrow_and_update().exited {
                return Ok(Waited::Met { line: None });
            }
            match changed(&mut activity, deadline).await {
                Wake::Changed => {}
                Wake::TimedOut => return Ok(Waited::TimedOut),
                Wake::Gone => return Ok(Waited::Met { line: None }),
            }
        },
        WaitUntil::AgentNeedsInput => {
            let feed = agent.ok_or_else(|| {
                Failure::new(ErrorCode::Invalid, "no agent status is tracked for this session")
            })?;
            agent_input(handle, feed, &mut activity, deadline).await
        }
    }
}

async fn command_done(
    handle: &SessionHandle,
    activity: &mut watch::Receiver<Activity>,
    deadline: Instant,
) -> Result<Waited, Failure> {
    let mark = match handle.mark() {
        Some(mark) => mark,
        None => handle.mark_if_unset(super::position(handle).await?),
    };
    let place = |p: Position| (p.line, p.col);
    // Past the last command reported, when that is later in the same numbering.
    let from = handle
        .command_mark()
        .filter(|c| c.epoch == mark.epoch && place(*c) >= place(mark))
        .map_or(mark, |c| Position { col: c.col.saturating_add(1), ..c });
    let start = activity.borrow().commands_ended;
    loop {
        let seen = *activity.borrow_and_update();
        let Ok(Text::Ended(ended)) = handle.read(Read::EndedAfter(from)).await else {
            return Ok(Waited::Closed);
        };
        if let Some(end) = ended {
            handle.advance_command_mark(end);
            return Ok(Waited::Met { line: None });
        }
        // One ended since the call started, but the lines were renumbered (a resize) and its
        // position says nothing against the mark's.
        if seen.commands_ended != start {
            return Ok(Waited::Met { line: None });
        }
        if seen.exited {
            return Ok(Waited::Closed);
        }
        // Only a command's end, or the program's, can change the answer.
        loop {
            match changed(activity, deadline).await {
                Wake::Changed => {
                    let now = *activity.borrow();
                    if now.commands_ended != seen.commands_ended || now.exited {
                        break;
                    }
                }
                Wake::TimedOut => return Ok(Waited::TimedOut),
                Wake::Gone => return Ok(Waited::Closed),
            }
        }
    }
}

async fn output(
    handle: &SessionHandle,
    pattern: &str,
    activity: &mut watch::Receiver<Activity>,
    deadline: Instant,
) -> Result<Waited, Failure> {
    let re = regex::RegexBuilder::new(pattern)
        .size_limit(1 << 20)
        .build()
        .map_err(|e| Failure::new(ErrorCode::Invalid, format!("bad pattern {pattern:?}: {e}")))?;
    let mut from = match handle.mark() {
        Some(mark) => mark,
        None => handle.mark_if_unset(super::position(handle).await?),
    };
    loop {
        let exited = activity.borrow_and_update().exited;
        let Ok(Text::Since(since)) = handle.read(Read::Since(from)).await else {
            return Ok(Waited::Closed);
        };
        if let Some((index, text)) = since.lines.into_iter().find(|(_, text)| re.is_match(text)) {
            let past = index.saturating_add(1);
            handle.advance_mark(Position { line: past, col: 0, epoch: since.next.epoch });
            return Ok(Waited::Met { line: Some(Line { index, text }) });
        }
        from = since.next;
        // Behind a flood: read on without waiting for more.
        if since.behind {
            continue;
        }
        if exited {
            return Ok(Waited::Closed);
        }
        match changed(activity, deadline).await {
            Wake::Changed => {}
            Wake::TimedOut => return Ok(Waited::TimedOut),
            Wake::Gone => return Ok(Waited::Closed),
        }
    }
}

/// Blocked on a human, idle at its prompt, or done with its turn.
const fn needs_input(status: &AgentStatus) -> bool {
    matches!(status, AgentStatus::Blocked(_) | AgentStatus::Idle | AgentStatus::Done)
}

async fn agent_input(
    handle: &SessionHandle,
    mut feed: AgentFeed,
    activity: &mut watch::Receiver<Activity>,
    deadline: Instant,
) -> Result<Waited, Failure> {
    if matches!(feed.now, Some(AgentStatus::Blocked(_))) {
        return Ok(Waited::Met { line: None });
    }
    let session = handle.id();
    loop {
        tokio::select! {
            ev = feed.events.recv() => match ev {
                Ok(HostMsg::Agent(ev)) if ev.session == session && needs_input(&ev.status) => {
                    return Ok(Waited::Met { line: None });
                }
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return Ok(Waited::Closed),
            },
            wake = changed(activity, deadline) => match wake {
                Wake::Changed => {
                    if activity.borrow().exited {
                        return Ok(Waited::Closed);
                    }
                }
                Wake::TimedOut => return Ok(Waited::TimedOut),
                Wake::Gone => return Ok(Waited::Closed),
            },
        }
    }
}

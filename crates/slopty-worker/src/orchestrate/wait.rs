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
//! - `AgentNeedsInput` is refused: an agent in a terminal is the person's, and the worker reads
//!   nothing of its screen, so nothing tells its turns. An agent Slopty drives is a thread, whose
//!   coming to rest orchestration hears from its row (`EventFilter::AgentNeedsInput`).
//!
//! Every condition but `Exit` ends as [`Waited::Closed`] when the program exits first.

use std::time::Duration;

use slopty_engine::ghostty::Position;
use slopty_proto::orchestration::{ErrorCode, Line, WaitUntil, Waited};
use tokio::sync::watch;
use tokio::time::Instant;

use super::Failure;
use crate::session::{Activity, Read, SessionHandle, Text};

/// Why a wait for a terminal's agent to come to rest is refused (see the module).
const NO_AGENT_WAIT: &str = "the worker cannot tell when an agent in a terminal comes to rest: \
    it knows only that one runs there; drive the agent as a thread (start_thread) and wait on \
    its events (events --agent-input)";

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
/// [`ErrorCode::Invalid`] for a pattern that does not compile, and for an agent wait (see the
/// module); [`ErrorCode::UnknownTerminal`] when the session is gone before the wait starts.
pub async fn wait_for(
    handle: &SessionHandle,
    until: &WaitUntil,
    timeout: Duration,
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
        WaitUntil::AgentNeedsInput => Err(Failure::new(ErrorCode::Invalid, NO_AGENT_WAIT)),
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

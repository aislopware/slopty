//! A thread gone on from in a new one, on another agent or on its own afresh
//! ([`Intent::Continue`](slopty_proto::thread::wire::Intent::Continue)).
//!
//! The new thread starts with nothing sent. A portable account of the old one
//! ([`slopty_agent::handoff`]) waits on the worker as its first message, a draft the person
//! reads, changes and sends ([`schedule::draft`]): no word reaches the agent behind them. The
//! new thread says which one it went on from ([`Host::forked`]), through the old one's last
//! turn.

use std::future::Future;

use slopty_agent::handoff;
use slopty_proto::thread::wire::{Outcome, Start};
use slopty_proto::thread::{AgentId, Cap, Fork, IntentId, ThreadId};

use super::{Host, schedule};

/// Go on from thread `from` in a new one on `agent` for intent `id`, once.
///
/// `begin` starts it as a client's start goes. A repeat of the id starts nothing again, and
/// finishes what the first may not have: its draft and where it came from.
pub async fn carry<F, Fut>(
    host: &Host,
    from: ThreadId,
    id: IntentId,
    agent: AgentId,
    begin: F,
) -> Outcome
where
    F: FnOnce(IntentId, Start) -> Fut,
    Fut: Future<Output = Outcome>,
{
    let Some((state, _)) = host.state(from) else {
        return refused("There is no such thread here");
    };
    let outcome = match host.started(id) {
        Some(first) => first,
        None if !state.meta.can(Cap::CONTINUE) => {
            return Outcome::Unsupported { cap: Cap::named(Cap::CONTINUE) };
        }
        None if state.turns.is_empty() => return refused("There is nothing to go on from yet"),
        None => {
            let cwd = state.meta.cwd.clone();
            let start = Start { agent, cwd, drive: None, prompt: None, model: None, args: vec![] };
            begin(id, start).await
        }
    };
    if let Outcome::Started { thread } = outcome {
        host.forked(thread, Fork { thread: from, turn: state.last_turn().map(|t| t.id) });
        let text = handoff::render(&state, handoff::BUDGET);
        if schedule::draft(host, thread, id, text).is_none() {
            tracing::warn!(%from, %thread, "the thread gone on in left before its draft was kept");
        }
    }
    outcome
}

fn refused(reason: &str) -> Outcome {
    Outcome::Refused { reason: reason.to_owned() }
}

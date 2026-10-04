//! Edit from a turn ([`Intent::Rewind`](slopty_proto::thread::wire::Intent::Rewind)): the thread
//! goes back to just before it, in a new thread.
//!
//! The conversation goes back only through the agent's own door: a branch of its session cut
//! before the turn, which Codex makes (`thread/fork` with `beforeTurnId`). No agent's session
//! file is written, and the thread edited from goes on as it was. The turn's message waits on
//! the new thread as a draft ([`schedule::draft`]), and with the files the folder goes back to
//! the turn's before-snapshot, what it held first kept under the thread's refs
//! ([`super::review::Snapshots::restore`]).

use std::future::Future;

use slopty_proto::thread::wire::Outcome;
use slopty_proto::thread::{
    Cap, IntentId, ItemBody, Phase, ThreadId, ThreadState, TurnId, TurnState,
};

use super::review::Snapshots;
use super::{Host, schedule};

/// Edit thread `from` from turn `turn` for intent `id`, once, its folder going back too with
/// `files`: `branch` asks the agent for the new thread cut before the turn.
///
/// A repeat of `id` branches nothing again and finishes what the first may not have: the
/// files and the draft, each once.
pub async fn rewind<F, Fut>(
    host: &Host,
    snapshots: &Snapshots,
    (from, id): (ThreadId, IntentId),
    (turn, files): (TurnId, bool),
    branch: F,
) -> Outcome
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Outcome>,
{
    let Some((state, _)) = host.state(from) else { return refused("There is no such thread here") };
    let Some(prompt) = prompt(&state, turn) else {
        return refused(&format!("Turn {} has no message of the person's to edit", turn.0));
    };
    if host.outcome(from, id).is_none() {
        if !state.meta.can(Cap::REWIND) {
            return Outcome::Unsupported { cap: Cap::named(Cap::REWIND) };
        }
        if working(&state) {
            return refused("Stop the turn under way first");
        }
        if files && state.turns.iter().find(|t| t.id == turn).is_none_or(|t| t.before.is_none()) {
            return refused("No snapshot was taken before that turn, so its files cannot go back");
        }
    }
    let outcome = branch().await;
    let Outcome::Started { thread } = outcome else { return outcome };
    if files {
        let restored = snapshots.restore(from, files_of(id), turn).await;
        if let Some(Outcome::Refused { reason }) = restored {
            return refused(&format!(
                "The new thread is there, but the files did not go back: {reason}"
            ));
        }
    }
    if schedule::draft(host, thread, id, prompt).is_none() {
        tracing::warn!(%from, %thread, "the edited thread left before its draft was kept");
    }
    outcome
}

/// The intent the files of edit `intent` go back under, on the thread edited from.
#[must_use]
pub fn files_of(intent: IntentId) -> IntentId {
    let derived = ThreadId::derived(&["rewind files", &intent.to_string()]);
    IntentId::from_uuid(*derived.as_uuid())
}

/// The words the person sent to start turn `turn` of `state`.
fn prompt(state: &ThreadState, turn: TurnId) -> Option<String> {
    let started = state.turns.iter().find(|t| t.id == turn)?;
    let mine = |body: &ItemBody| match body {
        ItemBody::User(message) => Some(message.text.text.clone()),
        _ => None,
    };
    let mut items = state.items.iter().filter(|i| i.turn == turn);
    let input = started
        .input
        .as_ref()
        .and_then(|input| state.items.iter().find(|i| i.id == *input).and_then(|i| mine(&i.body)));
    input.or_else(|| items.find_map(|i| mine(&i.body)))
}

/// Whether `state`'s agent has a turn under way.
fn working(state: &ThreadState) -> bool {
    matches!(state.status.phase, Phase::Working | Phase::NeedsYou)
        || state.last_turn().is_some_and(|turn| turn.state == TurnState::Active)
}

fn refused(reason: &str) -> Outcome {
    Outcome::Refused { reason: reason.to_owned() }
}

//! A thread as orchestration reads it ([`Verb::ReadThread`](slopty_proto::orchestration::Verb)):
//! whole turns after the one a reader holds, through the agent-neutral thread model, so a
//! Claude Code, Codex, pi or ACP agent's work reads alike.
//!
//! A read is small enough for a model to take in at once. Each text is cut to its head (the
//! person's words, the agent's answers) or its tail (what a tool printed, whose end says how it
//! went), and the read stops at the first whole turn past its budget. Its cursor is the last
//! whole turn it gave, so a turn under way is read again, grown, until it ends, and a reader that
//! goes on from the cursor misses nothing the budget left out.

use slopty_core::WorkerId;
use slopty_proto::orchestration::{ReadEntry, RequestRead, ThreadRead, ThreadView, TurnRead};
use slopty_proto::thread::{Clipped, ItemBody, ThreadState, TurnId, TurnState};

/// Most characters of one message or answer a read gives: its head.
pub const TEXT_CHARS: usize = 4_000;

/// Most characters of what one tool call printed a read gives: its tail.
pub const OUTPUT_CHARS: usize = 1_000;

/// Most characters of every text in one read together. One turn always goes, however long:
/// its texts are each cut already.
pub const READ_CHARS: usize = 48_000;

/// What `state`'s thread on `worker` did after turn `after` (from its first turn held when
/// `None`), as `view` asks.
#[must_use]
pub fn read(
    state: &ThreadState,
    worker: WorkerId,
    view: ThreadView,
    after: Option<TurnId>,
) -> ThreadRead {
    let after = after.unwrap_or(TurnId::BEFORE);
    let first_held = state.turns.first().map(|t| t.id);
    // A turn after the one asked from that is no longer held: some were let go unread.
    let skipped =
        state.older && first_held.is_some_and(|first| first.0 > after.0.saturating_add(1));
    let mut truncated = false;
    let mut spent = 0_usize;
    let mut turns = Vec::new();
    let mut next = after;
    // The cursor stops before the first turn under way: it is read again until it ends.
    let mut under_way = false;
    for turn in state.turns.iter().filter(|t| t.id > after) {
        let mut cut = false;
        let entries: Vec<ReadEntry> = state
            .items
            .iter()
            .filter(|i| i.turn == turn.id)
            .filter_map(|i| entry(&i.body, view, &mut cut))
            .collect();
        let chars: usize = entries.iter().map(chars_of).sum();
        if !turns.is_empty() && spent.saturating_add(chars) > READ_CHARS {
            truncated = true;
            break;
        }
        spent = spent.saturating_add(chars);
        truncated |= cut;
        under_way |= matches!(turn.state, TurnState::Active);
        if !under_way {
            next = turn.id;
        }
        turns.push(TurnRead {
            id: turn.id,
            state: turn.state.clone(),
            started_ms: turn.started_ms,
            ended_ms: turn.ended_ms,
            entries,
        });
    }
    let requests = state
        .requests
        .iter()
        .filter(|r| r.is_open())
        .map(|r| RequestRead {
            ask: r.id.clone(),
            kind: r.kind.clone(),
            title: r.title.clone(),
            choices: r.options.clone(),
            questions: r.questions.iter().map(|q| q.text.clone()).collect(),
            picks: slopty_proto::thread::wire::NoteChoice::of(r),
        })
        .collect();
    ThreadRead {
        worker,
        thread: state.meta.id,
        agent: state.meta.agent.clone(),
        title: state.meta.title.clone(),
        parent: state.meta.parent.as_ref().map(|l| l.thread),
        phase: state.status.phase,
        wait: state.status.wait.as_ref().map(|w| w.text.clone()),
        turns,
        requests,
        next,
        truncated,
        skipped,
    }
}

/// What a read gives of an item, in `view`; `cut` set when its text was cut short.
fn entry(body: &ItemBody, view: ThreadView, cut: &mut bool) -> Option<ReadEntry> {
    let activity = view == ThreadView::Activity;
    match body {
        ItemBody::User(message) => Some(ReadEntry::User(head(&message.text, cut))),
        ItemBody::Text(text) => Some(ReadEntry::Text(head(text, cut))),
        ItemBody::Tool(call) if activity => Some(ReadEntry::Tool {
            kind: call.kind.clone(),
            title: call.title.clone(),
            state: call.state.clone(),
            output: call.output.as_ref().map(|o| tail(o, cut)),
            child: call.child,
        }),
        ItemBody::Notice(notice) if activity => {
            Some(ReadEntry::Notice { kind: notice.kind.clone(), text: head(&notice.text, cut) })
        }
        ItemBody::Tool(_)
        | ItemBody::Notice(_)
        | ItemBody::Reasoning(_)
        | ItemBody::Compaction(_)
        | ItemBody::Review { .. }
        | ItemBody::Extra { .. } => None,
    }
}

/// The characters an entry's texts take.
fn chars_of(entry: &ReadEntry) -> usize {
    match entry {
        ReadEntry::User(text) | ReadEntry::Text(text) | ReadEntry::Notice { text, .. } => {
            text.chars().count()
        }
        ReadEntry::Tool { title, output, .. } => {
            title.chars().count().saturating_add(output.as_ref().map_or(0, |o| o.chars().count()))
        }
    }
}

/// The head of `text`, at most [`TEXT_CHARS`] of it, marked when it is not all of it.
fn head(text: &Clipped, cut: &mut bool) -> String {
    let whole = text.full.is_none();
    match text.text.char_indices().nth(TEXT_CHARS) {
        Some((at, _)) => {
            *cut = true;
            format!("{}{}", text.text.get(..at).unwrap_or_default(), ThreadRead::CUT)
        }
        None if whole => text.text.clone(),
        None => {
            *cut = true;
            format!("{}{}", text.text, ThreadRead::CUT)
        }
    }
}

/// The tail of `text`, at most [`OUTPUT_CHARS`] of it, marked when it is not all of it.
fn tail(text: &Clipped, cut: &mut bool) -> String {
    let count = text.text.chars().count();
    match count.checked_sub(OUTPUT_CHARS).filter(|skip| *skip > 0) {
        Some(skip) => {
            *cut = true;
            let at = text.text.char_indices().nth(skip).map_or(0, |(at, _)| at);
            format!("{}{}", ThreadRead::CUT.trim_start(), text.text.get(at..).unwrap_or_default())
        }
        None if text.full.is_none() => text.text.clone(),
        None => {
            *cut = true;
            format!("{}{}", ThreadRead::CUT.trim_start(), text.text)
        }
    }
}

#[cfg(test)]
mod tests;

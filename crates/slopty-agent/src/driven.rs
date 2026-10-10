//! What every adapter that drives its agent over a protocol (pi's RPC mode, ACP) maps the same way.
//!
//! How much of a text a thread keeps, a thread's title, its capabilities, an effort's name, an
//! answer offered, and what a thread is left as when its agent ends unheard.

use slopty_core::WallMs;
use slopty_proto::thread::detail::{Clip, Hunk, header_heading};
use slopty_proto::thread::{
    Action, Cap, Choice, Effect, Item, ItemBody, Liveness, Patch, Phase, RequestState, Status,
    ThreadState, ToolCall, ToolState, TurnState,
};

/// Prose: answers, thinking.
pub const PROSE: Clip = Clip { lines: 400, chars: 32_000 };
/// A call's input or output.
pub const OUTPUT: Clip = Clip { lines: 40, chars: 4_000 };
/// A thread's title, from its first message, in characters.
const TITLE_CHARS: usize = 80;

/// `names` as a thread's capabilities, sorted.
#[must_use]
pub fn caps(names: &[&str]) -> Vec<Cap> {
    let mut caps: Vec<Cap> = names.iter().map(|c| Cap::named(c)).collect();
    caps.sort();
    caps.dedup();
    caps
}

/// A reasoning effort's name for people, from the agent's own (`high` is "High"; `xhigh`, which
/// Codex and pi both name so, is "Extra high", as Codex's picker says it).
#[must_use]
pub fn effort_label(effort: &str) -> String {
    match effort {
        "xhigh" => "Extra high".to_owned(),
        other => {
            let mut chars = other.chars();
            chars
                .next()
                .map_or_else(String::new, |first| first.to_uppercase().chain(chars).collect())
        }
    }
}

/// An answer the agent offers.
#[must_use]
pub fn choice(id: &str, label: &str, effect: Effect, stops: bool) -> Choice {
    Choice { id: id.to_owned(), label: label.to_owned(), effect, scope: None, stops }
}

/// A tool call as an item's body.
#[must_use]
pub fn tool(call: ToolCall) -> ItemBody {
    ItemBody::Tool(Box::new(call))
}

/// The diff of texts replaced, each `(old, new)` one hunk without line numbers: the agent sends
/// the texts, not where in the file they are. An empty `old` is a whole file written.
#[must_use]
pub fn replaced_patch(replacements: &[(String, String)]) -> Patch {
    crate::conversation::proposed_patch(replacements)
}

/// A unified diff's hunks, as Codex writes a file change's and pi an edit's: file headers are
/// skipped, and each `@@` header starts a hunk numbered as it says.
#[must_use]
pub fn unified_patch(diff: &str) -> Patch {
    let mut patch = Patch::default();
    for line in diff.lines() {
        if let Some(head) = line.strip_prefix("@@ ") {
            let mut ranges = head.split(' ');
            let old = ranges.next().and_then(|r| r.strip_prefix('-')).map(range);
            let new = ranges.next().and_then(|r| r.strip_prefix('+')).map(range);
            let ((old_start, old_lines), (new_start, new_lines)) =
                (old.unwrap_or_default(), new.unwrap_or_default());
            patch.hunks.push(Hunk {
                old_start,
                old_lines,
                new_start,
                new_lines,
                heading: header_heading(line),
                lines: Vec::new(),
            });
            continue;
        }
        if line.starts_with("+++") || line.starts_with("---") {
            continue;
        }
        let Some(hunk) = patch.hunks.last_mut() else { continue };
        if line.starts_with('+') {
            patch.added = patch.added.saturating_add(1);
        } else if line.starts_with('-') {
            patch.removed = patch.removed.saturating_add(1);
        }
        hunk.lines.push(line.to_owned());
    }
    patch
}

/// `start,lines` of a hunk header; one line when it says no count.
fn range(text: &str) -> (u32, u32) {
    let mut parts = text.split(',');
    let start = parts.next().and_then(|s| s.parse().ok()).unwrap_or_default();
    let lines = parts.next().map_or(1, |s| s.parse().unwrap_or_default());
    (start, lines)
}

/// A thread's title from the first thing the person said: its first line, cut to a length.
#[must_use]
pub fn title_of(text: &str) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or_default().trim();
    match line.char_indices().nth(TITLE_CHARS) {
        Some((at, _)) => format!("{}…", line.get(..at).unwrap_or(line).trim_end()),
        None => line.to_owned(),
    }
}

/// When the usage windows in `limits` that are full have all reset: the latest of their
/// resets. `None` when none is full, or a full one says no reset.
#[must_use]
pub fn reset_of_full(limits: &[slopty_proto::thread::Limit]) -> Option<WallMs> {
    let full: Vec<Option<WallMs>> =
        limits.iter().filter(|l| l.used_bp >= 10_000).map(|l| l.resets_ms).collect();
    if full.is_empty() || full.iter().any(Option::is_none) {
        return None;
    }
    full.into_iter().flatten().max()
}

/// The thread `state` of an agent that ended unheard, as when the worker that ran it stopped.
///
/// Its requests are no longer asked, its calls are cancelled, the turn under way ends stopped,
/// and it is exited, `resumable` when its session can be taken up again.
#[must_use]
pub fn cut_short(state: &ThreadState, resumable: bool, now: WallMs) -> Vec<Action> {
    let mut actions: Vec<Action> = state
        .open_requests()
        .map(|r| Action::RequestResolved { id: r.id.clone(), state: RequestState::Withdrawn })
        .collect();
    for item in &state.items {
        let ItemBody::Tool(call) = &item.body else { continue };
        if call.state.is_final() || !call.state.may_become(&ToolState::Cancelled) {
            continue;
        }
        let mut call = call.clone();
        call.state = ToolState::Cancelled;
        call.ended_ms = Some(now);
        actions.push(Action::ItemCompleted(Item { body: tool(*call), ..item.clone() }));
    }
    let cut = state.last_turn().filter(|t| t.state == TurnState::Active);
    if let Some(turn) = cut {
        actions.push(Action::TurnEnded {
            turn: turn.id,
            state: TurnState::Interrupted,
            usage: turn.usage.clone(),
            ended_ms: now,
        });
    }
    let phase = match state.status.phase {
        _ if cut.is_some() => Phase::Stopped,
        Phase::Working | Phase::NeedsYou | Phase::Waiting => Phase::Idle,
        other => other,
    };
    let liveness = Liveness::Exited { resumable };
    actions.push(Action::Status(Status { phase, wait: None, liveness, since_ms: now }));
    actions
}

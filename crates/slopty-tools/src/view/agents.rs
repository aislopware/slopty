//! The answers of the verbs that reach another agent and the screen, and move files: turns of
//! a thread, a still picture, a file moved.

use std::borrow::Cow;
use std::fmt::Write as _;

use serde::Serialize;
use slopty_proto::orchestration::{ReadEntry, RequestRead, ThreadRead, TurnRead};
use slopty_proto::thread::{Phase, ToolState, TurnState};

use crate::bulk::Moved;

/// Turns of a thread, for JSON.
#[derive(Debug, Serialize)]
pub struct ThreadReadView<'a> {
    worker: String,
    thread: String,
    agent: &'a str,
    title: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent: Option<String>,
    phase: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    wait: Option<&'a str>,
    turns: Vec<TurnView<'a>>,
    requests: Vec<RequestView<'a>>,
    /// Read again from here (`after`) for what came next.
    next: u32,
    truncated: bool,
    skipped: bool,
}

/// A turn of a thread, for JSON.
#[derive(Debug, Serialize)]
pub struct TurnView<'a> {
    turn: u32,
    state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'a str>,
    started_ms: slopty_core::WallMs,
    #[serde(skip_serializing_if = "Option::is_none")]
    ended_ms: Option<slopty_core::WallMs>,
    entries: Vec<ReadEntryView<'a>>,
}

/// One thing in a turn, for JSON.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReadEntryView<'a> {
    /// The person's words.
    User {
        /// Them.
        text: &'a str,
    },
    /// The agent's answer.
    Agent {
        /// It.
        text: &'a str,
    },
    /// A tool call.
    Tool {
        /// Its kind.
        kind: &'a str,
        /// What it does.
        title: &'a str,
        /// Where it is.
        state: &'static str,
        /// The end of its output.
        #[serde(skip_serializing_if = "Option::is_none")]
        output: Option<&'a str>,
        /// A subagent's thread it started, to read with `thread`.
        #[serde(skip_serializing_if = "Option::is_none")]
        child: Option<String>,
    },
    /// The agent's own word.
    Notice {
        /// Its kind.
        kind: &'a str,
        /// What it says.
        text: &'a str,
    },
}

/// An open request, for JSON.
#[derive(Debug, Serialize)]
pub struct RequestView<'a> {
    ask: &'a str,
    kind: &'a str,
    title: &'a str,
    choices: Vec<ChoiceView<'a>>,
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    questions: &'a [String],
}

/// A choice a request offers, for JSON.
#[derive(Debug, Serialize)]
pub struct ChoiceView<'a> {
    id: &'a str,
    label: &'a str,
}

/// A thread's phase, as a word.
#[must_use]
pub const fn phase_key(phase: Phase) -> &'static str {
    match phase {
        Phase::Idle => "idle",
        Phase::Working => "working",
        Phase::Waiting => "waiting",
        Phase::NeedsYou => "needs_you",
        Phase::Done => "done",
        Phase::Failed => "failed",
        Phase::Stopped => "stopped",
    }
}

const fn turn_key(state: &TurnState) -> &'static str {
    match state {
        TurnState::Active => "active",
        TurnState::Complete => "complete",
        TurnState::Interrupted => "interrupted",
        TurnState::Failed { .. } => "failed",
    }
}

const fn tool_key(state: &ToolState) -> &'static str {
    match state {
        ToolState::Streaming => "streaming",
        ToolState::Pending { .. } => "pending",
        ToolState::Running => "running",
        ToolState::Completed => "completed",
        ToolState::Failed => "failed",
        ToolState::Rejected => "rejected",
        ToolState::Cancelled => "cancelled",
    }
}

fn entry(entry: &ReadEntry) -> ReadEntryView<'_> {
    match entry {
        ReadEntry::User(text) => ReadEntryView::User { text },
        ReadEntry::Text(text) => ReadEntryView::Agent { text },
        ReadEntry::Tool { kind, title, state, output, child } => ReadEntryView::Tool {
            kind,
            title,
            state: tool_key(state),
            output: output.as_deref(),
            child: child.map(|c| c.to_string()),
        },
        ReadEntry::Notice { kind, text } => ReadEntryView::Notice { kind, text },
    }
}

fn turn(turn: &TurnRead) -> TurnView<'_> {
    TurnView {
        turn: turn.id.0,
        state: turn_key(&turn.state),
        error: match &turn.state {
            TurnState::Failed { error, .. } => Some(error),
            TurnState::Active | TurnState::Complete | TurnState::Interrupted => None,
        },
        started_ms: turn.started_ms,
        ended_ms: turn.ended_ms,
        entries: turn.entries.iter().map(entry).collect(),
    }
}

fn request(request: &RequestRead) -> RequestView<'_> {
    RequestView {
        ask: &request.ask.0,
        kind: &request.kind,
        title: &request.title,
        choices: request
            .choices
            .iter()
            .map(|c| ChoiceView { id: &c.id, label: &c.label })
            .collect(),
        questions: &request.questions,
    }
}

/// Turns of a thread, for JSON.
#[must_use]
pub fn thread_read(read: &ThreadRead) -> ThreadReadView<'_> {
    ThreadReadView {
        worker: read.worker.to_string(),
        thread: read.thread.to_string(),
        agent: &read.agent.0,
        title: &read.title,
        parent: read.parent.map(|p| p.to_string()),
        phase: phase_key(read.phase),
        wait: read.wait.as_deref(),
        turns: read.turns.iter().map(turn).collect(),
        requests: read.requests.iter().map(request).collect(),
        next: read.next.0,
        truncated: read.truncated,
        skipped: read.skipped,
    }
}

/// The first line of `text`, at most 100 characters.
fn gist(text: &str) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    let mut gist: String = line.chars().take(100).collect();
    if line.chars().nth(100).is_some() {
        gist.push('…');
    }
    gist
}

/// Turns of a thread, for a person: a line for each thing in them, then the requests waiting,
/// each with the command that answers it.
#[must_use]
pub fn thread_read_text(read: &ThreadRead) -> String {
    let mut out = String::new();
    let wait = read.wait.as_deref().map(|w| format!(": {w}")).unwrap_or_default();
    let _infallible = writeln!(
        out,
        "{} {} ({}), {}{wait}",
        read.agent.0,
        read.thread,
        read.title,
        phase_key(read.phase).replace('_', " ")
    );
    if read.skipped {
        let _infallible = writeln!(out, "(earlier turns are no longer held)");
    }
    for t in &read.turns {
        let _infallible = writeln!(out, "turn {} ({})", t.id.0, turn_key(&t.state));
        for e in &t.entries {
            let line = match e {
                ReadEntry::User(text) => format!("you      {}", gist(text)),
                ReadEntry::Text(text) => format!("agent    {}", gist(text)),
                ReadEntry::Tool { title, state, child, .. } => {
                    let child = child.map(|c| format!(" -> thread {c}")).unwrap_or_default();
                    format!("tool     {} ({}){child}", gist(title), tool_key(state))
                }
                ReadEntry::Notice { kind, text } => format!("{kind:<8} {}", gist(text)),
            };
            let _infallible = writeln!(out, "  {line}");
        }
    }
    for r in &read.requests {
        let choices: Vec<&str> = r.choices.iter().map(|c| c.id.as_str()).collect();
        let _infallible = writeln!(
            out,
            "waiting  {}: slopty thread answer {} {} <{}>",
            r.title,
            read.thread,
            r.ask.0,
            choices.join(" | ")
        );
    }
    let more = if read.truncated { ", more to read" } else { "" };
    let _infallible = writeln!(out, "next: --after {}{more}", read.next.0);
    out
}

/// A still picture, for JSON: where it was written, when it was.
#[derive(Debug, Serialize)]
pub struct StillView<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<&'a str>,
    width: u32,
    height: u32,
    bytes: usize,
    format: &'static str,
}

/// A still picture of `bytes` PNG bytes, written to `path` when it was.
#[must_use]
pub const fn still(path: Option<&str>, width: u32, height: u32, bytes: usize) -> StillView<'_> {
    StillView { path, width, height, bytes, format: "png" }
}

/// A file moved, for JSON.
#[derive(Debug, Serialize)]
pub struct MovedView<'a> {
    worker: String,
    path: &'a str,
    local: Cow<'a, str>,
    size: u64,
}

/// A file moved, for JSON: its path on the worker and here.
#[must_use]
pub fn moved(moved: &Moved) -> MovedView<'_> {
    MovedView {
        worker: moved.worker.to_string(),
        path: &moved.remote,
        local: moved.local.to_string_lossy(),
        size: moved.size,
    }
}

#[cfg(test)]
mod tests {
    use slopty_core::{WallMs, WorkerId};
    use slopty_proto::thread::{AgentId, AskId, Choice, Effect, ThreadId, TurnId};

    use super::*;

    /// A read says each turn's words and calls in a line apiece, and each request waiting with
    /// the command that answers it; its JSON keeps every field, the next cursor too.
    #[test]
    fn a_read_names_its_turns_and_what_waits() {
        let thread = ThreadId::derived(&["t"]);
        let read = ThreadRead {
            worker: WorkerId::nil(),
            thread,
            agent: AgentId::named(AgentId::PI),
            title: "Fix the build".to_owned(),
            parent: None,
            phase: Phase::NeedsYou,
            wait: Some("Wants to run cargo test".to_owned()),
            turns: vec![TurnRead {
                id: TurnId(4),
                state: TurnState::Active,
                started_ms: WallMs::from_millis(1),
                ended_ms: None,
                entries: vec![
                    ReadEntry::User("fix the build\nplease".to_owned()),
                    ReadEntry::Tool {
                        kind: "exec".to_owned(),
                        title: "Run cargo test".to_owned(),
                        state: ToolState::Pending { ask: AskId("7".to_owned()) },
                        output: None,
                        child: None,
                    },
                ],
            }],
            requests: vec![RequestRead {
                ask: AskId("7".to_owned()),
                kind: "approval".to_owned(),
                title: "Run cargo test?".to_owned(),
                choices: vec![Choice {
                    id: "allow".to_owned(),
                    label: "Allow".to_owned(),
                    effect: Effect::Allow,
                    scope: None,
                    stops: false,
                }],
                questions: Vec::new(),
            }],
            next: TurnId(3),
            truncated: true,
            skipped: false,
        };
        let shown = thread_read_text(&read);
        assert!(shown.contains("  you      fix the build\n"), "{shown}");
        assert!(shown.contains("  tool     Run cargo test (pending)"), "{shown}");
        assert!(shown.contains(&format!("slopty thread answer {thread} 7 <allow>")), "{shown}");
        assert!(shown.contains("next: --after 3, more to read"), "{shown}");
        let json = serde_json::to_value(thread_read(&read)).unwrap();
        assert_eq!(json["phase"], "needs_you");
        assert_eq!(json["turns"][0]["entries"][0]["type"], "user");
        assert_eq!(json["turns"][0]["entries"][1]["state"], "pending");
        assert_eq!(json["requests"][0]["choices"][0]["id"], "allow");
        assert_eq!((json["next"].as_u64(), json["truncated"].as_bool()), (Some(3), Some(true)));
    }
}

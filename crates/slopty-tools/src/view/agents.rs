//! The answers of the verbs that reach another agent and the screen, and move files: a page of a
//! conversation, a still picture, a file moved.

use std::borrow::Cow;
use std::fmt::Write as _;

use serde::Serialize;
use slopty_proto::conversation::{
    Body, Entry, Meters, PermissionPrompt, ResultStatus, Suggestion, Task, ThreadId, ToolDetail,
};
use slopty_proto::orchestration::{ConversationPage, TermRef};

use super::term_string;
use crate::bulk::Moved;

/// How a thread is named in JSON and in arguments: `main`, or a subagent's id.
#[must_use]
pub fn thread_key(thread: &ThreadId) -> Cow<'_, str> {
    match thread {
        ThreadId::Main => Cow::Borrowed("main"),
        ThreadId::Agent(id) => Cow::Borrowed(id),
    }
}

/// The thread an argument names: `main` (or nothing) for the session's own, else a subagent's
/// id.
#[must_use]
pub fn thread_named(name: Option<&str>) -> ThreadId {
    match name {
        None | Some("main") => ThreadId::Main,
        Some(id) => ThreadId::Agent(id.to_owned()),
    }
}

/// A page of a conversation, for JSON. The entries, tasks and meters are the conversation
/// face's own types as they are.
#[derive(Debug, Serialize)]
pub struct ConversationView<'a> {
    term: String,
    thread: Cow<'a, str>,
    threads: Vec<ThreadView<'a>>,
    start: u32,
    next: u32,
    total: u32,
    entries: &'a [Entry],
    tasks: &'a [Task],
    meters: Option<&'a Meters>,
    held: Vec<HeldView<'a>>,
}

/// A thread of a conversation, for JSON.
#[derive(Debug, Serialize)]
pub struct ThreadView<'a> {
    thread: Cow<'a, str>,
    entries: u32,
    agent_type: Option<&'a str>,
    description: Option<&'a str>,
}

/// A permission prompt waiting for an answer, for JSON.
#[derive(Debug, Serialize)]
pub struct HeldView<'a> {
    ask: u64,
    tool: &'a str,
    detail: &'a ToolDetail,
    /// What `allow_always` grants.
    always: &'a [Suggestion],
    mode: Option<&'a str>,
    asked_ms: slopty_core::WallMs,
    until_ms: slopty_core::WallMs,
}

fn held(prompt: &PermissionPrompt) -> HeldView<'_> {
    HeldView {
        ask: prompt.ask,
        tool: &prompt.tool,
        detail: &prompt.detail,
        always: &prompt.suggestions,
        mode: prompt.mode.as_deref(),
        asked_ms: prompt.asked_ms,
        until_ms: prompt.until_ms,
    }
}

/// A page of a conversation, for JSON.
#[must_use]
pub fn conversation(term: TermRef, page: &ConversationPage) -> ConversationView<'_> {
    ConversationView {
        term: term_string(term),
        thread: thread_key(&page.thread),
        threads: page
            .threads
            .iter()
            .map(|t| ThreadView {
                thread: thread_key(&t.id),
                entries: t.entries,
                agent_type: t.origin.as_ref().and_then(|o| o.agent_type.as_deref()),
                description: t.origin.as_ref().and_then(|o| o.description.as_deref()),
            })
            .collect(),
        start: page.start,
        next: page.next,
        total: page.total,
        entries: &page.entries,
        tasks: &page.tasks,
        meters: page.meters.as_ref(),
        held: page.held.iter().map(held).collect(),
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

fn entry_text(entry: &Entry) -> String {
    match &entry.body {
        Body::Prompt(prompt) => format!("you       {}", gist(&prompt.text.text)),
        Body::Text(text) => format!("agent     {}", gist(&text.text)),
        Body::Thinking(text) => format!("thinking  {}", gist(&text.text)),
        Body::Tool(call) => {
            let status = match call.result.as_ref().map(|r| r.status) {
                None => "running",
                Some(ResultStatus::Ok) => "ok",
                Some(ResultStatus::Error) => "error",
                Some(ResultStatus::Rejected) => "rejected",
            };
            format!("tool      {} ({status})", call.name)
        }
        Body::Compact(_) => "compacted".to_owned(),
        Body::Interrupted { .. } => "interrupted".to_owned(),
        Body::Note(note) => format!("note      {}", gist(&note.text.text)),
        Body::Rewound { dropped } => format!("rewound   {dropped} entries dropped"),
    }
}

/// A page of a conversation, for a person: one line an entry, then the prompts waiting.
#[must_use]
pub fn conversation_text(term: TermRef, page: &ConversationPage) -> String {
    let mut out = String::new();
    let thread = thread_key(&page.thread);
    let _infallible =
        writeln!(out, "{thread}: entries {}..{} of {}", page.start, page.next, page.total);
    for (index, entry) in (page.start..).zip(&page.entries) {
        let _infallible = writeln!(out, "{index:>5}  {}", entry_text(entry));
    }
    let others: Vec<String> = page
        .threads
        .iter()
        .filter(|t| t.id != page.thread)
        .map(|t| format!("{} ({})", thread_key(&t.id), t.entries))
        .collect();
    if !others.is_empty() {
        let _infallible = writeln!(out, "threads: {}", others.join(", "));
    }
    for task in &page.tasks {
        let _infallible = writeln!(out, "task  [{}] {}", task.status, task.subject);
    }
    for prompt in &page.held {
        let _infallible = writeln!(
            out,
            "waiting  #{} {}: slopty agent answer {} {} --allow | --deny",
            prompt.ask,
            prompt.tool,
            term_string(term),
            prompt.ask,
        );
    }
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
    use slopty_core::{SessionId, WallMs, WorkerId};
    use slopty_proto::conversation::{Clipped, Prompt};
    use slopty_proto::orchestration::ThreadInfo;

    use super::*;

    /// A page reads as the face's entries with their place, the other threads, and each
    /// prompt waiting with the answer that takes it.
    #[test]
    fn a_page_names_its_entries_and_what_waits() {
        let term = TermRef { worker: WorkerId::nil(), session: SessionId::nil() };
        let text =
            Clipped { text: "fix the build\nplease".to_owned(), lines: 2, chars: 20, full: None };
        let page = ConversationPage {
            threads: vec![
                ThreadInfo { id: ThreadId::Main, origin: None, entries: 8 },
                ThreadInfo { id: ThreadId::Agent("a1".to_owned()), origin: None, entries: 2 },
            ],
            thread: ThreadId::Main,
            entries: vec![Entry {
                id: "u1".to_owned(),
                at_ms: WallMs::from_millis(1),
                body: Body::Prompt(Prompt { text, images: Vec::new(), command: None }),
            }],
            start: 7,
            next: 8,
            total: 8,
            tasks: Vec::new(),
            meters: None,
            held: Vec::new(),
        };
        let shown = conversation_text(term, &page);
        assert!(shown.contains("    7  you       fix the build\n"), "{shown}");
        assert!(shown.contains("threads: a1 (2)"), "{shown}");
        let json = serde_json::to_value(conversation(term, &page)).unwrap();
        assert_eq!(json["thread"], "main");
        assert_eq!(json["threads"][1]["thread"], "a1");
        assert_eq!(json["entries"][0]["id"], "u1");
        assert_eq!(thread_named(Some("a1")), ThreadId::Agent("a1".to_owned()));
        assert_eq!(thread_named(None), ThreadId::Main);
    }
}

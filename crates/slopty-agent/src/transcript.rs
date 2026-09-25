//! Reading a Claude Code transcript (JSONL) for status: the last thing the assistant said,
//! and what the newest record says the turn is doing ([`Tail`], [`progress`]).
//!
//! Each line is one record with a `timestamp`; assistant turns look like
//! `{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"…"
//! },{"type":"text","text":"…"},{"type":"tool_use","id":"…","name":"Bash","input":{…}}]}}`,
//! user turns carry a string, `text` blocks, or `tool_result` blocks, and the many other
//! record types (`summary`, `attachment`, `file-history-snapshot`, `system`…) are
//! bookkeeping. Sidechains (`isSidechain`) are subagents talking to themselves and are
//! skipped everywhere. [`last_assistant_line`] reads only the file's tail: a transcript grows
//! to megabytes, and the answer is always in the last few records.

use std::io::{BufReader, Read as _, Seek as _, SeekFrom};
use std::path::Path;

use serde_json::Value;
use slopty_proto::agent::AgentStatus;

/// How much of the file's end is scanned; one assistant record with a long thinking block can
/// run to tens of kilobytes.
const TAIL_BYTES: u64 = 256 * 1024;
/// The last non-empty line of the last assistant text block in the transcript at `path`.
/// `None` when the file cannot be read or holds no assistant text in its tail.
#[must_use]
pub fn last_assistant_line(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut tail = Vec::new();
    BufReader::new(file).read_to_end(&mut tail).ok()?;
    // The cut may land inside a character; that line is skipped whole anyway.
    last_assistant_line_in(&String::from_utf8_lossy(&tail), start > 0)
}

/// [`last_assistant_line`] over text already in memory; `cut` says the first line may be a
/// fragment (the read started mid-record) and must be skipped.
#[must_use]
pub fn last_assistant_line_in(tail: &str, cut: bool) -> Option<String> {
    let mut lines: Vec<&str> = tail.lines().collect();
    if cut {
        lines.drain(..1.min(lines.len()));
    }
    lines.iter().rev().find_map(|line| assistant_text(line))
}

/// The last non-empty text line of an assistant record; `None` for any other record.
fn assistant_text(line: &str) -> Option<String> {
    // Cheap pre-filter before parsing: most records are not assistant text.
    if !line.contains("\"assistant\"") {
        return None;
    }
    let record: Value = serde_json::from_str(line).ok()?;
    if record.get("type").and_then(Value::as_str) != Some("assistant")
        || record.get("isSidechain").and_then(Value::as_bool) == Some(true)
    {
        return None;
    }
    let content = record.get("message")?.get("content")?;
    let text = match content {
        Value::String(s) => s.as_str(),
        Value::Array(blocks) => blocks
            .iter()
            .rev()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .find_map(|b| b.get("text").and_then(Value::as_str))?,
        _ => return None,
    };
    last_line(text)
}

/// A cursor into a growing transcript file.
///
/// [`Tail::read`] returns the progress of what was appended since the previous call, keeping an
/// unterminated last line for the next one. A file that shrank (Claude Code rewrote it, or a
/// new session reuses the path) starts over, and the read says so.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Tail {
    /// Bytes consumed so far.
    offset: u64,
    /// The last line seen without its newline, waiting for the rest. Bytes, since a read may
    /// end inside a character the writer has not finished.
    partial: Vec<u8>,
}

/// What one [`Tail::read`] found.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Read {
    /// The file was replaced or truncated: the reader started over from its beginning, and
    /// whatever the caller kept before is stale.
    pub restarted: bool,
    /// What the newest record says the agent is doing ([`progress`]); `None` when nothing in
    /// this read moved the turn on.
    pub progress: Option<Progress>,
}

/// What the transcript's newest record says the agent is doing.
///
/// This is the second-strongest attribution signal, used when no hook has spoken for the
/// session. The records say what a turn is doing, never what it is waiting for: a permission
/// prompt is not written to the transcript until it has been answered, so a tail can report
/// [`AgentStatus::Working`], [`AgentStatus::Tool`] and [`AgentStatus::Done`], and never
/// [`AgentStatus::Blocked`]. Only the hooks report blocking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    /// The status the record implies.
    pub status: AgentStatus,
    /// One line about it, already [`crate::truncate`]d.
    pub detail: Option<String>,
}

impl Tail {
    /// The progress of what was appended since the last read. A missing file is an empty read, not
    /// an error (the agent may not have written it yet); other I/O errors are returned.
    ///
    /// # Errors
    ///
    /// When the file exists but cannot be read.
    pub fn read(&mut self, path: &Path) -> std::io::Result<Read> {
        let mut file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Read::default()),
            Err(e) => return Err(e),
        };
        let len = file.metadata()?.len();
        let restarted = len < self.offset;
        if restarted {
            *self = Self::default();
        }
        if len == self.offset {
            return Ok(Read { restarted, ..Read::default() });
        }
        file.seek(SeekFrom::Start(self.offset))?;
        let mut appended = Vec::new();
        BufReader::new(file).read_to_end(&mut appended)?;
        self.offset = self.offset.saturating_add(appended.len().try_into().unwrap_or(u64::MAX));
        self.partial.append(&mut appended);
        let Some(end) = self.partial.iter().rposition(|b| *b == b'\n') else {
            return Ok(Read { restarted, ..Read::default() });
        };
        let rest = self.partial.split_off(end.saturating_add(1));
        let complete = std::mem::replace(&mut self.partial, rest);
        Ok(Read { restarted, progress: progress(&String::from_utf8_lossy(&complete)) })
    }
}

/// What the newest record of `jsonl` says the agent is doing, or `None` when none of the
/// complete lines is a main-thread turn record.
#[must_use]
pub fn progress(jsonl: &str) -> Option<Progress> {
    jsonl.lines().rev().find_map(line_progress)
}

/// The progress one record implies; `None` for bookkeeping, sidechains and empty turns.
fn line_progress(line: &str) -> Option<Progress> {
    let record: Value = serde_json::from_str(line).ok()?;
    record_progress(&record)
}

/// The progress one parsed record implies; `None` for bookkeeping, sidechains and empty
/// turns. The records Claude Code streams over stdio have the same shape as the file's.
#[must_use]
pub fn record_progress(record: &Value) -> Option<Progress> {
    if is_subagent(record) {
        return None;
    }
    let message = record.get("message")?;
    let content = message.get("content")?;
    match record.get("type").and_then(Value::as_str)? {
        // A prompt, or the result of a tool the agent called: either way the turn is running —
        // unless it is the record Claude Code writes when the human pressed Esc, which ends
        // the turn with no `Stop` hook and is the one thing only the transcript can say.
        "user" => {
            let prompt = user_prompt(content);
            if prompt.as_deref().is_some_and(is_interrupt) {
                return Some(Progress {
                    status: AgentStatus::Idle,
                    detail: Some("interrupted".to_owned()),
                });
            }
            Some(Progress {
                status: AgentStatus::Working,
                detail: prompt.map(|t| crate::truncate(&t)),
            })
        }
        "assistant" => assistant_progress(message, content),
        _ => None,
    }
}

/// The user record Claude Code writes on Esc: "[Request interrupted by user]" or
/// "[Request interrupted by user for tool use]".
fn is_interrupt(text: &str) -> bool {
    text.starts_with("[Request interrupted by user")
}

/// The human's own words in a user record, if it holds any (a record of nothing but tool
/// results, or one of the app's injected texts, has none).
fn user_prompt(content: &Value) -> Option<String> {
    let text = match content {
        Value::String(s) => s.as_str(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .find_map(|b| b.get("text").and_then(Value::as_str))?,
        _ => return None,
    };
    let text = text.trim();
    if text.is_empty() || text.starts_with('<') {
        return None;
    }
    text.lines().next().map(|line| line.trim().to_owned())
}

/// An assistant record: the tool it is calling, or the answer that ended the turn.
fn assistant_progress(message: &Value, content: &Value) -> Option<Progress> {
    let blocks = match content {
        Value::String(s) => {
            return last_line(s).map(|l| Progress {
                status: AgentStatus::Working,
                detail: Some(crate::truncate(&l)),
            });
        }
        Value::Array(blocks) => blocks,
        _ => return None,
    };
    let tool = blocks
        .iter()
        .rev()
        .find(|b| {
            matches!(b.get("type").and_then(Value::as_str), Some("tool_use" | "server_tool_use"))
        })
        .and_then(|b| Some((b.get("name")?.as_str()?.to_owned(), b.get("input"))));
    let text = blocks
        .iter()
        .rev()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
        .find_map(|b| b.get("text").and_then(Value::as_str))
        .and_then(last_line);
    // `end_turn` is the only record that says the agent handed control back; a record with a
    // tool call is the agent running one, and anything else is still streaming.
    let ended = matches!(
        message.get("stop_reason").and_then(Value::as_str),
        Some("end_turn" | "stop_sequence" | "max_tokens")
    );
    if ended {
        return Some(Progress {
            status: AgentStatus::Done,
            detail: text.map(|t| crate::truncate(&t)),
        });
    }
    if let Some((name, input)) = tool {
        let summary = tool_summary(&name, input);
        return Some(Progress {
            status: AgentStatus::Tool { tool: name },
            detail: Some(summary).filter(|s| !s.is_empty()),
        });
    }
    text.map(|t| Progress { status: AgentStatus::Working, detail: Some(crate::truncate(&t)) })
}

/// A subagent's own record: a sidechain in the transcript file (`isSidechain`), or a record
/// under the call that spawned it over stream-json (`parent_tool_use_id`). Neither is the
/// agent's conversation.
#[must_use]
pub fn is_subagent(record: &Value) -> bool {
    record.get("isSidechain").and_then(Value::as_bool) == Some(true)
        || record.get("parent_tool_use_id").and_then(Value::as_str).is_some_and(|id| !id.is_empty())
}

/// One line saying what a tool call is about: the command, the file, the pattern, the URL,
/// the subagent's brief; for anything else the first string in the input.
#[must_use]
pub fn tool_summary(name: &str, input: Option<&Value>) -> String {
    let field = |key: &str| input?.get(key)?.as_str().map(str::to_owned);
    let text = match name {
        "Bash" => field("command"),
        "Read" | "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => field("file_path"),
        "Grep" | "Glob" => field("pattern"),
        "WebFetch" => field("url"),
        "WebSearch" => field("query"),
        "Agent" | "Task" => field("description").or_else(|| field("prompt")),
        "AskUserQuestion" => input
            .and_then(|i| i.get("questions")?.as_array()?.first()?.get("question")?.as_str())
            .map(str::to_owned),
        _ => input
            .and_then(Value::as_object)
            .and_then(|o| o.values().find_map(|v| v.as_str().map(str::to_owned))),
    };
    text.and_then(|t| last_line_first(&t)).unwrap_or_default()
}

/// The first non-empty line of a text, trimmed and cut to a readable length.
fn last_line_first(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    Some(crate::truncate(line))
}

/// The last non-empty line of a message, trimmed.
#[must_use]
pub fn last_line(text: &str) -> Option<String> {
    text.lines().map(str::trim).rfind(|l| !l.is_empty()).map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TAIL: &str = concat!(
        r#"{"type":"user","timestamp":"2026-09-05T10:00:00.000Z","message":{"role":"user","content":"fix it"}}"#,
        "\n",
        r#"{"type":"assistant","timestamp":"2026-09-05T10:00:01.500Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"hm"},{"type":"text","text":"Looking at the build.\n\nRunning the tests now."},{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"cargo test"}}]}}"#,
        "\n",
        r#"{"type":"assistant","isSidechain":true,"message":{"role":"assistant","content":[{"type":"text","text":"subagent chatter"}]}}"#,
        "\n",
        r#"{"type":"progress","data":{}}"#,
        "\n",
    );

    #[test]
    fn finds_the_last_main_thread_text() {
        assert_eq!(last_assistant_line_in(TAIL, false).as_deref(), Some("Running the tests now."));
    }

    #[test]
    fn a_cut_first_line_is_skipped() {
        let cut = TAIL.get(10..).unwrap_or_default();
        assert_eq!(last_assistant_line_in(cut, true).as_deref(), Some("Running the tests now."));
        assert_eq!(last_assistant_line_in("garbage\n", true), None);
        assert_eq!(last_assistant_line_in("", false), None);
    }

    #[test]
    fn the_newest_record_says_what_the_turn_is_doing() {
        // The fixture ends on the tool call the assistant made.
        assert_eq!(
            progress(TAIL),
            Some(Progress {
                status: AgentStatus::Tool { tool: "Bash".to_owned() },
                detail: Some("cargo test".to_owned()),
            })
        );
        // A prompt starts a turn and names it, whether its content is a string or blocks.
        let prompt =
            r#"{"type":"user","message":{"role":"user","content":"fix the build\nplease"}}"#;
        assert_eq!(
            progress(prompt),
            Some(Progress {
                status: AgentStatus::Working,
                detail: Some("fix the build".to_owned()),
            })
        );
        let blocks = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"in blocks"}]}}"#;
        assert_eq!(progress(blocks).and_then(|p| p.detail).as_deref(), Some("in blocks"));
        // An assistant record whose content is one string: its progress is its last line,
        // still working.
        let words = r#"{"type":"assistant","message":{"role":"assistant","content":"Just words.\nLast line."}}"#;
        assert_eq!(
            progress(words),
            Some(Progress { status: AgentStatus::Working, detail: Some("Last line.".to_owned()) })
        );
        // A tool result is the turn continuing, with nothing to say for itself.
        let result = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}"#;
        assert_eq!(progress(result), Some(Progress { status: AgentStatus::Working, detail: None }));
        // Esc: the turn is over without a Stop; the transcript is the only witness.
        for text in ["[Request interrupted by user]", "[Request interrupted by user for tool use]"]
        {
            let esc = format!(
                r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"{text}"}}]}}}}"#
            );
            assert_eq!(
                progress(&esc),
                Some(Progress {
                    status: AgentStatus::Idle,
                    detail: Some("interrupted".to_owned())
                }),
                "{text}"
            );
        }
        // `end_turn` is the turn handing control back, with the last line it said.
        let done = r#"{"type":"assistant","message":{"role":"assistant","stop_reason":"end_turn","content":[{"type":"text","text":"All green.\n\nDone: two edits."}]}}"#;
        assert_eq!(
            progress(done),
            Some(Progress {
                status: AgentStatus::Done,
                detail: Some("Done: two edits.".to_owned()),
            })
        );
        // Streaming text without a stop reason is still working.
        let streaming = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Looking at it."}]}}"#;
        assert_eq!(
            progress(streaming),
            Some(Progress {
                status: AgentStatus::Working,
                detail: Some("Looking at it.".to_owned()),
            })
        );
        // Bookkeeping, sidechains and the app's injected texts move nothing on.
        assert_eq!(progress(r#"{"type":"summary","summary":"s"}"#), None);
        assert_eq!(progress(""), None);
        let sidechain = r#"{"type":"assistant","isSidechain":true,"message":{"role":"assistant","stop_reason":"end_turn","content":[{"type":"text","text":"subagent"}]}}"#;
        assert_eq!(progress(sidechain), None);
        let injected = r#"{"type":"user","message":{"role":"user","content":"<system-reminder>x</system-reminder>"}}"#;
        assert_eq!(
            progress(injected),
            Some(Progress { status: AgentStatus::Working, detail: None })
        );
    }

    #[test]
    fn a_tail_reports_the_progress_of_what_it_read() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("t.jsonl");
        let mut tail = Tail::default();
        std::fs::write(&path, TAIL).expect("write");
        let read = tail.read(&path).expect("read");
        assert_eq!(
            read.progress.map(|p| p.status),
            Some(AgentStatus::Tool { tool: "Bash".to_owned() })
        );
        // Nothing new: nothing to say (the caller keeps the status it had).
        assert_eq!(tail.read(&path).expect("read").progress, None);
    }

    #[test]
    fn a_tail_returns_only_what_was_appended_and_keeps_partial_lines() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("t.jsonl");
        let mut tail = Tail::default();
        assert_eq!(tail.read(&path).expect("missing file is empty"), Read::default());

        let (first, rest) = TAIL.split_at(TAIL.find('\n').expect("newline") + 30);
        std::fs::write(&path, first).expect("write");
        let read = tail.read(&path).expect("read");
        assert_eq!(read.progress.map(|p| p.status), Some(AgentStatus::Working));
        assert!(!read.restarted);

        let mut appended = std::fs::read(&path).expect("read back");
        appended.extend_from_slice(rest.as_bytes());
        std::fs::write(&path, &appended).expect("append");
        let read = tail.read(&path).expect("read");
        assert_eq!(
            read.progress.map(|p| p.status),
            Some(AgentStatus::Tool { tool: "Bash".to_owned() }),
            "the appended records moved the turn on"
        );
        assert_eq!(tail.read(&path).expect("read"), Read::default(), "nothing new");

        // A result arriving in a later read keeps the turn running.
        let result = concat!(
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}"#,
            "\n"
        );
        appended.extend_from_slice(result.as_bytes());
        std::fs::write(&path, &appended).expect("append");
        let read = tail.read(&path).expect("read");
        assert_eq!(read.progress.as_ref().map(|p| p.status.clone()), Some(AgentStatus::Working));

        // A shorter file is a new transcript: start over and say so.
        std::fs::write(&path, first).expect("rewrite");
        let read = tail.read(&path).expect("read");
        assert!(read.restarted);
        assert_eq!(read.progress.map(|p| p.status), Some(AgentStatus::Working));
        // So is one cut back to a line still being written, or to nothing at all.
        std::fs::write(&path, "{\"type\":\"user\"").expect("rewrite");
        assert_eq!(tail.read(&path).expect("read"), Read { restarted: true, ..Read::default() });
        std::fs::write(&path, "").expect("truncate");
        assert_eq!(tail.read(&path).expect("read"), Read { restarted: true, ..Read::default() });
        // Only a missing file reads as nothing; a directory is an error.
        let _error = tail.read(dir.path()).expect_err("a directory is not a transcript");
    }

    #[test]
    fn reads_a_file_tail() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("t.jsonl");
        let mut big = String::new();
        for _ in 0..3000 {
            big.push_str(r#"{"type":"user","message":{"role":"user","content":"padding padding padding padding"}}"#);
            big.push('\n');
        }
        big.push_str(TAIL);
        std::fs::write(&path, &big).expect("write");
        assert_eq!(last_assistant_line(&path).as_deref(), Some("Running the tests now."));
        assert_eq!(last_assistant_line(&dir.path().join("missing.jsonl")), None);
        // A long tool result after the last words: the scan reaches back past it.
        let result = format!(
            r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"tool_result","content":"{}"}}]}}}}"#,
            "x".repeat(64 * 1024)
        );
        let later = format!("{big}{result}\n");
        std::fs::write(&path, &later).expect("write");
        assert_eq!(last_assistant_line(&path).as_deref(), Some("Running the tests now."));
    }

    /// A tail read that starts inside a multi-byte character still finds the last words.
    #[test]
    fn a_cut_inside_a_character_still_reads_the_tail() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("t.jsonl");
        let last =
            r#"{"type":"assistant","message":{"role":"assistant","content":"Xong rồi — đã sửa."}}"#;
        // One long line of three-byte characters reaching past the tail, then ASCII to move the
        // tail's first byte onto the second byte of one of them.
        let tail: usize = TAIL_BYTES.try_into().expect("fits");
        let wide = "ở".repeat(tail / 3 + 8);
        let body = (0..3)
            .map(|pad| format!("{{\"x\":\"{wide}{}\"}}\n{last}\n", "a".repeat(pad)))
            .find(|body| !body.is_char_boundary(body.len() - tail))
            .expect("one of three paddings");
        let cut = body.len() - tail;
        assert!(!body.is_char_boundary(cut), "the cut lands inside a character");
        std::fs::write(&path, &body).expect("write");
        assert_eq!(last_assistant_line(&path).as_deref(), Some("Xong rồi — đã sửa."));
    }

    /// A read that ends inside a character keeps its bytes for the next one, and a failed read
    /// loses nothing already kept.
    #[test]
    fn a_tail_keeps_a_character_split_across_reads() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("t.jsonl");
        let line = concat!(
            r#"{"type":"assistant","message":{"role":"assistant","content":"Đang chạy thử"}}"#,
            "\n"
        );
        let split = line.find('Đ').expect("Đ") + 1;
        assert!(!line.is_char_boundary(split));
        let mut tail = Tail::default();
        std::fs::write(&path, &line.as_bytes()[..split]).expect("write");
        assert_eq!(tail.read(&path).expect("read"), Read::default(), "no whole line yet");
        let _error = tail.read(dir.path()).expect_err("a directory is not a transcript");
        std::fs::write(&path, line).expect("write the rest");
        let read = tail.read(&path).expect("read");
        assert_eq!(read.progress.and_then(|p| p.detail).as_deref(), Some("Đang chạy thử"));
    }

    #[test]
    fn the_line_the_tail_starts_in_is_never_trusted_and_a_whole_file_is() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("t.jsonl");
        // A file read whole: its first line counts, even when it is the only one.
        let first =
            r#"{"type":"assistant","message":{"role":"assistant","content":"plain words"}}"#;
        std::fs::write(&path, format!("{first}\n")).expect("write");
        assert_eq!(last_assistant_line(&path).as_deref(), Some("plain words"));
        // A file read from the tail: the line the read starts in may be a fragment, so it is
        // skipped even when, as here, the cut lands exactly on its first byte.
        let user = r#"{"type":"user","message":{"role":"user","content":"pad"}}"#;
        let tail: usize = TAIL_BYTES.try_into().expect("fits");
        let frame = r#"{"type":"user","x":""}"#;
        let filler = "y".repeat(tail - (first.len() + 1) - (frame.len() + 1));
        let body = format!("{user}\n{first}\n{{\"type\":\"user\",\"x\":\"{filler}\"}}\n");
        assert_eq!(body.len() - tail, user.len() + 1, "the cut lands on the assistant line");
        std::fs::write(&path, &body).expect("write");
        assert_eq!(last_assistant_line(&path), None);
    }
}

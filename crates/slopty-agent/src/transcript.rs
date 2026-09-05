//! Reading a Claude Code transcript (JSONL): the last thing the assistant said, and the
//! conversation as entries a client can show ([`Tail`]).
//!
//! Each line is one record; assistant turns look like
//! `{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"…"},…]}}`,
//! user turns carry a string or `text`/`tool_result` blocks, and the many other record types
//! (`summary`, `attachment`, `file-history-snapshot`, `system`…) are bookkeeping. Sidechains
//! (`isSidechain`) are subagents talking to themselves and are skipped everywhere.
//! [`last_assistant_line`] reads only the file's tail: a transcript grows to megabytes, and
//! the answer is always in the last few records.

use std::io::{BufReader, Read as _, Seek as _, SeekFrom};
use std::path::Path;

use serde_json::Value;
use slopty_proto::agent::TranscriptEntry;

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
    let mut tail = String::new();
    BufReader::new(file).read_to_string(&mut tail).ok()?;
    last_assistant_line_in(&tail, start > 0)
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
/// [`Tail::read`] returns the entries appended since the previous call, keeping an
/// unterminated last line for the next one. A file that shrank (Claude Code rewrote it, or a
/// new session reuses the path) starts over, and the read says so.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Tail {
    /// Bytes consumed so far.
    offset: u64,
    /// The last line seen without its newline, waiting for the rest.
    partial: String,
}

/// What one [`Tail::read`] found.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Read {
    /// Entries in file order.
    pub entries: Vec<TranscriptEntry>,
    /// The file was replaced or truncated: the reader started over from its beginning, and
    /// whatever the caller showed before is stale.
    pub restarted: bool,
}

impl Tail {
    /// Entries appended since the last read. A missing file is an empty read, not an error
    /// (the agent may not have written it yet); other I/O errors are returned.
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
            return Ok(Read { entries: Vec::new(), restarted });
        }
        file.seek(SeekFrom::Start(self.offset))?;
        let mut text = std::mem::take(&mut self.partial);
        BufReader::new(file).read_to_string(&mut text)?;
        self.offset = len;
        let Some(end) = text.rfind('\n') else {
            self.partial = text;
            return Ok(Read { entries: Vec::new(), restarted });
        };
        let (complete, rest) = text.split_at(end);
        rest.get(1..).unwrap_or_default().clone_into(&mut self.partial);
        Ok(Read { entries: entries(complete), restarted })
    }
}

/// The conversation entries in complete JSONL lines, in order.
#[must_use]
pub fn entries(jsonl: &str) -> Vec<TranscriptEntry> {
    jsonl.lines().flat_map(line_entries).collect()
}

/// The entries one record contributes (none for bookkeeping, sidechains and tool results).
fn line_entries(line: &str) -> Vec<TranscriptEntry> {
    let Ok(record) = serde_json::from_str::<Value>(line) else { return Vec::new() };
    if record.get("isSidechain").and_then(Value::as_bool) == Some(true) {
        return Vec::new();
    }
    let kind = record.get("type").and_then(Value::as_str);
    let Some(content) = record.get("message").and_then(|m| m.get("content")) else {
        return Vec::new();
    };
    match kind {
        Some("user") => user_entries(content),
        Some("assistant") => assistant_entries(content),
        _ => Vec::new(),
    }
}

/// A user record: what the human typed. Tool results ride in user records too and are
/// skipped, as are the app's own injected texts (`<command-name>`, `<system-reminder>`…).
fn user_entries(content: &Value) -> Vec<TranscriptEntry> {
    let texts: Vec<&str> = match content {
        Value::String(s) => vec![s.as_str()],
        Value::Array(blocks) => blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect(),
        _ => Vec::new(),
    };
    texts
        .into_iter()
        .map(str::trim)
        .filter(|t| !t.is_empty() && !t.starts_with('<'))
        .map(|t| TranscriptEntry::User { text: t.to_owned() })
        .collect()
}

/// An assistant record: its text blocks and tool calls, in order; thinking is skipped.
fn assistant_entries(content: &Value) -> Vec<TranscriptEntry> {
    let blocks = match content {
        Value::String(s) => {
            return trimmed(s)
                .map(|markdown| vec![TranscriptEntry::Assistant { markdown }])
                .unwrap_or_default();
        }
        Value::Array(blocks) => blocks,
        _ => return Vec::new(),
    };
    blocks
        .iter()
        .filter_map(|block| match block.get("type").and_then(Value::as_str) {
            Some("text") => trimmed(block.get("text")?.as_str()?)
                .map(|markdown| TranscriptEntry::Assistant { markdown }),
            Some("tool_use" | "server_tool_use") => {
                let name = block.get("name")?.as_str()?.to_owned();
                let summary = tool_summary(&name, block.get("input"));
                Some(TranscriptEntry::ToolUse { name, summary })
            }
            _ => None,
        })
        .collect()
}

/// One line saying what a tool call is about: the command, the file, the pattern, the URL,
/// the subagent's brief; for anything else the first string in the input.
fn tool_summary(name: &str, input: Option<&Value>) -> String {
    let field = |key: &str| input?.get(key)?.as_str().map(str::to_owned);
    let text = match name {
        "Bash" => field("command"),
        "Read" | "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => field("file_path"),
        "Grep" | "Glob" => field("pattern"),
        "WebFetch" => field("url"),
        "WebSearch" => field("query"),
        "Agent" | "Task" => field("description").or_else(|| field("prompt")),
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

/// The text trimmed, `None` when nothing is left.
fn trimmed(text: &str) -> Option<String> {
    let t = text.trim();
    (!t.is_empty()).then(|| t.to_owned())
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
        r#"{"type":"user","message":{"role":"user","content":"fix it"}}"#,
        "\n",
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"hm"},{"type":"text","text":"Looking at the build.\n\nRunning the tests now."},{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"cargo test"}}]}}"#,
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
        let cut = &TAIL[10..];
        assert_eq!(last_assistant_line_in(cut, true).as_deref(), Some("Running the tests now."));
        assert_eq!(last_assistant_line_in("garbage\n", true), None);
        assert_eq!(last_assistant_line_in("", false), None);
    }

    #[test]
    fn entries_read_like_a_chat() {
        let got = entries(TAIL);
        assert_eq!(
            got,
            vec![
                TranscriptEntry::User { text: "fix it".to_owned() },
                TranscriptEntry::Assistant {
                    markdown: "Looking at the build.\n\nRunning the tests now.".to_owned()
                },
                TranscriptEntry::ToolUse {
                    name: "Bash".to_owned(),
                    summary: "cargo test".to_owned()
                },
            ],
            "{got:#?}"
        );
        // Tool results, injected texts and unknown records are not entries.
        let noise = concat!(
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":"<command-name>/clear</command-name>"}}"#,
            "\n",
            r#"{"type":"summary","summary":"s"}"#,
            "\n",
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t2","name":"Edit","input":{"file_path":"src/a.rs","old_string":"x"}},{"type":"tool_use","id":"t3","name":"Mystery","input":{"n":1,"why":"because"}}]}}"#,
            "\n",
        );
        assert_eq!(
            entries(noise),
            vec![
                TranscriptEntry::ToolUse {
                    name: "Edit".to_owned(),
                    summary: "src/a.rs".to_owned()
                },
                TranscriptEntry::ToolUse {
                    name: "Mystery".to_owned(),
                    summary: "because".to_owned()
                },
            ]
        );
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
        assert_eq!(read.entries, vec![TranscriptEntry::User { text: "fix it".to_owned() }]);
        assert!(!read.restarted);

        let mut appended = std::fs::read(&path).expect("read back");
        appended.extend_from_slice(rest.as_bytes());
        std::fs::write(&path, &appended).expect("append");
        let read = tail.read(&path).expect("read");
        assert_eq!(read.entries.len(), 2, "{read:#?}");
        assert!(matches!(read.entries[0], TranscriptEntry::Assistant { .. }));
        assert_eq!(tail.read(&path).expect("read"), Read::default(), "nothing new");

        // A shorter file is a new transcript: start over and say so.
        std::fs::write(&path, first).expect("rewrite");
        let read = tail.read(&path).expect("read");
        assert!(read.restarted);
        assert_eq!(read.entries, vec![TranscriptEntry::User { text: "fix it".to_owned() }]);
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
    }
}

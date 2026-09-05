//! Reading a Claude Code transcript (JSONL): the last thing the assistant said, and the
//! conversation as entries a client can show ([`Tail`]).
//!
//! Each line is one record with a `timestamp`; assistant turns look like
//! `{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"…"
//! },{"type":"text","text":"…"},{"type":"tool_use","id":"…","name":"Bash","input":{…}}]}}`,
//! user turns carry a string, `text` blocks, or `tool_result` blocks (`tool_use_id`, `content`
//! as a string or text blocks, `is_error`), and the many other record types (`summary`,
//! `attachment`, `file-history-snapshot`, `system`…) are bookkeeping. Sidechains
//! (`isSidechain`) are subagents talking to themselves and are skipped everywhere.
//! [`last_assistant_line`] reads only the file's tail: a transcript grows to megabytes, and
//! the answer is always in the last few records.

use std::collections::HashMap;
use std::io::{BufReader, Read as _, Seek as _, SeekFrom};
use std::path::Path;

use serde_json::Value;
use slopty_proto::agent::{AgentStatus, Clipped, TranscriptBody, TranscriptEntry};

/// How much of the file's end is scanned; one assistant record with a long thinking block can
/// run to tens of kilobytes.
const TAIL_BYTES: u64 = 256 * 1024;
/// Whole lines kept of a tool result, a thinking block or a tool input ([`clip`]).
pub const CLIP_LINES: usize = 40;
/// Characters kept of the same, whichever cap is hit first.
pub const CLIP_CHARS: usize = 4_000;
/// Tool ids remembered for naming their results; past this the table starts over.
const TOOL_NAMES_MAX: usize = 512;

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
    /// Tool calls seen, so their results can be named.
    tools: ToolNames,
}

/// `tool_use` ids → tool names, bounded.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ToolNames(HashMap<String, String>);

impl ToolNames {
    fn remember(&mut self, id: &str, name: &str) {
        if self.0.len() >= TOOL_NAMES_MAX {
            self.0.clear();
        }
        self.0.insert(id.to_owned(), name.to_owned());
    }

    fn name(&self, id: &str) -> Option<String> {
        self.0.get(id).cloned()
    }
}

/// What one [`Tail::read`] found.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Read {
    /// Entries in file order.
    pub entries: Vec<TranscriptEntry>,
    /// The file was replaced or truncated: the reader started over from its beginning, and
    /// whatever the caller showed before is stale.
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
            return Ok(Read { restarted, ..Read::default() });
        }
        file.seek(SeekFrom::Start(self.offset))?;
        let mut text = std::mem::take(&mut self.partial);
        BufReader::new(file).read_to_string(&mut text)?;
        self.offset = len;
        let Some(end) = text.rfind('\n') else {
            self.partial = text;
            return Ok(Read { restarted, ..Read::default() });
        };
        let (complete, rest) = text.split_at(end);
        rest.get(1..).unwrap_or_default().clone_into(&mut self.partial);
        Ok(Read {
            entries: entries_named(&mut self.tools, complete),
            restarted,
            progress: progress(complete),
        })
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
    if record.get("isSidechain").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let message = record.get("message")?;
    let content = message.get("content")?;
    match record.get("type").and_then(Value::as_str)? {
        // A prompt, or the result of a tool the agent called: either way the turn is running.
        "user" => Some(Progress {
            status: AgentStatus::Working,
            detail: user_prompt(content).map(|t| crate::truncate(&t)),
        }),
        "assistant" => assistant_progress(message, content),
        _ => None,
    }
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

/// The conversation entries in complete JSONL lines, in order. Results of tool calls in
/// the same text are named; for a growing file use [`Tail`], which remembers them across
/// reads.
#[must_use]
pub fn entries(jsonl: &str) -> Vec<TranscriptEntry> {
    entries_named(&mut ToolNames::default(), jsonl)
}

fn entries_named(tools: &mut ToolNames, jsonl: &str) -> Vec<TranscriptEntry> {
    jsonl.lines().flat_map(|line| line_entries(tools, line)).collect()
}

/// The entries one record contributes (none for bookkeeping and sidechains).
fn line_entries(tools: &mut ToolNames, line: &str) -> Vec<TranscriptEntry> {
    let Ok(record) = serde_json::from_str::<Value>(line) else { return Vec::new() };
    if record.get("isSidechain").and_then(Value::as_bool) == Some(true) {
        return Vec::new();
    }
    let kind = record.get("type").and_then(Value::as_str);
    let Some(content) = record.get("message").and_then(|m| m.get("content")) else {
        return Vec::new();
    };
    let at = record.get("timestamp").and_then(Value::as_str).and_then(timestamp_millis);
    let bodies = match kind {
        Some("user") => user_bodies(tools, content),
        Some("assistant") => assistant_bodies(tools, content),
        _ => Vec::new(),
    };
    bodies.into_iter().map(|body| TranscriptEntry { at, body }).collect()
}

/// An RFC 3339 record timestamp as milliseconds since the Unix epoch.
fn timestamp_millis(text: &str) -> Option<u64> {
    let at = chrono::DateTime::parse_from_rfc3339(text).ok()?;
    u64::try_from(at.timestamp_millis()).ok()
}

/// A user record: what the human typed, and the results of the tools the agent called (they
/// ride in user records too). The app's own injected texts (`<command-name>`,
/// `<system-reminder>`…) are skipped.
fn user_bodies(tools: &ToolNames, content: &Value) -> Vec<TranscriptBody> {
    let blocks = match content {
        Value::String(s) => return user_text(s).into_iter().collect(),
        Value::Array(blocks) => blocks,
        _ => return Vec::new(),
    };
    blocks
        .iter()
        .filter_map(|block| match block.get("type").and_then(Value::as_str) {
            Some("text") => user_text(block.get("text")?.as_str()?),
            Some("tool_result") => Some(TranscriptBody::ToolResult {
                tool: block
                    .get("tool_use_id")
                    .and_then(Value::as_str)
                    .and_then(|id| tools.name(id)),
                output: clip(&block_text(block.get("content"))),
                is_error: block.get("is_error").and_then(Value::as_bool).unwrap_or(false),
            }),
            _ => None,
        })
        .collect()
}

/// A typed prompt, unless empty or one of the app's injected texts.
fn user_text(text: &str) -> Option<TranscriptBody> {
    let t = text.trim();
    (!t.is_empty() && !t.starts_with('<')).then(|| TranscriptBody::User { text: t.to_owned() })
}

/// The text of a `content` field: a string, or the `text` of each text block joined by
/// blank lines.
fn block_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => String::new(),
    }
}

/// An assistant record: its thinking, text blocks and tool calls, in order.
fn assistant_bodies(tools: &mut ToolNames, content: &Value) -> Vec<TranscriptBody> {
    let blocks = match content {
        Value::String(s) => {
            return trimmed(s)
                .map(|markdown| vec![TranscriptBody::Assistant { markdown }])
                .unwrap_or_default();
        }
        Value::Array(blocks) => blocks,
        _ => return Vec::new(),
    };
    blocks
        .iter()
        .filter_map(|block| match block.get("type").and_then(Value::as_str) {
            Some("text") => trimmed(block.get("text")?.as_str()?)
                .map(|markdown| TranscriptBody::Assistant { markdown }),
            Some("thinking") => trimmed(block.get("thinking")?.as_str()?)
                .map(|text| TranscriptBody::Thinking { text: clip(&text) }),
            Some("tool_use" | "server_tool_use") => {
                let name = block.get("name")?.as_str()?.to_owned();
                if let Some(id) = block.get("id").and_then(Value::as_str) {
                    tools.remember(id, &name);
                }
                let input = block.get("input");
                let summary = tool_summary(&name, input);
                let input = clip(&input.map(pretty).unwrap_or_default());
                Some(TranscriptBody::ToolUse { name, summary, input })
            }
            _ => None,
        })
        .collect()
}

/// A tool input as the agent wrote it, indented.
fn pretty(input: &Value) -> String {
    serde_json::to_string_pretty(input).unwrap_or_default()
}

/// Cut `text` to at most [`CLIP_LINES`] whole lines and [`CLIP_CHARS`] characters, counting
/// the lines dropped; one line longer than the cap is cut mid-way with an ellipsis.
#[must_use]
pub fn clip(text: &str) -> Clipped {
    let text = text.trim_end();
    let mut kept = String::new();
    let mut lines = text.lines();
    let mut count = 0_usize;
    let mut chars = 0_usize;
    for line in lines.by_ref() {
        let len = line.chars().count();
        if count >= CLIP_LINES || chars.saturating_add(len) > CLIP_CHARS {
            if kept.is_empty() {
                // The very first line is too long on its own: keep its head.
                let head: String = line.chars().take(CLIP_CHARS.saturating_sub(1)).collect();
                kept = format!("{}…", head.trim_end());
                return Clipped {
                    text: kept,
                    more_lines: u32::try_from(lines.count()).unwrap_or(u32::MAX),
                };
            }
            let more = lines.count().saturating_add(1);
            return Clipped { text: kept, more_lines: u32::try_from(more).unwrap_or(u32::MAX) };
        }
        if count > 0 {
            kept.push('\n');
        }
        kept.push_str(line);
        count = count.saturating_add(1);
        chars = chars.saturating_add(len);
    }
    Clipped::whole(kept)
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
        r#"{"type":"user","timestamp":"2026-09-05T10:00:00.000Z","message":{"role":"user","content":"fix it"}}"#,
        "\n",
        r#"{"type":"assistant","timestamp":"2026-09-05T10:00:01.500Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"hm"},{"type":"text","text":"Looking at the build.\n\nRunning the tests now."},{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"cargo test"}}]}}"#,
        "\n",
        r#"{"type":"assistant","isSidechain":true,"message":{"role":"assistant","content":[{"type":"text","text":"subagent chatter"}]}}"#,
        "\n",
        r#"{"type":"progress","data":{}}"#,
        "\n",
    );

    fn user(at: Option<u64>, text: &str) -> TranscriptEntry {
        TranscriptEntry { at, body: TranscriptBody::User { text: text.to_owned() } }
    }

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
    fn entries_read_like_a_chat_with_timestamps() {
        let got = entries(TAIL);
        let at = Some(1_788_602_401_500);
        assert_eq!(
            got,
            vec![
                user(Some(1_788_602_400_000), "fix it"),
                TranscriptEntry {
                    at,
                    body: TranscriptBody::Thinking { text: Clipped::whole("hm".to_owned()) }
                },
                TranscriptEntry {
                    at,
                    body: TranscriptBody::Assistant {
                        markdown: "Looking at the build.\n\nRunning the tests now.".to_owned()
                    }
                },
                TranscriptEntry {
                    at,
                    body: TranscriptBody::ToolUse {
                        name: "Bash".to_owned(),
                        summary: "cargo test".to_owned(),
                        input: Clipped::whole("{\n  \"command\": \"cargo test\"\n}".to_owned()),
                    }
                },
            ],
            "{got:#?}"
        );
    }

    #[test]
    fn tool_results_are_named_after_their_call_and_injected_texts_are_not_entries() {
        let jsonl = concat!(
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t2","name":"Edit","input":{"file_path":"src/a.rs","old_string":"x"}},{"type":"tool_use","id":"t3","name":"Mystery","input":{"n":1,"why":"because"}}]}}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t2","content":"ok"},{"type":"tool_result","tool_use_id":"t3","content":[{"type":"text","text":"boom"},{"type":"text","text":"bang"}],"is_error":true},{"type":"tool_result","tool_use_id":"t9","content":""}]}}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":"<command-name>/clear</command-name>"}}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"  "},{"type":"text","text":"and then?"}]}}"#,
            "\n",
            r#"{"type":"summary","summary":"s"}"#,
            "\n",
        );
        let got = entries(jsonl);
        assert_eq!(
            got.iter().map(|e| e.body.clone()).collect::<Vec<_>>(),
            vec![
                TranscriptBody::ToolUse {
                    name: "Edit".to_owned(),
                    summary: "src/a.rs".to_owned(),
                    input: Clipped::whole(
                        "{\n  \"file_path\": \"src/a.rs\",\n  \"old_string\": \"x\"\n}".to_owned()
                    ),
                },
                TranscriptBody::ToolUse {
                    name: "Mystery".to_owned(),
                    summary: "because".to_owned(),
                    input: Clipped::whole("{\n  \"n\": 1,\n  \"why\": \"because\"\n}".to_owned()),
                },
                TranscriptBody::ToolResult {
                    tool: Some("Edit".to_owned()),
                    output: Clipped::whole("ok".to_owned()),
                    is_error: false,
                },
                TranscriptBody::ToolResult {
                    tool: Some("Mystery".to_owned()),
                    output: Clipped::whole("boom\n\nbang".to_owned()),
                    is_error: true,
                },
                TranscriptBody::ToolResult {
                    tool: None,
                    output: Clipped::default(),
                    is_error: false,
                },
                TranscriptBody::User { text: "and then?".to_owned() },
            ],
            "{got:#?}"
        );
        assert!(got.iter().all(|e| e.at.is_none()), "no timestamps in these records");
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
        // A prompt starts a turn and names it.
        let prompt =
            r#"{"type":"user","message":{"role":"user","content":"fix the build\nplease"}}"#;
        assert_eq!(
            progress(prompt),
            Some(Progress {
                status: AgentStatus::Working,
                detail: Some("fix the build".to_owned()),
            })
        );
        // A tool result is the turn continuing, with nothing to say for itself.
        let result = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}"#;
        assert_eq!(progress(result), Some(Progress { status: AgentStatus::Working, detail: None }));
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
    fn long_texts_are_clipped_with_a_count_of_what_was_dropped() {
        assert_eq!(clip("  a\nb  \n\n"), Clipped::whole("  a\nb".to_owned()));
        assert_eq!(clip(""), Clipped::default());

        let many: Vec<String> = (0..100).map(|i| format!("line {i}")).collect();
        let got = clip(&many.join("\n"));
        assert_eq!(got.text.lines().count(), CLIP_LINES);
        assert!(got.text.ends_with("line 39"), "{}", got.text);
        assert_eq!(got.more_lines, 60);

        // The character cap cuts at a whole line too.
        let wide: Vec<String> = (0..10).map(|i| format!("{i}{}", "x".repeat(999))).collect();
        let got = clip(&wide.join("\n"));
        assert_eq!(got.text.lines().count(), 4, "4 × 1000 chars fit, the fifth does not");
        assert_eq!(got.more_lines, 6);

        // One monster line keeps its head and an ellipsis.
        let got = clip(&format!("{}\nnext", "y".repeat(5000)));
        assert_eq!(got.text.chars().count(), CLIP_CHARS);
        assert!(got.text.ends_with('…'));
        assert_eq!(got.more_lines, 1);
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
        assert_eq!(read.entries, vec![user(Some(1_788_602_400_000), "fix it")]);
        assert!(!read.restarted);

        let mut appended = std::fs::read(&path).expect("read back");
        appended.extend_from_slice(rest.as_bytes());
        std::fs::write(&path, &appended).expect("append");
        let read = tail.read(&path).expect("read");
        assert_eq!(read.entries.len(), 3, "{read:#?}");
        assert!(matches!(read.entries[1].body, TranscriptBody::Assistant { .. }));
        assert_eq!(tail.read(&path).expect("read"), Read::default(), "nothing new");

        // A result arriving in a later read is still named after its call.
        let result = concat!(
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}"#,
            "\n"
        );
        appended.extend_from_slice(result.as_bytes());
        std::fs::write(&path, &appended).expect("append");
        let read = tail.read(&path).expect("read");
        assert!(
            matches!(&read.entries[..], [TranscriptEntry { body: TranscriptBody::ToolResult { tool: Some(t), .. }, .. }] if t == "Bash"),
            "{read:#?}"
        );

        // A shorter file is a new transcript: start over and say so.
        std::fs::write(&path, first).expect("rewrite");
        let read = tail.read(&path).expect("read");
        assert!(read.restarted);
        assert_eq!(read.entries, vec![user(Some(1_788_602_400_000), "fix it")]);
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

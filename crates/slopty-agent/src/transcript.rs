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
use slopty_proto::agent::{
    AgentStatus, Choice, Clipped, DiffKind, DiffLine, NoticeLevel, Question, Todo, TodoStatus,
    ToolDetail, TranscriptBody, TranscriptEntry,
};

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

/// A subagent's own record: a sidechain in the transcript file (`isSidechain`), or a record
/// under the call that spawned it over stream-json (`parent_tool_use_id`). Neither is the
/// agent's conversation.
#[must_use]
pub fn is_subagent(record: &Value) -> bool {
    record.get("isSidechain").and_then(Value::as_bool) == Some(true)
        || record.get("parent_tool_use_id").and_then(Value::as_str).is_some_and(|id| !id.is_empty())
}

/// The entries one record contributes (none for bookkeeping and sidechains).
fn line_entries(tools: &mut ToolNames, line: &str) -> Vec<TranscriptEntry> {
    let Ok(record) = serde_json::from_str::<Value>(line) else { return Vec::new() };
    record_entries(tools, &record)
}

/// The entries one parsed record contributes (none for bookkeeping and sidechains).
///
/// `tools` remembers the calls so their results can be named; keep one per conversation. An
/// edit's line is looked up against the record's own `cwd`.
#[must_use]
pub fn record_entries(tools: &mut ToolNames, record: &Value) -> Vec<TranscriptEntry> {
    let cwd = record.get("cwd").and_then(Value::as_str);
    record_entries_in(tools, record, cwd)
}

/// [`record_entries`] with the directory relative paths resolve against given by the
/// caller (a driven agent's records carry none; the fold knows it from `init`).
#[must_use]
pub fn record_entries_in(
    tools: &mut ToolNames,
    record: &Value,
    cwd: Option<&str>,
) -> Vec<TranscriptEntry> {
    if is_subagent(record) {
        return Vec::new();
    }
    let kind = record.get("type").and_then(Value::as_str);
    let at = record.get("timestamp").and_then(Value::as_str).and_then(timestamp_millis);
    if kind == Some("system") {
        return system_body(record).map(|body| TranscriptEntry { at, body }).into_iter().collect();
    }
    // The summary a compaction leaves is a user record to the agent, not to the human.
    if record.get("isCompactSummary").and_then(Value::as_bool) == Some(true) {
        return Vec::new();
    }
    let Some(content) = record.get("message").and_then(|m| m.get("content")) else {
        return Vec::new();
    };
    let bodies = match kind {
        Some("user") => user_bodies(tools, content),
        Some("assistant") => assistant_bodies(tools, content, cwd),
        _ => Vec::new(),
    };
    bodies.into_iter().map(|body| TranscriptEntry { at, body }).collect()
}

/// The entry a `system` record makes, if any: a compaction boundary, or a line from the
/// loop (`informational` past the `info` level, a model fallback, a permission retry).
pub fn system_body(record: &Value) -> Option<TranscriptBody> {
    let text = |key: &str| {
        record.get(key).and_then(Value::as_str).map(str::trim).filter(|t| !t.is_empty())
    };
    match record.get("subtype").and_then(Value::as_str)? {
        "compact_boundary" => compacted(record),
        // `info` lines show only in Claude Code's own transcript mode, and a line keyed to a
        // tool use is a progress message that would repeat.
        "informational" if record.get("tool_use_id").is_none() => {
            let level = match record.get("level").and_then(Value::as_str)? {
                "notice" => NoticeLevel::Notice,
                "suggestion" => NoticeLevel::Suggestion,
                "warning" => NoticeLevel::Warning,
                _ => return None,
            };
            Some(TranscriptBody::Notice { level, text: text("content")?.to_owned() })
        }
        "model_fallback" => {
            let fallback = text("fallback_model").unwrap_or("the fallback model");
            let why = text("content").map(|c| format!(": {c}")).unwrap_or_default();
            Some(TranscriptBody::Notice {
                level: NoticeLevel::Warning,
                text: format!("Switched to {fallback} for this turn{why}"),
            })
        }
        "permission_retry" => Some(TranscriptBody::Notice {
            level: NoticeLevel::Notice,
            text: text("content")?.to_owned(),
        }),
        // A Stop hook that failed to run (its feedback, when it ran, came as `informational`).
        "stop_hook_summary" => {
            let errors: Vec<&str> = record
                .get("hook_errors")
                .and_then(Value::as_array)
                .map(|list| {
                    list.iter()
                        .filter_map(Value::as_str)
                        .map(str::trim)
                        .filter(|e| !e.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            (!errors.is_empty()).then(|| TranscriptBody::Notice {
                level: NoticeLevel::Warning,
                text: format!("Stop hook failed: {}", errors.join("; ")),
            })
        }
        _ => None,
    }
}

/// A `system/compact_boundary` record: the stream spells its metadata `compact_metadata`
/// with snake keys, the transcript file `compactMetadata` with camel ones.
fn compacted(record: &Value) -> Option<TranscriptBody> {
    let meta = record.get("compact_metadata").or_else(|| record.get("compactMetadata"))?;
    let count = |snake: &str, camel: &str| {
        meta.get(snake).or_else(|| meta.get(camel)).and_then(Value::as_u64)
    };
    Some(TranscriptBody::Compacted {
        trigger: meta.get("trigger").and_then(Value::as_str).unwrap_or("auto").to_owned(),
        pre_tokens: count("pre_tokens", "preTokens").unwrap_or(0),
        post_tokens: count("post_tokens", "postTokens"),
    })
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
    let mut bodies: Vec<TranscriptBody> = blocks
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
        .collect();
    // The pictures ride on the prompt they were sent with (one entry, not one per block);
    // sent alone they are an entry with nothing typed.
    let pictures =
        blocks.iter().filter(|b| b.get("type").and_then(Value::as_str) == Some("image")).count();
    if let Ok(pictures) = u32::try_from(pictures)
        && pictures > 0
    {
        match bodies.iter_mut().find(|b| matches!(b, TranscriptBody::User { .. })) {
            Some(TranscriptBody::User { images, .. }) => *images = pictures,
            _ => bodies.insert(0, TranscriptBody::User { text: String::new(), images: pictures }),
        }
    }
    bodies
}

/// A typed prompt, unless empty or one of the app's injected texts.
fn user_text(text: &str) -> Option<TranscriptBody> {
    let t = text.trim();
    (!t.is_empty() && !t.starts_with('<'))
        .then(|| TranscriptBody::User { text: t.to_owned(), images: 0 })
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
fn assistant_bodies(
    tools: &mut ToolNames,
    content: &Value,
    cwd: Option<&str>,
) -> Vec<TranscriptBody> {
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
                let call = block.get("id").and_then(Value::as_str).unwrap_or_default().to_owned();
                if !call.is_empty() {
                    tools.remember(&call, &name);
                }
                let input = block.get("input");
                let summary = tool_summary(&name, input);
                let mut detail = tool_detail(&name, input);
                locate(&mut detail, input, cwd);
                Some(TranscriptBody::ToolUse { call, name, summary, detail })
            }
            _ => None,
        })
        .collect()
}

/// A tool input as the agent wrote it, indented.
fn pretty(input: &Value) -> String {
    serde_json::to_string_pretty(input).unwrap_or_default()
}

/// What a tool call would do, in the shape the client draws ([`ToolDetail`]).
///
/// The tools Claude Code ships are read by their input fields; anything else keeps its
/// input as pretty JSON. A known tool whose input lacks the field it is known by (a `Bash`
/// with no `command`) falls back to JSON too, so a shape change on the agent's side degrades
/// to what the card showed before, never to an empty block.
#[must_use]
pub fn tool_detail(name: &str, input: Option<&Value>) -> ToolDetail {
    let field = |key: &str| input?.get(key)?.as_str();
    let number = |key: &str| input?.get(key)?.as_u64().and_then(|n| u32::try_from(n).ok());
    let json = || ToolDetail::Json { input: clip(&input.map(pretty).unwrap_or_default()) };
    match name {
        "Bash" => field("command").map_or_else(json, |command| ToolDetail::Command {
            command: clip(command),
            description: field("description").map(str::to_owned),
        }),
        "Edit" => match (field("file_path"), field("old_string"), field("new_string")) {
            (Some(path), Some(old), Some(new)) => {
                let (lines, more_lines) = diff_lines(old, new);
                ToolDetail::Diff {
                    path: path.to_owned(),
                    line: None,
                    lines,
                    more_lines,
                    replace_all: input
                        .and_then(|i| i.get("replace_all"))
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                }
            }
            _ => json(),
        },
        "Write" => match (field("file_path"), field("content")) {
            (Some(path), Some(content)) => {
                ToolDetail::Write { path: path.to_owned(), content: clip(content) }
            }
            _ => json(),
        },
        "Read" => field("file_path").map_or_else(json, |path| ToolDetail::Read {
            path: path.to_owned(),
            offset: number("offset"),
            limit: number("limit"),
        }),
        "Grep" | "Glob" => field("pattern").map_or_else(json, |pattern| ToolDetail::Search {
            pattern: pattern.to_owned(),
            path: field("path").map(str::to_owned),
            glob: field("glob").map(str::to_owned),
        }),
        "TodoWrite" => input
            .and_then(|i| i.get("todos"))
            .and_then(Value::as_array)
            .map_or_else(json, |todos| ToolDetail::Todos {
                items: todos.iter().filter_map(todo).collect(),
            }),
        "AskUserQuestion" => input
            .and_then(|i| i.get("questions"))
            .and_then(Value::as_array)
            .map_or_else(json, |questions| ToolDetail::Question {
                questions: questions.iter().filter_map(question).collect(),
            }),
        "Agent" | "Task" => match (field("description"), field("prompt")) {
            (Some(description), Some(prompt)) => ToolDetail::Agent {
                description: description.to_owned(),
                kind: field("subagent_type").map(str::to_owned),
                prompt: clip(prompt),
            },
            _ => json(),
        },
        _ => json(),
    }
}

/// One `AskUserQuestion` question; `None` when it has no text.
fn question(item: &Value) -> Option<Question> {
    let text = item.get("question")?.as_str()?.trim();
    if text.is_empty() {
        return None;
    }
    let string = |v: &Value, key: &str| v.get(key).and_then(Value::as_str).unwrap_or("").to_owned();
    let options = item
        .get("options")
        .and_then(Value::as_array)
        .map(|options| {
            options
                .iter()
                .filter(|o| o.get("label").and_then(Value::as_str).is_some_and(|l| !l.is_empty()))
                .map(|o| Choice {
                    label: string(o, "label"),
                    description: string(o, "description"),
                })
                .collect()
        })
        .unwrap_or_default();
    Some(Question {
        text: text.to_owned(),
        header: string(item, "header"),
        multi: item.get("multiSelect").and_then(Value::as_bool).unwrap_or(false),
        options,
    })
}

/// One `TodoWrite` item; `None` when it has no text.
fn todo(item: &Value) -> Option<Todo> {
    let text = item.get("content")?.as_str()?.trim();
    if text.is_empty() {
        return None;
    }
    let status = match item.get("status").and_then(Value::as_str) {
        Some("completed") => TodoStatus::Completed,
        Some("in_progress") => TodoStatus::InProgress,
        _ => TodoStatus::Pending,
    };
    Some(Todo { text: text.to_owned(), status })
}

/// Files larger than this are not searched for an edit's line.
const LOCATE_BYTES: u64 = 4 * 1024 * 1024;

/// Fill an edit's `line` from the file it edits.
///
/// Where `old_string` starts in the file at `file_path` (made absolute against `cwd` when
/// relative), else where `new_string` does once the edit has landed; anything else, or a
/// file that cannot be read, leaves it `None`.
pub fn locate(detail: &mut ToolDetail, input: Option<&Value>, cwd: Option<&str>) {
    let ToolDetail::Diff { path, line, .. } = detail else { return };
    let field = |key: &str| input?.get(key)?.as_str();
    let (Some(old), Some(new)) = (field("old_string"), field("new_string")) else { return };
    let file = if path.starts_with('/') {
        std::path::PathBuf::from(&*path)
    } else {
        match cwd {
            Some(cwd) => Path::new(cwd).join(&*path),
            None => return,
        }
    };
    *line = edit_line(&file, old, new);
}

/// The 1-based line where `old` starts in the file at `path`, else where `new` does; `None`
/// when neither is there, the file is not text, or it is past `LOCATE_BYTES`.
#[must_use]
pub fn edit_line(path: &Path, old: &str, new: &str) -> Option<u32> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > LOCATE_BYTES {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    let at = [old, new]
        .into_iter()
        .filter(|needle| !needle.is_empty())
        .find_map(|needle| text.find(needle))?;
    let line = text.get(..at)?.matches('\n').count().saturating_add(1);
    u32::try_from(line).ok()
}

/// `old` against `new` line by line, cut the way [`clip`] cuts a text.
///
/// Kept to [`CLIP_LINES`] lines and [`CLIP_CHARS`] characters with the count of what was
/// dropped. Lines the two share are context, so a one-line change inside a ten-line
/// `old_string` reads as nine kept lines around one removed and one added.
#[must_use]
pub fn diff_lines(old: &str, new: &str) -> (Vec<DiffLine>, u32) {
    let diff = similar::TextDiff::from_lines(old, new);
    let mut lines = Vec::new();
    let mut chars = 0_usize;
    let mut dropped = 0_u32;
    for change in diff.iter_all_changes() {
        let kind = match change.tag() {
            similar::ChangeTag::Equal => DiffKind::Context,
            similar::ChangeTag::Delete => DiffKind::Removed,
            similar::ChangeTag::Insert => DiffKind::Added,
        };
        let text = change.value().trim_end_matches(['\n', '\r']);
        let len = text.chars().count();
        if lines.len() >= CLIP_LINES || chars.saturating_add(len) > CLIP_CHARS {
            dropped = dropped.saturating_add(1);
            continue;
        }
        chars = chars.saturating_add(len);
        lines.push(DiffLine { kind, text: text.to_owned() });
    }
    (lines, dropped)
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
        TranscriptEntry { at, body: TranscriptBody::User { text: text.to_owned(), images: 0 } }
    }

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
                        call: "t1".to_owned(),
                        name: "Bash".to_owned(),
                        summary: "cargo test".to_owned(),
                        detail: ToolDetail::Command {
                            command: Clipped::whole("cargo test".to_owned()),
                            description: None,
                        },
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
            r#"{"type":"system","subtype":"compact_boundary","compactMetadata":{"trigger":"auto","preTokens":167000,"postTokens":12000}}"#,
            "\n",
            r#"{"type":"user","isCompactSummary":true,"message":{"role":"user","content":"This session is being continued from a previous conversation."}}"#,
            "\n",
            // Loop lines: a hook's word is a notice, an `info` line and a keyed progress line
            // are not, a fallback names the model, a retry says what was allowed.
            r#"{"type":"system","subtype":"informational","content":"Stop says: the tests are red","level":"warning","isMeta":false}"#,
            "\n",
            r#"{"type":"system","subtype":"informational","content":"Tip: use /compact","level":"info"}"#,
            "\n",
            r#"{"type":"system","subtype":"informational","content":"Running for 3s","level":"notice","tool_use_id":"t2"}"#,
            "\n",
            r#"{"type":"system","subtype":"model_fallback","trigger":"overloaded","original_model":"claude-fable-5-1","fallback_model":"claude-sonnet-5","content":"Fable 5.1 is overloaded"}"#,
            "\n",
            r#"{"type":"system","subtype":"permission_retry","content":"Allowed cargo test","commands":["cargo test"]}"#,
            "\n",
            r#"{"type":"system","subtype":"stop_hook_summary","hook_count":2,"hook_infos":[],"hook_errors":[],"prevented_continuation":false,"has_output":true,"level":"info"}"#,
            "\n",
            r#"{"type":"system","subtype":"stop_hook_summary","hook_count":1,"hook_infos":[],"hook_errors":["goal: exit 1"],"prevented_continuation":false,"has_output":false,"level":"warning"}"#,
            "\n",
            // Pictures ride on the prompt they went with; alone they are an entry of their own.
            r#"{"type":"user","message":{"role":"user","content":[{"type":"image","source":{"type":"base64","media_type":"image/png","data":"iVBORw0KGgo="}},{"type":"text","text":"what colour?"}]}}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":[{"type":"image","source":{"type":"base64","media_type":"image/png","data":"iVBORw0KGgo="}},{"type":"image","source":{"type":"base64","media_type":"image/jpeg","data":"/9j/"}}]}}"#,
            "\n",
        );
        let got = entries(jsonl);
        assert_eq!(
            got.iter().map(|e| e.body.clone()).collect::<Vec<_>>(),
            vec![
                TranscriptBody::ToolUse {
                    call: "t2".to_owned(),
                    name: "Edit".to_owned(),
                    summary: "src/a.rs".to_owned(),
                    // An edit missing its replacement keeps the JSON, not an empty diff.
                    detail: ToolDetail::Json {
                        input: Clipped::whole(
                            "{\n  \"file_path\": \"src/a.rs\",\n  \"old_string\": \"x\"\n}"
                                .to_owned()
                        )
                    },
                },
                TranscriptBody::ToolUse {
                    call: "t3".to_owned(),
                    name: "Mystery".to_owned(),
                    summary: "because".to_owned(),
                    detail: ToolDetail::Json {
                        input: Clipped::whole(
                            "{\n  \"n\": 1,\n  \"why\": \"because\"\n}".to_owned()
                        )
                    },
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
                TranscriptBody::User { text: "and then?".to_owned(), images: 0 },
                // The boundary is an entry; the summary the agent reads is not.
                TranscriptBody::Compacted {
                    trigger: "auto".to_owned(),
                    pre_tokens: 167_000,
                    post_tokens: Some(12_000),
                },
                TranscriptBody::Notice {
                    level: NoticeLevel::Warning,
                    text: "Stop says: the tests are red".to_owned(),
                },
                TranscriptBody::Notice {
                    level: NoticeLevel::Warning,
                    text: "Switched to claude-sonnet-5 for this turn: Fable 5.1 is overloaded"
                        .to_owned(),
                },
                TranscriptBody::Notice {
                    level: NoticeLevel::Notice,
                    text: "Allowed cargo test".to_owned(),
                },
                // A Stop hook summary is an entry only when a hook failed to run.
                TranscriptBody::Notice {
                    level: NoticeLevel::Warning,
                    text: "Stop hook failed: goal: exit 1".to_owned(),
                },
                TranscriptBody::User { text: "what colour?".to_owned(), images: 1 },
                TranscriptBody::User { text: String::new(), images: 2 },
            ],
            "{got:#?}"
        );
        assert!(got.iter().all(|e| e.at.is_none()), "no timestamps in these records");
    }

    /// An edit's line is where its old text starts in the file, or its new text once the
    /// edit has landed; a relative path resolves against the record's `cwd`, and a file that
    /// is not there leaves the line unknown.
    #[test]
    fn an_edits_line_is_found_in_the_file_before_and_after_it_lands() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.rs");
        std::fs::write(&file, "fn a() {\n    1\n}\nfn b() {\n    x\n}\n").unwrap();
        assert_eq!(edit_line(&file, "fn b() {\n    x", "fn b() {\n    y"), Some(4));
        assert_eq!(edit_line(&file, "gone", "    1"), Some(2), "the new text after the edit");
        assert_eq!(edit_line(&file, "gone", "also gone"), None);
        let input = serde_json::json!({
            "file_path": "a.rs", "old_string": "    x", "new_string": "    y"
        });
        let mut detail = tool_detail("Edit", Some(&input));
        locate(&mut detail, Some(&input), dir.path().to_str());
        assert!(matches!(detail, ToolDetail::Diff { line: Some(5), .. }), "{detail:?}");
        let mut relative_without_cwd = tool_detail("Edit", Some(&input));
        locate(&mut relative_without_cwd, Some(&input), None);
        assert!(matches!(relative_without_cwd, ToolDetail::Diff { line: None, .. }));
        let record = serde_json::json!({
            "type": "assistant", "cwd": dir.path(),
            "message": {"role": "assistant", "content": [
                {"type": "tool_use", "id": "t", "name": "Edit", "input": input}
            ]}
        });
        let entries = record_entries(&mut ToolNames::default(), &record);
        assert!(
            matches!(
                entries.first().map(|e| &e.body),
                Some(TranscriptBody::ToolUse {
                    detail: ToolDetail::Diff { line: Some(5), .. },
                    ..
                })
            ),
            "{entries:?}"
        );
    }

    #[test]
    fn a_file_of_a_few_megabytes_is_searched_and_a_huge_one_is_not() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("big.txt");
        let rows = 1024 * 1024;
        let mut text = "x\n".repeat(rows);
        text.push_str("needle\n");
        std::fs::write(&path, &text).expect("write");
        assert_eq!(edit_line(&path, "needle", ""), Some(u32::try_from(rows + 1).expect("fits")));
        // Past the cap the file is left alone, whatever it holds.
        let file = std::fs::File::create(&path).expect("create");
        std::io::Write::write_all(&mut &file, b"needle\n").expect("write");
        file.set_len(LOCATE_BYTES + 1).expect("grow");
        assert_eq!(edit_line(&path, "needle", ""), None);
        file.set_len(LOCATE_BYTES).expect("shrink");
        assert_eq!(edit_line(&path, "needle", ""), Some(1), "at the cap it is searched");
    }

    #[test]
    fn an_edit_is_a_diff_a_todo_list_a_checklist_and_a_stranger_its_json() {
        let edit = serde_json::json!({
            "file_path": "src/a.rs",
            "old_string": "fn a() {\n    1\n}",
            "new_string": "fn a() {\n    2\n}",
            "replace_all": true
        });
        let line = |kind, text: &str| DiffLine { kind, text: text.to_owned() };
        assert_eq!(
            tool_detail("Edit", Some(&edit)),
            ToolDetail::Diff {
                path: "src/a.rs".to_owned(),
                line: None,
                lines: vec![
                    line(DiffKind::Context, "fn a() {"),
                    line(DiffKind::Removed, "    1"),
                    line(DiffKind::Added, "    2"),
                    line(DiffKind::Context, "}"),
                ],
                more_lines: 0,
                replace_all: true,
            }
        );
        let todos = serde_json::json!({"todos": [
            {"content": "read", "status": "completed", "activeForm": "Reading"},
            {"content": "write", "status": "in_progress"},
            {"content": "  ", "status": "pending"},
            {"content": "test"}
        ]});
        let todo = |text: &str, status| Todo { text: text.to_owned(), status };
        assert_eq!(
            tool_detail("TodoWrite", Some(&todos)),
            ToolDetail::Todos {
                items: vec![
                    todo("read", TodoStatus::Completed),
                    todo("write", TodoStatus::InProgress),
                    todo("test", TodoStatus::Pending),
                ]
            }
        );
        let bash =
            serde_json::json!({"command": "cargo test\n# twice", "description": "Run the tests"});
        assert_eq!(
            tool_detail("Bash", Some(&bash)),
            ToolDetail::Command {
                command: Clipped::whole("cargo test\n# twice".to_owned()),
                description: Some("Run the tests".to_owned()),
            }
        );
        let read = serde_json::json!({"file_path": "a.rs", "offset": 10, "limit": 20});
        assert_eq!(
            tool_detail("Read", Some(&read)),
            ToolDetail::Read { path: "a.rs".to_owned(), offset: Some(10), limit: Some(20) }
        );
        let grep = serde_json::json!({"pattern": "fn main", "glob": "*.rs"});
        assert_eq!(
            tool_detail("Grep", Some(&grep)),
            ToolDetail::Search {
                pattern: "fn main".to_owned(),
                path: None,
                glob: Some("*.rs".to_owned())
            }
        );
        let agent = serde_json::json!({
            "description": "Find it", "prompt": "Look everywhere", "subagent_type": "Explore"
        });
        assert_eq!(
            tool_detail("Agent", Some(&agent)),
            ToolDetail::Agent {
                description: "Find it".to_owned(),
                kind: Some("Explore".to_owned()),
                prompt: Clipped::whole("Look everywhere".to_owned()),
            }
        );
        let ask = serde_json::json!({"questions": [
            {"question": "Which colour?", "header": "Colour", "multiSelect": false,
             "options": [{"label": "Red", "description": "warm"}, {"label": "Blue"}, {"label": ""}]},
            {"question": "  "},
            {"question": "Which sizes?", "multiSelect": true}
        ]});
        assert_eq!(
            tool_detail("AskUserQuestion", Some(&ask)),
            ToolDetail::Question {
                questions: vec![
                    Question {
                        text: "Which colour?".to_owned(),
                        header: "Colour".to_owned(),
                        multi: false,
                        options: vec![
                            Choice { label: "Red".to_owned(), description: "warm".to_owned() },
                            Choice { label: "Blue".to_owned(), description: String::new() },
                        ],
                    },
                    Question {
                        text: "Which sizes?".to_owned(),
                        header: String::new(),
                        multi: true,
                        options: Vec::new(),
                    },
                ]
            }
        );
        assert_eq!(tool_summary("AskUserQuestion", Some(&ask)), "Which colour?");
        // Each tool's own field, not the first string in its input.
        let summary = |name: &str, input: Value| tool_summary(name, Some(&input));
        assert_eq!(summary("Grep", serde_json::json!({"glob":"*.rs","pattern":"todo"})), "todo");
        assert_eq!(
            summary("Glob", serde_json::json!({"path":"src","pattern":"**/*.rs"})),
            "**/*.rs"
        );
        assert_eq!(
            summary("WebFetch", serde_json::json!({"prompt":"summarise","url":"https://x.test"})),
            "https://x.test"
        );
        assert_eq!(summary("WebSearch", serde_json::json!({"query":"gpui"})), "gpui");
        assert_eq!(summary("Agent", serde_json::json!({"model":"opus","prompt":"do it"})), "do it");
        assert_eq!(
            summary("Task", serde_json::json!({"description":"scan","prompt":"do it"})),
            "scan"
        );
        // A known tool with a strange input, and a tool the host does not know.
        assert_eq!(
            tool_detail("Bash", Some(&serde_json::json!({"cmd": "ls"}))),
            ToolDetail::Json { input: Clipped::whole("{\n  \"cmd\": \"ls\"\n}".to_owned()) }
        );
        assert_eq!(tool_detail("WebFetch", None), ToolDetail::Json { input: Clipped::default() });
    }

    /// Over stream-json a subagent's records carry the spawning call as
    /// `parent_tool_use_id` (probed on CLI 2.1.269); like a sidechain in the file, they are
    /// not the agent's conversation and move no status.
    #[test]
    fn a_subagents_own_records_are_not_entries() {
        let jsonl = concat!(
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t9","name":"Bash","input":{"command":"ls"}}]},"parent_tool_use_id":"toolu_parent"}"#,
            "
",
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t9","content":"a.txt"}]},"parent_tool_use_id":"toolu_parent"}"#,
            "
",
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"done"}]},"parent_tool_use_id":null}"#,
            "
",
        );
        let got = entries(jsonl);
        assert_eq!(
            got.iter().map(|e| e.body.clone()).collect::<Vec<_>>(),
            [TranscriptBody::Assistant { markdown: "done".to_owned() }]
        );
        let sub: Value = serde_json::from_str(jsonl.lines().next().unwrap_or_default()).unwrap();
        assert_eq!(record_progress(&sub), None);
    }

    #[test]
    fn a_long_diff_is_cut_like_any_long_text() {
        let old = "line\n".repeat(30);
        let new = "row\n".repeat(30);
        let (lines, more) = diff_lines(&old, &new);
        assert_eq!(lines.len(), CLIP_LINES);
        assert_eq!(usize::try_from(more).unwrap_or(0), 60 - CLIP_LINES);
        assert!(lines.iter().all(|l| l.kind != DiffKind::Context), "{lines:?}");
        // The character cap is inclusive: a line that lands exactly on it is kept.
        let (lines, more) = diff_lines("", &"x".repeat(CLIP_CHARS));
        assert_eq!((lines.len(), more), (1, 0));
        let (lines, more) = diff_lines("", &"x".repeat(CLIP_CHARS + 1));
        assert_eq!((lines.len(), more), (0, 1));
        // A text without a final newline diffs the same as one with it.
        let (lines, more) = diff_lines("a\nb", "a\nc\n");
        assert_eq!(
            lines,
            [
                DiffLine { kind: DiffKind::Context, text: "a".to_owned() },
                DiffLine { kind: DiffKind::Removed, text: "b".to_owned() },
                DiffLine { kind: DiffKind::Added, text: "c".to_owned() },
            ]
        );
        assert_eq!(more, 0);
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
        // An assistant record whose content is one string is one Markdown entry, and its
        // progress is its last line, still working.
        let words_only = r#"{"type":"assistant","message":{"role":"assistant","content":"Just words.\nLast line."}}"#;
        assert_eq!(
            entries(words_only).into_iter().map(|e| e.body).collect::<Vec<_>>(),
            vec![TranscriptBody::Assistant { markdown: "Just words.\nLast line.".to_owned() }]
        );
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
    fn an_informational_line_is_a_notice_at_its_own_level() {
        let info = |level: &str| {
            system_body(&serde_json::json!({
                "type": "system", "subtype": "informational", "content": " Tip ", "level": level
            }))
        };
        let notice =
            |level: NoticeLevel| Some(TranscriptBody::Notice { level, text: "Tip".to_owned() });
        assert_eq!(info("notice"), notice(NoticeLevel::Notice));
        assert_eq!(info("suggestion"), notice(NoticeLevel::Suggestion));
        assert_eq!(info("warning"), notice(NoticeLevel::Warning));
        assert_eq!(info("debug"), None);
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
            matches!(&*read.entries, [TranscriptEntry { body: TranscriptBody::ToolResult { tool: Some(t), .. }, .. }] if t == "Bash"),
            "{read:#?}"
        );

        // A shorter file is a new transcript: start over and say so.
        std::fs::write(&path, first).expect("rewrite");
        let read = tail.read(&path).expect("read");
        assert!(read.restarted);
        assert_eq!(read.entries, vec![user(Some(1_788_602_400_000), "fix it")]);
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

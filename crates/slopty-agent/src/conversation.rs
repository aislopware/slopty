//! The conversation in a Claude Code transcript, decoded into typed entries for the conversation
//! face.
//!
//! The transcript is JSONL that Claude Code calls internal and changes between releases. Clients
//! never parse it: this decoder is the one place that does, it is tolerant (unknown record types,
//! unknown fields and unknown tools are passed over or kept generically), and it is pinned by
//! fixtures captured from real CLI versions (`tests/fixtures/conversation`, recaptured with
//! `cargo xtask fixtures claude`).
//!
//! **Records.** Each line is one record. `user` records carry a prompt, tool results (with a
//! structured `toolUseResult` beside the text the model saw), the interrupt marker, slash
//! commands and their output, or a compaction summary (`isCompactSummary`). `assistant` records
//! carry one content block each in current versions (text, thinking or a tool call), several in
//! older ones. `system` records mark compaction (`compact_boundary`) and a few notes worth
//! showing. `attachment` records are context for the model; the one kept is the queued
//! `task-notification` that says a background command or agent finished; the same notice is
//! read from the `queue-operation` Claude Code writes the moment the work ends, so a finished
//! command says so before the model has taken the notice in. Everything else is bookkeeping
//! and is skipped.
//!
//! **Threads.** A subagent writes its own file (`<session>/subagents/agent-<id>.jsonl`) whose
//! records say `isSidechain` and carry `agentId`; older versions wrote those records into the
//! main file. Either way they go to [`ThreadId::Agent`], keyed by agent id, and the main
//! conversation is [`ThreadId::Main`]. The `Agent` call that started a subagent names the same
//! id in its [`AgentDetail::agent_id`].
//!
//! **Chains.** Records link through `uuid`/`parentUuid`. A prompt whose parent is an earlier
//! record, not the newest one, starts a branch (a rewind or an edited prompt): the entries made
//! after the branch point are removed and the new branch continues from there. Any other record
//! continues the thread whatever its parent says: the results of parallel tool calls each name
//! the record of their own call, not the newest one. So does a record with no parent, or one
//! never seen (the file was picked up mid-way).
//!
//! **Incremental.** [`Conversation::read`] builds on [`Tail`]: each call decodes the lines
//! appended since the previous one and returns [`Change`]s. A tool call is an entry keyed by its
//! `tool_use_id`; its result, or the notice that its background work finished, arrives in a later
//! record and updates the same entry, which comes back as an [`Change::Upsert`]. Entry ids are
//! stable across reads: the call id for a tool call, the record uuid (with the block index for
//! text and thinking) for everything else.
//!
//! **Clipping.** The decoder runs on the worker, and nothing it produces carries a whole file or
//! a whole log. Prose (prompts, answers, thinking, plans, reports, summaries) is cut at
//! [`PROSE`], tool output and inputs at [`OUTPUT`] (Bash keeps the tail, the rest the head), and
//! a diff at [`PATCH_LINES`]. A cut text says how long the whole was and carries a [`TextRef`]
//! that [`full_text`] resolves against the transcript when someone asks to see all of it.
//!
//! **Pictures.** An image pasted into a prompt or returned by a tool is described, not carried
//! ([`media`]): its digest, type and size, and where it is, which [`image_bytes`] resolves.
//! A background command's output is not in the transcript at all; [`output::Outputs`] tails
//! the file the command writes.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::path::Path;

use serde_json::Value;
use slopty_core::WallMs;
pub use slopty_proto::conversation::{
    AgentDetail, AgentRun, Answer, BashDetail, Body, Cap, Change, Choice, Clipped, Compact,
    EditDetail, Entry, GlobDetail, GrepDetail, Hunk, IMAGE_BYTES, Image, Link, McpDetail, Note,
    NoteKind, Origin, Output, Part, Patch, Prompt, Question, QuestionDetail, ReadDetail,
    ResultStatus, Retry, ShellStatus, Task, TaskCreateDetail, TaskUpdateDetail, TextRef, ThreadId,
    ThreadState, ToolCall, ToolDetail, ToolResult, Turn, Usage, WebFetchDetail, WebSearchDetail,
    WriteDetail, WriteKind,
};

use crate::transcript::Tail;

pub mod media;
pub mod output;
mod session;
pub use session::{Transcripts, subagents_dir};

/// Prose: prompts, answers, thinking, plans, subagent reports, compaction summaries. Long enough
/// for any answer a person reads through, short of a pasted log.
pub const PROSE: Cap = Cap { lines: 400, chars: 32_000 };

/// Tool output and inputs: a Bash tail, a result's text, an unknown tool's input. The same
/// 40 lines or 4 000 characters the old conversation view settled on (`claude-code.md`).
pub const OUTPUT: Cap = Cap { lines: 40, chars: 4_000 };

/// Diff lines kept per call, across its hunks.
pub const PATCH_LINES: usize = 400;

/// Links a web search keeps: the first page of results.
pub const LINKS: usize = 10;

/// Results and notices waiting for a call not seen yet; past this many, they are dropped (a tail
/// that started after the calls would otherwise hold them forever).
const PENDING_MAX: usize = 512;

/// The thread a transcript file feeds: `…/subagents/agent-<id>.jsonl` is that agent's, any
/// other file the main conversation.
#[must_use]
pub fn thread_of(path: &Path) -> ThreadId {
    let in_subagents =
        path.parent().and_then(Path::file_name).is_some_and(|dir| dir == "subagents");
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    match stem.strip_prefix("agent-") {
        Some(id) if in_subagents && !id.is_empty() => ThreadId::Agent(id.to_owned()),
        _ => ThreadId::Main,
    }
}

#[derive(Debug, Default)]
struct Thread {
    entries: Vec<Entry>,
    /// The chain position of the record each entry came from, beside `entries`.
    made_at: Vec<usize>,
    /// Entry id → position in `entries`.
    index: HashMap<String, usize>,
    /// Record uuids along the current branch.
    chain: Vec<String>,
    /// Record uuid → position in `chain`.
    at: HashMap<String, usize>,
    tasks: Vec<Task>,
    origin: Option<Origin>,
    /// Its turns, oldest first; the last is the one records now add to.
    turns: Vec<Turn>,
    /// The request the assistant records now arriving belong to: its message id and the usage
    /// already counted for it. A user record ends it, since the next request answers that.
    request: Option<(String, Usage)>,
}

impl Thread {
    /// Put a record on the chain; `None` when it was seen already. Returns its position and the
    /// entries a branch abandoned.
    fn link(&mut self, uuid: &str, parent: Option<&str>) -> Option<(usize, Vec<Entry>)> {
        if self.at.contains_key(uuid) {
            return None;
        }
        let mut dropped = Vec::new();
        if let Some(&fork) = parent.and_then(|p| self.at.get(p))
            && fork.saturating_add(1) < self.chain.len()
        {
            for gone in self.chain.drain(fork.saturating_add(1)..) {
                self.at.remove(&gone);
            }
            while self.made_at.last().is_some_and(|&made| made > fork) {
                self.made_at.pop();
                if let Some(entry) = self.entries.pop() {
                    self.index.remove(&entry.id);
                    dropped.push(entry);
                }
            }
        }
        let position = self.chain.len();
        self.at.insert(uuid.to_owned(), position);
        self.chain.push(uuid.to_owned());
        Some((position, dropped))
    }

    fn push(&mut self, made_at: usize, entry: Entry) -> Option<&Entry> {
        if self.index.contains_key(&entry.id) {
            return None;
        }
        self.index.insert(entry.id.clone(), self.entries.len());
        self.made_at.push(made_at);
        self.entries.push(entry);
        self.entries.last()
    }

    fn get_mut(&mut self, id: &str) -> Option<&mut Entry> {
        let at = *self.index.get(id)?;
        self.entries.get_mut(at)
    }
}

/// Something that names a call not seen yet.
#[derive(Debug)]
enum Pending {
    Result { uuid: String, at_ms: WallMs, block: Value, result: Option<Value> },
    Notice(Notice),
}

/// A background task's notice (`<task-notification>`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Notice {
    task_id: Option<String>,
    tool_use_id: Option<String>,
    output_file: Option<String>,
    status: Option<String>,
    summary: Option<String>,
    result: Option<String>,
    /// A subagent's figures, from its `<usage>`: tokens, tool uses, how long it ran.
    usage: (Option<u64>, Option<u64>, Option<u64>),
    /// When the record carrying it was written.
    at_ms: WallMs,
}

/// The changes one read makes, an entry updated twice kept once at its first place.
#[derive(Default)]
struct Batch {
    changes: Vec<Change>,
    upserts: HashMap<(ThreadId, String), usize>,
    tasks: HashMap<ThreadId, usize>,
    turns: HashMap<(ThreadId, String), usize>,
}

impl Batch {
    fn upsert(&mut self, thread: &ThreadId, entry: Entry) {
        let key = (thread.clone(), entry.id.clone());
        if let Some(change) = self.upserts.get(&key).and_then(|&at| self.changes.get_mut(at)) {
            *change = Change::Upsert { thread: thread.clone(), entry };
            return;
        }
        self.upserts.insert(key, self.changes.len());
        self.changes.push(Change::Upsert { thread: thread.clone(), entry });
    }

    fn remove(&mut self, thread: &ThreadId, id: String) {
        self.upserts.remove(&(thread.clone(), id.clone()));
        self.changes.push(Change::Remove { thread: thread.clone(), id });
    }

    fn tasks(&mut self, thread: &ThreadId, tasks: Vec<Task>) {
        if let Some(change) = self.tasks.get(thread).and_then(|&at| self.changes.get_mut(at)) {
            *change = Change::Tasks { thread: thread.clone(), tasks };
            return;
        }
        self.tasks.insert(thread.clone(), self.changes.len());
        self.changes.push(Change::Tasks { thread: thread.clone(), tasks });
    }

    fn turn(&mut self, thread: &ThreadId, turn: Turn) {
        let key = (thread.clone(), turn.prompt.clone());
        if let Some(change) = self.turns.get(&key).and_then(|&at| self.changes.get_mut(at)) {
            *change = Change::Turn { thread: thread.clone(), turn };
            return;
        }
        self.turns.insert(key, self.changes.len());
        self.changes.push(Change::Turn { thread: thread.clone(), turn });
    }
}

/// A session's conversation: the main thread and every subagent's, built from transcript
/// records as they are appended.
#[derive(Debug, Default)]
pub struct Conversation {
    threads: BTreeMap<ThreadId, Thread>,
    /// Which thread holds each tool call.
    calls: HashMap<String, ThreadId>,
    /// What arrived for calls not seen yet, by `tool_use_id`.
    pending: HashMap<String, Vec<Pending>>,
    /// Calls that run in the background and write their output to a file, oldest first.
    background: Vec<(ThreadId, String)>,
}

impl Conversation {
    /// Decode what was appended to the transcript at `path` since `tail` last read it. The file
    /// feeds the thread [`thread_of`] names for `path` unless its records say otherwise. A file
    /// that shrank starts over: its thread (every thread, for the main file) is dropped and a
    /// [`Change::Reset`] says so.
    ///
    /// # Errors
    ///
    /// When the file exists but cannot be read.
    pub fn read(&mut self, tail: &mut Tail, path: &Path) -> std::io::Result<Vec<Change>> {
        let lines = tail.read_lines(path)?;
        let thread = thread_of(path);
        let mut batch = Batch::default();
        if lines.restarted {
            let reset = match thread {
                ThreadId::Main => {
                    *self = Self::default();
                    None
                }
                ThreadId::Agent(_) => {
                    self.threads.remove(&thread);
                    Some(thread.clone())
                }
            };
            batch.changes.push(Change::Reset { thread: reset });
        }
        self.decode(&thread, &lines.text, &mut batch);
        Ok(batch.changes)
    }

    /// Decode complete JSONL lines from a file feeding `thread`.
    #[cfg(test)]
    pub(crate) fn ingest_jsonl(&mut self, thread: &ThreadId, jsonl: &str) -> Vec<Change> {
        let mut batch = Batch::default();
        self.decode(thread, jsonl, &mut batch);
        batch.changes
    }

    /// Decode one parsed record from a file feeding `thread`.
    pub fn ingest(&mut self, thread: &ThreadId, record: &Value) -> Vec<Change> {
        let mut batch = Batch::default();
        self.record(thread, record, &mut batch);
        batch.changes
    }

    fn decode(&mut self, thread: &ThreadId, jsonl: &str, batch: &mut Batch) {
        for line in jsonl.lines().filter(|l| !l.trim().is_empty()) {
            // A line that is not JSON (a torn write, a future format) is skipped, not fatal.
            if let Ok(record) = serde_json::from_str::<Value>(line) {
                self.record(thread, &record, batch);
            }
        }
    }

    /// Every thread, the main one first.
    pub fn threads(&self) -> impl Iterator<Item = &ThreadId> {
        self.threads.keys()
    }

    /// A thread's entries, oldest first.
    #[must_use]
    pub fn entries(&self, thread: &ThreadId) -> &[Entry] {
        self.threads.get(thread).map_or(&[], |t| t.entries.as_slice())
    }

    /// A thread's task list.
    #[must_use]
    pub fn tasks(&self, thread: &ThreadId) -> &[Task] {
        self.threads.get(thread).map_or(&[], |t| t.tasks.as_slice())
    }

    /// A thread's turns, oldest first.
    #[must_use]
    pub fn turns(&self, thread: &ThreadId) -> &[Turn] {
        self.threads.get(thread).map_or(&[], |t| t.turns.as_slice())
    }

    /// The Bash calls running (or once run) in the background with a file for their output,
    /// oldest first.
    pub fn background(&self) -> impl Iterator<Item = (&ThreadId, &Entry)> {
        self.background.iter().filter_map(|(thread, id)| {
            let t = self.threads.get(thread)?;
            Some((thread, t.entries.get(*t.index.get(id)?)?))
        })
    }

    /// Everything as it stands: what a client that starts following is sent first.
    #[must_use]
    pub fn snapshot(&self) -> Vec<ThreadState> {
        self.threads
            .iter()
            .map(|(id, t)| ThreadState {
                id: id.clone(),
                origin: t.origin.clone(),
                entries: t.entries.clone(),
                tasks: t.tasks.clone(),
                turns: t.turns.clone(),
            })
            .collect()
    }

    fn record(&mut self, source: &ThreadId, record: &Value, batch: &mut Batch) {
        let kind = str_at(record, "type").unwrap_or_default();
        if kind == "queue-operation" {
            // Written the moment background work ends, before the model takes the notice in.
            if str_at(record, "operation") == Some("enqueue")
                && let Some(mut notice) = str_at(record, "content").and_then(parse_notice)
            {
                notice.at_ms = str_at(record, "timestamp").and_then(parse_ms).unwrap_or_default();
                self.notice(notice, batch);
            }
            return;
        }
        if kind == "agent_metadata" {
            self.threads.entry(source.clone()).or_default().origin = Some(Origin {
                tool_use_id: string_at(record, "toolUseId"),
                agent_type: string_at(record, "agentType"),
                description: string_at(record, "description"),
            });
            return;
        }
        let Some(uuid) = str_at(record, "uuid") else { return };
        let thread = match (bool_at(record, "isSidechain"), str_at(record, "agentId")) {
            (true, Some(agent)) if !agent.is_empty() => ThreadId::Agent(agent.to_owned()),
            (true, _) if *source == ThreadId::Main => ThreadId::Agent(String::new()),
            _ => source.clone(),
        };
        // Only a person's prompt starts a branch. Other records may name an older parent
        // without abandoning anything: the results of parallel calls each name the record of
        // their own call.
        let parent = str_at(record, "parentUuid").filter(|_| is_prompt(record));
        let Some((made_at, dropped)) =
            self.threads.entry(thread.clone()).or_default().link(uuid, parent)
        else {
            return;
        };
        let at_ms = str_at(record, "timestamp").and_then(parse_ms).unwrap_or_default();
        let context = Ctx { thread: &thread, uuid, at_ms, made_at };
        if !dropped.is_empty() {
            if let Some(t) = self.threads.get_mut(&thread) {
                t.turns.retain(|turn| !dropped.iter().any(|e| e.id == turn.prompt));
            }
            let count = to_u32(dropped.len());
            for entry in dropped {
                self.calls.remove(&entry.id);
                batch.remove(&thread, entry.id);
            }
            // The rewind marker sits where the old branch left off, before the prompt that
            // starts the new one.
            let marker = Body::Rewound { dropped: count };
            self.add(&context, format!("{uuid}:rewound"), marker, batch);
        }
        match kind {
            "user" => self.user(&context, record, batch),
            "assistant" => self.assistant(&context, record, batch),
            "system" => self.system(&context, record, batch),
            "attachment" => {
                let attachment = record.get("attachment");
                let queued =
                    attachment.and_then(|a| str_at(a, "commandMode")) == Some("task-notification");
                if queued
                    && let Some(mut notice) =
                        attachment.and_then(|a| str_at(a, "prompt")).and_then(parse_notice)
                {
                    notice.at_ms = at_ms;
                    self.notice(notice, batch);
                }
            }
            _ => {}
        }
    }

    fn add(&mut self, ctx: &Ctx<'_>, id: String, body: Body, batch: &mut Batch) {
        let entry = Entry { id, at_ms: ctx.at_ms, body };
        if let Some(entry) =
            self.threads.entry(ctx.thread.clone()).or_default().push(ctx.made_at, entry)
        {
            batch.upsert(ctx.thread, entry.clone());
        }
    }

    fn user(&mut self, ctx: &Ctx<'_>, record: &Value, batch: &mut Batch) {
        if let Some(t) = self.threads.get_mut(ctx.thread) {
            t.request = None;
        }
        if bool_at(record, "isMeta") {
            return;
        }
        let Some(content) = record.get("message").and_then(|m| m.get("content")) else { return };
        if bool_at(record, "isCompactSummary") {
            let text = content_text(content);
            self.summary(ctx, &text, batch);
            return;
        }
        let blocks: Vec<&Value> = match content {
            Value::String(_) => vec![content],
            Value::Array(blocks) => blocks.iter().collect(),
            _ => return,
        };
        let mut images = media::in_prompt(ctx.uuid, &blocks);
        let mut prompt_made = false;
        for (index, block) in blocks.iter().enumerate() {
            let text = match block {
                Value::String(s) => Some(s.as_str()),
                _ if str_at(block, "type") == Some("text") => str_at(block, "text"),
                _ if str_at(block, "type") == Some("tool_result") => {
                    self.result(ctx, block, record.get("toolUseResult"), batch);
                    None
                }
                _ => None,
            };
            let Some(text) = text else { continue };
            let reference = || TextRef {
                record: ctx.uuid.to_owned(),
                part: Part::Block { index: to_u32(index) },
            };
            let body = if let Some(marker) =
                text.trim().strip_prefix("[Request interrupted by user")
            {
                Body::Interrupted { during_tool: marker.contains("for tool use") }
            } else if let Some(mut notice) = parse_notice(text) {
                notice.at_ms = ctx.at_ms;
                self.notice(notice, batch);
                continue;
            } else if let Some(name) = tag(text, "command-name") {
                Body::Prompt(Prompt {
                    text: Clipped::head(
                        tag(text, "command-args").unwrap_or_default().trim(),
                        PROSE,
                        Some(reference()),
                    ),
                    images: Vec::new(),
                    command: Some(name.trim().to_owned()),
                })
            } else if let Some(input) = tag(text, "bash-input") {
                Body::Prompt(Prompt {
                    text: Clipped::head(input.trim(), PROSE, Some(reference())),
                    images: Vec::new(),
                    command: Some("!".to_owned()),
                })
            } else if let Some(output) =
                ["local-command-stdout", "local-command-stderr", "bash-stdout", "bash-stderr"]
                    .iter()
                    .find_map(|name| tag(text, name))
            {
                let output = strip_ansi(output.trim());
                if output.is_empty() {
                    continue;
                }
                Body::Note(Note {
                    kind: NoteKind::Command,
                    text: Clipped::head(&output, OUTPUT, Some(reference())),
                    retry: None,
                })
            } else if text.trim_start().starts_with('<') || text.trim().is_empty() || prompt_made {
                // Text Claude Code injects for the model (reminders, caveats) is not the person's.
                continue;
            } else {
                prompt_made = true;
                Body::Prompt(Prompt {
                    text: Clipped::head(text, PROSE, Some(reference())),
                    images: std::mem::take(&mut images),
                    command: None,
                })
            };
            let id = if index == 0 { ctx.uuid.to_owned() } else { format!("{}:{index}", ctx.uuid) };
            let opens_turn = matches!(body, Body::Prompt(_));
            self.add(ctx, id.clone(), body, batch);
            if opens_turn {
                self.start_turn(ctx, &id, string_at(record, "permissionMode"), batch);
            }
        }
        // Pictures pasted with no words are a prompt still.
        if !prompt_made && !images.is_empty() && is_prompt(record) {
            let body = Body::Prompt(Prompt {
                text: Clipped::head("", PROSE, None),
                images,
                command: None,
            });
            self.add(ctx, ctx.uuid.to_owned(), body, batch);
            self.start_turn(ctx, ctx.uuid, string_at(record, "permissionMode"), batch);
        }
    }

    /// A prompt opens a turn: what the thread's records say from here on is its.
    fn start_turn(&mut self, ctx: &Ctx<'_>, prompt: &str, mode: Option<String>, batch: &mut Batch) {
        let thread = self.threads.entry(ctx.thread.clone()).or_default();
        let turn =
            Turn { prompt: prompt.to_owned(), started_ms: ctx.at_ms, mode, ..Turn::default() };
        thread.turns.push(turn.clone());
        batch.turn(ctx.thread, turn);
    }

    /// An assistant record's model and usage, counted in its turn. The records of one request
    /// (one per content block) repeat its usage, so a request is counted once, at the latest
    /// figures its records give.
    fn usage(&mut self, ctx: &Ctx<'_>, message: &Value, batch: &mut Batch) {
        let model = str_at(message, "model").filter(|m| !m.is_empty() && !m.starts_with('<'));
        let Some(usage) = message.get("usage").filter(|u| u.is_object()) else { return };
        let n = |key: &str| u64_at(usage, key).unwrap_or(0);
        let now = Usage {
            input: n("input_tokens"),
            cache_read: n("cache_read_input_tokens"),
            cache_write: n("cache_creation_input_tokens"),
            output: n("output_tokens"),
            thinking: usage
                .get("output_tokens_details")
                .and_then(|d| u64_at(d, "thinking_tokens"))
                .unwrap_or(0),
        };
        let id = string_at(message, "id").unwrap_or_default();
        let thread = self.threads.entry(ctx.thread.clone()).or_default();
        let counted = match thread.request.take() {
            Some((same, before)) if same == id => Some(before),
            _ => None,
        };
        thread.request = Some((id, now));
        // Work before any prompt (a thread picked up mid-way) has a turn of its own.
        if thread.turns.is_empty() {
            thread.turns.push(Turn { started_ms: ctx.at_ms, ..Turn::default() });
        }
        let Some(turn) = thread.turns.last_mut() else { return };
        if let Some(before) = counted {
            turn.usage = turn.usage.minus(before).plus(now);
        } else {
            turn.requests = turn.requests.saturating_add(1);
            turn.usage = turn.usage.plus(now);
        }
        turn.context_tokens = Some(now.context());
        if let Some(model) = model
            && !turn.models.iter().any(|m| m == model)
        {
            turn.models.push(model.to_owned());
        }
        if let Some(stop) = str_at(message, "stop_reason") {
            turn.stop = Some(stop.to_owned());
        }
        let turn = turn.clone();
        batch.turn(ctx.thread, turn);
    }

    /// Claude Code closed the thread's turn at this record.
    fn end_turn(&mut self, ctx: &Ctx<'_>, batch: &mut Batch) {
        let Some(turn) = self.threads.get_mut(ctx.thread).and_then(|t| t.turns.last_mut()) else {
            return;
        };
        turn.ended_ms = Some(ctx.at_ms);
        let turn = turn.clone();
        batch.turn(ctx.thread, turn);
    }

    fn assistant(&mut self, ctx: &Ctx<'_>, record: &Value, batch: &mut Batch) {
        let Some(message) = record.get("message") else { return };
        let Some(content) = message.get("content") else { return };
        if bool_at(record, "isApiErrorMessage") {
            let text = content_text(content);
            let body = Body::Note(Note {
                kind: NoteKind::ApiError,
                text: Clipped::head(text.trim(), OUTPUT, None),
                retry: None,
            });
            self.add(ctx, ctx.uuid.to_owned(), body, batch);
            return;
        }
        self.usage(ctx, message, batch);
        let blocks: Vec<&Value> = match content {
            Value::String(_) => vec![content],
            Value::Array(blocks) => blocks.iter().collect(),
            _ => return,
        };
        for (index, block) in blocks.into_iter().enumerate() {
            let reference = || TextRef {
                record: ctx.uuid.to_owned(),
                part: Part::Block { index: to_u32(index) },
            };
            let id = || format!("{}:{index}", ctx.uuid);
            match block {
                Value::String(text) if !text.trim().is_empty() => {
                    self.add(
                        ctx,
                        id(),
                        Body::Text(Clipped::head(text, PROSE, Some(reference()))),
                        batch,
                    );
                }
                _ => match str_at(block, "type") {
                    Some("text") => {
                        let text = str_at(block, "text").unwrap_or_default();
                        if !text.trim().is_empty() {
                            self.add(
                                ctx,
                                id(),
                                Body::Text(Clipped::head(text, PROSE, Some(reference()))),
                                batch,
                            );
                        }
                    }
                    Some("thinking") => {
                        let text = str_at(block, "thinking").unwrap_or_default();
                        if !text.trim().is_empty() {
                            self.add(
                                ctx,
                                id(),
                                Body::Thinking(Clipped::head(text, PROSE, Some(reference()))),
                                batch,
                            );
                        }
                    }
                    Some("tool_use" | "server_tool_use") => self.call(ctx, block, batch),
                    _ => {}
                },
            }
        }
    }

    fn system(&mut self, ctx: &Ctx<'_>, record: &Value, batch: &mut Batch) {
        let content = str_at(record, "content").unwrap_or_default();
        let body = match str_at(record, "subtype") {
            Some("compact_boundary") => {
                let meta = record.get("compactMetadata");
                Body::Compact(Compact {
                    trigger: meta.and_then(|m| string_at(m, "trigger")),
                    pre_tokens: meta.and_then(|m| u64_at(m, "preTokens")),
                    post_tokens: meta.and_then(|m| u64_at(m, "postTokens")),
                    summary: None,
                })
            }
            Some("api_error") => {
                let text = if content.is_empty() {
                    record
                        .get("error")
                        .and_then(|e| {
                            str_at(e, "message")
                                .or_else(|| {
                                    e.get("error").and_then(|inner| str_at(inner, "message"))
                                })
                                .or_else(|| e.as_str())
                        })
                        .unwrap_or("API error")
                        .to_owned()
                } else {
                    content.to_owned()
                };
                let attempt = u64_at(record, "retryAttempt");
                let retry = attempt.map(|attempt| Retry {
                    attempt: u32::try_from(attempt).unwrap_or(u32::MAX),
                    max: u64_at(record, "maxRetries")
                        .map_or(0, |m| u32::try_from(m).unwrap_or(u32::MAX)),
                    in_ms: record.get("retryInMs").and_then(Value::as_f64).map_or(0, ms_of),
                });
                Body::Note(Note {
                    kind: NoteKind::ApiError,
                    text: Clipped::head(&text, OUTPUT, None),
                    retry,
                })
            }
            Some("stop_hook_summary" | "turn_duration") => {
                self.end_turn(ctx, batch);
                let errors: Vec<String> = record
                    .get("hookErrors")
                    .and_then(Value::as_array)
                    .map(|errors| {
                        errors
                            .iter()
                            .filter_map(|e| {
                                e.as_str().map(str::to_owned).or_else(|| string_at(e, "message"))
                            })
                            .filter(|e| !e.trim().is_empty())
                            .collect()
                    })
                    .unwrap_or_default();
                let stopped = bool_at(record, "preventedContinuation")
                    .then(|| string_at(record, "stopReason"))
                    .flatten()
                    .filter(|r| !r.trim().is_empty());
                let said: Vec<String> = errors.into_iter().chain(stopped).collect();
                if said.is_empty() {
                    return;
                }
                Body::Note(Note {
                    kind: NoteKind::Hook,
                    text: Clipped::head(&strip_ansi(&said.join("\n")), OUTPUT, None),
                    retry: None,
                })
            }
            Some("local_command") => {
                let inner = ["local-command-stdout", "local-command-stderr"]
                    .iter()
                    .find_map(|name| tag(content, name))
                    .unwrap_or(content);
                let text = strip_ansi(inner.trim());
                if text.is_empty() {
                    return;
                }
                Body::Note(Note {
                    kind: NoteKind::Command,
                    text: Clipped::head(&text, OUTPUT, None),
                    retry: None,
                })
            }
            Some("informational") if !content.trim().is_empty() => Body::Note(Note {
                kind: NoteKind::Info,
                text: Clipped::head(content.trim(), OUTPUT, None),
                retry: None,
            }),
            _ => return,
        };
        self.add(ctx, ctx.uuid.to_owned(), body, batch);
    }

    /// A compaction summary belongs to the newest boundary.
    fn summary(&mut self, ctx: &Ctx<'_>, text: &str, batch: &mut Batch) {
        let summary = Clipped::head(
            text,
            PROSE,
            Some(TextRef { record: ctx.uuid.to_owned(), part: Part::Block { index: 0 } }),
        );
        let thread = self.threads.entry(ctx.thread.clone()).or_default();
        let boundary = thread.entries.iter_mut().rev().find_map(|e| match &mut e.body {
            Body::Compact(compact) if compact.summary.is_none() => Some((e.id.clone(), compact)),
            _ => None,
        });
        if let Some((id, compact)) = boundary {
            compact.summary = Some(summary);
            if let Some(entry) = thread.get_mut(&id) {
                batch.upsert(ctx.thread, entry.clone());
            }
            return;
        }
        let body = Body::Compact(Compact {
            trigger: None,
            pre_tokens: None,
            post_tokens: None,
            summary: Some(summary),
        });
        self.add(ctx, ctx.uuid.to_owned(), body, batch);
    }

    fn call(&mut self, ctx: &Ctx<'_>, block: &Value, batch: &mut Batch) {
        let Some(id) = str_at(block, "id") else { return };
        let name = str_at(block, "name").unwrap_or_default();
        let empty = Value::Null;
        let input = block.get("input").unwrap_or(&empty);
        let detail = detail(name, input, Some((id, ctx.uuid)));
        self.calls.insert(id.to_owned(), ctx.thread.clone());
        let call = ToolCall { name: name.to_owned(), detail, result: None };
        self.add(ctx, id.to_owned(), Body::Tool(Box::new(call)), batch);
        for waiting in self.pending.remove(id).unwrap_or_default() {
            match waiting {
                Pending::Result { uuid, at_ms, block, result } => {
                    let later = Ctx { uuid: &uuid, at_ms, ..*ctx };
                    self.result(&later, &block, result.as_ref(), batch);
                }
                Pending::Notice(notice) => self.notice(notice, batch),
            }
        }
    }

    fn park(&mut self, id: &str, pending: Pending) {
        if self.pending.values().map(Vec::len).sum::<usize>() >= PENDING_MAX {
            self.pending.clear();
        }
        self.pending.entry(id.to_owned()).or_default().push(pending);
    }

    fn result(&mut self, ctx: &Ctx<'_>, block: &Value, result: Option<&Value>, batch: &mut Batch) {
        let Some(id) = str_at(block, "tool_use_id") else { return };
        let Some(thread) = self.calls.get(id).cloned() else {
            let pending = Pending::Result {
                uuid: ctx.uuid.to_owned(),
                at_ms: ctx.at_ms,
                block: block.clone(),
                result: result.cloned(),
            };
            self.park(id, pending);
            return;
        };
        let Some(entry) = self.threads.get_mut(&thread).and_then(|t| t.get_mut(id)) else { return };
        let Body::Tool(call) = &mut entry.body else { return };
        let text = content_text(block.get("content").unwrap_or(&Value::Null));
        let rejected = result.and_then(Value::as_str) == Some("User rejected tool use")
            || text.starts_with("The user doesn't want to proceed with this tool use");
        let status = if rejected {
            ResultStatus::Rejected
        } else if bool_at(block, "is_error") {
            ResultStatus::Error
        } else {
            ResultStatus::Ok
        };
        let structured = result.filter(|r| r.is_object());
        let images = media::in_result(ctx.uuid, id, block, structured);
        let shown = apply_result(&mut call.detail, status, &text, structured, ctx.uuid, id);
        let text = (!shown || status != ResultStatus::Ok).then(|| {
            Clipped::head(
                text.trim(),
                OUTPUT,
                Some(TextRef {
                    record: ctx.uuid.to_owned(),
                    part: Part::Result { tool_use_id: id.to_owned() },
                }),
            )
        });
        let text = text.filter(|t| !t.text.is_empty() || images.is_empty());
        call.result = Some(ToolResult { status, text, at_ms: ctx.at_ms, images });
        let changed_tasks = task_change(&call.detail, call.result.as_ref());
        let background = matches!(&call.detail, ToolDetail::Bash(b) if b.output_file.is_some());
        let entry = entry.clone();
        batch.upsert(&thread, entry);
        if background {
            self.note_background(&thread, id);
        }
        if let Some(change) = changed_tasks
            && let Some(t) = self.threads.get_mut(&thread)
        {
            change.apply(&mut t.tasks);
            batch.tasks(&thread, t.tasks.clone());
        }
    }

    fn notice(&mut self, notice: Notice, batch: &mut Batch) {
        let Some(id) = notice.tool_use_id.clone() else { return };
        let Some(thread) = self.calls.get(&id).cloned() else {
            self.park(&id, Pending::Notice(notice));
            return;
        };
        let Some(entry) = self.threads.get_mut(&thread).and_then(|t| t.get_mut(&id)) else {
            return;
        };
        let before = entry.clone();
        let started = entry.at_ms;
        let at = (!notice.at_ms.is_zero()).then_some(notice.at_ms);
        let Body::Tool(call) = &mut entry.body else { return };
        match &mut call.detail {
            ToolDetail::Bash(bash) => {
                bash.task_id = bash.task_id.take().or(notice.task_id);
                bash.output_file = bash.output_file.take().or(notice.output_file);
                let exit = notice.summary.as_deref().and_then(exit_code_in_summary);
                bash.exit_code = exit.or(bash.exit_code);
                bash.status = match notice.status.as_deref() {
                    Some("completed") if exit.unwrap_or(0) == 0 => ShellStatus::Done,
                    Some("completed" | "failed") => ShellStatus::Failed,
                    Some("killed" | "stopped") => ShellStatus::Killed,
                    _ => bash.status,
                };
                if bash.status != ShellStatus::Running {
                    bash.finished_ms = bash.finished_ms.or(at);
                }
            }
            ToolDetail::Agent(agent) => {
                agent.status = match notice.status.as_deref() {
                    Some("completed") => AgentRun::Completed,
                    Some("failed") => AgentRun::Failed,
                    Some("killed" | "stopped") => AgentRun::Killed,
                    _ => agent.status,
                };
                if let Some(report) = notice.result.as_deref() {
                    agent.report = Some(Clipped::head(report.trim(), PROSE, None));
                }
                let (tokens, tool_uses, duration) = notice.usage;
                agent.tokens = agent.tokens.or(tokens);
                agent.tool_uses = agent.tool_uses.or(tool_uses);
                let ran = at.filter(|_| agent.status != AgentRun::Running && !started.is_zero());
                agent.duration_ms = agent
                    .duration_ms
                    .or(duration)
                    .or_else(|| ran?.as_millis().checked_sub(started.as_millis()));
            }
            _ => return,
        }
        if *entry == before {
            return;
        }
        let entry = entry.clone();
        batch.upsert(&thread, entry);
        self.note_background(&thread, &id);
    }

    /// Remember a background command with an output file, for [`Self::background`].
    fn note_background(&mut self, thread: &ThreadId, id: &str) {
        let Some(Entry { body: Body::Tool(call), .. }) =
            self.threads.get(thread).and_then(|t| t.entries.get(*t.index.get(id)?))
        else {
            return;
        };
        let ToolDetail::Bash(bash) = &call.detail else { return };
        if bash.output_file.is_none() || !bash.background {
            return;
        }
        if !self.background.iter().any(|(t, known)| t == thread && known == id) {
            self.background.push((thread.clone(), id.to_owned()));
        }
    }
}

/// Where a record is being decoded.
#[derive(Clone, Copy)]
struct Ctx<'a> {
    thread: &'a ThreadId,
    uuid: &'a str,
    at_ms: WallMs,
    made_at: usize,
}

/// What a call Claude Code asks permission for would do.
///
/// Its detail is what the transcript will show, with an edit's or a write's change as a patch,
/// since no result has made one yet. The input is not in a transcript, so a clipped text has
/// no [`TextRef`].
#[must_use]
pub fn proposed(name: &str, input: &Value) -> ToolDetail {
    let mut detail = detail(name, input, None);
    let replacements = |input: &Value| -> Vec<(String, String)> {
        let pair = |edit: &Value| {
            (
                string_at(edit, "old_string").unwrap_or_default(),
                string_at(edit, "new_string").unwrap_or_default(),
            )
        };
        match input.get("edits").and_then(Value::as_array) {
            Some(edits) => edits.iter().map(pair).collect(),
            None => vec![pair(input)],
        }
    };
    match &mut detail {
        ToolDetail::Edit(edit) => edit.patch = proposed_patch(&replacements(input)),
        ToolDetail::Write(write) => {
            let content = string_at(input, "content").unwrap_or_default();
            write.patch = proposed_patch(&[(String::new(), content)]);
        }
        _ => {}
    }
    detail
}

/// A patch of replacements without line numbers (the file is not read): each is one hunk, the
/// lines it keeps at either end as context around the lines it removes and adds. Text that
/// replaces nothing is a whole file, numbered from its first line, as git numbers a new one.
pub(crate) fn proposed_patch(replacements: &[(String, String)]) -> Patch {
    let mut patch = Patch::default();
    let mut room = PATCH_LINES;
    for (old, new) in replacements {
        let old: Vec<&str> = old.lines().collect();
        let new: Vec<&str> = new.lines().collect();
        let same = |(a, b): (&&str, &&str)| a == b;
        let head = old.iter().zip(&new).take_while(|pair| same(*pair)).count();
        let rest = old.len().min(new.len()).saturating_sub(head);
        let tail =
            old.iter().rev().zip(new.iter().rev()).take(rest).take_while(|p| same(*p)).count();
        let removed = old.get(head..old.len().saturating_sub(tail)).unwrap_or_default();
        let added = new.get(head..new.len().saturating_sub(tail)).unwrap_or_default();
        patch.removed = patch.removed.saturating_add(to_u32(removed.len()));
        patch.added = patch.added.saturating_add(to_u32(added.len()));
        let lines: Vec<String> = old
            .get(..head)
            .unwrap_or_default()
            .iter()
            .map(|l| format!(" {l}"))
            .chain(removed.iter().map(|l| format!("-{l}")))
            .chain(added.iter().map(|l| format!("+{l}")))
            .chain(
                old.get(old.len().saturating_sub(tail)..)
                    .unwrap_or_default()
                    .iter()
                    .map(|l| format!(" {l}")),
            )
            .collect();
        let kept = lines.len().min(room);
        room = room.saturating_sub(kept);
        patch.clipped_lines =
            patch.clipped_lines.saturating_add(to_u32(lines.len().saturating_sub(kept)));
        if kept > 0 {
            let whole = old.is_empty() && !new.is_empty();
            patch.hunks.push(Hunk {
                old_start: 0,
                old_lines: to_u32(old.len()),
                new_start: u32::from(whole),
                new_lines: to_u32(new.len()),
                // Only the text replaced is known, not where in the file it is.
                heading: None,
                lines: lines.into_iter().take(kept).collect(),
            });
        }
    }
    patch
}

/// A call's detail from its input alone; the result fills in the rest. `at` is the call's id
/// and its record's uuid, where a clipped input is found again; `None` for a call not written
/// yet.
fn detail(name: &str, input: &Value, at: Option<(&str, &str)>) -> ToolDetail {
    let text = |key: &str| string_at(input, key);
    let path = || text("file_path").or_else(|| text("notebook_path")).unwrap_or_default();
    let input_ref = |field: &str| {
        at.map(|(id, uuid)| TextRef {
            record: uuid.to_owned(),
            part: Part::Input { tool_use_id: id.to_owned(), field: field.to_owned() },
        })
    };
    match name {
        "Edit" | "MultiEdit" => ToolDetail::Edit(EditDetail {
            path: path(),
            edits: input.get("edits").and_then(Value::as_array).map_or(1, |e| to_u32(e.len())),
            replace_all: bool_at(input, "replace_all"),
            patch: Patch::default(),
        }),
        "Write" => ToolDetail::Write(WriteDetail {
            path: path(),
            lines: to_u32(str_at(input, "content").map_or(0, |c| c.lines().count())),
            kind: WriteKind::Unknown,
            patch: Patch::default(),
        }),
        "Read" => ToolDetail::Read(ReadDetail {
            path: path(),
            offset: u64_at(input, "offset"),
            limit: u64_at(input, "limit"),
            start_line: None,
            lines: None,
            total_lines: None,
        }),
        "Grep" => ToolDetail::Grep(GrepDetail {
            pattern: text("pattern").unwrap_or_default(),
            path: text("path"),
            glob: text("glob"),
            mode: text("output_mode"),
            files: None,
            lines: None,
        }),
        "Glob" => ToolDetail::Glob(GlobDetail {
            pattern: text("pattern").unwrap_or_default(),
            path: text("path"),
            files: None,
            truncated: false,
        }),
        "Bash" => ToolDetail::Bash(BashDetail {
            command: Clipped::head(
                str_at(input, "command").unwrap_or_default(),
                OUTPUT,
                input_ref("command"),
            ),
            description: text("description"),
            background: bool_at(input, "run_in_background"),
            task_id: None,
            status: ShellStatus::Running,
            exit_code: None,
            stdout: None,
            stderr: None,
            output_file: None,
            finished_ms: None,
        }),
        "WebFetch" => ToolDetail::WebFetch(WebFetchDetail {
            url: text("url").unwrap_or_default(),
            prompt: text("prompt"),
            code: None,
            bytes: None,
        }),
        "WebSearch" => ToolDetail::WebSearch(WebSearchDetail {
            query: text("query").unwrap_or_default(),
            results: None,
            links: Vec::new(),
        }),
        "Agent" | "Task" => ToolDetail::Agent(AgentDetail {
            agent_id: None,
            agent_type: text("subagent_type"),
            description: text("description"),
            prompt: Clipped::head(
                str_at(input, "prompt").unwrap_or_default(),
                PROSE,
                input_ref("prompt"),
            ),
            background: bool_at(input, "run_in_background"),
            status: AgentRun::Running,
            report: None,
            tokens: None,
            tool_uses: None,
            duration_ms: None,
        }),
        "TaskCreate" => ToolDetail::TaskCreate(TaskCreateDetail {
            task_id: None,
            subject: text("subject").unwrap_or_default(),
            description: text("description"),
        }),
        "TaskUpdate" => ToolDetail::TaskUpdate(TaskUpdateDetail {
            task_id: text("taskId").unwrap_or_default(),
            from: None,
            to: text("status"),
            subject: text("subject"),
            fields: Vec::new(),
        }),
        "TodoWrite" => ToolDetail::TodoWrite {
            todos: input
                .get("todos")
                .and_then(Value::as_array)
                .map(|todos| {
                    todos
                        .iter()
                        .enumerate()
                        .map(|(i, t)| Task {
                            id: i.saturating_add(1).to_string(),
                            subject: string_at(t, "content").unwrap_or_default(),
                            status: string_at(t, "status").unwrap_or_default(),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        },
        "AskUserQuestion" => ToolDetail::Question(QuestionDetail {
            questions: input
                .get("questions")
                .and_then(Value::as_array)
                .map(|qs| qs.iter().map(question).collect())
                .unwrap_or_default(),
            answers: Vec::new(),
        }),
        "ExitPlanMode" => ToolDetail::Plan {
            plan: Clipped::head(
                str_at(input, "plan").unwrap_or_default(),
                PROSE,
                input_ref("plan"),
            ),
        },
        _ => {
            let json = Clipped::head(&input.to_string(), OUTPUT, None);
            match name.strip_prefix("mcp__").and_then(|rest| rest.split_once("__")) {
                Some((server, tool)) => ToolDetail::Mcp(McpDetail {
                    server: server.to_owned(),
                    tool: tool.to_owned(),
                    input: json,
                }),
                None => ToolDetail::Other { input: json },
            }
        }
    }
}

fn question(q: &Value) -> Question {
    Question {
        text: string_at(q, "question").unwrap_or_default(),
        header: string_at(q, "header"),
        options: q
            .get("options")
            .and_then(Value::as_array)
            .map(|os| {
                os.iter()
                    .filter_map(|o| {
                        let label =
                            string_at(o, "label").or_else(|| o.as_str().map(str::to_owned))?;
                        Some(Choice { label, description: string_at(o, "description") })
                    })
                    .collect()
            })
            .unwrap_or_default(),
        multi_select: bool_at(q, "multiSelect"),
    }
}

/// Fold a result into the call's detail. Returns whether the detail now shows the outcome, so
/// the result's own text would only repeat it.
fn apply_result(
    detail: &mut ToolDetail,
    status: ResultStatus,
    text: &str,
    result: Option<&Value>,
    uuid: &str,
    id: &str,
) -> bool {
    let field = |key: &str| result.and_then(|r| r.get(key));
    let patch = || result.map(|r| patch(r, uuid)).unwrap_or_default();
    match detail {
        ToolDetail::Edit(edit) => {
            edit.patch = patch();
            result.is_some()
        }
        ToolDetail::Write(write) => {
            write.kind = match field("type").and_then(Value::as_str) {
                Some("create") => WriteKind::Create,
                Some("update") => WriteKind::Overwrite,
                _ => write.kind,
            };
            write.patch = patch();
            result.is_some()
        }
        ToolDetail::Read(read) => {
            let file = field("file");
            read.start_line = file.and_then(|f| u64_at(f, "startLine"));
            read.lines = file.and_then(|f| u64_at(f, "numLines"));
            read.total_lines = file.and_then(|f| u64_at(f, "totalLines"));
            result.is_some()
        }
        ToolDetail::Grep(grep) => {
            let listed = field("filenames").and_then(Value::as_array).map(Vec::len);
            grep.files = result
                .and_then(|r| u64_at(r, "numFiles"))
                .filter(|n| *n > 0)
                .or_else(|| listed.map(to_u64));
            grep.lines = result.and_then(|r| u64_at(r, "numLines"));
            false
        }
        ToolDetail::Glob(glob) => {
            glob.files = result.and_then(|r| u64_at(r, "numFiles"));
            glob.truncated = result.is_some_and(|r| bool_at(r, "truncated"));
            false
        }
        ToolDetail::Bash(bash) => {
            let out = |key: &str, part: Part| {
                result.and_then(|r| str_at(r, key)).filter(|s| !s.is_empty()).map(|s| {
                    Clipped::tail(s, OUTPUT, Some(TextRef { record: uuid.to_owned(), part }))
                })
            };
            bash.stdout = out("stdout", Part::Stdout);
            bash.stderr = out("stderr", Part::Stderr);
            if let Some(task) = result.and_then(|r| string_at(r, "backgroundTaskId")) {
                bash.task_id = Some(task);
                bash.background = true;
                bash.output_file = bash.output_file.take().or_else(|| output_file_in(text));
            }
            let interrupted = result.is_some_and(|r| bool_at(r, "interrupted"));
            let exit = text.strip_prefix("Exit code ").and_then(|rest| {
                rest.split(|c: char| !c.is_ascii_digit() && c != '-').next()?.parse::<i32>().ok()
            });
            bash.status = match status {
                ResultStatus::Rejected => ShellStatus::Interrupted,
                _ if interrupted => ShellStatus::Interrupted,
                ResultStatus::Error => ShellStatus::Failed,
                ResultStatus::Ok if bash.task_id.is_some() => ShellStatus::Running,
                ResultStatus::Ok => ShellStatus::Done,
            };
            bash.exit_code = match bash.status {
                ShellStatus::Done => Some(0),
                _ => exit,
            };
            result.is_some()
        }
        ToolDetail::WebFetch(fetch) => {
            fetch.code = result.and_then(|r| u64_at(r, "code"));
            fetch.bytes = result.and_then(|r| u64_at(r, "bytes"));
            false
        }
        ToolDetail::WebSearch(search) => {
            let found: Vec<&Value> = field("results")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|r| r.get("content").and_then(Value::as_array))
                .flatten()
                .collect();
            search.results = field("results").is_some().then(|| to_u64(found.len()));
            search.links = found
                .iter()
                .filter_map(|link| {
                    let url = string_at(link, "url")?;
                    let title = string_at(link, "title").filter(|t| !t.trim().is_empty());
                    Some(Link { title: title.unwrap_or_else(|| url.clone()), url })
                })
                .take(LINKS)
                .collect();
            !search.links.is_empty()
        }
        ToolDetail::Agent(agent) => {
            let Some(r) = result else { return false };
            agent.agent_id = string_at(r, "agentId").or_else(|| agent.agent_id.take());
            agent.agent_type = string_at(r, "agentType").or_else(|| agent.agent_type.take());
            agent.tokens = u64_at(r, "totalTokens");
            agent.tool_uses = u64_at(r, "totalToolUseCount");
            agent.duration_ms = u64_at(r, "totalDurationMs");
            if bool_at(r, "isAsync") {
                agent.background = true;
                agent.status = AgentRun::Running;
                return true;
            }
            agent.status = match (status, str_at(r, "status")) {
                (ResultStatus::Ok, Some("completed") | None) => AgentRun::Completed,
                (ResultStatus::Rejected, _) | (_, Some("killed")) => AgentRun::Killed,
                _ => AgentRun::Failed,
            };
            let report = r.get("content").map(content_text).filter(|t| !t.trim().is_empty());
            agent.report = report.map(|t| {
                Clipped::head(
                    t.trim(),
                    PROSE,
                    Some(TextRef {
                        record: uuid.to_owned(),
                        part: Part::Result { tool_use_id: id.to_owned() },
                    }),
                )
            });
            true
        }
        ToolDetail::TaskCreate(create) => {
            create.task_id = field("task").and_then(|t| string_at(t, "id"));
            true
        }
        ToolDetail::TaskUpdate(update) => {
            let change = field("statusChange");
            update.from = change.and_then(|c| string_at(c, "from"));
            update.to = change.and_then(|c| string_at(c, "to")).or_else(|| update.to.take());
            update.fields = field("updatedFields")
                .and_then(Value::as_array)
                .map(|f| f.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect())
                .unwrap_or_default();
            true
        }
        ToolDetail::TodoWrite { .. } => true,
        ToolDetail::Question(question) => {
            question.answers = field("answers")
                .and_then(Value::as_object)
                .map(|answers| {
                    answers
                        .iter()
                        .map(|(q, a)| Answer {
                            question: q.clone(),
                            answer: a.as_str().map_or_else(|| a.to_string(), str::to_owned),
                        })
                        .collect()
                })
                .unwrap_or_default();
            !question.answers.is_empty()
        }
        ToolDetail::Plan { .. } | ToolDetail::Mcp(_) | ToolDetail::Other { .. } => false,
    }
}

/// The diff in a result's `structuredPatch`, each hunk headed as git heads it from the file as
/// it was (`originalFile`).
fn patch(result: &Value, uuid: &str) -> Patch {
    let mut out = Patch::default();
    let Some(hunks) = result.get("structuredPatch").and_then(Value::as_array) else { return out };
    let original = original_lines(result);
    let mut kept = 0_usize;
    for hunk in hunks {
        let lines: Vec<&str> = hunk
            .get("lines")
            .and_then(Value::as_array)
            .map(|l| l.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        for line in &lines {
            if line.starts_with('+') {
                out.added = out.added.saturating_add(1);
            } else if line.starts_with('-') {
                out.removed = out.removed.saturating_add(1);
            }
        }
        let room = PATCH_LINES.saturating_sub(kept);
        let shown: Vec<String> = lines.iter().take(room).map(|l| (*l).to_owned()).collect();
        kept = kept.saturating_add(shown.len());
        out.clipped_lines =
            out.clipped_lines.saturating_add(to_u32(lines.len().saturating_sub(shown.len())));
        if !shown.is_empty() {
            let at = |key: &str| u32::try_from(u64_at(hunk, key).unwrap_or(0)).unwrap_or(u32::MAX);
            out.hunks.push(Hunk {
                old_start: at("oldStart"),
                old_lines: at("oldLines"),
                new_start: at("newStart"),
                new_lines: at("newLines"),
                heading: heading_above(&original, hunk),
                lines: shown,
            });
        }
    }
    if out.clipped_lines > 0 {
        out.full = Some(TextRef { record: uuid.to_owned(), part: Part::Patch });
    }
    out
}

/// The file an edit result changed, as it was, by line (`originalFile`; empty when unsaid).
fn original_lines(result: &Value) -> Vec<&str> {
    result
        .get("originalFile")
        .and_then(Value::as_str)
        .map(|f| f.lines().collect())
        .unwrap_or_default()
}

/// A `structuredPatch` hunk's heading, from the lines of `original` above its `oldStart`.
fn heading_above(original: &[&str], hunk: &Value) -> Option<String> {
    let first = usize::try_from(u64_at(hunk, "oldStart").unwrap_or(0)).unwrap_or(usize::MAX);
    slopty_proto::thread::detail::heading(
        original.get(..first.saturating_sub(1)).unwrap_or_default().iter().copied(),
    )
}

/// How a result changes the task list.
enum TaskChange {
    Add(Task),
    Update { id: String, status: Option<String>, subject: Option<String> },
    Replace(Vec<Task>),
}

impl TaskChange {
    fn apply(self, tasks: &mut Vec<Task>) {
        match self {
            Self::Add(task) => {
                tasks.retain(|t| t.id != task.id);
                tasks.push(task);
            }
            Self::Update { id, status, subject } => {
                if status.as_deref() == Some("deleted") {
                    tasks.retain(|t| t.id != id);
                    return;
                }
                if let Some(task) = tasks.iter_mut().find(|t| t.id == id) {
                    if let Some(status) = status {
                        task.status = status;
                    }
                    if let Some(subject) = subject {
                        task.subject = subject;
                    }
                }
            }
            Self::Replace(list) => *tasks = list,
        }
    }
}

fn task_change(detail: &ToolDetail, result: Option<&ToolResult>) -> Option<TaskChange> {
    if result.is_none_or(|r| r.status != ResultStatus::Ok) {
        return None;
    }
    match detail {
        ToolDetail::TaskCreate(create) => Some(TaskChange::Add(Task {
            id: create.task_id.clone()?,
            subject: create.subject.clone(),
            status: "pending".to_owned(),
        })),
        ToolDetail::TaskUpdate(update) => Some(TaskChange::Update {
            id: update.task_id.clone(),
            status: update.to.clone(),
            subject: update.subject.clone(),
        }),
        ToolDetail::TodoWrite { todos } => Some(TaskChange::Replace(todos.clone())),
        _ => None,
    }
}

/// A `<task-notification>` in a queued command or a user record.
fn parse_notice(text: &str) -> Option<Notice> {
    let body = tag(text, "task-notification")?;
    let field = |name: &str| tag(body, name).map(|v| v.trim().to_owned());
    Some(Notice {
        task_id: field("task-id"),
        tool_use_id: field("tool-use-id"),
        output_file: field("output-file"),
        status: field("status"),
        summary: field("summary"),
        result: field("result"),
        usage: usage_in(body),
        at_ms: WallMs::ZERO,
    })
}

/// Where a background command writes, as its result says it: "Output is being written to:
/// `<path>`. You will be notified …".
fn output_file_in(text: &str) -> Option<String> {
    let (_, rest) = text.split_once("Output is being written to: ")?;
    let path = rest.split_whitespace().next()?.trim_end_matches('.');
    path.ends_with(".output").then(|| path.to_owned())
}

/// A subagent's `<usage>` in its notice: `total_tokens: 1200`, `tool_uses: 4`,
/// `duration_ms: 3000`, one to a line (or each in a tag of its own).
fn usage_in(body: &str) -> (Option<u64>, Option<u64>, Option<u64>) {
    let Some(usage) = tag(body, "usage") else { return (None, None, None) };
    let figure = |keys: &[&str]| {
        keys.iter().find_map(|key| {
            let value = tag(usage, key).or_else(|| {
                usage.lines().find_map(|line| line.trim().strip_prefix(key)?.strip_prefix(':'))
            })?;
            value.trim().parse().ok()
        })
    };
    (figure(&["total_tokens", "subagent_tokens"]), figure(&["tool_uses"]), figure(&["duration_ms"]))
}

/// "… completed (exit code 3)".
fn exit_code_in_summary(summary: &str) -> Option<i32> {
    let (_, rest) = summary.rsplit_once("exit code ")?;
    rest.split(|c: char| !c.is_ascii_digit() && c != '-').next()?.parse().ok()
}

/// Whether a record is something the person typed: a user record with words of theirs, not a
/// tool result, an interrupt marker, a notice or text Claude Code injected.
fn is_prompt(record: &Value) -> bool {
    if str_at(record, "type") != Some("user")
        || bool_at(record, "isMeta")
        || bool_at(record, "isCompactSummary")
    {
        return false;
    }
    let Some(content) = record.get("message").and_then(|m| m.get("content")) else { return false };
    let first = match content {
        Value::String(text) => Some(text.as_str()),
        Value::Array(blocks) => {
            if blocks.iter().any(|b| str_at(b, "type") == Some("tool_result")) {
                return false;
            }
            let text = blocks.iter().find_map(|b| str_at(b, "text"));
            // A picture pasted with no words.
            if text.is_none() && blocks.iter().any(|b| str_at(b, "type") == Some("image")) {
                return true;
            }
            text
        }
        _ => None,
    };
    first.is_some_and(|text| {
        let text = text.trim_start();
        !text.is_empty()
            && !text.starts_with('<')
            && !text.starts_with("[Request interrupted by user")
    })
}

/// The text between `<name>` and `</name>`.
fn tag<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let (_, rest) = text.split_once(open.as_str())?;
    Some(rest.split_once(close.as_str()).map_or(rest, |(inner, _)| inner))
}

/// Terminal colour codes Claude Code leaves in command output.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.next() == Some('[') {
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// A message content's text: the string, or its text blocks joined.
fn content_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => {
            let mut out = String::new();
            for text in blocks.iter().filter_map(|b| b.as_str().or_else(|| str_at(b, "text"))) {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(text);
            }
            out
        }
        _ => String::new(),
    }
}

fn str_at<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key)?.as_str()
}

fn string_at(value: &Value, key: &str) -> Option<String> {
    str_at(value, key).map(str::to_owned)
}

fn bool_at(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool) == Some(true)
}

fn u64_at(value: &Value, key: &str) -> Option<u64> {
    let v = value.get(key)?;
    v.as_u64().or_else(|| v.as_str()?.parse().ok())
}

fn to_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn to_u64(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

/// A count of milliseconds Claude Code wrote as a fraction (`1084.53`), whole.
fn ms_of(ms: f64) -> u64 {
    if !ms.is_finite() || ms <= 0.0 {
        return 0;
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        clippy::cast_sign_loss,
        reason = "checked positive and finite, a wait in ms; the bound need not be exact"
    )]
    let whole = ms.round().min(u64::MAX as f64) as u64;
    whole
}

/// An RFC 3339 UTC stamp (`2026-09-27T03:15:25.849Z`) in ms since the Unix epoch.
fn parse_ms(stamp: &str) -> Option<WallMs> {
    let (date, time) = stamp.split_once('T')?;
    let time = time.strip_suffix('Z')?;
    let mut ymd = date.splitn(3, '-').map(str::parse::<i64>);
    let (year, month, day) = (ymd.next()?.ok()?, ymd.next()?.ok()?, ymd.next()?.ok()?);
    let (clock, fraction) = time.split_once('.').unwrap_or((time, ""));
    let mut hms = clock.splitn(3, ':').map(str::parse::<i64>);
    let (hour, minute, second) = (hms.next()?.ok()?, hms.next()?.ok()?, hms.next()?.ok()?);
    let millis: i64 =
        format!("{:0<3}", fraction.get(..3.min(fraction.len())).unwrap_or("")).parse().ok()?;
    let days = days_from_civil(year, month, day)?;
    let seconds = days
        .checked_mul(86_400)?
        .checked_add(hour.checked_mul(3_600)?)?
        .checked_add(minute.checked_mul(60)?)?
        .checked_add(second)?;
    u64::try_from(seconds.checked_mul(1_000)?.checked_add(millis)?).ok().map(WallMs::from_millis)
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's `days_from_civil`).
fn days_from_civil(year: i64, month: i64, day: i64) -> Option<i64> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let year = if month <= 2 { year.checked_sub(1)? } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year.checked_sub(era.checked_mul(400)?)?;
    let shifted_month = if month > 2 { month.checked_sub(3)? } else { month.checked_add(9)? };
    let day_of_year = shifted_month
        .checked_mul(153)?
        .checked_add(2)?
        .checked_div(5)?
        .checked_add(day)?
        .checked_sub(1)?;
    let day_of_era = year_of_era
        .checked_mul(365)?
        .checked_add(year_of_era.checked_div(4)?)?
        .checked_sub(year_of_era.checked_div(100)?)?
        .checked_add(day_of_year)?;
    era.checked_mul(146_097)?.checked_add(day_of_era)?.checked_sub(719_468)
}

/// The whole of a clipped text, from the transcript's JSONL (`None` when the record or the part
/// is not there).
#[must_use]
pub fn full_text(jsonl: &str, reference: &TextRef) -> Option<String> {
    let needle = format!("\"{}\"", reference.record);
    jsonl
        .lines()
        .filter(|line| line.contains(needle.as_str()))
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|record| str_at(record, "uuid") == Some(reference.record.as_str()))
        .and_then(|record| part_text(&record, &reference.part))
}

/// The bytes of the picture `reference` names, from the transcript's JSONL: `None` when the
/// record or the picture is not there, or it is larger than [`IMAGE_BYTES`].
#[must_use]
pub fn image_bytes(jsonl: &str, reference: &TextRef) -> Option<Vec<u8>> {
    let needle = format!("\"{}\"", reference.record);
    jsonl
        .lines()
        .filter(|line| line.contains(needle.as_str()))
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|record| str_at(record, "uuid") == Some(reference.record.as_str()))
        .and_then(|record| media::bytes_at(&record, &reference.part))
        .filter(|bytes| bytes.len() <= IMAGE_BYTES)
}

/// [`full_text`] from the transcript file at `path`.
///
/// # Errors
///
/// When the file cannot be read.
pub fn full_text_at(path: &Path, reference: &TextRef) -> std::io::Result<Option<String>> {
    Ok(full_text(&std::fs::read_to_string(path)?, reference))
}

fn part_text(record: &Value, part: &Part) -> Option<String> {
    let content = record.get("message").and_then(|m| m.get("content"));
    let blocks = || content.and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    let result = record.get("toolUseResult");
    match part {
        Part::Block { index } => match content? {
            Value::String(s) if *index == 0 => Some(s.clone()),
            Value::Array(blocks) => {
                let block = blocks.get(usize::try_from(*index).ok()?)?;
                block
                    .as_str()
                    .or_else(|| str_at(block, "text"))
                    .or_else(|| str_at(block, "thinking"))
                    .map(str::to_owned)
            }
            _ => None,
        },
        Part::Input { tool_use_id, field } => blocks()
            .iter()
            .find(|b| str_at(b, "id") == Some(tool_use_id.as_str()))
            .and_then(|b| string_at(b.get("input")?, field)),
        Part::Result { tool_use_id } => {
            let block =
                blocks().iter().find(|b| str_at(b, "tool_use_id") == Some(tool_use_id.as_str()))?;
            let from_result =
                result.and_then(|r| r.get("content")).map(content_text).filter(|t| !t.is_empty());
            Some(
                from_result
                    .unwrap_or_else(|| content_text(block.get("content").unwrap_or(&Value::Null))),
            )
        }
        // Not text of the transcript: a picture's bytes, a file the worker tails.
        Part::Image { .. } | Part::Output { .. } => None,
        Part::Stdout => result.and_then(|r| string_at(r, "stdout")),
        Part::Stderr => result.and_then(|r| string_at(r, "stderr")),
        Part::Patch => {
            let result = result?;
            let hunks = result.get("structuredPatch")?.as_array()?;
            let original = original_lines(result);
            let mut out = String::new();
            for hunk in hunks {
                let n = |key: &str| u64_at(hunk, key).unwrap_or(0);
                let _written = write!(
                    out,
                    "@@ -{},{} +{},{} @@",
                    n("oldStart"),
                    n("oldLines"),
                    n("newStart"),
                    n("newLines")
                );
                if let Some(heading) = heading_above(&original, hunk) {
                    out.push(' ');
                    out.push_str(&heading);
                }
                out.push('\n');
                for line in hunk
                    .get("lines")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                {
                    out.push_str(line);
                    out.push('\n');
                }
            }
            Some(out)
        }
    }
}

#[cfg(test)]
mod tests;

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
pub use vocabulary::*;

use crate::transcript::Tail;

pub mod media;
pub mod output;
mod said;
mod session;
mod vocabulary;
pub use session::{Located, Transcripts, subagents_dir};

/// Prose: prompts, answers, thinking, plans, subagent reports, compaction summaries. Long enough
/// for any answer a person reads through, short of a pasted log.
pub const PROSE: Cap = Cap { lines: 400, chars: 32_000 };

/// Tool output and inputs: a Bash tail, a result's text, an unknown tool's input. The same
/// 40 lines or 4 000 characters the old conversation view settled on (`claude-code.md`).
pub const OUTPUT: Cap = Cap { lines: 40, chars: 4_000 };

/// What is kept of the text the model read where the call shows something else
/// ([`ToolResult::seen`]): nothing, only its size and where it is.
const SEEN: Cap = Cap { lines: 0, chars: 0 };

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
    /// A fork's transcript opens with what it inherited of its parent's (`fork-context-ref`):
    /// the parent's records up to its own first message are not its.
    inherited: bool,
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

    fn get(&self, id: &str) -> Option<&Entry> {
        self.entries.get(*self.index.get(id)?)
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
    /// A monitor's event, from its `<event>`.
    event: Option<String>,
    /// When the record carrying it was written.
    at_ms: WallMs,
    /// The tasks it names past the first: one notice ends several when the session that ran
    /// them ended ("4 background agents didn't finish before the previous session ended").
    more_tasks: Vec<String>,
}

/// The words a subagent's notice says when it stopped with background work of its own still
/// running (Claude Code 2.1.301): its result may be interim.
const WAITING_WORDS: &str = "is waiting on its own background work";

impl Notice {
    /// Whether it says a subagent waits on work of its own rather than finished.
    fn waits(&self) -> bool {
        self.status.as_deref() == Some("completed")
            && self.summary.as_deref().is_some_and(|s| s.contains(WAITING_WORDS))
    }
}

/// The changes one read makes, an entry updated twice kept once at its first place.
#[derive(Default)]
struct Batch {
    changes: Vec<Change>,
    upserts: HashMap<(ThreadId, String), usize>,
    tasks: HashMap<ThreadId, usize>,
    turns: HashMap<(ThreadId, String), usize>,
    queued: Option<usize>,
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

    fn queued(&mut self, queued: Vec<Queued>) {
        if let Some(change) = self.queued.and_then(|at| self.changes.get_mut(at)) {
            *change = Change::Queued(queued);
            return;
        }
        self.queued = Some(self.changes.len());
        self.changes.push(Change::Queued(queued));
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
    /// The call that started each background task, by the task's id: a notice of a run taken
    /// up again (a subagent sent more words) names only the task.
    task_calls: HashMap<String, String>,
    /// What arrived for calls not seen yet, by `tool_use_id`.
    pending: HashMap<String, Vec<Pending>>,
    /// Calls that run in the background and write their output to a file, oldest first.
    background: Vec<(ThreadId, String)>,
    /// The task notices made rows already, by what they say, and how many each task has had:
    /// the same notice read twice (its queueing and its delivery) is one row.
    task_rows: std::collections::HashSet<(String, String, String, String)>,
    task_counts: HashMap<String, u32>,
    /// The prompts the person queued in the TUI behind the main thread's turn, in their order.
    queue: Vec<Queued>,
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
        Ok(self.take_lines(path, &lines))
    }

    /// Decode `lines` a [`Tail`] read from the file at `path`, as [`Self::read`] does.
    pub fn take_lines(&mut self, path: &Path, lines: &crate::transcript::Lines) -> Vec<Change> {
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
        batch.changes
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

    /// Apply one of the TUI's queue operations on the person's prompts: one queued at the end
    /// (`enqueue`), all taken back into the box (`popAll`), the first given the agent
    /// (`dequeue`), or one taken out (`remove`). Its background work's notices ride the same
    /// queue and are not the person's; they are queued at a lower priority, so a `dequeue`,
    /// which names nothing, takes the person's first. Whether the queue changed.
    fn queue_op(&mut self, operation: Option<&str>, record: &Value, at_ms: WallMs) -> bool {
        let content = record.get("content");
        match operation {
            Some("enqueue") => {
                let text = content.map(joined_text).unwrap_or_default();
                self.queue.push(Queued { text, at_ms });
                true
            }
            Some("popAll") => !std::mem::take(&mut self.queue).is_empty(),
            Some("dequeue") if !self.queue.is_empty() => {
                let _given = self.queue.remove(0);
                true
            }
            Some("remove") => {
                let text = content.map(joined_text).unwrap_or_default();
                let at = self.queue.iter().position(|q| q.text == text);
                at.map(|at| self.queue.remove(at)).is_some()
            }
            _ => false,
        }
    }

    fn record(&mut self, source: &ThreadId, record: &Value, batch: &mut Batch) {
        let kind = str_at(record, "type").unwrap_or_default();
        if kind == "queue-operation" {
            let at_ms = str_at(record, "timestamp").and_then(parse_ms).unwrap_or_default();
            let operation = str_at(record, "operation");
            // Written the moment background work ends, before the model takes the notice in.
            if operation == Some("enqueue")
                && let Some(mut notice) = str_at(record, "content").and_then(parse_notice)
            {
                notice.at_ms = at_ms;
                self.task_row(source, None, &notice, batch);
                self.notice(notice, batch);
            } else if *source == ThreadId::Main && self.queue_op(operation, record, at_ms) {
                batch.queued(self.queue.clone());
            }
            return;
        }
        if kind == "agent_metadata" {
            self.threads.entry(source.clone()).or_default().origin = Some(Origin {
                tool_use_id: string_at(record, "toolUseId"),
                agent_type: string_at(record, "agentType").map(agent_kind),
                description: string_at(record, "description"),
            });
            return;
        }
        if kind == "fork-context-ref" {
            let owner = str_at(record, "agentId")
                .filter(|a| !a.is_empty())
                .map_or_else(|| source.clone(), |a| ThreadId::Agent(a.to_owned()));
            self.threads.entry(owner).or_default().inherited = true;
            return;
        }
        let Some(uuid) = str_at(record, "uuid") else { return };
        let thread = match (bool_at(record, "isSidechain"), str_at(record, "agentId")) {
            (true, Some(agent)) if !agent.is_empty() => ThreadId::Agent(agent.to_owned()),
            (true, _) if *source == ThreadId::Main => ThreadId::Agent(String::new()),
            _ => source.clone(),
        };
        // What a fork inherited is its parent's: its own records begin at its first message.
        if let Some(t) = self.threads.get_mut(&thread).filter(|t| t.inherited) {
            if kind != "user" {
                return;
            }
            t.inherited = false;
        }
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
                    self.task_row(&thread, Some(made_at), &notice, batch);
                    self.notice(notice, batch);
                }
            }
            _ => {}
        }
        if kind == "user" && has_tool_result(record) {
            self.name_turn(&thread, record, batch);
        }
    }

    /// A tool's result names the prompt the open turn goes on under (`promptId`): one queued
    /// behind it and taken in mid-turn moves it on to that one's id, and its hooks name it so.
    fn name_turn(&mut self, thread: &ThreadId, record: &Value, batch: &mut Batch) {
        let Some(id) = str_at(record, "promptId").filter(|id| !id.is_empty()) else { return };
        let Some(t) = self.threads.get_mut(thread) else { return };
        let Some(open) = t.turns.last_mut().filter(|t| t.ended_ms.is_none()) else { return };
        if !open.prompt_ids.iter().any(|known| known == id) {
            open.prompt_ids.push(id.to_owned());
            batch.turn(thread, open.clone());
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
        let Some(content) = record.get("message").and_then(|m| m.get("content")) else { return };
        // A stop hook's feedback wakes the model as a message of Claude Code's own: the TUI
        // says it as the hook's row, not the person's.
        if let Some(feedback) =
            content_text(content).trim_start().strip_prefix("Stop hook feedback")
        {
            let said = feedback.trim_start_matches(':').trim();
            let text = if said.is_empty() {
                "Stop hook feedback".to_owned()
            } else {
                format!("Stop hook feedback\n{said}")
            };
            let note = Note {
                kind: NoteKind::Hook,
                text: Clipped::head(&text, OUTPUT, None),
                retry: None,
                stop: None,
            };
            self.add(ctx, ctx.uuid.to_owned(), Body::Note(note), batch);
            return;
        }
        if bool_at(record, "isMeta") {
            return;
        }
        // A prompt a plugin queued says whose it is, above it.
        if let Some(origin) = record.get("origin").filter(|o| str_at(o, "kind") == Some("plugin"))
            && let Some(name) = str_at(origin, "name").or_else(|| str_at(origin, "plugin"))
        {
            let note = Note {
                kind: NoteKind::Quiet,
                text: Clipped::head(&format!("Prompt from the {name} plugin"), OUTPUT, None),
                retry: None,
                stop: None,
            };
            self.add(ctx, format!("{}:origin", ctx.uuid), Body::Note(note), batch);
        }
        if bool_at(record, "isCompactSummary") {
            let text = summary_text(&joined_text(content));
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
        // A fork's first message answers its parent's call in the context it inherited ("Fork
        // started"), and says its directive under Claude Code's words for a fork: its prompt
        // is the directive, and that answer is not its.
        let seeds_fork = blocks.iter().any(|b| fork_directive(block_text(b)).is_some());
        for (index, block) in blocks.iter().enumerate() {
            let text = match block {
                Value::String(s) => Some(s.as_str()),
                _ if str_at(block, "type") == Some("text") => str_at(block, "text"),
                _ if str_at(block, "type") == Some("tool_result") => {
                    if !seeds_fork {
                        self.result(ctx, block, record.get("toolUseResult"), batch);
                    }
                    None
                }
                _ => None,
            };
            let Some(text) = text else { continue };
            let text = fork_directive(text).unwrap_or(text);
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
                self.task_row(ctx.thread, Some(ctx.made_at), &notice, batch);
                self.notice(notice, batch);
                continue;
            } else if let Some(name) =
                tag(text, "command-name").filter(|_| leads(text, &COMMAND_TAGS))
            {
                Body::Prompt(Prompt {
                    text: Clipped::head(
                        tag(text, "command-args").unwrap_or_default().trim(),
                        PROSE,
                        Some(reference()),
                    ),
                    images: Vec::new(),
                    command: Some(name.trim().to_owned()),
                })
            } else if let Some(input) = tag(text, "bash-input").filter(|_| leads(text, &SHELL_TAGS))
            {
                Body::Prompt(Prompt {
                    text: Clipped::head(input.trim(), PROSE, Some(reference())),
                    images: Vec::new(),
                    command: Some("!".to_owned()),
                })
            } else if leads(text, &OUTPUT_TAGS) {
                // What it printed, then its errors apart, as the TUI draws them in red.
                let printed = |names: [&str; 2]| {
                    let said = names.iter().find_map(|name| tag(text, name)).unwrap_or_default();
                    strip_ansi(said.trim())
                };
                let id =
                    if index == 0 { ctx.uuid.to_owned() } else { format!("{}:{index}", ctx.uuid) };
                for (kind, said, at) in [
                    (
                        NoteKind::Command,
                        printed(["local-command-stdout", "bash-stdout"]),
                        id.clone(),
                    ),
                    (
                        NoteKind::CommandError,
                        printed(["local-command-stderr", "bash-stderr"]),
                        format!("{id}:stderr"),
                    ),
                ] {
                    if said.is_empty() {
                        continue;
                    }
                    let note = Note {
                        kind,
                        text: Clipped::head(&said, OUTPUT, Some(reference())),
                        retry: None,
                        stop: None,
                    };
                    self.add(ctx, at, Body::Note(note), batch);
                }
                self.end_command(ctx, batch);
                continue;
            } else if let Some(update) = mcp_update(text) {
                Body::Note(Note {
                    kind: NoteKind::Quiet,
                    text: Clipped::head(&update, OUTPUT, Some(reference())),
                    retry: None,
                    stop: None,
                })
            } else if let Some(message) = peer_message(text) {
                Body::Note(Note {
                    kind: NoteKind::Quiet,
                    text: Clipped::head(&message, PROSE, Some(reference())),
                    retry: None,
                    stop: None,
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
            // An Esc ends the turn there: Claude Code writes no end of its own for it.
            let ends_turn = matches!(body, Body::Interrupted { .. });
            self.add(ctx, id.clone(), body, batch);
            if opens_turn {
                self.start_turn(ctx, &id, record, batch);
            } else if ends_turn {
                self.end_turn(ctx, batch);
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
            self.start_turn(ctx, ctx.uuid, record, batch);
        }
    }

    /// A prompt opens a turn, named for the hooks by its `record`'s `promptId` and run in the
    /// permission mode it says (`permissionMode`): what the thread's records say from here on
    /// is its. One the person sent while the turn before it still ran ends that turn here, if
    /// Claude Code did not close it first: no turn stays running behind a later one.
    fn start_turn(&mut self, ctx: &Ctx<'_>, prompt: &str, record: &Value, batch: &mut Batch) {
        let prompt_id = str_at(record, "promptId");
        let thread = self.threads.entry(ctx.thread.clone()).or_default();
        if let Some(open) = thread.turns.last_mut().filter(|t| t.ended_ms.is_none()) {
            open.ended_ms = Some(ctx.at_ms);
            batch.turn(ctx.thread, open.clone());
        }
        let turn = Turn {
            prompt: prompt.to_owned(),
            prompt_ids: prompt_id
                .filter(|id| !id.is_empty())
                .map(str::to_owned)
                .into_iter()
                .collect(),
            started_ms: ctx.at_ms,
            mode: string_at(record, "permissionMode").filter(|m| !m.trim().is_empty()),
            ..Turn::default()
        };
        thread.turns.push(turn.clone());
        batch.turn(ctx.thread, turn);
    }

    /// A command Claude Code ran itself (`/model`, `/compact`, a `!` shell line) printed what it
    /// had to say: its turn ends there. No model answers such a command and no hook ends its
    /// turn, so this record is the only end it has. Output in a turn a command did not open
    /// leaves the turn as it is.
    fn end_command(&mut self, ctx: &Ctx<'_>, batch: &mut Batch) {
        let Some(thread) = self.threads.get(ctx.thread) else { return };
        let Some(open) = thread.turns.last().filter(|t| t.ended_ms.is_none()) else { return };
        let by_command = matches!(
            thread.get(&open.prompt).map(|e| &e.body),
            Some(Body::Prompt(Prompt { command: Some(_), .. }))
        );
        if by_command {
            self.end_turn(ctx, batch);
        }
    }

    /// An assistant record's model and usage, counted in its turn, and the context its request
    /// carried, the turn's as the latest request's. The records of one request (one per content
    /// block) repeat its usage, so a request is counted once, at the latest figures its records
    /// give.
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
            turn.usage = turn.usage.plus(now);
        }
        let read = now.input.saturating_add(now.cache_read).saturating_add(now.cache_write);
        turn.context = (read > 0).then_some(read).or(turn.context);
        if let Some(model) = model
            && !turn.models.iter().any(|m| m == model)
        {
            turn.models.push(model.to_owned());
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

    /// An assistant record: its blocks, its request's usage, and the end of its turn when it is
    /// the model's final answer ([`answers_last`]). That end is the turn's own, and the only one
    /// a session Claude Code ran with no Stop hook writes: without it, the turn would run on to
    /// the next prompt, or to now. A hook's summary after it, or a later final answer (the model
    /// woken again by a notice or a blocking hook), moves it on, as each end does.
    fn assistant(&mut self, ctx: &Ctx<'_>, record: &Value, batch: &mut Batch) {
        let Some(message) = record.get("message") else { return };
        self.answer(ctx, record, message, batch);
        if answers_last(message) {
            self.end_turn(ctx, batch);
        }
    }

    fn answer(&mut self, ctx: &Ctx<'_>, record: &Value, message: &Value, batch: &mut Batch) {
        let Some(content) = message.get("content") else { return };
        if bool_at(record, "isApiErrorMessage") {
            let text = content_text(content);
            // Claude Code records the error it gave up on with its kind.
            let stop =
                Stop { error: string_at(record, "error").unwrap_or_else(|| "unknown".to_owned()) };
            let body = Body::Note(Note {
                kind: NoteKind::ApiError,
                text: Clipped::head(text.trim(), OUTPUT, None),
                retry: None,
                stop: Some(stop),
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
                        // Thinking its signature marks as narration is said to the person, and
                        // the TUI draws it as the answer's own words.
                        let said = Clipped::head(text, PROSE, Some(reference()));
                        let body = if said::narration(str_at(block, "signature")) {
                            Body::Text(said)
                        } else {
                            Body::Thinking(said)
                        };
                        if !text.trim().is_empty() {
                            self.add(ctx, id(), body, batch);
                        }
                    }
                    Some("tool_use" | "server_tool_use") => self.call(ctx, block, batch),
                    // A server tool's result rides the answer itself (the advisor's, a web
                    // search's): it settles its call as a person's tool result would.
                    Some(kind) if kind.ends_with("_tool_result") => {
                        let content = block.get("content").cloned().unwrap_or(Value::Null);
                        let failed = content
                            .get("type")
                            .and_then(Value::as_str)
                            .is_some_and(|t| t.ends_with("_error"));
                        let text = str_at(&content, "text").unwrap_or_default();
                        let settled = serde_json::json!({
                            "type": "tool_result",
                            "tool_use_id": block.get("tool_use_id"),
                            "content": text,
                            "is_error": failed,
                        });
                        let structured = content.is_object().then_some(&content);
                        self.result(ctx, &settled, structured, batch);
                    }
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
                    stop: None,
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
                    stop: None,
                })
            }
            Some("local_command") => {
                self.end_command(ctx, batch);
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
                    stop: None,
                })
            }
            Some("informational") if !content.trim().is_empty() => Body::Note(Note {
                kind: NoteKind::Info,
                text: Clipped::head(content.trim(), OUTPUT, None),
                retry: None,
                stop: None,
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
        let title = said::header(name, input);
        let whole = TextRef {
            record: ctx.uuid.to_owned(),
            part: Part::Call { tool_use_id: id.to_owned() },
        };
        let input = Clipped::head(&input_json(input), OUTPUT, Some(whole));
        let call = ToolCall {
            name: name.to_owned(),
            input,
            detail,
            result: None,
            title,
            record: ctx.uuid.to_owned(),
        };
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
        let input = said_input(call);
        let said = said::said(
            &call.name,
            said::Outcome {
                input: input.as_ref(),
                // A list result (`ListMcpResourcesTool`'s) is the result whole.
                result: result.filter(|r| r.is_object() || r.is_array()),
                text: &text,
                status,
                at_ms: ctx.at_ms.as_millis(),
            },
        );
        let shown =
            apply_result(&mut call.detail, status, &text, structured, (ctx.thread, ctx.uuid));
        let full = Some(TextRef {
            record: ctx.uuid.to_owned(),
            part: Part::Result { tool_use_id: id.to_owned() },
        });
        // A list the TUI draws under the call stands for the text the model read, and a line
        // drawn alone (a refusal by auto mode) for all of it. A list too long to show whole
        // gives way to that text, which opens whole where the list has nowhere to open.
        let long_list = !text.trim().is_empty()
            && said.body.as_deref().is_some_and(|body| !OUTPUT.holds(body.trim()));
        let body = said.body.filter(|_| !long_list);
        let kept = body.is_none() && (long_list || !shown || status != ResultStatus::Ok);
        // What the model read, where the call shows something else in its place.
        let seen =
            (!kept && !text.trim().is_empty() && shows_otherwise(&call.detail, &text, structured))
                .then(|| Clipped::head_trimmed(&text, SEEN, full.clone()));
        let text = match body {
            Some(body) => {
                (!body.trim().is_empty()).then(|| Clipped::head(body.trim(), OUTPUT, None))
            }
            None => kept.then(|| Clipped::head_trimmed(&text, OUTPUT, full)),
        };
        let text = text.filter(|t| !t.text.is_empty() || images.is_empty());
        call.result = Some(ToolResult {
            status,
            text,
            at_ms: ctx.at_ms,
            images,
            said: said.line,
            record: ctx.uuid.to_owned(),
            seen,
        });
        let changed_tasks = task_change(&call.detail, call.result.as_ref());
        // A task stopped by a call (`TaskStop`) says so in no notice: the call's result is its end.
        let stopped = (status == ResultStatus::Ok
            && matches!(call.name.as_str(), "TaskStop" | "KillShell" | "KillBash"))
        .then(|| {
            structured.and_then(|r| string_at(r, "task_id").or_else(|| string_at(r, "shell_id")))
        })
        .flatten();
        let task = match &call.detail {
            ToolDetail::Agent(agent) => agent.agent_id.clone(),
            ToolDetail::Bash(bash) => bash.task_id.clone(),
            _ => None,
        };
        if let Some(task) = task {
            self.task_calls.insert(task, id.to_owned());
        }
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
        if let Some(task) = stopped {
            let notice = Notice {
                task_id: Some(task),
                status: Some("stopped".to_owned()),
                at_ms: ctx.at_ms,
                ..Notice::default()
            };
            self.notice(notice, batch);
        }
    }

    /// Whether call `id` is one that starts background work: a command or a subagent.
    fn starts_task(&self, id: &str) -> bool {
        let entry = self.calls.get(id).and_then(|thread| self.threads.get(thread)?.get(id));
        entry.is_some_and(|entry| {
            matches!(&entry.body, Body::Tool(call)
                if matches!(call.detail, ToolDetail::Bash(_) | ToolDetail::Agent(_)))
        })
    }

    /// The calls started before `at` that still run: a command or a subagent, in the background
    /// or not.
    fn running_before(&self, at: WallMs) -> Vec<String> {
        let mut ids: Vec<String> = self
            .calls
            .iter()
            .filter_map(|(id, thread)| {
                let entry = self.threads.get(thread)?.get(id)?;
                let Body::Tool(call) = &entry.body else { return None };
                let running = match &call.detail {
                    ToolDetail::Bash(bash) => bash.status == ShellStatus::Running,
                    ToolDetail::Agent(agent) => {
                        matches!(agent.status, AgentRun::Running | AgentRun::Waiting)
                    }
                    _ => false,
                };
                (running && entry.at_ms < at).then(|| id.clone())
            })
            .collect();
        ids.sort();
        ids
    }

    fn notice(&mut self, mut notice: Notice, batch: &mut Batch) {
        let more_tasks = std::mem::take(&mut notice.more_tasks);
        // Claude Code taken up again says which of its tasks the session before it left
        // unfinished, though not all of them: whatever that session started and left running
        // ended with it.
        if notice.summary.as_deref().is_some_and(|said| said.contains(PREVIOUS_SESSION_ENDED))
            && !notice.at_ms.is_zero()
        {
            for id in self.running_before(notice.at_ms) {
                let ended = Notice {
                    task_id: None,
                    tool_use_id: Some(id),
                    status: Some("stopped".to_owned()),
                    summary: None,
                    result: None,
                    usage: (None, None, None),
                    ..notice.clone()
                };
                self.notice(ended, batch);
            }
        }
        // Each other task it names ends by it too, found by its task: it names no call for them.
        for task in more_tasks {
            self.notice(
                Notice { task_id: Some(task), tool_use_id: None, summary: None, ..notice.clone() },
                batch,
            );
        }
        // The call that started the task: the one the notice names, unless that is another
        // (the `SendMessage` that took a subagent up again), else the one its task names.
        let by_task = notice.task_id.as_ref().and_then(|task| self.task_calls.get(task)).cloned();
        let id = match notice.tool_use_id.clone() {
            Some(named) if self.starts_task(&named) => named,
            Some(named) if by_task.is_none() && !self.calls.contains_key(&named) => {
                self.park(&named, Pending::Notice(notice));
                return;
            }
            named => {
                let Some(id) = by_task.or(named) else { return };
                id
            }
        };
        if let Some(task) = &notice.task_id
            && self.starts_task(&id)
        {
            self.task_calls.insert(task.clone(), id.clone());
        }
        let Some(thread) = self.calls.get(&id).cloned() else { return };
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
                    Some("completed") if notice.waits() => AgentRun::Waiting,
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
                let ran = at.filter(|_| {
                    !matches!(agent.status, AgentRun::Running | AgentRun::Waiting)
                        && !started.is_zero()
                });
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

    /// A task's notice as a row of its own in `thread`, made at chain position `made_at` (the
    /// end of the chain for a notice no record of the chain carries): every notice, with a call
    /// or without one, once.
    fn task_row(
        &mut self,
        thread: &ThreadId,
        made_at: Option<usize>,
        notice: &Notice,
        batch: &mut Batch,
    ) {
        let Some(task) = notice.task_id.clone().filter(|t| !t.is_empty()) else { return };
        let key = (
            task.clone(),
            notice.status.clone().unwrap_or_default(),
            notice.summary.clone().unwrap_or_default(),
            notice.event.clone().unwrap_or_default(),
        );
        if !self.task_rows.insert(key) {
            return;
        }
        let n = self.task_counts.entry(task.clone()).or_insert(0);
        *n = n.saturating_add(1);
        let id = format!("task:{task}:{n}");
        let called = notice.tool_use_id.as_deref().and_then(|call| {
            let t = self.threads.get(self.calls.get(call)?)?;
            match &t.entries.get(*t.index.get(call)?)?.body {
                Body::Tool(call) => Some((call.name.clone(), call.detail.clone())),
                _ => None,
            }
        });
        let row = task_notice(&task, notice, called.as_ref());
        let made_at = made_at.unwrap_or_else(|| {
            self.threads.get(thread).map_or(0, |t| t.chain.len().saturating_sub(1))
        });
        let ctx = Ctx { thread, uuid: &id, at_ms: notice.at_ms, made_at };
        self.add(&ctx, id.clone(), Body::Task(Box::new(row)), batch);
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
        ToolDetail::Edit(edit) if name == "NotebookEdit" => edit.patch = notebook_patch(input),
        ToolDetail::Edit(edit) => edit.patch = proposed_patch(&replacements(input)),
        ToolDetail::Write(write) => {
            let content = string_at(input, "content").unwrap_or_default();
            write.patch = proposed_patch(&[(String::new(), content)]);
        }
        _ => {}
    }
    detail
}

/// A `NotebookEdit`'s change to its cell, from its input or its result (`toolUseResult`), which
/// says the same and adds the cell's source before (`old_source`): a replaced cell's source
/// for its new one, an inserted cell's all added, a deleted cell's all removed. Before the
/// result, a replaced cell's old source is not known, and its new one reads as added.
fn notebook_patch(said: &Value) -> Patch {
    let old = string_at(said, "old_source").unwrap_or_default();
    let new = string_at(said, "new_source").unwrap_or_default();
    match str_at(said, "edit_mode") {
        Some("delete") => proposed_patch(&[(old, String::new())]),
        Some("insert") => proposed_patch(&[(String::new(), new)]),
        _ => proposed_patch(&[(old, new)]),
    }
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
        // A notebook's cell is an edit of the notebook: its patch is the cell's source, from
        // the result's `old_source` once it came ([`notebook_patch`]).
        "NotebookEdit" => ToolDetail::Edit(EditDetail {
            path: path(),
            edits: 1,
            replace_all: false,
            patch: Patch::default(),
        }),
        "Edit" | "MultiEdit" => ToolDetail::Edit(EditDetail {
            path: path(),
            edits: input.get("edits").and_then(Value::as_array).map_or(1, |e| to_u32(e.len())),
            replace_all: bool_at(input, "replace_all"),
            patch: Patch::default(),
        }),
        "Write" => ToolDetail::Write(WriteDetail {
            path: path(),
            lines: to_u32(str_at(input, "content").map_or(0, |c| c.lines().count())),
            created: None,
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
        // PowerShell is a shell of its own: its result has Bash's fields.
        "Bash" | "PowerShell" => ToolDetail::Bash(BashDetail {
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
            edits: None,
            interpretation: None,
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
            agent_type: text("subagent_type").map(agent_kind),
            description: text("description"),
            name: text("name"),
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
            model: None,
            tally: BTreeMap::new(),
            changed: slopty_proto::thread::Changed::default(),
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
            // Slopty's own visual or page, read whole from the call's input.
            if let Some(shown) =
                crate::shown::named(name).and_then(|tool| crate::shown::shown(tool, input, ""))
            {
                return ToolDetail::Shown(Box::new(shown));
            }
            match name.strip_prefix("mcp__").and_then(|rest| rest.split_once("__")) {
                Some((server, tool)) => ToolDetail::Mcp(McpDetail {
                    server: server.to_owned(),
                    tool: tool.to_owned(),
                    structured: None,
                    links: Vec::new(),
                    result: None,
                }),
                None => ToolDetail::Other,
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
                        Some(Choice {
                            label,
                            description: string_at(o, "description"),
                            preview: string_at(o, "preview").filter(|p| !p.trim().is_empty()),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
        multi_select: bool_at(q, "multiSelect"),
    }
}

/// The input a call was given, as the transcript wrote it.
///
/// Its whole JSON when the call holds all of it, else what each tool's own fields keep of it (a
/// dialog about the call is worded from it, `crate::claude::history::asks_for`).
#[must_use]
pub fn call_input(call: &ToolCall) -> Value {
    use serde_json::json;
    if let Some(whole) = whole_input(call) {
        return whole;
    }
    match &call.detail {
        ToolDetail::Bash(bash) => json!({ "command": bash.command.text }),
        ToolDetail::Edit(EditDetail { path, .. })
        | ToolDetail::Write(WriteDetail { path, .. })
        | ToolDetail::Read(ReadDetail { path, .. }) => json!({ "file_path": path }),
        ToolDetail::Grep(GrepDetail { pattern, .. })
        | ToolDetail::Glob(GlobDetail { pattern, .. }) => {
            json!({ "pattern": pattern })
        }
        ToolDetail::WebFetch(fetch) => json!({ "url": fetch.url }),
        ToolDetail::WebSearch(search) => json!({ "query": search.query }),
        ToolDetail::Agent(agent) => json!({ "description": agent.description }),
        other => kept_input(other).unwrap_or(Value::Null),
    }
}

/// The input [`said::said`] reads of a call: its whole JSON, else what a question's or a plan's
/// detail keeps of it.
fn said_input(call: &ToolCall) -> Option<Value> {
    whole_input(call).or_else(|| kept_input(&call.detail))
}

/// The call's input, when it holds all of it.
fn whole_input(call: &ToolCall) -> Option<Value> {
    if call.input.is_clipped() { None } else { serde_json::from_str(&call.input.text).ok() }
}

/// What a question's or a plan's detail keeps of its input.
fn kept_input(detail: &ToolDetail) -> Option<Value> {
    match detail {
        ToolDetail::Question(question) => Some(serde_json::json!({
            "questions": question
                .questions
                .iter()
                .map(|q| serde_json::json!({
                    "question": q.text,
                    "options": q.options.iter().map(|o| serde_json::json!({ "label": o.label }))
                        .collect::<Vec<_>>(),
                }))
                .collect::<Vec<_>>(),
        })),
        ToolDetail::Plan { plan } => Some(serde_json::json!({ "plan": plan.text })),
        _ => None,
    }
}

/// A call's input as its whole is shown and fetched ([`Part::Call`]): pretty JSON, so a clipped
/// head reads by its lines.
fn input_json(input: &Value) -> String {
    serde_json::to_string_pretty(input).unwrap_or_default()
}

/// Fold a result into the call's detail. Returns whether the detail now shows the outcome, so
/// the result's own text would only repeat it.
/// Whether what `detail` shows of a result is not the text the model read (`read`, with its
/// structured `result`). A subagent's card reads its report. A command's output and standard
/// error are that text, and a command that printed nothing shows so, which Claude Code's note
/// ("(Bash completed with no output)") only says again. An edit's diff and a write's lines say
/// what Claude Code's one line of thanks does; only a longer answer (a snippet of the file as it
/// is now) says more.
fn shows_otherwise(detail: &ToolDetail, read: &str, result: Option<&Value>) -> bool {
    let read = read.trim();
    match detail {
        ToolDetail::Agent(_) => false,
        ToolDetail::Bash(_) => {
            let printed: Vec<&str> = ["stdout", "stderr"]
                .into_iter()
                .filter_map(|key| result.and_then(|r| str_at(r, key)))
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .collect();
            let note = read.starts_with('(') && read.ends_with(')') && !read.contains('\n');
            if printed.is_empty() && note {
                return false;
            }
            printed.join("\n") != read
        }
        // A step that draws what it did (a diff, the task list, the answers, a plan) stands for
        // the line confirming it, and differs from the text only when there is more to it.
        ToolDetail::Edit(_)
        | ToolDetail::Write(_)
        | ToolDetail::TaskCreate(_)
        | ToolDetail::TaskUpdate(_)
        | ToolDetail::TodoWrite { .. }
        | ToolDetail::Question(_)
        | ToolDetail::Plan { .. }
        | ToolDetail::Shown(_) => read.lines().nth(1).is_some(),
        _ => true,
    }
}

fn apply_result(
    detail: &mut ToolDetail,
    status: ResultStatus,
    text: &str,
    result: Option<&Value>,
    (thread, uuid): (&ThreadId, &str),
) -> bool {
    let field = |key: &str| result.and_then(|r| r.get(key));
    let patch = || result.map(|r| patch(r, thread, uuid)).unwrap_or_default();
    match detail {
        ToolDetail::Edit(edit) => {
            edit.patch = match result {
                Some(result) if result.get("edit_mode").is_some() => notebook_patch(result),
                _ => patch(),
            };
            result.is_some()
        }
        ToolDetail::Write(write) => {
            write.created = match field("type").and_then(Value::as_str) {
                Some("create") => Some(true),
                Some("update") => Some(false),
                _ => write.created,
            };
            write.patch = patch();
            // A file made whole comes with an empty diff: every line it wrote is added.
            if write.created == Some(true) && write.patch.hunks.is_empty() {
                write.patch.added = write.patch.added.max(write.lines);
            }
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
            bash.edits = result
                .and_then(|r| r.get("bashEditDiff"))
                .filter(|d| d.is_object())
                .map(|d| shell_edits(d, &bash.command.text));
            if let Some(task) = result.and_then(|r| string_at(r, "backgroundTaskId")) {
                bash.task_id = Some(task);
                bash.background = true;
                bash.output_file = bash.output_file.take().or_else(|| output_file_in(text));
            }
            bash.interpretation = result
                .and_then(|r| string_at(r, "returnCodeInterpretation"))
                .filter(|i| !i.trim().is_empty());
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
            agent.agent_type =
                string_at(r, "agentType").map(agent_kind).or_else(|| agent.agent_type.take());
            // A teammate is named by its name, as the TUI's `@name` row does.
            if str_at(r, "status") == Some("teammate_spawned") {
                let name = string_at(r, "name").or_else(|| agent.name.clone());
                agent.agent_type = name.map(|n| format!("@{}", n.trim_start_matches('@')));
            }
            agent.tokens = u64_at(r, "totalTokens");
            agent.tool_uses = u64_at(r, "totalToolUseCount");
            agent.duration_ms = u64_at(r, "totalDurationMs");
            agent.model = string_at(r, "resolvedModel").filter(|m| !m.trim().is_empty());
            if let Some(stats) = r.get("toolStats").and_then(Value::as_object) {
                (agent.tally, agent.changed) = tally(stats);
            }
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
                Clipped::head_trimmed(
                    &t,
                    PROSE,
                    Some(TextRef { record: uuid.to_owned(), part: Part::Report }),
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
        ToolDetail::Mcp(mcp) => {
            // What the server returned beside the text the model read: its structured content,
            // the resources it links, and the whole of it for an app's page.
            let Some(result) = result else { return false };
            mcp.structured = crate::shown::structured(field("structuredContent"));
            mcp.links = field("resourceLinks")
                .and_then(Value::as_array)
                .map(|links| crate::shown::mcp_links(links))
                .unwrap_or_default();
            let mut spelled = serde_json::Map::new();
            for key in ["content", "structuredContent", "_meta"] {
                if let Some(value) = result.get(key) {
                    spelled.insert(key.to_owned(), value.clone());
                }
            }
            if !spelled.contains_key("content") {
                let said = serde_json::json!([{ "type": "text", "text": text }]);
                spelled.insert("content".to_owned(), said);
            }
            if status == ResultStatus::Error {
                spelled.insert("isError".to_owned(), Value::Bool(true));
            }
            let spelled = Value::Object(spelled).to_string();
            mcp.result =
                Some(slopty_proto::thread::Clipped::head(&spelled, crate::shown::RESULT, None));
            false
        }
        ToolDetail::Plan { .. } | ToolDetail::Shown(_) | ToolDetail::Other => false,
    }
}

/// The diff in a result's `structuredPatch`, each hunk headed as git heads it from the file as
/// it was (`originalFile`).
fn patch(result: &Value, thread: &ThreadId, uuid: &str) -> Patch {
    let Some(hunks) = result.get("structuredPatch").and_then(Value::as_array) else {
        return Patch::default();
    };
    let mut out = hunks_patch(hunks, &original_lines(result));
    if out.clipped_lines > 0 {
        let whole = TextRef { record: uuid.to_owned(), part: Part::Patch };
        out.full = Some(crate::claude::history::content_ref(thread, &whole));
    }
    out
}

/// `hunks` in `structuredPatch`'s shape as a patch, each headed from the lines of `original`
/// above it, at most [`PATCH_LINES`] lines of them kept.
fn hunks_patch(hunks: &[Value], original: &[&str]) -> Patch {
    let mut out = Patch::default();
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
                heading: heading_above(original, hunk),
                lines: shown,
            });
        }
    }
    out
}

/// The files a Bash command changed, as Claude Code took them while it ran (`bashEditDiff`), for
/// command `command`.
fn shell_edits(diff: &Value, command: &str) -> slopty_proto::thread::detail::ShellEdits {
    let files = diff
        .get("files")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|file| {
            let hunks = file.get("hunks").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
            Some(slopty_proto::thread::detail::ShellEdit {
                path: str_at(file, "filePath")?.to_owned(),
                created: bool_at(file, "created"),
                deleted: bool_at(file, "deleted"),
                patch: hunks_patch(hunks, &[]),
            })
        })
        .collect();
    slopty_proto::thread::detail::ShellEdits {
        files,
        more: u32::try_from(u64_at(diff, "moreFiles").unwrap_or(0)).unwrap_or(u32::MAX),
        unavailable: bool_at(diff, "unavailable"),
        skipped: bool_at(diff, "skipped"),
        shared: bool_at(diff, "shared"),
        moves_tree: moves_tree(command),
    }
}

/// Whether `command` has a git step that can move the working tree: what changed while it ran
/// may be that step's, not edits.
fn moves_tree(command: &str) -> bool {
    const MOVES: [&str; 7] = ["checkout", "reset", "rebase", "switch", "stash", "merge", "pull"];
    command.split(['&', '|', ';', '\n', '(', ')']).any(|step| {
        let mut words = step.split_whitespace().skip_while(|w| w.contains('='));
        if words.next().is_none_or(|w| w != "git" && !w.ends_with("/git")) {
            return false;
        }
        // Past git's own options (`-C dir`, `-c key=value`, `--no-pager`) to its subcommand.
        let mut rest = words;
        while let Some(word) = rest.next() {
            if word == "-C" || word == "-c" {
                rest.next();
                continue;
            }
            if word.starts_with('-') {
                continue;
            }
            return MOVES.contains(&word);
        }
        false
    })
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

/// What Claude Code's notice says of tasks a session left unfinished when it ended, read by
/// the session that took it up again.
const PREVIOUS_SESSION_ENDED: &str = "before the previous session ended";

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
        event: tag(body, "event").map(|v| v.trim().to_owned()),
        at_ms: WallMs::ZERO,
        more_tasks: body
            .split("<task-id>")
            .skip(2)
            .filter_map(|rest| Some(rest.split_once("</task-id>")?.0.trim().to_owned()))
            .filter(|task| !task.is_empty())
            .collect(),
    })
}

/// The row a task's `notice` makes, its call `called` (its tool's name and detail) when known:
/// its kind, its words as Claude Code's TUI says them, and what opens under it.
fn task_notice(task: &str, notice: &Notice, called: Option<&(String, ToolDetail)>) -> TaskNotice {
    let summary = notice.summary.clone().unwrap_or_default();
    let kind = match called {
        Some((name, _)) if name == "Monitor" => "monitor",
        Some((_, ToolDetail::Bash(_))) => "shell",
        Some((_, ToolDetail::Agent(_))) => "agent",
        _ if summary.starts_with("Agent ") => "agent",
        _ if summary.starts_with("Background command") => "shell",
        _ if summary.starts_with("Monitor") => "monitor",
        _ if summary.starts_with("Remote task") => "remote",
        _ => "task",
    };
    let quoted = summary.split_once('"').and_then(|(_, rest)| rest.split_once('"')).map(|(q, _)| q);
    let event = notice.event.as_deref().map(str::trim).filter(|e| !e.is_empty());
    let expired = event.is_some_and(|e| e.contains("Monitor expired after"));
    let status = match notice.status.as_deref() {
        _ if notice.waits() => "waiting",
        _ if expired => "expired",
        _ if event.is_some() => "event",
        Some("failed") => "failed",
        Some("killed" | "stopped") => "killed",
        _ => "completed",
    };
    // A monitor's event reads by its first line, the monitor's description beside it.
    let words = match event {
        Some(event) if kind == "monitor" || summary.starts_with("Monitor event") => {
            let headline: String = event
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .unwrap_or_default()
                .chars()
                .take(500)
                .collect();
            format!("Monitor event: {headline}")
        }
        _ => summary.clone(),
    };
    let body = event.map(str::to_owned).or_else(|| notice.result.clone()).filter(|b| !b.is_empty());
    TaskNotice {
        task_id: task.to_owned(),
        call: notice.tool_use_id.clone().filter(|c| !c.is_empty()),
        kind: kind.to_owned(),
        status: status.to_owned(),
        title: quoted.unwrap_or_default().to_owned(),
        summary: words,
        body: body.map(|b| Clipped::head(&b, OUTPUT, None)),
        usage: notice.usage,
        exit_code: exit_code_in_summary(&summary),
    }
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

/// Whether `record` carries a tool's result.
fn has_tool_result(record: &Value) -> bool {
    record
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
        .is_some_and(|blocks| blocks.iter().any(|b| str_at(b, "type") == Some("tool_result")))
}

/// Whether an assistant record's `message` is the model's final answer: its request stopped
/// with nothing more to do (`end_turn`, a stop sequence, as Claude Code's own error records do,
/// or a refusal). A request that stopped to call a tool, to pause or at its token limit goes on.
fn answers_last(message: &Value) -> bool {
    matches!(str_at(message, "stop_reason"), Some("end_turn" | "stop_sequence" | "refusal"))
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

/// An MCP server's word that a resource or a polled tool changed, as the TUI's row says it:
/// "↻ docs: guide.md · re-indexed". A `file://` resource is named by its file, any other cut to
/// 40 characters.
fn mcp_update(text: &str) -> Option<String> {
    let attr = |head: &str, name: &str| -> Option<String> {
        let at = head.find(&format!("{name}=\""))?;
        let rest = head.get(at.saturating_add(name.len()).saturating_add(2)..)?;
        Some(rest.get(..rest.find('"')?)?.to_owned())
    };
    let mut rows = Vec::new();
    let mut rest = text;
    while let Some(start) = ["<mcp-resource-update", "<mcp-polling-update"]
        .iter()
        .filter_map(|open| rest.find(open))
        .min()
    {
        let from = rest.get(start..)?;
        let head = from.get(..from.find('>')?)?;
        let server = attr(head, "server").unwrap_or_default();
        let target = attr(head, "uri").or_else(|| attr(head, "tool")).unwrap_or_default();
        let target = match target.strip_prefix("file://") {
            Some(path) => path.rsplit('/').find(|p| !p.is_empty()).unwrap_or(path).to_owned(),
            None if target.chars().count() > 40 => {
                let mut cut: String = target.chars().take(39).collect();
                cut.push('…');
                cut
            }
            None => target,
        };
        let closes = from.find("</mcp-").unwrap_or(from.len());
        let body = from.get(..closes).unwrap_or(from);
        let reason = tag(body, "reason").map(str::trim).filter(|r| !r.is_empty());
        rows.push(match reason {
            Some(reason) => format!("↻ {server}: {target} · {reason}"),
            None => format!("↻ {server}: {target}"),
        });
        rest = from.get(closes.max(1)..).unwrap_or_default();
    }
    (!rows.is_empty()).then(|| rows.join("\n"))
}

/// A message another Claude Code session sent this one (`<cross-session-message from="…">`):
/// who sent it, then what it said.
fn peer_message(text: &str) -> Option<String> {
    let trimmed = text.trim();
    let open = trimmed.strip_prefix("<cross-session-message")?;
    let head = open.get(..open.find('>')?)?;
    let attr = |name: &str| -> Option<&str> {
        let at = head.find(&format!("{name}=\""))?;
        let rest = head.get(at.saturating_add(name.len()).saturating_add(2)..)?;
        rest.get(..rest.find('"')?)
    };
    let from = attr("from-name").or_else(|| attr("from")).unwrap_or("another session");
    let after = open.get(head.len().saturating_add(1)..).unwrap_or_default();
    let body = after.split_once("</cross-session-message>").map_or(after, |(body, _)| body).trim();
    Some(format!("Message from another session · {from}\n{body}").trim_end().to_owned())
}

/// The tags a slash command's record opens with (`command-message` first in older versions).
const COMMAND_TAGS: [&str; 2] = ["command-name", "command-message"];

/// The tag a shell line typed in the TUI (`!`) is recorded in.
const SHELL_TAGS: [&str; 1] = ["bash-input"];

/// The tags what a command Claude Code ran itself printed is recorded in.
const OUTPUT_TAGS: [&str; 4] =
    ["local-command-stdout", "local-command-stderr", "bash-stdout", "bash-stderr"];

/// Whether `text` opens with one of the tags `names`, as Claude Code writes its own records: a
/// prompt that only mentions one is the person's words.
fn leads(text: &str, names: &[&str]) -> bool {
    let text = text.trim_start();
    names.iter().any(|name| {
        text.strip_prefix('<')
            .and_then(|t| t.strip_prefix(name))
            .is_some_and(|t| t.starts_with('>'))
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

/// A message content's text: the string, or its text blocks joined, without the tools-used
/// manifest Claude Code opens a subagent's or a forked skill's result with for the model
/// ([`TOOLS_USED`]).
fn content_text(content: &Value) -> String {
    let text = joined_text(content);
    manifest(&text, None).unwrap_or(text)
}

/// Claude Code's manifest of the tools a part of the conversation called that its requests did
/// not name yet (`<tools-used-before-summary>Read, Bash</tools-used-before-summary>`, a line of
/// its own): it opens a subagent's or a forked skill's result and a compaction summary, and is
/// the model's, not the person's (2.1.304 on).
const TOOLS_USED: &str = "tools-used-before-summary";

/// A compaction summary as the TUI's transcript view shows it: the tools called before it said
/// in a line of their own, and nothing of an empty manifest.
fn summary_text(text: &str) -> String {
    manifest(text, Some("Tools called before this summary: ")).unwrap_or_else(|| text.to_owned())
}

/// `text` with each tools-used manifest line said after `say` when it names tools, or dropped,
/// and the `<system-reminder>` around it when it held nothing else; `None` when it holds none.
fn manifest(text: &str, say: Option<&str>) -> Option<String> {
    let open = format!("<{TOOLS_USED}>");
    if !text.contains(&open) {
        return None;
    }
    let close = format!("</{TOOLS_USED}>");
    // The tools a line names, when it is a manifest.
    let named = |line: &str| {
        let tools = line.trim().strip_prefix(open.as_str())?.strip_suffix(close.as_str())?;
        (!tools.contains('<')).then(|| tools.trim().to_owned())
    };
    // What a manifest becomes: its tools said, or nothing.
    let said = |tools: String| say.filter(|_| !tools.is_empty()).map(|say| format!("{say}{tools}"));
    let lines: Vec<&str> = text.split('\n').collect();
    let mut kept = Vec::with_capacity(lines.len());
    let mut at = 0_usize;
    while let Some(&line) = lines.get(at) {
        // A reminder that holds only a manifest is the manifest.
        if line.trim() == "<system-reminder>"
            && let Some(tools) = lines.get(at.saturating_add(1)).copied().and_then(named)
            && lines.get(at.saturating_add(2)).is_some_and(|l| l.trim() == "</system-reminder>")
        {
            kept.extend(said(tools));
            at = at.saturating_add(3);
            continue;
        }
        match named(line) {
            Some(tools) => kept.extend(said(tools)),
            None => kept.push(line.to_owned()),
        }
        at = at.saturating_add(1);
    }
    Some(kept.join("\n"))
}

/// The text of `content` as Claude Code wrote it, its text blocks joined.
fn joined_text(content: &Value) -> String {
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
pub(crate) fn parse_ms(stamp: &str) -> Option<WallMs> {
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

/// `at` as an RFC 3339 UTC stamp (`2026-09-27T03:15:25.849Z`), as Claude Code stamps its
/// records: the inverse of `parse_ms`.
#[must_use]
pub fn format_ms(at: WallMs) -> String {
    let ms = at.as_millis();
    let days = i64::try_from(ms / 86_400_000).unwrap_or(i64::MAX);
    let rest = ms % 86_400_000;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rest / 3_600_000,
        rest / 60_000 % 60,
        rest / 1_000 % 60,
        rest % 1_000,
    )
}

/// The proleptic Gregorian date `days` after 1970-01-01 (Howard Hinnant's `civil_from_days`).
#[expect(
    clippy::arithmetic_side_effects,
    reason = "every step stays far inside `i64` for any day a `WallMs` names"
)]
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days.saturating_add(719_468);
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 { shifted_month + 3 } else { shifted_month - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
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

fn part_text(record: &Value, part: &Part) -> Option<String> {
    let content = record.get("message").and_then(|m| m.get("content"));
    let blocks = || content.and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    let result = record.get("toolUseResult");
    match part {
        // A compaction summary reads as the decoder said it ([`summary_text`]).
        Part::Block { index: 0 } if bool_at(record, "isCompactSummary") => {
            Some(summary_text(&joined_text(content?)))
        }
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
        Part::Call { tool_use_id } => blocks()
            .iter()
            .find(|b| str_at(b, "id") == Some(tool_use_id.as_str()))
            .and_then(|b| b.get("input"))
            .map(input_json),
        Part::Input { tool_use_id, field } => blocks()
            .iter()
            .find(|b| str_at(b, "id") == Some(tool_use_id.as_str()))
            .and_then(|b| string_at(b.get("input")?, field)),
        Part::Result { tool_use_id } => blocks()
            .iter()
            .find(|b| str_at(b, "tool_use_id") == Some(tool_use_id.as_str()))
            .map(|b| content_text(b.get("content").unwrap_or(&Value::Null))),
        Part::Report => result?.get("content").map(content_text),
        // Not text of the transcript: a picture's bytes, a file the worker tails.
        Part::Image { .. } | Part::Output { .. } => None,
        Part::Record => serde_json::to_string_pretty(record).ok(),
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

/// A subagent's calls by kind and the lines its edits changed, from its result's `toolStats`
/// (`readCount`, `searchCount`, `bashCount`, `editFileCount`, `otherToolCount`, `linesAdded`,
/// `linesRemoved`): each count under the thread model's kind of call, a count it adds later
/// under its own word (`fooCount` as `foo`), and none that is zero.
fn tally(
    stats: &serde_json::Map<String, Value>,
) -> (BTreeMap<String, u64>, slopty_proto::thread::Changed) {
    use slopty_proto::thread::kind;
    let mut calls = BTreeMap::new();
    let mut changed = slopty_proto::thread::Changed::default();
    let lines = |n: u64| u32::try_from(n).unwrap_or(u32::MAX);
    for (key, n) in stats.iter().filter_map(|(key, n)| Some((key.as_str(), n.as_u64()?))) {
        let named = match key {
            "linesAdded" => {
                changed.added = lines(n);
                continue;
            }
            "linesRemoved" => {
                changed.removed = lines(n);
                continue;
            }
            "readCount" => kind::READ,
            "searchCount" => kind::SEARCH,
            "bashCount" => kind::EXEC,
            "editFileCount" => kind::EDIT,
            "otherToolCount" => kind::OTHER,
            other => match other.strip_suffix("Count") {
                Some(word) if !word.is_empty() => word,
                _ => continue,
            },
        };
        if n > 0 {
            let have = calls.entry(named.to_owned()).or_insert(0_u64);
            *have = have.saturating_add(n);
        }
    }
    (calls, changed)
}

/// A kind of subagent as the card names it: Claude Code's own name for it, a fork (which
/// inherits its parent's whole context) as "Fork".
#[must_use]
pub fn agent_kind(name: String) -> String {
    if name == "fork" { "Fork".to_owned() } else { name }
}

/// The words of a content block: a text block's text, a bare string as it is.
fn block_text(block: &Value) -> &str {
    block.as_str().or_else(|| str_at(block, "text")).unwrap_or_default()
}

/// The directive a fork's first message gives it, after Claude Code's words for a fork
/// (`<fork-boilerplate>`): "Your directive: …".
fn fork_directive(text: &str) -> Option<&str> {
    let (_, after) = text.split_once("</fork-boilerplate>")?;
    let after = after.trim();
    Some(after.strip_prefix("Your directive:").map_or(after, str::trim))
}

#[cfg(test)]
mod tests;

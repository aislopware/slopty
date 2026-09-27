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
//! `task-notification` that says a background command or agent finished. Everything else is
//! bookkeeping and is skipped.
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

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::transcript::Tail;

/// A clipping limit: whichever of the two is reached first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cap {
    /// Whole lines kept.
    pub lines: usize,
    /// Characters kept.
    pub chars: usize,
}

/// Prose: prompts, answers, thinking, plans, subagent reports, compaction summaries. Long enough
/// for any answer a person reads through, short of a pasted log.
pub const PROSE: Cap = Cap { lines: 400, chars: 32_000 };

/// Tool output and inputs: a Bash tail, a result's text, an unknown tool's input. The same
/// 40 lines or 4 000 characters the old conversation view settled on (`claude-code.md`).
pub const OUTPUT: Cap = Cap { lines: 40, chars: 4_000 };

/// Diff lines kept per call, across its hunks.
pub const PATCH_LINES: usize = 400;

/// Results and notices waiting for a call not seen yet; past this many, they are dropped (a tail
/// that started after the calls would otherwise hold them forever).
const PENDING_MAX: usize = 512;

/// Which conversation an entry belongs to.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ThreadId {
    /// The session's own conversation.
    Main,
    /// A subagent's, by its agent id.
    Agent(String),
}

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

/// Where the whole of a clipped text is in the transcript.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextRef {
    /// The record's `uuid`.
    pub record: String,
    /// Which part of it.
    pub part: Part,
}

/// A part of a record that can be long.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Part {
    /// The message's content block at `index` (a prompt, an answer, thinking, a summary). A
    /// content that is one string is block 0.
    Block {
        /// The block's position in `message.content`.
        index: u32,
    },
    /// A string field of a tool call's input.
    Input {
        /// The call.
        tool_use_id: String,
        /// The input field.
        field: String,
    },
    /// The text of a tool result the model saw.
    Result {
        /// The call.
        tool_use_id: String,
    },
    /// A Bash call's standard output (`toolUseResult.stdout`).
    Stdout,
    /// A Bash call's standard error.
    Stderr,
    /// An edit's whole diff (`toolUseResult.structuredPatch`), as unified hunks.
    Patch,
}

/// A text, cut to a cap when it is longer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Clipped {
    /// What is shown: all of it, or its head or tail.
    pub text: String,
    /// Lines in the whole text.
    pub lines: u32,
    /// Characters in the whole text.
    pub chars: u32,
    /// Where the whole text is, when `text` is not all of it.
    pub full: Option<TextRef>,
}

impl Clipped {
    /// The first lines of `text` within `cap`.
    #[must_use]
    pub fn head(text: &str, cap: Cap, full: Option<TextRef>) -> Self {
        Self::cut(text, cap, full, false)
    }

    /// The last lines of `text` within `cap`: what a log ended with.
    #[must_use]
    pub fn tail(text: &str, cap: Cap, full: Option<TextRef>) -> Self {
        Self::cut(text, cap, full, true)
    }

    /// Whether `text` is less than the whole.
    #[must_use]
    pub const fn is_clipped(&self) -> bool {
        self.full.is_some()
    }

    fn cut(text: &str, cap: Cap, full: Option<TextRef>, from_end: bool) -> Self {
        let lines = text.lines().count();
        let chars = text.chars().count();
        let count = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
        if lines <= cap.lines && chars <= cap.chars {
            return Self {
                text: text.to_owned(),
                lines: count(lines),
                chars: count(chars),
                full: None,
            };
        }
        let kept = if from_end { keep_tail(text, cap) } else { keep_head(text, cap) };
        Self { text: kept, lines: count(lines), chars: count(chars), full }
    }
}

fn keep_head(text: &str, cap: Cap) -> String {
    let mut out = String::new();
    let mut used = 0_usize;
    for line in text.split_inclusive('\n').take(cap.lines) {
        let n = line.chars().count();
        if used.saturating_add(n) > cap.chars {
            out.extend(line.chars().take(cap.chars.saturating_sub(used)));
            out.push('…');
            break;
        }
        out.push_str(line);
        used = used.saturating_add(n);
    }
    out.truncate(out.trim_end_matches('\n').len());
    out
}

fn keep_tail(text: &str, cap: Cap) -> String {
    let trimmed = text.trim_end_matches('\n');
    let mut kept: Vec<String> = Vec::new();
    let mut used = 0_usize;
    for line in trimmed.split('\n').rev().take(cap.lines) {
        let n = line.chars().count().saturating_add(1);
        if used.saturating_add(n) > cap.chars {
            let room = cap.chars.saturating_sub(used);
            let skip = line.chars().count().saturating_sub(room);
            kept.push(format!("…{}", line.chars().skip(skip).collect::<String>()));
            break;
        }
        kept.push(line.to_owned());
        used = used.saturating_add(n);
    }
    kept.reverse();
    kept.join("\n")
}

/// One thing in a conversation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Stable id: the `tool_use_id` of a tool call, else the record's `uuid` (with `:<block>`
    /// for an answer or thinking block).
    pub id: String,
    /// When the record was written, in ms since the Unix epoch; 0 when it says nothing.
    pub at_ms: u64,
    /// What it is.
    pub body: Body,
}

/// What an entry is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Body {
    /// Something the person sent.
    Prompt(Prompt),
    /// The assistant's answer text.
    Text(Clipped),
    /// The assistant's thinking (as Claude Code writes it: a summary, or nothing).
    Thinking(Clipped),
    /// A tool call, with its result once it has one.
    Tool(Box<ToolCall>),
    /// The conversation was compacted here.
    Compact(Compact),
    /// The person pressed Esc.
    Interrupted {
        /// It stopped a tool call rather than the model's answer.
        during_tool: bool,
    },
    /// A line from Claude Code itself.
    Note(Note),
}

/// A prompt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prompt {
    /// The words, or a command's arguments.
    pub text: Clipped,
    /// Images pasted with it.
    pub images: u32,
    /// A slash command (`/compact`), or `!` for a shell command typed in bash mode.
    pub command: Option<String>,
}

/// A compaction boundary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Compact {
    /// `manual` (`/compact`) or `auto`.
    pub trigger: Option<String>,
    /// Tokens in the context before.
    pub pre_tokens: Option<u64>,
    /// And after.
    pub post_tokens: Option<u64>,
    /// The summary the conversation continues from.
    pub summary: Option<Clipped>,
}

/// A note from Claude Code.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    /// What kind.
    pub kind: NoteKind,
    /// What it says.
    pub text: Clipped,
}

/// Kinds of [`Note`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NoteKind {
    /// The API failed or is being retried.
    ApiError,
    /// The output of a slash command or of a bash-mode command.
    Command,
    /// Something Claude Code wanted to say.
    Info,
}

/// A tool call.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    /// The tool's name as the model called it.
    pub name: String,
    /// What it was asked to do, and what came of it, typed by tool.
    pub detail: ToolDetail,
    /// The result, once it arrived.
    pub result: Option<ToolResult>,
}

/// How a call ended.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResult {
    /// Whether it worked.
    pub status: ResultStatus,
    /// The text the model saw, when the detail does not already carry the output, or on an
    /// error.
    pub text: Option<Clipped>,
    /// When the result was written.
    pub at_ms: u64,
}

/// How a call ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResultStatus {
    /// It ran.
    Ok,
    /// It failed, or a hook or the permission system denied it.
    Error,
    /// The person refused it, or pressed Esc while it ran.
    Rejected,
}

/// A call's detail, by tool.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolDetail {
    /// `Edit` and `MultiEdit`.
    Edit(EditDetail),
    /// `Write`.
    Write(WriteDetail),
    /// `Read`.
    Read(ReadDetail),
    /// `Grep`.
    Grep(GrepDetail),
    /// `Glob`.
    Glob(GlobDetail),
    /// `Bash`.
    Bash(BashDetail),
    /// `WebFetch`.
    WebFetch(WebFetchDetail),
    /// `WebSearch`.
    WebSearch(WebSearchDetail),
    /// `Agent` (`Task` in older versions): a subagent.
    Agent(AgentDetail),
    /// `TaskCreate`: a task added to the list.
    TaskCreate(TaskCreateDetail),
    /// `TaskUpdate`: a task changed.
    TaskUpdate(TaskUpdateDetail),
    /// `TodoWrite` (older versions): the whole list at once.
    TodoWrite {
        /// The list as written.
        todos: Vec<Task>,
    },
    /// `AskUserQuestion`.
    Question(QuestionDetail),
    /// `ExitPlanMode`: a plan for approval.
    Plan {
        /// The plan.
        plan: Clipped,
    },
    /// A tool an MCP server provides (`mcp__<server>__<tool>`).
    Mcp(McpDetail),
    /// Any other tool: its input, clipped.
    Other {
        /// The input as JSON.
        input: Clipped,
    },
}

/// A diff.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Patch {
    /// Hunks, as `git diff` would cut them.
    pub hunks: Vec<Hunk>,
    /// Lines added, over the whole diff.
    pub added: u32,
    /// Lines removed.
    pub removed: u32,
    /// Diff lines left out of `hunks` past [`PATCH_LINES`].
    pub clipped_lines: u32,
    /// Where the whole diff is, when lines were left out.
    pub full: Option<TextRef>,
}

/// One hunk of a diff.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hunk {
    /// First line in the old file.
    pub old_start: u32,
    /// Lines of the old file the hunk spans.
    pub old_lines: u32,
    /// First line in the new file.
    pub new_start: u32,
    /// Lines of the new file the hunk spans.
    pub new_lines: u32,
    /// The lines, each starting with ` `, `-` or `+` (or `\` for "no newline at end").
    pub lines: Vec<String>,
}

/// `Edit` / `MultiEdit`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditDetail {
    /// The file.
    pub path: String,
    /// Replacements asked for (`MultiEdit` asks several).
    pub edits: u32,
    /// Every occurrence was replaced.
    pub replace_all: bool,
    /// What changed, from the result; empty until it arrives.
    pub patch: Patch,
}

/// `Write`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteDetail {
    /// The file.
    pub path: String,
    /// Lines written.
    pub lines: u32,
    /// Whether the file was new.
    pub kind: WriteKind,
    /// For an overwrite, what changed.
    pub patch: Patch,
}

/// Whether a `Write` made a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WriteKind {
    /// Not known until the result arrives.
    Unknown,
    /// A new file.
    Create,
    /// An existing file replaced.
    Overwrite,
}

/// `Read`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadDetail {
    /// The file.
    pub path: String,
    /// The line it was asked to start at.
    pub offset: Option<u64>,
    /// How many lines it asked for.
    pub limit: Option<u64>,
    /// First line read, from the result.
    pub start_line: Option<u64>,
    /// Lines read.
    pub lines: Option<u64>,
    /// Lines in the file.
    pub total_lines: Option<u64>,
}

/// `Grep`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrepDetail {
    /// The pattern.
    pub pattern: String,
    /// Where it searched.
    pub path: Option<String>,
    /// The file filter.
    pub glob: Option<String>,
    /// `content`, `files_with_matches` or `count`.
    pub mode: Option<String>,
    /// Files that matched.
    pub files: Option<u64>,
    /// Matching lines (content mode).
    pub lines: Option<u64>,
}

/// `Glob`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlobDetail {
    /// The pattern.
    pub pattern: String,
    /// Where it looked.
    pub path: Option<String>,
    /// Files found.
    pub files: Option<u64>,
    /// More matched than were listed.
    pub truncated: bool,
}

/// `Bash`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BashDetail {
    /// The command.
    pub command: Clipped,
    /// What the model said it does.
    pub description: Option<String>,
    /// Asked to run in the background.
    pub background: bool,
    /// The background task's id, once it started.
    pub task_id: Option<String>,
    /// Where it stands.
    pub status: ShellStatus,
    /// Its exit code, when known.
    pub exit_code: Option<i32>,
    /// The end of its standard output.
    pub stdout: Option<Clipped>,
    /// The end of its standard error.
    pub stderr: Option<Clipped>,
    /// Where a background command's output goes; the worker tails it.
    pub output_file: Option<String>,
}

/// Where a shell command stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShellStatus {
    /// Running, or running in the background.
    Running,
    /// Exited 0.
    Done,
    /// Exited otherwise, or could not run.
    Failed,
    /// Stopped by Esc.
    Interrupted,
    /// A background command that was stopped.
    Killed,
}

/// `WebFetch`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebFetchDetail {
    /// The address.
    pub url: String,
    /// What it asked of the page.
    pub prompt: Option<String>,
    /// The HTTP status.
    pub code: Option<u64>,
    /// Bytes fetched.
    pub bytes: Option<u64>,
}

/// `WebSearch`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebSearchDetail {
    /// The query.
    pub query: String,
    /// Links found.
    pub results: Option<u64>,
}

/// A subagent (`Agent`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentDetail {
    /// Its id: [`ThreadId::Agent`] of its own thread. Known once it started.
    pub agent_id: Option<String>,
    /// `general-purpose`, `Explore`, a custom agent's name.
    pub agent_type: Option<String>,
    /// The short description the model gave it.
    pub description: Option<String>,
    /// Its brief.
    pub prompt: Clipped,
    /// It runs in the background.
    pub background: bool,
    /// Where it stands.
    pub status: AgentRun,
    /// What it reported back.
    pub report: Option<Clipped>,
    /// Tokens it used.
    pub tokens: Option<u64>,
    /// Tool calls it made.
    pub tool_uses: Option<u64>,
    /// How long it ran.
    pub duration_ms: Option<u64>,
}

/// Where a subagent stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentRun {
    /// Working.
    Running,
    /// Reported back.
    Completed,
    /// Failed.
    Failed,
    /// Stopped.
    Killed,
}

/// `TaskCreate`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskCreateDetail {
    /// The new task's id, from the result.
    pub task_id: Option<String>,
    /// Its title.
    pub subject: String,
    /// Its description.
    pub description: Option<String>,
}

/// `TaskUpdate`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskUpdateDetail {
    /// The task.
    pub task_id: String,
    /// Its status before, from the result.
    pub from: Option<String>,
    /// Its status after.
    pub to: Option<String>,
    /// A new title.
    pub subject: Option<String>,
    /// The fields the update changed, from the result.
    pub fields: Vec<String>,
}

/// One item of an agent's task list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    /// Its id (`TaskCreate`'s, or the position in a `TodoWrite` list).
    pub id: String,
    /// Its title.
    pub subject: String,
    /// `pending`, `in_progress`, `completed`.
    pub status: String,
}

/// `AskUserQuestion`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionDetail {
    /// What it asked.
    pub questions: Vec<Question>,
    /// What the person picked, once answered.
    pub answers: Vec<Answer>,
}

/// One question.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    /// The question.
    pub text: String,
    /// Its short label.
    pub header: Option<String>,
    /// The choices offered.
    pub options: Vec<String>,
    /// More than one may be picked.
    pub multi_select: bool,
}

/// One answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    /// The question it answers.
    pub question: String,
    /// The answer.
    pub answer: String,
}

/// A tool an MCP server provides.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpDetail {
    /// The server.
    pub server: String,
    /// The tool.
    pub tool: String,
    /// The input as JSON.
    pub input: Clipped,
}

/// What a subagent's thread knows of the call that started it (its file's `agent_metadata`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    /// The `Agent` call in the parent thread.
    pub tool_use_id: Option<String>,
    /// The agent's type.
    pub agent_type: Option<String>,
    /// The call's description.
    pub description: Option<String>,
}

/// One thread as it stands.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadState {
    /// Which.
    pub id: ThreadId,
    /// For a subagent, the call that started it.
    pub origin: Option<Origin>,
    /// Its entries, oldest first.
    pub entries: Vec<Entry>,
    /// Its task list.
    pub tasks: Vec<Task>,
}

/// What a read changed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Change {
    /// Replace the entry with this id in the thread, or append it when the thread has none.
    Upsert {
        /// The thread.
        thread: ThreadId,
        /// The entry.
        entry: Entry,
    },
    /// The entry is gone (its branch was abandoned).
    Remove {
        /// The thread.
        thread: ThreadId,
        /// The entry's id.
        id: String,
    },
    /// The thread's task list is now this.
    Tasks {
        /// The thread.
        thread: ThreadId,
        /// The whole list.
        tasks: Vec<Task>,
    },
    /// The file started over: drop what the thread held, or every thread when `None`.
    Reset {
        /// The thread, or all.
        thread: Option<ThreadId>,
    },
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
    Result { uuid: String, at_ms: u64, block: Value, result: Option<Value> },
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
}

/// The changes one read makes, an entry updated twice kept once at its first place.
#[derive(Default)]
struct Batch {
    changes: Vec<Change>,
    upserts: HashMap<(ThreadId, String), usize>,
    tasks: HashMap<ThreadId, usize>,
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
    pub fn ingest_jsonl(&mut self, thread: &ThreadId, jsonl: &str) -> Vec<Change> {
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
            })
            .collect()
    }

    fn record(&mut self, source: &ThreadId, record: &Value, batch: &mut Batch) {
        let kind = str_at(record, "type").unwrap_or_default();
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
        for entry in dropped {
            self.calls.remove(&entry.id);
            batch.remove(&thread, entry.id);
        }
        let at_ms = str_at(record, "timestamp").and_then(parse_ms).unwrap_or(0);
        let context = Ctx { thread: &thread, uuid, at_ms, made_at };
        match kind {
            "user" => self.user(&context, record, batch),
            "assistant" => self.assistant(&context, record, batch),
            "system" => self.system(&context, record, batch),
            "attachment" => {
                let attachment = record.get("attachment");
                let queued =
                    attachment.and_then(|a| str_at(a, "commandMode")) == Some("task-notification");
                if queued
                    && let Some(notice) =
                        attachment.and_then(|a| str_at(a, "prompt")).and_then(parse_notice)
                {
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
        let images = blocks.iter().filter(|b| str_at(b, "type") == Some("image")).count();
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
            } else if let Some(notice) = parse_notice(text) {
                self.notice(notice, batch);
                continue;
            } else if let Some(name) = tag(text, "command-name") {
                Body::Prompt(Prompt {
                    text: Clipped::head(
                        tag(text, "command-args").unwrap_or_default().trim(),
                        PROSE,
                        Some(reference()),
                    ),
                    images: 0,
                    command: Some(name.trim().to_owned()),
                })
            } else if let Some(input) = tag(text, "bash-input") {
                Body::Prompt(Prompt {
                    text: Clipped::head(input.trim(), PROSE, Some(reference())),
                    images: 0,
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
                })
            } else if text.trim_start().starts_with('<') || text.trim().is_empty() || prompt_made {
                // Text Claude Code injects for the model (reminders, caveats) is not the person's.
                continue;
            } else {
                prompt_made = true;
                Body::Prompt(Prompt {
                    text: Clipped::head(text, PROSE, Some(reference())),
                    images: to_u32(images),
                    command: None,
                })
            };
            let id = if index == 0 { ctx.uuid.to_owned() } else { format!("{}:{index}", ctx.uuid) };
            self.add(ctx, id, body, batch);
        }
    }

    fn assistant(&mut self, ctx: &Ctx<'_>, record: &Value, batch: &mut Batch) {
        let Some(content) = record.get("message").and_then(|m| m.get("content")) else { return };
        if bool_at(record, "isApiErrorMessage") {
            let text = content_text(content);
            let body = Body::Note(Note {
                kind: NoteKind::ApiError,
                text: Clipped::head(text.trim(), OUTPUT, None),
            });
            self.add(ctx, ctx.uuid.to_owned(), body, batch);
            return;
        }
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
                        .and_then(|e| str_at(e, "message").or_else(|| e.as_str()))
                        .unwrap_or("API error")
                        .to_owned()
                } else {
                    content.to_owned()
                };
                Body::Note(Note {
                    kind: NoteKind::ApiError,
                    text: Clipped::head(&text, OUTPUT, None),
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
                })
            }
            Some("informational") if !content.trim().is_empty() => Body::Note(Note {
                kind: NoteKind::Info,
                text: Clipped::head(content.trim(), OUTPUT, None),
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
        let detail = detail(name, input, id, ctx.uuid);
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
        call.result = Some(ToolResult { status, text, at_ms: ctx.at_ms });
        let changed_tasks = task_change(&call.detail, call.result.as_ref());
        let entry = entry.clone();
        batch.upsert(&thread, entry);
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
            }
            _ => return,
        }
        let entry = entry.clone();
        batch.upsert(&thread, entry);
    }
}

/// Where a record is being decoded.
#[derive(Clone, Copy)]
struct Ctx<'a> {
    thread: &'a ThreadId,
    uuid: &'a str,
    at_ms: u64,
    made_at: usize,
}

/// A call's detail from its input alone; the result fills in the rest.
fn detail(name: &str, input: &Value, id: &str, uuid: &str) -> ToolDetail {
    let text = |key: &str| string_at(input, key);
    let path = || text("file_path").or_else(|| text("notebook_path")).unwrap_or_default();
    let input_ref = |field: &str| TextRef {
        record: uuid.to_owned(),
        part: Part::Input { tool_use_id: id.to_owned(), field: field.to_owned() },
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
                Some(input_ref("command")),
            ),
            description: text("description"),
            background: bool_at(input, "run_in_background"),
            task_id: None,
            status: ShellStatus::Running,
            exit_code: None,
            stdout: None,
            stderr: None,
            output_file: None,
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
        }),
        "Agent" | "Task" => ToolDetail::Agent(AgentDetail {
            agent_id: None,
            agent_type: text("subagent_type"),
            description: text("description"),
            prompt: Clipped::head(
                str_at(input, "prompt").unwrap_or_default(),
                PROSE,
                Some(input_ref("prompt")),
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
                Some(input_ref("plan")),
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
                    .filter_map(|o| string_at(o, "label").or_else(|| o.as_str().map(str::to_owned)))
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
            search.results = field("results").and_then(Value::as_array).map(|results| {
                results
                    .iter()
                    .map(|r| r.get("content").and_then(Value::as_array).map_or(0, Vec::len))
                    .fold(0_u64, |sum, n| sum.saturating_add(to_u64(n)))
            });
            false
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

/// The diff in a result's `structuredPatch`.
fn patch(result: &Value, uuid: &str) -> Patch {
    let mut out = Patch::default();
    let Some(hunks) = result.get("structuredPatch").and_then(Value::as_array) else { return out };
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
                lines: shown,
            });
        }
    }
    if out.clipped_lines > 0 {
        out.full = Some(TextRef { record: uuid.to_owned(), part: Part::Patch });
    }
    out
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
    })
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
            blocks.iter().find_map(|b| str_at(b, "text"))
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

/// An RFC 3339 UTC stamp (`2026-09-27T03:15:25.849Z`) in ms since the Unix epoch.
fn parse_ms(stamp: &str) -> Option<u64> {
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
    u64::try_from(seconds.checked_mul(1_000)?.checked_add(millis)?).ok()
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
        Part::Stdout => result.and_then(|r| string_at(r, "stdout")),
        Part::Stderr => result.and_then(|r| string_at(r, "stderr")),
        Part::Patch => {
            let hunks = result?.get("structuredPatch")?.as_array()?;
            let mut out = String::new();
            for hunk in hunks {
                let n = |key: &str| u64_at(hunk, key).unwrap_or(0);
                let _written = writeln!(
                    out,
                    "@@ -{},{} +{},{} @@",
                    n("oldStart"),
                    n("oldLines"),
                    n("newStart"),
                    n("newLines")
                );
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

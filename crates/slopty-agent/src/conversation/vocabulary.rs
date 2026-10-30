//! What the worker decodes from Claude Code's transcript and hooks, before the thread model.
//!
//! The worker is the only place that reads Claude Code's transcript (the tolerant decoder in
//! `slopty-agent`), and `slopty_agent::claude::history` turns what it reads into the agent-neutral
//! thread model (`slopty_proto::thread`), which is what clients are sent. The types here are that
//! decoder's vocabulary, and the parts of it the thread model and the hooks share.
//!
//! **Entries.** A thread ([`ThreadId`]) is the session's own conversation or one subagent's.
//! Each [`Entry`] keeps its id across reads (a tool call's `tool_use_id`, else the record's
//! uuid), so a [`Change::Upsert`] replaces what came before; a result that arrives later
//! comes back as an upsert of the call it belongs to. Every text is clipped on the worker
//! ([`Clipped`]); a clipped one carries a [`TextRef`] to the whole of it.
//!
//! **Turns.** Beside its entries, a thread's turns carry what the transcript says of each one
//! and no entry shows: the models that answered, the tokens they read and wrote, the context
//! the last request carried, the permission mode, and when Claude Code closed it
//! ([`Change::Turn`], keyed by the prompt that opened the turn).
//!
//! **Background output.** A command Claude Code runs in the background writes to a file of its
//! own, not to the transcript. The worker tails that file while the command runs ([`Output`]);
//! how the command ended comes with its entry.
//!
//! **Images.** An entry names a picture ([`Image`]) by its digest and its size, never with its
//! bytes, which are fetched by [`Image::at`] when the picture comes into view.
//!
//! **Asks.** A dialog the TUI holds (a permission, a question, a plan) is answered in its
//! terminal; the hooks say it waits on the person (`crate::hooks`), and its call's entry says
//! how it went.

use serde::{Deserialize, Serialize};
use slopty_core::WallMs;
pub use slopty_proto::thread::detail::{EditDetail, Hunk, Patch, WriteDetail};

/// A clipping limit: whichever of the two is reached first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cap {
    /// Whole lines kept.
    pub lines: usize,
    /// Characters kept.
    pub chars: usize,
}

/// Which conversation an entry belongs to.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ThreadId {
    /// The session's own conversation.
    Main,
    /// A subagent's, by its agent id.
    Agent(String),
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
    /// An image: the message's content block at `index`, or, with `tool_use_id`, the block at
    /// `index` of that call's result (a result with no image block keeps its picture in
    /// `toolUseResult.file`, as block 0).
    Image {
        /// The call whose result holds it; `None` for a picture in a prompt.
        tool_use_id: Option<String>,
        /// The block's position.
        index: u32,
    },
    /// What a background command printed, read from the end of the file it writes (the
    /// record is the one that named the file).
    Output {
        /// The command's call.
        tool_use_id: String,
    },
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
    /// When the record was written; zero when it says nothing.
    pub at_ms: WallMs,
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
    /// A background task's notice.
    Task(Box<TaskNotice>),
    /// The person went back to an earlier prompt (an edited prompt, a rewind): what came after
    /// it on the old branch is gone from the thread, and the new branch goes on from here.
    Rewound {
        /// Entries the old branch had past the point it left.
        dropped: u32,
    },
}

/// A prompt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prompt {
    /// The words, or a command's arguments.
    pub text: Clipped,
    /// Images pasted with it.
    pub images: Vec<Image>,
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
    /// For an API error Claude Code retries: which attempt comes next, and when.
    pub retry: Option<Retry>,
    /// For an API error Claude Code gave up on, which ends its turn: why.
    pub stop: Option<Stop>,
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
    /// A hook failed or stopped the turn (the text is what it said).
    Hook,
    /// What a command printed as its errors, drawn apart from its output.
    CommandError,
    /// A quiet line that is neither a failure nor the person's words: a prompt's origin, an
    /// MCP server's update, a message from another session.
    Quiet,
}

/// The API error that ended a turn, as Claude Code records it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stop {
    /// Claude Code's kind of error ([`Stop::RATE_LIMIT`], `invalid_request`,
    /// `authentication_failed`, `server_error`, …); `unknown` when it names none.
    pub error: String,
}

impl Stop {
    /// A usage or rate limit refused the request.
    pub const RATE_LIMIT: &str = "rate_limit";

    /// Whether a limit refused the request.
    #[must_use]
    pub fn limited(&self) -> bool {
        self.error == Self::RATE_LIMIT
    }
}

/// Claude Code retrying a request the API refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Retry {
    /// The attempt that comes next, from 1.
    pub attempt: u32,
    /// Attempts it makes before it gives up.
    pub max: u32,
    /// How long it waits before that attempt, in ms.
    pub in_ms: u64,
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
    /// Its header as the TUI words it where that says more than its kind: "Read report.pdf ·
    /// pages 1-3", "Create notes.md".
    #[serde(default)]
    pub title: Option<String>,
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
    pub at_ms: WallMs,
    /// Images it returned: a screenshot, a picture read from a file.
    pub images: Vec<Image>,
    /// What came of it as the TUI's line under the call says it, from its structured result,
    /// where its detail does not already say it: "Read PDF (1.2MB)", "Did 3 searches in 4s".
    #[serde(default)]
    pub said: Option<String>,
}

/// Most bytes of an image a worker sends; a larger one is described and never sent.
pub const IMAGE_BYTES: usize = 8 * 1024 * 1024;

/// A picture in the conversation, described; its bytes are sent only when asked for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Image {
    /// BLAKE3 of its bytes, in hex: the same picture has the same digest wherever it shows.
    pub digest: String,
    /// `image/png`, `image/jpeg`, `image/gif` or `image/webp`.
    pub media_type: String,
    /// Its size in bytes.
    pub bytes: u64,
    /// Its width in pixels, as its header gives it; 0 when it does not.
    pub width: u32,
    /// Its height in pixels; 0 when its header does not give it.
    pub height: u32,
    /// Where it is in the transcript ([`Part::Image`]).
    pub at: TextRef,
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
    /// Slopty's own `show_visual` or `show_page` (`mcp__slopty__…`): the visual or page it
    /// shows, read from its whole input ([`crate::shown::shown`]); its paths as the call gave
    /// them, made absolute against the session's folder where the thread is mapped.
    Shown(Box<slopty_proto::thread::ToolDetail>),
    /// Any other tool: its input, clipped.
    Other {
        /// The input as JSON.
        input: Clipped,
    },
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
    /// When a background command finished, by Claude Code's notice of it.
    pub finished_ms: Option<WallMs>,
    /// The files it changed in its repository, as Claude Code took them (`bashEditDiff`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edits: Option<slopty_proto::thread::detail::ShellEdits>,
    /// What its exit code means, where Claude Code says (`returnCodeInterpretation`): "No
    /// matches found" for grep's 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interpretation: Option<String>,
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
    /// The first of them, as the search gave them.
    pub links: Vec<Link>,
}

/// A page a web search found.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    /// Its title.
    pub title: String,
    /// Its address.
    pub url: String,
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
    /// The name it was given, for an agent the others message by name (a teammate's).
    #[serde(default)]
    pub name: Option<String>,
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
    /// It stopped with background work of its own still running: what it said may be interim,
    /// and it may go on, and notify again, by itself.
    Waiting,
}

/// A background task's notice as a row of its own (`<task-notification>`): a subagent that
/// finished or waits on its own work, a command that exited, a monitor's event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskNotice {
    /// The task.
    pub task_id: String,
    /// The call that started it, when the notice names one (a monitor's events name none).
    pub call: Option<String>,
    /// What kind of task: `agent`, `shell`, `monitor`, `remote`, or `task` when the notice does
    /// not say.
    pub kind: String,
    /// `completed`, `failed`, `killed`, `waiting`, `event` or `expired`.
    pub status: String,
    /// The task's description, as its summary quotes it.
    pub title: String,
    /// The row's words, as the TUI words them.
    pub summary: String,
    /// What opens under the row: a monitor's event, an agent's result.
    pub body: Option<Clipped>,
    /// Tokens, tool uses and run time, when it says.
    pub usage: (Option<u64>, Option<u64>, Option<u64>),
    /// A command's exit code, when its summary gives one.
    pub exit_code: Option<i32>,
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
    pub options: Vec<Choice>,
    /// More than one may be picked.
    pub multi_select: bool,
}

/// One choice a question offers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    /// What it is called, and what the answer says when it is picked.
    pub label: String,
    /// What picking it means, when the model said.
    pub description: Option<String>,
    /// What it would look like (`options[].preview`): markdown or code to compare before
    /// picking, when the model gave one.
    #[serde(default)]
    pub preview: Option<String>,
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
    /// Its result's `structuredContent`, as JSON, once it came with one.
    #[serde(default)]
    pub structured: Option<slopty_proto::thread::Clipped>,
    /// The resources its result links or embeds (`resourceLinks`), in its order.
    #[serde(default)]
    pub links: Vec<slopty_proto::visual::ResourceLink>,
    /// Its result as MCP spells it (`content`, `structuredContent`, `_meta`), as JSON, for an
    /// MCP App's page ([`slopty_proto::visual::McpApp::result`]); `None` until it came.
    #[serde(default)]
    pub result: Option<slopty_proto::thread::Clipped>,
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
    /// What each of its turns took, oldest first.
    pub turns: Vec<Turn>,
}

/// What a turn took: from a prompt up to the next, the requests the model answered in it, as
/// the transcript records each one (its model and its usage).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Turn {
    /// The prompt entry that opened it; empty for work before a thread's first prompt.
    pub prompt: String,
    /// When the prompt was sent.
    pub started_ms: WallMs,
    /// When Claude Code closed the turn (its end-of-turn record); `None` while it runs, or
    /// when the transcript never says.
    pub ended_ms: Option<WallMs>,
    /// The models that answered in it, in the order they first did (`claude-opus-5-5`).
    pub models: Vec<String>,
    /// Tokens, summed over its requests.
    pub usage: Usage,
}

/// Tokens a request (or a turn's) used.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Input read fresh.
    pub input: u64,
    /// Input read from the prompt cache.
    pub cache_read: u64,
    /// Input written to the prompt cache.
    pub cache_write: u64,
    /// Output written, thinking included.
    pub output: u64,
    /// Of the output, thinking.
    pub thinking: u64,
}

impl Usage {
    /// `self` with `other` added.
    #[must_use]
    pub const fn plus(self, other: Self) -> Self {
        Self {
            input: self.input.saturating_add(other.input),
            cache_read: self.cache_read.saturating_add(other.cache_read),
            cache_write: self.cache_write.saturating_add(other.cache_write),
            output: self.output.saturating_add(other.output),
            thinking: self.thinking.saturating_add(other.thinking),
        }
    }

    /// `self` with `other` taken away.
    #[must_use]
    pub const fn minus(self, other: Self) -> Self {
        Self {
            input: self.input.saturating_sub(other.input),
            cache_read: self.cache_read.saturating_sub(other.cache_read),
            cache_write: self.cache_write.saturating_sub(other.cache_write),
            output: self.output.saturating_sub(other.output),
            thinking: self.thinking.saturating_sub(other.thinking),
        }
    }
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
    /// A turn's figures are now these: replace the thread's turn opened by the same prompt,
    /// or add it. A turn whose prompt is removed goes with it.
    Turn {
        /// The thread.
        thread: ThreadId,
        /// The turn.
        turn: Turn,
    },
    /// The prompts the person queued in the TUI behind the main thread's turn are now these, in
    /// the order they go.
    Queued(Vec<Queued>),
}

/// A prompt the person queued in the TUI, as its `queue-operation` record says it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Queued {
    /// What it says.
    pub text: String,
    /// When it was queued.
    pub at_ms: WallMs,
}

/// The end of what a background command has printed.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Output {
    /// The thread of the call that started it.
    pub thread: ThreadId,
    /// That call's `tool_use_id`.
    pub call: String,
    /// The last lines, colour codes taken out; `full` asks for more of it ([`Part::Output`]).
    pub tail: Clipped,
    /// Bytes it has written.
    pub bytes: u64,
}

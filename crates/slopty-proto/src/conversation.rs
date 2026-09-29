//! The conversation face of a coding agent's session: what a client that follows a session is
//! sent, and what it answers.
//!
//! The worker is the only place that reads Claude Code's transcript (the tolerant decoder in
//! `slopty-agent`); clients get the typed entries here. A client that follows a session
//! ([`ConversationRequest::Follow`]) is sent the conversation on a unidirectional stream of its
//! own that opens with [`crate::transfer::UniHead::Conversation`] and carries
//! [`ConversationEvent`]s at a priority below the terminals, the control stream and video, so a
//! long history never holds up an echo. Permission prompts are small and urgent: they go on the
//! control stream as [`crate::WorkerMsg::Permission`], to the followers only.
//!
//! **Entries.** A thread ([`ThreadId`]) is the session's own conversation or one subagent's.
//! Each [`Entry`] keeps its id across reads (a tool call's `tool_use_id`, else the record's
//! uuid), so a [`Change::Upsert`] replaces what a client holds; a result that arrives later
//! comes back as an upsert of the call it belongs to. Every text is clipped on the worker
//! ([`Clipped`]); a clipped one carries a [`TextRef`] that [`ConversationRequest::Expand`]
//! resolves.
//!
//! **Turns.** Beside its entries, a thread's turns carry what the transcript says of each one
//! and no entry shows: the models that answered, the tokens they read and wrote, the context
//! the last request carried, the permission mode, and when Claude Code closed it
//! ([`Change::Turn`], keyed by the prompt that opened the turn).
//!
//! **Live blocks.** Where Claude Code runs Slopty's mod, the worker also hears the answer,
//! thinking and tool input as the model writes them, before the transcript has them. They go
//! as [`ConversationEvent::Live`]: uncommitted text a client shows at the end of its thread,
//! cleared once the transcript settles it.
//!
//! **Background output.** A command Claude Code runs in the background writes to a file of its
//! own, not to the transcript. The worker tails that file while the command runs and sends its
//! end as [`ConversationEvent::Output`]; how the command ended comes with its entry.
//!
//! **Images.** An entry names a picture ([`Image`]) by its digest and its size, never with its
//! bytes. A client asks for the bytes when the picture comes into view
//! ([`ConversationRequest::Expand`] of its [`Image::at`]) and keeps them by digest, so a picture
//! that shows twice travels once.
//!
//! **The composer's menus.** The same stream carries the slash commands the agent takes
//! ([`ConversationEvent::Commands`]) and the answer to a follower's `@` search of the agent's
//! working directory ([`ConversationRequest::Search`], [`ConversationEvent::Found`]).

use serde::{Deserialize, Serialize};
use slopty_core::{ClientId, SessionId, WallMs};

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
    /// `toolUseResult.file`, as block 0). Its bytes come as [`ConversationEvent::Image`].
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
    /// Diff lines left out of `hunks` past the decoder's cap.
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
    /// When a background command finished, by Claude Code's notice of it.
    pub finished_ms: Option<WallMs>,
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
    /// Requests the model answered.
    pub requests: u32,
    /// Tokens, summed over its requests.
    pub usage: Usage,
    /// The context the last request carried: its input, cached or not.
    pub context_tokens: Option<u64>,
    /// The permission mode the prompt was sent in (`default`, `plan`, `acceptEdits`, …).
    pub mode: Option<String>,
    /// Why the model last stopped (`end_turn`, `tool_use`, `max_tokens`, `refusal`).
    pub stop: Option<String>,
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
    /// Everything the request read: the context it carried.
    #[must_use]
    pub const fn context(&self) -> u64 {
        self.input.saturating_add(self.cache_read).saturating_add(self.cache_write)
    }

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
}

/// What the face shows from Claude Code's status line.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Meters {
    /// The model's display name (`Opus`).
    pub model: Option<String>,
    /// Its id (`claude-opus-5-5`).
    pub model_id: Option<String>,
    /// Share of the context window in use, 0 to 100.
    pub context_used_pct: Option<f64>,
    /// The context window, in tokens.
    pub context_window: Option<u64>,
    /// The session's cost so far, in US dollars, as Claude Code estimates it.
    pub cost_usd: Option<f64>,
    /// The five-hour rate limit (subscribers only).
    pub five_hour: Option<RateWindow>,
    /// The seven-day rate limit.
    pub seven_day: Option<RateWindow>,
}

/// One rate-limit window.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct RateWindow {
    /// Share used, 0 to 100.
    pub used_pct: f64,
    /// When the window resets, in seconds since the Unix epoch.
    pub resets_at: Option<u64>,
}

/// Most characters of a text [`ConversationRequest::Expand`] sends; past it the text comes
/// clipped still, its head kept.
pub const EXPAND_CHARS: usize = 1_000_000;

/// Client → worker, about the conversation face.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ConversationRequest {
    /// Follow a session's conversation. The worker opens a conversation stream and sends the
    /// conversation as it stands, then every change as the transcript grows, and from now on
    /// sends this client the session's permission prompts, those already waiting first.
    /// Following a session already followed changes nothing.
    Follow {
        /// The terminal session the agent runs in.
        session: SessionId,
    },
    /// Stop following: the worker finishes the stream. When the last follower goes, the
    /// session's waiting prompts are released to the TUI's own dialog.
    Unfollow {
        /// The session.
        session: SessionId,
    },
    /// Answer a permission prompt. The first answer wins; one that comes after the prompt was
    /// answered or released, or from a client it was not shown to (one that neither follows the
    /// session nor answers [`Self::Approvals`]), is dropped.
    Answer {
        /// The session.
        session: SessionId,
        /// [`PermissionPrompt::ask`].
        ask: u64,
        /// The answer.
        verdict: Verdict,
    },
    /// Send the whole of a clipped text (one whose `full` is set), as
    /// [`ConversationEvent::Expanded`] on the session's conversation stream, or an image's
    /// bytes ([`Part::Image`]) as [`ConversationEvent::Image`]. Only while following.
    Expand {
        /// The session.
        session: SessionId,
        /// The thread the text is in.
        thread: ThreadId,
        /// Where the whole text is.
        reference: TextRef,
    },
    /// Files and folders under the agent's working directory that `query` matches, for an
    /// `@` mention; answered as [`ConversationEvent::Found`] on the session's conversation
    /// stream. Only while following.
    Search {
        /// The session.
        session: SessionId,
        /// What follows the `@`; empty lists the top of the tree.
        query: String,
        /// Paths wanted at most.
        limit: u32,
    },
    /// Answer permission prompts from outside the conversation (a notification, the inbox), or
    /// stop. While on, the worker holds a yes-or-no prompt of any session for a bounded time
    /// even when nobody follows it, and sends it here as it sends a follower's. The last
    /// client to stop hands such prompts back to the TUI.
    Approvals {
        /// Answer them from now on.
        on: bool,
    },
    /// Hand a held prompt back to the TUI now, undecided: the person is at the terminal and
    /// answers there. Taken as an answer is: only from a client the prompt was shown to, and
    /// only while it is still held.
    Release {
        /// The session.
        session: SessionId,
        /// [`PermissionPrompt::ask`].
        ask: u64,
    },
}

/// Worker → client on a conversation stream.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum ConversationEvent {
    /// Changes to apply in order. The stream's first frame starts with
    /// [`Change::Reset`] for every thread, and so does the first after the agent moved on to
    /// another transcript (`/clear`, `/resume`): what follows it up to
    /// [`ConversationEvent::Current`] is the conversation as it stands.
    Changes(Vec<Change>),
    /// The conversation as it stood has been sent in full; what comes after is live.
    Current,
    /// The status line's meters, when they change, and first when following begins.
    Meters(Meters),
    /// The answer to [`ConversationRequest::Expand`].
    Expanded {
        /// The thread asked about.
        thread: ThreadId,
        /// The text asked for.
        reference: TextRef,
        /// The whole text, clipped at [`EXPAND_CHARS`]; `None` when the transcript no longer
        /// has it.
        text: Option<Clipped>,
    },
    /// Blocks the model is writing now, ahead of the transcript, in order. Only where the
    /// agent runs Slopty's Claude Code mod, and only after [`ConversationEvent::Current`].
    Live(Vec<Live>),
    /// What background commands have printed since they were last sent, each as the end of
    /// its output now. Sent after [`ConversationEvent::Current`], and all again after a
    /// [`Change::Reset`] of every thread.
    Output(Vec<Output>),
    /// The answer to [`ConversationRequest::Expand`] for an image.
    Image {
        /// The thread asked about.
        thread: ThreadId,
        /// The image asked for.
        reference: TextRef,
        /// Its bytes; `None` when the transcript no longer has it or it is larger than
        /// [`IMAGE_BYTES`].
        blob: Option<Blob>,
    },
    /// The slash commands the agent takes, the whole list: sent after
    /// [`ConversationEvent::Current`] and again whenever it changes.
    Commands(Vec<SlashCommand>),
    /// The answer to [`ConversationRequest::Search`].
    Found {
        /// The query answered, so a stale answer can be told from the current one.
        query: String,
        /// Paths relative to the agent's working directory, best first; a directory ends in
        /// `/`.
        paths: Vec<String>,
    },
}

/// A slash command the agent takes, for the composer's menu.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SlashCommand {
    /// Its name without the slash: `compact`, `frontend:component`, `cloudflare:build-agent`.
    pub name: String,
    /// What it does, one line.
    pub description: String,
    /// What its argument is, as Claude Code hints it (`<path>`, `[model]`).
    pub argument_hint: Option<String>,
    /// Where it comes from.
    pub source: CommandSource,
}

/// Where a [`SlashCommand`] comes from.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum CommandSource {
    /// Claude Code's own.
    BuiltIn,
    /// The person's: `~/.claude/commands` or `~/.claude/skills`.
    Personal,
    /// The project's: `.claude/commands` or `.claude/skills` in the agent's directory or one
    /// above it.
    Project,
    /// An enabled plugin's command or skill, named `<plugin>:<name>`.
    Plugin,
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

/// An image's bytes, named by their digest.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Blob {
    /// BLAKE3 of `data`, in hex, as [`Image::digest`].
    pub digest: String,
    /// The encoded picture, as its [`Image::media_type`] says.
    #[serde(with = "serde_bytes")]
    pub data: Vec<u8>,
}

/// A block the model is writing, not yet in the transcript.
///
/// It is the answer, thinking or a tool call's input as it streams, and never an entry. A
/// client shows live blocks after its thread's last entry, in [`LiveId`] order, until
/// [`Live::Clear`]. That comes once the transcript has the block (and its [`Change`] went
/// first), or when the block was abandoned.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Live {
    /// A new block, empty.
    Start {
        /// Its thread.
        thread: ThreadId,
        /// Which block.
        id: LiveId,
        /// What it is.
        kind: LiveKind,
    },
    /// More of a started block: `text` goes on its end.
    Append {
        /// Which block.
        id: LiveId,
        /// The next piece.
        text: String,
    },
    /// Drop the block.
    Clear {
        /// Which block.
        id: LiveId,
    },
}

/// Where a live block is: its turn, the model request within the turn and the content block
/// within the answer. Unique within a session, subagents' turns included.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct LiveId {
    /// Claude Code's id for the turn.
    pub turn: String,
    /// The model request within the turn, from 0.
    pub step: u32,
    /// The content block within the answer, from 0.
    pub block: u32,
}

/// What a live block is.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum LiveKind {
    /// Answer text.
    Text,
    /// Thinking.
    Thinking,
    /// A tool call; its text is the input JSON as the model writes it.
    Tool {
        /// Its `tool_use_id`: the transcript entry that settles it has this id.
        id: String,
        /// The tool.
        name: String,
    },
}

/// Worker → client on the control stream: a followed session's permission prompts.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum PermissionEvent {
    /// Claude Code asks, and the worker holds the question for the session's followers.
    Asked(Box<PermissionPrompt>),
    /// The prompt is no longer waiting.
    Settled {
        /// The session.
        session: SessionId,
        /// [`PermissionPrompt::ask`].
        ask: u64,
        /// How it ended.
        outcome: Settled,
    },
}

impl PermissionEvent {
    /// The session whose prompt this is.
    #[must_use]
    pub const fn session(&self) -> SessionId {
        match self {
            Self::Asked(prompt) => prompt.session,
            Self::Settled { session, .. } => *session,
        }
    }

    /// The prompt's [`PermissionPrompt::ask`].
    #[must_use]
    pub const fn ask(&self) -> u64 {
        match self {
            Self::Asked(prompt) => prompt.ask,
            Self::Settled { ask, .. } => *ask,
        }
    }
}

/// A permission Claude Code asks for before running a tool, held while a client follows the
/// session or, for a yes-or-no prompt, while one answers [`ConversationRequest::Approvals`].
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PermissionPrompt {
    /// The terminal session the agent runs in.
    pub session: SessionId,
    /// Names the prompt in answers, unique on the worker.
    pub ask: u64,
    /// The tool's name as the model called it.
    pub tool: String,
    /// What the call would do, as a conversation entry shows a call (an edit's proposed change
    /// as a patch).
    pub detail: ToolDetail,
    /// What "allow always" grants: the permission updates Claude Code suggested.
    pub suggestions: Vec<Suggestion>,
    /// The session's permission mode (`default`, `plan`, `acceptEdits`, …).
    pub mode: Option<String>,
    /// When Claude Code asked, by the worker's clock.
    pub asked_ms: WallMs,
    /// When the worker gives up holding it and the TUI's dialog shows instead, on that clock.
    pub until_ms: WallMs,
}

/// One permission update "allow always" applies.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Suggestion {
    /// What it grants.
    pub grant: Grant,
    /// Where Claude Code keeps it: `session`, `localSettings`, `projectSettings`,
    /// `userSettings`.
    pub destination: Option<String>,
}

/// What a [`Suggestion`] grants.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Grant {
    /// Permission rules (`Bash(npm test:*)`), added or replacing the ones there.
    Rules {
        /// `allow`, `deny` or `ask`.
        behavior: String,
        /// The rules, as Claude Code writes them in settings.
        rules: Vec<String>,
    },
    /// A permission mode for the session (`acceptEdits`).
    Mode {
        /// The mode.
        mode: String,
    },
    /// Directories the agent may work in.
    Directories {
        /// Absolute paths.
        directories: Vec<String>,
    },
    /// An update of a kind this worker does not know; applied as Claude Code gave it.
    Other {
        /// Its `type`.
        kind: String,
    },
}

/// An answer to a permission prompt.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Verdict {
    /// Allow this call.
    Allow,
    /// Allow it and apply every [`PermissionPrompt::suggestions`].
    AllowAlways,
    /// Refuse the call.
    Deny {
        /// Why, for the model; the worker words one when this is empty.
        message: String,
        /// Also stop the turn.
        interrupt: bool,
    },
    /// Answer an `AskUserQuestion`: the call runs with these answers, as if the person had
    /// picked them in the TUI's own dialog.
    Answer {
        /// One per question answered, by the question's text; the options of a multi-select
        /// question joined with `", "`, or the words typed instead of an option.
        answers: Vec<Answer>,
    },
}

/// How a permission prompt stopped waiting.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Settled {
    /// A follower answered.
    Answered {
        /// The answer Claude Code was given.
        verdict: Verdict,
        /// Who gave it.
        by: ClientId,
    },
    /// Handed back undecided: the TUI shows its own dialog now. The last follower or approver
    /// left, a client released it ([`ConversationRequest::Release`]), or the worker held it
    /// as long as it may.
    Released,
    /// Claude Code stopped waiting (the turn was interrupted, the agent quit).
    Withdrawn,
}

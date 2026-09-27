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

use serde::{Deserialize, Serialize};
use slopty_core::{ClientId, SessionId};

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
    /// answered or released, or from a client that does not follow the session, is dropped.
    Answer {
        /// The session.
        session: SessionId,
        /// [`PermissionPrompt::ask`].
        ask: u64,
        /// The answer.
        verdict: Verdict,
    },
    /// Send the whole of a clipped text (one whose `full` is set), as
    /// [`ConversationEvent::Expanded`] on the session's conversation stream. Only while
    /// following.
    Expand {
        /// The session.
        session: SessionId,
        /// The thread the text is in.
        thread: ThreadId,
        /// Where the whole text is.
        reference: TextRef,
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
}

/// A permission Claude Code asks for before running a tool, held while a client follows the
/// session.
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
    /// When Claude Code asked, in ms since the Unix epoch by the worker's clock.
    pub asked_ms: u64,
    /// When the worker gives up holding it and the TUI's dialog shows instead, on that clock.
    pub until_ms: u64,
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
    /// Handed back undecided: the TUI shows its own dialog now. The last follower left, or
    /// the worker held it as long as it may.
    Released,
    /// Claude Code stopped waiting (the turn was interrupted, the agent quit).
    Withdrawn,
}

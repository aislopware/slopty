//! The bodies of a thread's items: clipped text, pictures, diffs, and the typed detail of a
//! tool call by its kind.
//!
//! Every text is clipped on the worker; a clipped one names where its whole is
//! ([`ContentRef`]), which [`super::wire::ThreadRequest::Expand`] resolves. Pictures travel as
//! their digest and size, and their bytes only when asked for.

use serde::{Deserialize, Serialize};

/// Where the whole of a clipped text or a picture is, in words only the worker's adapter
/// reads (a transcript record and part, a Codex item, a file).
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct ContentRef(pub String);

/// A clipping limit: whichever of the two is reached first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Clip {
    /// Whole lines kept.
    pub lines: usize,
    /// Characters kept.
    pub chars: usize,
}

/// A text, cut to a cap when it is longer.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Clipped {
    /// What is shown: all of it, or its head or tail.
    pub text: String,
    /// Lines in the whole text, as [`str::lines`] counts them.
    pub lines: u32,
    /// Characters in the whole text.
    pub chars: u32,
    /// Where the whole text is, when `text` is not all of it.
    pub full: Option<ContentRef>,
}

impl Clipped {
    /// All of `text`.
    #[must_use]
    pub fn whole(text: &str) -> Self {
        Self {
            text: text.to_owned(),
            lines: count(text.lines().count()),
            chars: count(text.chars().count()),
            full: None,
        }
    }

    /// The first lines of `text` within `clip`.
    #[must_use]
    pub fn head(text: &str, clip: Clip, full: Option<ContentRef>) -> Self {
        Self::cut(text, clip, full, false)
    }

    /// The last lines of `text` within `clip`: what a log ended with.
    #[must_use]
    pub fn tail(text: &str, clip: Clip, full: Option<ContentRef>) -> Self {
        Self::cut(text, clip, full, true)
    }

    /// Whether `text` is less than the whole.
    #[must_use]
    pub const fn is_clipped(&self) -> bool {
        self.full.is_some()
    }

    /// `more` at the end, counted as if the two had come as one.
    pub fn append(&mut self, more: &str) {
        if more.is_empty() {
            return;
        }
        let joins = !self.text.is_empty() && !self.text.ends_with('\n');
        let added = count(more.lines().count()).saturating_sub(u32::from(joins));
        self.lines = self.lines.saturating_add(added);
        self.chars = self.chars.saturating_add(count(more.chars().count()));
        self.text.push_str(more);
    }

    fn cut(text: &str, clip: Clip, full: Option<ContentRef>, from_end: bool) -> Self {
        let lines = text.lines().count();
        let chars = text.chars().count();
        if lines <= clip.lines && chars <= clip.chars {
            return Self::whole(text);
        }
        let kept = if from_end { keep_tail(text, clip) } else { keep_head(text, clip) };
        Self { text: kept, lines: count(lines), chars: count(chars), full }
    }
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn keep_head(text: &str, clip: Clip) -> String {
    let mut out = String::new();
    let mut used = 0_usize;
    for line in text.split_inclusive('\n').take(clip.lines) {
        let n = line.chars().count();
        if used.saturating_add(n) > clip.chars {
            out.extend(line.chars().take(clip.chars.saturating_sub(used)));
            out.push('…');
            break;
        }
        out.push_str(line);
        used = used.saturating_add(n);
    }
    out.truncate(out.trim_end_matches('\n').len());
    out
}

fn keep_tail(text: &str, clip: Clip) -> String {
    let trimmed = text.trim_end_matches('\n');
    let mut kept: Vec<String> = Vec::new();
    let mut used = 0_usize;
    for line in trimmed.split('\n').rev().take(clip.lines) {
        let n = line.chars().count().saturating_add(1);
        if used.saturating_add(n) > clip.chars {
            let room = clip.chars.saturating_sub(used);
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

/// A picture, described; its bytes are sent only when asked for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Image {
    /// BLAKE3 of its bytes, in hex: the same picture has the same digest wherever it shows.
    pub digest: String,
    /// Its media type (`image/png`).
    pub media_type: String,
    /// Its size in bytes.
    pub bytes: u64,
    /// Its width in pixels; 0 when its header does not say.
    pub width: u32,
    /// Its height in pixels; 0 when its header does not say.
    pub height: u32,
    /// Where its bytes are.
    pub at: ContentRef,
}

/// A diff.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Patch {
    /// Hunks, as `git diff` cuts them.
    pub hunks: Vec<Hunk>,
    /// Lines added, over the whole diff.
    pub added: u32,
    /// Lines removed.
    pub removed: u32,
    /// Diff lines left out of `hunks` past the cap.
    pub clipped_lines: u32,
    /// Where the whole diff is, when lines were left out.
    pub full: Option<ContentRef>,
}

/// One hunk of a diff.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hunk {
    /// First line in the old file.
    pub old_start: u32,
    /// Lines of the old file it covers.
    pub old_lines: u32,
    /// First line in the new file.
    pub new_start: u32,
    /// Lines of the new file it covers.
    pub new_lines: u32,
    /// Its lines, each with its ` `, `-` or `+`.
    pub lines: Vec<String>,
}

/// A tool call's typed body, by the kind of call it is rather than by any agent's tool names:
/// Claude Code's `Bash`, Codex's `commandExecution` and pi's `bash` are all [`Self::Exec`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolDetail {
    /// An edit in place.
    Edit(EditDetail),
    /// A whole file written.
    Write(WriteDetail),
    /// A file read.
    Read(ReadDetail),
    /// A search of files or their contents.
    Search(SearchDetail),
    /// A command run.
    Exec(ExecDetail),
    /// A URL fetched.
    Fetch(FetchDetail),
    /// A web search.
    WebSearch(WebSearchDetail),
    /// A subagent started; its thread is [`super::ToolCall::child`].
    Agent(AgentDetail),
    /// Questions for the person, and their answers.
    Question(QuestionDetail),
    /// A plan proposed.
    Plan {
        /// The plan.
        text: Clipped,
    },
    /// The task list written or changed.
    Tasks {
        /// The steps written, the whole list when `whole`.
        steps: Vec<super::Step>,
        /// The whole list rather than a change to it.
        whole: bool,
    },
    /// An MCP server's tool.
    Mcp(McpDetail),
}

/// An edit in place.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditDetail {
    /// The file.
    pub path: String,
    /// Edits made in one call.
    pub edits: u32,
    /// Every match was replaced.
    pub replace_all: bool,
    /// The diff.
    pub patch: Patch,
}

/// A whole file written.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteDetail {
    /// The file.
    pub path: String,
    /// Lines written.
    pub lines: u32,
    /// Whether it made the file, when known.
    pub created: Option<bool>,
    /// The diff against what was there.
    pub patch: Patch,
}

/// A file read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadDetail {
    /// The file.
    pub path: String,
    /// The first line asked for.
    pub offset: Option<u64>,
    /// Lines asked for.
    pub limit: Option<u64>,
    /// Lines read.
    pub lines: Option<u64>,
    /// Lines in the file.
    pub total_lines: Option<u64>,
}

/// A search.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchDetail {
    /// What was looked for: a pattern, a glob.
    pub pattern: String,
    /// Where.
    pub path: Option<String>,
    /// Which files, by glob.
    pub glob: Option<String>,
    /// Files matched.
    pub files: Option<u64>,
    /// Lines matched.
    pub matches: Option<u64>,
    /// The agent cut the results short.
    pub truncated: bool,
}

/// A command run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecDetail {
    /// The command.
    pub command: Clipped,
    /// What it is for, in the agent's words.
    pub description: Option<String>,
    /// Where it ran.
    pub cwd: Option<String>,
    /// It runs on in the background, past its call.
    pub background: bool,
    /// The background task it became.
    pub task: Option<String>,
    /// How it stands.
    pub status: ExecStatus,
    /// Its exit code.
    pub exit_code: Option<i32>,
    /// Its standard error, apart from the call's output, when the agent keeps them apart.
    pub stderr: Option<Clipped>,
    /// How long it ran.
    pub duration_ms: Option<u64>,
}

/// How a command stands. A background command outlives its call, so this is apart from the
/// call's own state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecStatus {
    /// It runs.
    Running,
    /// It ended well.
    Done,
    /// It ended badly.
    Failed,
    /// It was stopped.
    Interrupted,
}

/// A URL fetched.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchDetail {
    /// The URL.
    pub url: String,
    /// What the agent asked of the page.
    pub prompt: Option<String>,
    /// The HTTP status.
    pub code: Option<u64>,
    /// Bytes fetched.
    pub bytes: Option<u64>,
}

/// A web search.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebSearchDetail {
    /// The query.
    pub query: String,
    /// Results found.
    pub results: Option<u64>,
    /// The links it came back with.
    pub links: Vec<WebLink>,
}

/// A link a search found.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebLink {
    /// Its title.
    pub title: String,
    /// Where.
    pub url: String,
}

/// A subagent started.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentDetail {
    /// Its kind, as the agent names it (`general-purpose`).
    pub agent_type: Option<String>,
    /// What it is for, in a few words.
    pub description: Option<String>,
    /// What it was told.
    pub prompt: Clipped,
    /// It runs on in the background.
    pub background: bool,
    /// What it came back with.
    pub report: Option<Clipped>,
    /// Tokens it took.
    pub tokens: Option<u64>,
    /// Tool calls it made.
    pub tool_uses: Option<u64>,
    /// How long it ran.
    pub duration_ms: Option<u64>,
}

/// Questions for the person.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionDetail {
    /// The questions.
    pub questions: Vec<Question>,
    /// The answers given, once they are.
    pub answers: Vec<Answer>,
}

/// A question.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    /// What it asks.
    pub text: String,
    /// A short heading.
    pub header: Option<String>,
    /// The answers it offers.
    pub options: Vec<Offered>,
    /// More than one may be taken.
    pub multi_select: bool,
}

/// An answer a question offers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offered {
    /// What it says.
    pub label: String,
    /// What it means.
    pub description: Option<String>,
}

/// A question's answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    /// The question.
    pub question: String,
    /// The answer.
    pub answer: String,
}

impl Answer {
    /// How several picks of one question, and the words of one's own, make one answer: in the
    /// order given, one's own words last, as Claude Code's own dialog joins them.
    pub const JOIN: &'static str = ", ";

    /// The choice of an [`Intent::Answer`](super::wire::Intent::Answer) that answers
    /// `questions` with `answers`: the words alone for one question that offers nothing,
    /// otherwise the answers as a JSON list, each keyed by its question's text.
    #[must_use]
    pub fn choice(questions: &[Question], answers: &[Self]) -> String {
        match (questions, answers) {
            ([question], [answer]) if question.options.is_empty() => answer.answer.clone(),
            _ => serde_json::to_string(answers).unwrap_or_default(),
        }
    }

    /// The answers a [`choice`](Self::choice) gives `questions`, one per question in their
    /// order; `None` when it does not answer each of them, or answers one not asked.
    #[must_use]
    pub fn read(questions: &[Question], choice: &str) -> Option<Vec<Self>> {
        if let [question] = questions
            && question.options.is_empty()
        {
            return Some(vec![Self { question: question.text.clone(), answer: choice.to_owned() }]);
        }
        let given: Vec<Self> = serde_json::from_str(choice).ok()?;
        if given.iter().any(|a| !questions.iter().any(|q| q.text == a.question)) {
            return None;
        }
        questions.iter().map(|q| given.iter().find(|a| a.question == q.text).cloned()).collect()
    }

    /// The picks of `question` this answer holds, then the words of one's own when there are
    /// any: the labels it offers that the answer starts with, in order, and what follows them.
    #[must_use]
    pub fn parts(&self, question: &Question) -> Vec<String> {
        let mut parts = Vec::new();
        let mut rest = self.answer.as_str();
        while !rest.is_empty() {
            let labels = || question.options.iter().map(|o| o.label.as_str());
            // A label that is the whole of what is left, else the longest one a join follows.
            let picked = labels().find(|label| rest == *label).or_else(|| {
                labels()
                    .filter(|label| {
                        rest.strip_prefix(label).is_some_and(|after| after.starts_with(Self::JOIN))
                    })
                    .max_by_key(|label| label.len())
            });
            let Some(label) = picked else { break };
            parts.push(label.to_owned());
            rest = rest.get(label.len()..).unwrap_or_default();
            rest = rest.strip_prefix(Self::JOIN).unwrap_or(rest);
        }
        if !rest.is_empty() {
            parts.push(rest.to_owned());
        }
        parts
    }
}

/// An MCP server's tool.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpDetail {
    /// The server.
    pub server: String,
    /// The tool.
    pub tool: String,
}

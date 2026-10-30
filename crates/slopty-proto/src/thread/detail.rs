//! The bodies of a thread's items: clipped text, pictures, diffs, and the typed detail of a
//! tool call by its kind.
//!
//! Every text is clipped on the worker; a clipped one names where its whole is
//! ([`ContentRef`]), which [`super::wire::ThreadRequest::Expand`] resolves. Pictures travel as
//! their digest and size, and their bytes only when asked for.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Where the whole of a clipped text or a picture is.
///
/// In words only the worker's adapter reads (a transcript record and part, a Codex item, a
/// file), or a blob of the thread's repository ([`ContentRef::blob`]).
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct ContentRef(pub String);

impl ContentRef {
    /// How a blob is named: `blob:` and its object id.
    const BLOB: &'static str = "blob:";
    /// How an item's asset is named: `item-asset:`, the item's id, `/`, the asset's name.
    const ITEM_ASSET: &'static str = "item-asset:";

    /// Asset `name` of item `item`, read whole by [`super::wire::ThreadRequest::Expand`] as
    /// bytes: for a visual ([`crate::visual::VisualDetail`]) the file its `assets` map names
    /// `name`; for a page from a file ([`crate::visual::PageSource::File`]) `name` as a path
    /// under the page file's folder. Anything else, or a file past the cap the worker keeps,
    /// is answered [`super::wire::Expanded::Gone`].
    #[must_use]
    pub fn item_asset(item: &str, name: &str) -> Self {
        Self(format!("{}{item}/{name}", Self::ITEM_ASSET))
    }

    /// The item and the asset name this names, when it names an item's asset
    /// ([`Self::item_asset`]). An item's id holds no `/`; the name may.
    #[must_use]
    pub fn item_asset_of(&self) -> Option<(&str, &str)> {
        let rest = self.0.strip_prefix(Self::ITEM_ASSET)?;
        let (item, name) = rest.split_once('/')?;
        (!item.is_empty() && !name.is_empty()).then_some((item, name))
    }

    /// The blob `id` of the thread's repository, a side of a file a review showed
    /// ([`FileDiff::from`](super::wire::FileDiff::from),
    /// [`FileDiff::to`](super::wire::FileDiff::to)): the worker's snapshots read it from git,
    /// whole, for the unchanged lines between the file's hunks.
    #[must_use]
    pub fn blob(id: &str) -> Self {
        Self(format!("{}{id}", Self::BLOB))
    }

    /// The object id this names, when it names a blob ([`Self::blob`]).
    #[must_use]
    pub fn blob_id(&self) -> Option<&str> {
        self.0.strip_prefix(Self::BLOB).filter(|id| !id.is_empty())
    }
}

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
    /// What it is in, as git's `@@ … @@` line names it after the ranges: the enclosing
    /// function or block, when one is known ([`heading`]).
    pub heading: Option<String>,
    /// Its lines, each with its ` `, `-` or `+`.
    pub lines: Vec<String>,
}

/// The longest heading git writes, in bytes.
const HEADING_BYTES: usize = 80;

/// A hunk's heading as git's diff finds one with no diff driver set.
///
/// It is the nearest of the lines above the hunk's first line, `before` (read from the top),
/// that starts with an ASCII letter, `_` or `$`, cut at 80 bytes (on a character boundary) and
/// then trimmed of trailing space, in that order, as git does it.
#[must_use]
pub fn heading<'a>(before: impl DoubleEndedIterator<Item = &'a str>) -> Option<String> {
    let line = before.rev().find(|line| {
        line.bytes().next().is_some_and(|b| b.is_ascii_alphabetic() || b == b'_' || b == b'$')
    })?;
    let mut end = line.len().min(HEADING_BYTES);
    while !line.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    line.get(..end).map(|cut| cut.trim_end().to_owned())
}

/// A unified diff's hunk header, `@@ -a,b +c,d @@ heading`, read: the heading git wrote after
/// the ranges, when it wrote one.
#[must_use]
pub fn header_heading(header: &str) -> Option<String> {
    let rest = header.strip_prefix("@@ ")?;
    let (_, heading) = rest.split_once(" @@")?;
    Some(heading.trim().to_owned()).filter(|h| !h.is_empty())
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
    /// A native visual Slopty's `show_visual` shows ([`crate::visual::SHOW_VISUAL`]).
    Visual(crate::visual::VisualDetail),
    /// A page shown inline: Slopty's `show_page` ([`crate::visual::SHOW_PAGE`]), or an MCP
    /// App's UI ([`crate::visual::PageSource::App`]).
    Page(crate::visual::PageDetail),
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
    /// The files it changed in its repository, when the agent says (Claude Code's
    /// `bashEditDiff`).
    pub edits: Option<ShellEdits>,
    /// What its exit code means where the agent says, which is no failure: grep's 1 is "No
    /// matches found", diff's 1 "Files differ" (Claude Code's `returnCodeInterpretation`).
    #[serde(default)]
    pub interpretation: Option<String>,
}

/// The files a command changed in its repository, as the agent took them while it ran.
///
/// The list is the agent's best effort: it can miss a change, list one another process made, or
/// stop at its limits, and says which of those it knows of.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ShellEdits {
    /// The files shown with their diff, a few at most.
    pub files: Vec<ShellEdit>,
    /// How many more files changed, shown with no diff.
    pub more: u32,
    /// The diff is incomplete, or could not be taken.
    pub unavailable: bool,
    /// The command moves the working tree (a `git checkout`, a `git stash`), so no diff was
    /// taken.
    pub skipped: bool,
    /// Another command ran in the same repository at the same time, so a change listed may be
    /// that one's.
    pub shared: bool,
    /// A git step in the command can move the working tree (checkout, reset, rebase, switch,
    /// stash, merge, pull), so a change listed may be that step's rather than an edit.
    pub moves_tree: bool,
}

/// One file a command changed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellEdit {
    /// Its path, as the agent gave it.
    pub path: String,
    /// The command made it.
    pub created: bool,
    /// The command removed it.
    pub deleted: bool,
    /// What changed in it.
    pub patch: Patch,
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
    /// The model it ran on, as the agent resolved it (`claude-haiku-5-5`), once it says.
    pub model: Option<String>,
    /// Its tool calls by kind, as the agent tallies them once it ends: the [`super::kind`]
    /// names it counts by (`read`, `search`, `exec`, `edit`, `other`), each with its count;
    /// a kind it did not call is left out.
    pub tally: BTreeMap<String, u64>,
    /// Lines its edits added and removed, as the agent tallies them once it ends.
    pub changed: super::Changed,
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
    /// What it would look like, for the person to compare before picking: markdown or code
    /// (Claude Code's `options[].preview`), shown in a pane beside the options.
    #[serde(default)]
    pub preview: Option<String>,
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

    /// The choice that answers `questions` with `answers`, as a request's answer carries it: the
    /// words alone for one question that offers nothing, otherwise the answers as a JSON list, each
    /// keyed by its question's text.
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
    /// Its result's `structuredContent`, as JSON, when it returned one.
    pub structured: Option<Clipped>,
    /// The resources its result linked (`resource_link`) or embedded, in its order.
    pub links: Vec<crate::visual::ResourceLink>,
}

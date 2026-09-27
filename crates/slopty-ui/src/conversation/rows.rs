//! A thread as the list's rows.
//!
//! Turns fold once they settle, reads and searches are grouped, and each tool shows at the
//! level of detail the density gives it. Then come what is live and what was sent and not yet
//! recorded.
//!
//! A **turn** is a prompt and everything up to the next one. A settled turn folds to one
//! row, "Worked for 3 m 12 s · 14 steps · +120 −8", between its prompt and its answer: the
//! work hides, the question and the answer stay. The turn the agent is on never folds, so
//! nothing moves under the reader while it grows, and Verbose folds nothing.

use std::collections::HashSet;
use std::time::Duration;

use slopty_proto::conversation::{
    Body, Entry, LiveId, LiveKind, NoteKind, ResultStatus, ThreadId, ToolCall, ToolDetail, Turn,
    WriteKind,
};

use super::model::{LiveBlock, Pending, Thread};

/// How much of the work the list shows, cycled from the palette (Claude Desktop's three).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Density {
    /// Settled turns fold; thinking hides; each tool at the level its kind deserves.
    #[default]
    Normal,
    /// Normal, with the model's thinking shown.
    Thinking,
    /// Everything: nothing folds or groups, every tool in full, thinking shown.
    Verbose,
}

impl Density {
    /// The next density, round.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::Normal => Self::Thinking,
            Self::Thinking => Self::Verbose,
            Self::Verbose => Self::Normal,
        }
    }

    /// Its name as the chip and the palette say it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Normal => "Normal",
            Self::Thinking => "Thinking",
            Self::Verbose => "Verbose",
        }
    }

    const fn shows_thinking(self) -> bool {
        !matches!(self, Self::Normal)
    }
}

/// How much of a tool call a row shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Level {
    /// One line: what it did and to what.
    Title,
    /// The line and a short look at what came of it: a diff's first lines, a command's last.
    Summary,
    /// All of it the worker sent.
    Full,
}

/// What a tool is for, which decides its default level and whether it groups.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ToolKind {
    /// Looking: reads, searches, fetches. Grouped.
    Explore,
    /// Keeping the task list. Grouped.
    Tasks,
    /// Changing a file.
    Change,
    /// Running a command.
    Shell,
    /// A subagent.
    Agent,
    /// Asking, planning.
    Ask,
    /// Anything else.
    Other,
}

/// The kind of a tool call.
#[must_use]
pub fn tool_kind(call: &ToolCall) -> ToolKind {
    match &call.detail {
        ToolDetail::Read(_)
        | ToolDetail::Grep(_)
        | ToolDetail::Glob(_)
        | ToolDetail::WebFetch(_)
        | ToolDetail::WebSearch(_) => ToolKind::Explore,
        ToolDetail::TaskCreate(_) | ToolDetail::TaskUpdate(_) | ToolDetail::TodoWrite { .. } => {
            ToolKind::Tasks
        }
        ToolDetail::Edit(_) | ToolDetail::Write(_) => ToolKind::Change,
        ToolDetail::Bash(_) => ToolKind::Shell,
        ToolDetail::Agent(_) => ToolKind::Agent,
        ToolDetail::Question(_) | ToolDetail::Plan { .. } => ToolKind::Ask,
        // Loading deferred tools is bookkeeping, as looking is.
        ToolDetail::Other { .. } if call.name == "ToolSearch" => ToolKind::Explore,
        ToolDetail::Mcp(_) | ToolDetail::Other { .. } => ToolKind::Other,
    }
}

/// The level a tool shows at under `density`, before the reader flips it.
#[must_use]
pub const fn default_level(density: Density, kind: ToolKind) -> Level {
    match (density, kind) {
        (Density::Verbose, _) => Level::Full,
        (_, ToolKind::Change | ToolKind::Shell | ToolKind::Agent | ToolKind::Ask) => Level::Summary,
        (_, ToolKind::Explore | ToolKind::Tasks | ToolKind::Other) => Level::Title,
    }
}

/// Lines an entry added and removed: an edit's patch, a write's (a new file adds its every
/// line).
#[must_use]
pub fn entry_changes(entry: &Entry) -> (u32, u32) {
    let Body::Tool(call) = &entry.body else { return (0, 0) };
    if call.result.as_ref().is_some_and(|r| r.status != ResultStatus::Ok) {
        return (0, 0);
    }
    match &call.detail {
        ToolDetail::Edit(edit) => (edit.patch.added, edit.patch.removed),
        ToolDetail::Write(write)
            if write.kind == WriteKind::Create && write.patch.hunks.is_empty() =>
        {
            (write.lines, 0)
        }
        ToolDetail::Write(write) => (write.patch.added, write.patch.removed),
        _ => (0, 0),
    }
}

/// What a folded turn did.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Fold {
    /// From the prompt to the turn's last record; `None` when the records carry no time.
    pub took_ms: Option<u64>,
    /// Tool calls.
    pub steps: u32,
    /// Lines added by its edits and writes.
    pub added: u32,
    /// Lines removed.
    pub removed: u32,
    /// Tool calls that failed.
    pub failed: u32,
    /// Esc stopped it.
    pub interrupted: bool,
}

/// A duration as the fold says it: `42 s`, `3 m 12 s`, `1 h 04 m`.
#[must_use]
pub fn took(ms: u64) -> String {
    let secs = Duration::from_millis(ms).as_secs();
    match secs {
        0 => format!("{ms} ms"),
        1..60 => format!("{secs} s"),
        60..3_600 => format!("{} m {:02} s", secs / 60, secs % 60),
        _ => format!("{} h {:02} m", secs / 3_600, (secs % 3_600) / 60),
    }
}

impl Fold {
    /// "Worked for 3 m 12 s", "Stopped after 12 s", or the verb alone without a time.
    #[must_use]
    pub fn lead(&self) -> String {
        match (self.interrupted, self.took_ms) {
            (false, Some(ms)) => format!("Worked for {}", took(ms)),
            (true, Some(ms)) => format!("Stopped after {}", took(ms)),
            (false, None) => "Worked".to_owned(),
            (true, None) => "Stopped".to_owned(),
        }
    }

    /// "14 steps", "1 step".
    #[must_use]
    pub fn steps_label(&self) -> Option<String> {
        match self.steps {
            0 => None,
            1 => Some("1 step".to_owned()),
            n => Some(format!("{n} steps")),
        }
    }

    /// The whole line, as the accessibility tree reads it:
    /// "Worked for 3 m 12 s · 14 steps · +120 −8 · 1 failed".
    #[must_use]
    pub fn label(&self) -> String {
        let mut parts = vec![self.lead()];
        parts.extend(self.steps_label());
        if self.added > 0 || self.removed > 0 {
            parts.push(format!("+{} \u{2212}{}", self.added, self.removed));
        }
        if self.failed > 0 {
            parts.push(format!("{} failed", self.failed));
        }
        parts.join(" \u{b7} ")
    }
}

/// One row of the list.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Row {
    /// Something the person sent.
    Prompt {
        /// The entry.
        id: String,
    },
    /// A settled turn's work, folded or open.
    Fold {
        /// The turn's prompt.
        id: String,
        /// What the work was.
        fold: Fold,
        /// Opened by the reader.
        open: bool,
    },
    /// Answer text. The last answer of a settled turn is its `end`: it carries the turn's
    /// time and its copy.
    Answer {
        /// The entry.
        id: String,
        /// It ends a settled turn.
        end: bool,
    },
    /// The files a settled turn changed, under its answer.
    Changes {
        /// The turn's prompt.
        prompt: String,
    },
    /// In the session's changes: a file, heading the edits that changed it.
    File {
        /// Its path.
        path: String,
    },
    /// In the session's changes: one edit or write, its diff whole.
    Edit {
        /// Its thread.
        thread: ThreadId,
        /// The call.
        id: String,
    },
    /// Thinking, a tool call, a compaction, an interruption, a note, a rewind.
    Entry {
        /// The entry.
        id: String,
        /// For a tool, how much of it shows.
        level: Level,
    },
    /// Calls of one kind in a row, as one line that opens.
    Group {
        /// What they are.
        kind: ToolKind,
        /// The calls, in order.
        ids: Vec<String>,
        /// Opened by the reader.
        open: bool,
    },
    /// A block the model is writing now.
    Live {
        /// Which.
        id: LiveId,
    },
    /// The agent is on its turn and nothing is streaming.
    Working,
    /// A message sent from the composer that the transcript has not recorded yet.
    Pending {
        /// Its place in [`super::model::Model::pending`].
        index: usize,
    },
}

impl Row {
    /// What names the row across rebuilds: the list keeps a row that keeps its key.
    #[must_use]
    pub fn key(&self) -> String {
        match self {
            Self::Prompt { id } => format!("p:{id}"),
            Self::Answer { id, .. } => format!("a:{id}"),
            Self::Changes { prompt } => format!("c:{prompt}"),
            Self::File { path } => format!("file:{path}"),
            Self::Edit { thread, id } => match thread {
                ThreadId::Main => format!("edit:{id}"),
                ThreadId::Agent(agent) => format!("edit:{agent}:{id}"),
            },
            Self::Fold { id, .. } => fold_key(id),
            Self::Entry { id, .. } => format!("e:{id}"),
            Self::Group { ids, .. } => group_key(ids.first().map_or("", String::as_str)),
            Self::Live { id } => format!("l:{}:{}:{}", id.turn, id.step, id.block),
            Self::Working => "w".to_owned(),
            Self::Pending { index } => format!("q:{index}"),
        }
    }
}

/// The key a turn's fold is opened by.
#[must_use]
pub fn fold_key(prompt: &str) -> String {
    format!("f:{prompt}")
}

/// The key a group is opened by.
#[must_use]
pub fn group_key(first: &str) -> String {
    format!("g:{first}")
}

/// The key a tool call's level is flipped by.
#[must_use]
pub fn entry_key(id: &str) -> String {
    format!("e:{id}")
}

/// What a thread's rows are built from.
#[derive(Clone, Copy, Debug)]
pub struct Input<'a> {
    /// The thread.
    pub thread: &'a Thread,
    /// Which one.
    pub id: &'a ThreadId,
    /// Its last turn is still going: the agent works, or the subagent runs.
    pub live_turn: bool,
    /// The agent waits on the person (a permission, a question): the turn is live, but
    /// nothing works, so no "Working" row claims it does.
    pub waiting: bool,
    /// How much shows.
    pub density: Density,
    /// Folds and groups the reader opened and tools whose level they flipped, by key.
    pub toggled: &'a HashSet<String>,
    /// The thread's live blocks, in order.
    pub live: &'a [(&'a LiveId, &'a LiveBlock)],
    /// Messages sent and not recorded (the main thread only).
    pub pending: &'a [Pending],
}

/// Whether an entry is part of a turn's work, which a fold hides: thinking, tool calls, and
/// answer text before the last of them.
const fn is_work(entry: &Entry) -> bool {
    matches!(entry.body, Body::Thinking(_) | Body::Tool(_))
}

/// The turns of `entries`, as index ranges: each from a prompt up to the next, the first
/// from the start when the thread does not open with one.
fn turns(entries: &[Entry]) -> Vec<std::ops::Range<usize>> {
    let mut starts: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| matches!(e.body, Body::Prompt(_)))
        .map(|(ix, _)| ix)
        .collect();
    if starts.first() != Some(&0) {
        starts.insert(0, 0);
    }
    let ends = starts.iter().skip(1).copied().chain(std::iter::once(entries.len()));
    starts.iter().copied().zip(ends).map(|(a, b)| a..b).filter(|r| !r.is_empty()).collect()
}

/// What a turn's work adds up to; `figures` are the transcript's own for it, whose end Claude
/// Code stamped when it closed the turn.
fn fold_of(turn: &[Entry], figures: Option<&Turn>) -> Fold {
    let mut fold = Fold::default();
    let start = turn.first().map_or(0, |e| e.at_ms);
    let mut end = start;
    for entry in turn {
        end = end.max(entry.at_ms);
        match &entry.body {
            Body::Tool(call) => {
                fold.steps = fold.steps.saturating_add(1);
                if let Some(result) = &call.result {
                    end = end.max(result.at_ms);
                    if result.status == ResultStatus::Error {
                        fold.failed = fold.failed.saturating_add(1);
                    }
                }
                let (added, removed) = entry_changes(entry);
                fold.added = fold.added.saturating_add(added);
                fold.removed = fold.removed.saturating_add(removed);
            }
            Body::Interrupted { .. } => fold.interrupted = true,
            _ => {}
        }
    }
    fold.took_ms = (start > 0 && end > start).then(|| end.saturating_sub(start));
    if let Some(figures) = figures
        && let Some(ended) = figures.ended_ms
        && figures.started_ms > 0
        && ended > figures.started_ms
    {
        fold.took_ms = Some(ended.saturating_sub(figures.started_ms));
    }
    fold
}

/// Builds a thread's rows.
struct Builder<'a> {
    input: Input<'a>,
    rows: Vec<Row>,
}

impl Builder<'_> {
    /// The level a tool entry shows at: its kind's default, flipped when the reader flipped
    /// it (a summary opens to full, full closes to its title).
    fn level(&self, entry: &Entry) -> Level {
        let Body::Tool(call) = &entry.body else { return Level::Full };
        let level = default_level(self.input.density, tool_kind(call));
        if !self.input.toggled.contains(&entry_key(&entry.id)) {
            return level;
        }
        match level {
            Level::Title | Level::Summary => Level::Full,
            Level::Full => Level::Title,
        }
    }

    /// The group a tool entry joins, where the density groups.
    fn group_kind(&self, entry: &Entry) -> Option<ToolKind> {
        if self.input.density == Density::Verbose {
            return None;
        }
        let Body::Tool(call) = &entry.body else { return None };
        let kind = tool_kind(call);
        matches!(kind, ToolKind::Explore | ToolKind::Tasks).then_some(kind)
    }

    /// Rows for `entries` in order, thinking left out where the density hides it and runs of
    /// looking or of task keeping grouped.
    fn emit(&mut self, entries: &[&Entry]) {
        let shown: Vec<&Entry> = entries
            .iter()
            .copied()
            .filter(|e| self.input.density.shows_thinking() || !matches!(e.body, Body::Thinking(_)))
            .collect();
        let mut at = 0_usize;
        while let Some(entry) = shown.get(at) {
            let kind = self.group_kind(entry);
            let run = kind.map_or(1, |kind| {
                shown.iter().skip(at).take_while(|e| self.group_kind(e) == Some(kind)).count()
            });
            match kind {
                Some(kind) if run >= 2 => {
                    let ids: Vec<String> =
                        shown.iter().skip(at).take(run).map(|e| e.id.clone()).collect();
                    let open =
                        ids.first().is_some_and(|f| self.input.toggled.contains(&group_key(f)));
                    self.rows.push(Row::Group { kind, ids: ids.clone(), open });
                    if open {
                        for id in ids {
                            self.rows.push(Row::Entry { id, level: Level::Title });
                        }
                    }
                }
                _ if matches!(entry.body, Body::Text(_)) => {
                    self.rows.push(Row::Answer { id: entry.id.clone(), end: false });
                }
                _ => {
                    let level = self.level(entry);
                    self.rows.push(Row::Entry { id: entry.id.clone(), level });
                }
            }
            at = at.saturating_add(run.max(1));
        }
    }

    fn turn(&mut self, turn: &[Entry], live: bool) {
        let (prompt, body) = match turn.split_first() {
            Some((first, rest)) if matches!(first.body, Body::Prompt(_)) => (Some(first), rest),
            _ => (None, turn),
        };
        if let Some(prompt) = prompt {
            self.rows.push(Row::Prompt { id: prompt.id.clone() });
        }
        let start = self.rows.len();
        self.work(turn, prompt, body, live);
        let Some(prompt) = prompt.filter(|_| !live) else { return };
        // A settled turn ends on its last answer, which carries its time and its copy, and
        // then the files it changed.
        if let Some(Row::Answer { end, .. }) = self
            .rows
            .get_mut(start..)
            .and_then(|rows| rows.iter_mut().rev().find(|r| matches!(r, Row::Answer { .. })))
        {
            *end = true;
        }
        let thread = self.input.id;
        if !super::figures::files(body.iter().map(|e| (thread, e))).is_empty() {
            self.rows.push(Row::Changes { prompt: prompt.id.clone() });
        }
    }

    /// The work of a turn under its prompt: folded once settled, or all of it.
    fn work(&mut self, turn: &[Entry], prompt: Option<&Entry>, body: &[Entry], live: bool) {
        let all: Vec<&Entry> = body.iter().collect();
        let foldable = self.input.density != Density::Verbose && !live;
        let last_work = body.iter().rposition(is_work);
        let (Some(prompt), true, Some(last_work)) = (prompt, foldable, last_work) else {
            self.emit(&all);
            return;
        };
        let open = self.input.toggled.contains(&fold_key(&prompt.id));
        let figures = self.input.thread.turn(&prompt.id);
        self.rows.push(Row::Fold { id: prompt.id.clone(), fold: fold_of(turn, figures), open });
        if open {
            self.emit(&all);
            return;
        }
        // Folded: what is not work stays (notes, a compaction, Esc), and so does the answer,
        // the text after the last of the work.
        let kept: Vec<&Entry> = body
            .iter()
            .enumerate()
            .filter(|(ix, e)| match e.body {
                Body::Text(_) => *ix > last_work,
                Body::Thinking(_) | Body::Tool(_) => false,
                Body::Prompt(_)
                | Body::Compact(_)
                | Body::Interrupted { .. }
                | Body::Note(_)
                | Body::Rewound { .. } => true,
            })
            .map(|(_, e)| e)
            .collect();
        self.emit(&kept);
    }
}

/// The rows of a thread.
#[must_use]
pub fn build(input: Input<'_>) -> Vec<Row> {
    let mut builder = Builder { input, rows: Vec::new() };
    let entries = input.thread.entries();
    let turns = turns(entries);
    let count = turns.len();
    for (ix, range) in turns.into_iter().enumerate() {
        let live = input.live_turn && ix.saturating_add(1) == count;
        if let Some(turn) = entries.get(range) {
            builder.turn(turn, live);
        }
    }
    let mut writing = false;
    for (id, block) in input.live {
        let shown = match block.kind {
            LiveKind::Thinking => input.density.shows_thinking(),
            LiveKind::Text => {
                writing = true;
                true
            }
            LiveKind::Tool { .. } => true,
        };
        if shown {
            builder.rows.push(Row::Live { id: (*id).clone() });
        }
    }
    if input.live_turn && !input.waiting && !writing && *input.id == ThreadId::Main {
        builder.rows.push(Row::Working);
    }
    for index in 0..input.pending.len() {
        builder.rows.push(Row::Pending { index });
    }
    builder.rows
}

/// The rows of the session's changes: each file the session changed, in the order it was
/// first changed, over the edits and writes that changed it.
#[must_use]
pub fn changes(files: &[super::figures::FileChange]) -> Vec<Row> {
    let mut rows = Vec::new();
    for file in files {
        rows.push(Row::File { path: file.path.clone() });
        for (thread, id) in &file.edits {
            rows.push(Row::Edit { thread: thread.clone(), id: id.clone() });
        }
    }
    rows
}

/// The rows of `rows` that are prompts, with their row index: the prompt rail's ticks and
/// ⌘↑/⌘↓'s stops.
pub fn prompts(rows: &[Row]) -> impl Iterator<Item = (usize, &str)> {
    rows.iter().enumerate().filter_map(|(ix, row)| match row {
        Row::Prompt { id } => Some((ix, id.as_str())),
        _ => None,
    })
}

/// Whether a note is worth a row in Normal density: every kind is, an API error loudest.
#[must_use]
pub const fn note_is_error(kind: NoteKind) -> bool {
    matches!(kind, NoteKind::ApiError)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::fixtures::scenario;
    use crate::conversation::model::Model;

    /// The rows as short words, for comparing.
    fn words(
        model: &Model,
        thread: &ThreadId,
        density: Density,
        live: bool,
        toggled: &[&str],
    ) -> Vec<String> {
        let toggled: HashSet<String> = toggled.iter().map(|s| (*s).to_owned()).collect();
        let t = model.thread(thread).unwrap();
        let rows = build(Input {
            thread: t,
            id: thread,
            live_turn: live,
            waiting: false,
            density,
            toggled: &toggled,
            live: &[],
            pending: &[],
        });
        rows.iter()
            .map(|row| match row {
                Row::Prompt { .. } => "prompt".to_owned(),
                Row::Fold { fold, open, .. } => {
                    format!("fold{} {}", if *open { "+" } else { "" }, fold.label())
                }
                Row::Entry { id, level } => {
                    let entry = t.entry(id).unwrap();
                    match &entry.body {
                        Body::Tool(call) => format!("{} {level:?}", call.name),
                        Body::Text(_) => "text".to_owned(),
                        Body::Thinking(_) => "thinking".to_owned(),
                        Body::Compact(_) => "compact".to_owned(),
                        Body::Interrupted { .. } => "esc".to_owned(),
                        Body::Note(_) => "note".to_owned(),
                        Body::Prompt(_) => "prompt?".to_owned(),
                        Body::Rewound { .. } => "rewound".to_owned(),
                    }
                }
                Row::Group { kind, ids, open } => {
                    format!("group{} {kind:?} {}", if *open { "+" } else { "" }, ids.len())
                }
                Row::Answer { end, .. } => if *end { "answer." } else { "answer" }.to_owned(),
                Row::Changes { .. } => "changes".to_owned(),
                Row::File { .. } => "file".to_owned(),
                Row::Edit { .. } => "edit".to_owned(),
                Row::Live { .. } => "live".to_owned(),
                Row::Working => "working".to_owned(),
                Row::Pending { .. } => "pending".to_owned(),
            })
            .collect()
    }

    /// A settled turn folds to its prompt, the one line of what it did, and its answer, which
    /// ends the turn; the files it changed come after.
    #[test]
    fn a_settled_turn_folds_to_its_prompt_its_work_and_its_answer() {
        let model = scenario("tools");
        let rows = words(&model, &ThreadId::Main, Density::Normal, false, &[]);
        assert_eq!(
            rows,
            [
                "prompt",
                "fold Worked for 35 s \u{b7} 13 steps \u{b7} +2 \u{2212}0 \u{b7} 1 failed",
                "answer.",
                "changes",
            ]
        );
    }

    /// The turn the agent is on never folds: its work shows as it grows, thinking hidden,
    /// looking and task keeping grouped, each tool at its kind's level.
    #[test]
    fn a_live_turn_never_folds() {
        let model = scenario("tools");
        let rows = words(&model, &ThreadId::Main, Density::Normal, true, &[]);
        assert_eq!(
            rows,
            [
                "prompt",
                "ToolSearch Title",
                "answer",
                "group Tasks 3",
                "group Explore 2",
                "Bash Summary",
                "Bash Summary",
                "Bash Summary",
                "Write Summary",
                "Agent Summary",
                "group Tasks 2",
                "answer",
                "working",
            ]
        );
    }

    /// An opened fold shows the work as a live turn does; an opened group lists its calls;
    /// a flipped tool opens to full.
    #[test]
    fn what_the_reader_opens_stays_open() {
        let model = scenario("tools");
        let main = model.thread(&ThreadId::Main).unwrap();
        let prompt = main.entries().first().unwrap().id.clone();
        let glob = main.entries().iter().find(|e| e.id == "toolu_05").unwrap().id.clone();
        let rows = words(
            &model,
            &ThreadId::Main,
            Density::Normal,
            false,
            &[&fold_key(&prompt), &group_key(&glob), &entry_key("toolu_07")],
        );
        assert_eq!(rows.get(1).map(String::as_str).map(|r| r.starts_with("fold+")), Some(true));
        let at = rows.iter().position(|r| r == "group+ Explore 2").unwrap();
        assert_eq!(&rows[at + 1..at + 3], ["Glob Title", "Grep Title"]);
        assert_eq!(rows[at + 3], "Bash Full", "the flipped call opens");
        assert_eq!(rows[at + 4], "Bash Summary");
    }

    /// Verbose folds and groups nothing, shows every thinking block and every tool in full;
    /// Thinking is Normal with the thinking.
    #[test]
    fn the_densities_show_more_in_turn() {
        let model = scenario("tools");
        let verbose = words(&model, &ThreadId::Main, Density::Verbose, false, &[]);
        assert!(!verbose.iter().any(|r| r.starts_with("fold") || r.starts_with("group")));
        assert_eq!(verbose.iter().filter(|r| *r == "thinking").count(), 12);
        assert!(!verbose.iter().any(|r| r.ends_with("Title") || r.ends_with("Summary")));
        let thinking = words(&model, &ThreadId::Main, Density::Thinking, true, &[]);
        assert_eq!(thinking.iter().filter(|r| *r == "thinking").count(), 12);
        assert!(
            thinking.contains(&"group Tasks 2".to_owned()),
            "thinking between calls breaks a group: {thinking:?}"
        );
        let normal = words(&model, &ThreadId::Main, Density::Normal, true, &[]);
        assert!(!normal.iter().any(|r| r == "thinking"));
        assert_eq!(Density::Normal.next().next().next(), Density::Normal);
    }

    /// An interrupted turn folds to "Stopped after …" and keeps the Esc in view.
    #[test]
    fn an_interrupted_turn_says_it_was_stopped() {
        let model = scenario("interrupt");
        let rows = words(&model, &ThreadId::Main, Density::Normal, false, &[]);
        assert_eq!(rows, ["prompt", "fold Stopped after 5 s \u{b7} 1 step", "esc"]);
    }

    /// A compaction and Claude Code's notes are never hidden in a fold. The fold is timed to
    /// Claude Code's own end of the turn: the compaction 16 s later is not the turn's work.
    #[test]
    fn a_compaction_stays_outside_the_fold() {
        let model = scenario("compact");
        let rows = words(&model, &ThreadId::Main, Density::Normal, false, &[]);
        assert_eq!(rows, ["prompt", "fold Worked for 1 s", "answer.", "compact", "prompt", "note"]);
    }

    /// A subagent's thread builds the same way, from its brief.
    #[test]
    fn a_subagent_thread_has_its_own_rows() {
        let model = scenario("tools");
        let agent = ThreadId::Agent("a0000000000000001".to_owned());
        assert_eq!(
            words(&model, &agent, Density::Normal, false, &[]),
            ["prompt", "fold Worked for 3 s \u{b7} 1 step", "answer."]
        );
        let (description, kind) = model.subagent("a0000000000000001");
        assert_eq!(
            description.as_deref(),
            Some("Count lines"),
            "named by the call that started it"
        );
        assert!(kind.is_some());
    }

    /// Row keys are stable across rebuilds of the same thread and name what they show.
    #[test]
    fn row_keys_are_unique_and_stable() {
        let model = scenario("tools");
        let toggled = HashSet::new();
        let t = model.thread(&ThreadId::Main).unwrap();
        let input = Input {
            thread: t,
            id: &ThreadId::Main,
            live_turn: true,
            waiting: false,
            density: Density::Normal,
            toggled: &toggled,
            live: &[],
            pending: &[],
        };
        let a: Vec<String> = build(input).iter().map(Row::key).collect();
        let b: Vec<String> = build(input).iter().map(Row::key).collect();
        assert_eq!(a, b);
        let unique: HashSet<&String> = a.iter().collect();
        assert_eq!(unique.len(), a.len(), "{a:?}");
    }

    #[test]
    fn durations_read_as_the_navigator_reads_them() {
        assert_eq!(took(850), "850 ms");
        assert_eq!(took(42_000), "42 s");
        assert_eq!(took(192_000), "3 m 12 s");
        assert_eq!(took(3_840_000), "1 h 04 m");
    }
}

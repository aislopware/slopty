//! A thread as the view's rows.
//!
//! Each turn's message, then its work under one line over its answer, and the sends on their
//! way at the foot. The line is open while the turn is under way, its work showing under it
//! as it comes (two or more quiet calls in a row there as one line, "Read 3 files · Searched
//! once"), and folded once the turn is done (`docs/decisions/ui.md`, "A turn's work is one
//! line, open while it runs").
//!
//! Nothing here draws. [`build`] runs over the mirror on every change and is cheap enough to:
//! it walks the items once and allocates a row each.

use std::collections::HashSet;
use std::hash::{Hash as _, Hasher as _};
use std::ops::Range;
use std::time::Duration;

use slopty_client::threads::Sent;
use slopty_proto::thread::wire::Intent;
use slopty_proto::thread::{
    Delivery, IntentId, Item, ItemBody, ItemId, Phase, ThreadState, ToolCall, ToolDetail,
    ToolState, Turn, TurnId, TurnState, kind,
};

/// How many kinds of work a fold names before it counts the rest.
const NAMED: usize = 2;

/// One row of the thread.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Row {
    /// What the person sent.
    User {
        /// The item.
        item: ItemId,
    },
    /// A turn's work as one line, open or folded: one for the work before each message the
    /// person sent into the turn, and one after the last. Open while the turn is under way,
    /// folded once it settled, unless the reader turned it.
    Fold {
        /// The turn.
        turn: TurnId,
        /// Which stretch of the turn's work, counted by the messages before it: 0 is the work
        /// before the first steer.
        part: u32,
        /// The reader opened it.
        open: bool,
    },
    /// What the agent wrote.
    Text {
        /// The item.
        item: ItemId,
    },
    /// The agent's reasoning.
    Reasoning {
        /// The item.
        item: ItemId,
    },
    /// A tool call.
    Tool {
        /// The item.
        item: ItemId,
    },
    /// Quiet calls done one after another in the live turn's open work, as one line, open or
    /// folded.
    Group {
        /// The first of them, which names the group.
        first: ItemId,
        /// The reader opened it.
        open: bool,
    },
    /// A notice, a compaction, a review, or an item of a kind this client does not know: a
    /// quiet line.
    Note {
        /// The item.
        item: ItemId,
    },
    /// The files the latest turn changed, under its answer once it settled: kept, put back
    /// or reviewed from there.
    Changes {
        /// The turn.
        turn: TurnId,
    },
    /// The agent is on its turn.
    Working {
        /// The turn.
        turn: TurnId,
    },
    /// A message on its way, or one the worker turned down.
    Sending {
        /// Its intent.
        intent: IntentId,
    },
}

impl Row {
    /// What names the row across rebuilds: the list keeps a row that keeps its key.
    #[must_use]
    pub fn key(&self) -> u64 {
        let mut h = std::hash::DefaultHasher::new();
        std::mem::discriminant(self).hash(&mut h);
        match self {
            Self::User { item }
            | Self::Text { item }
            | Self::Reasoning { item }
            | Self::Tool { item }
            | Self::Note { item }
            | Self::Group { first: item, .. } => item.hash(&mut h),
            Self::Fold { turn, part, .. } => (turn, part).hash(&mut h),
            Self::Working { turn } | Self::Changes { turn } => turn.hash(&mut h),
            Self::Sending { intent } => intent.hash(&mut h),
        }
        h.finish()
    }
}

/// What the rows are built from.
#[derive(Clone, Copy, Debug)]
pub struct Input<'a> {
    /// The thread.
    pub state: &'a ThreadState,
    /// This client's intents the state does not show yet.
    pub unshown: &'a [&'a Sent],
    /// The settled turns the reader opened.
    pub open: &'a HashSet<TurnId>,
    /// The turns under way the reader folded.
    pub shut: &'a HashSet<TurnId>,
    /// The turns whose changed files the person kept or put back: their card is done.
    pub kept: &'a HashSet<TurnId>,
    /// The groups of quiet calls the reader opened, by their first call.
    pub groups: &'a HashSet<ItemId>,
}

/// The rows of a thread, and where in its items each row's content is.
#[derive(Clone, Debug, Default)]
pub struct Built {
    /// The rows.
    pub rows: Vec<Row>,
    /// For each row, the items it draws ([`ThreadState::items`]): one item, a fold's whole
    /// turn, or none.
    pub spans: Vec<Range<usize>>,
}

impl Built {
    fn push(&mut self, row: Row, span: Range<usize>) {
        self.rows.push(row);
        self.spans.push(span);
    }
}

/// The rows of a thread.
#[must_use]
pub fn build(input: Input<'_>) -> Vec<Row> {
    build_spans(input).rows
}

/// The rows of a thread, with where each row's items are: a row is drawn from them without a
/// search.
#[must_use]
pub fn build_spans(input: Input<'_>) -> Built {
    let state = input.state;
    let capacity = state.items.len().saturating_add(4);
    let mut built =
        Built { rows: Vec::with_capacity(capacity), spans: Vec::with_capacity(capacity) };
    let mut at = 0;
    while let Some(first) = state.items.get(at) {
        let turn = first.turn;
        let len = state
            .items
            .get(at..)
            .map_or(0, |rest| rest.iter().take_while(|i| i.turn == turn).count());
        let run = at..at.saturating_add(len);
        let items = state.items.get(run.clone()).unwrap_or_default();
        if turn == TurnId::BEFORE {
            live_rows(&mut built, items, run.start, input.groups);
        } else if settled(state, turn) {
            let open = input.open.contains(&turn);
            turn_rows(&mut built, turn, items, run.clone(), open, None);
        } else {
            let open = !input.shut.contains(&turn);
            turn_rows(&mut built, turn, items, run.clone(), open, Some(input.groups));
        }
        at = run.end;
    }
    if let Some(last) = state.last_turn().filter(|t| changed_files(state, t.id, input.kept)) {
        built.push(Row::Changes { turn: last.id }, 0..0);
    }
    if let Some(last) = under_way(state) {
        built.push(Row::Working { turn: last.id }, 0..0);
    }
    for sent in input.unshown.iter().filter(|s| bubble(s)) {
        built.push(Row::Sending { intent: sent.id }, 0..0);
    }
    built
}

/// Whether `turn` is over: ended, or not the last.
///
/// A turn the agent never wrote the end of (its stop went unheard) is over once a newer one
/// began: only the last turn can still be under way. One the thread does not hold is not.
#[must_use]
pub fn settled(state: &ThreadState, turn: TurnId) -> bool {
    let last = state.last_turn().map(|t| t.id);
    state.turn(turn).is_some_and(|t| !matches!(t.state, TurnState::Active) || Some(t.id) != last)
}

/// Whether the latest turn `turn` ends in a card of the files it changed: it settled, its
/// edits were made, and the person has not kept or put them back yet.
fn changed_files(state: &ThreadState, turn: TurnId, kept: &HashSet<TurnId>) -> bool {
    settled(state, turn) && !kept.contains(&turn) && !super::activity::edited(state).is_empty()
}

/// The turn the agent is working on: the last one, while open and the agent says it works.
///
/// A turn whose end the agent never wrote (a session left mid-turn, a transcript read from
/// before) is not, once the agent says it is idle.
#[must_use]
pub fn under_way(state: &ThreadState) -> Option<&Turn> {
    let working = matches!(state.status.phase, Phase::Working | Phase::Waiting);
    state.last_turn().filter(|t| working && matches!(t.state, TurnState::Active))
}

/// Whether an unshown intent is drawn in the thread: a message sent now, or one the worker
/// turned down. A queued one waits in the activity bar.
const fn bubble(sent: &Sent) -> bool {
    match &sent.intent {
        Intent::Send { delivery: Delivery::Steer, .. } => true,
        Intent::Send { delivery: Delivery::Queue, .. } => sent.failed(),
        _ => false,
    }
}

/// Whether `item` is a plan the agent proposed: a document read as an answer is, never folded
/// into the work or grouped with the calls that only looked.
#[must_use]
pub fn plan(item: &Item) -> bool {
    matches!(&item.body, ItemBody::Tool(call) if matches!(call.detail, Some(ToolDetail::Plan { .. })))
}

/// A turn: its message, the fold over its work, then its answer, the last thing the agent
/// wrote so far. A plan stands outside the fold where it was proposed. Open, the work shows
/// between the fold and the answer, in its order; a turn under way's quiet calls in a row as
/// one line there (`groups`, the ones the reader opened).
///
/// A message the person sent into the turn while it ran (a steer) stands where it was sent,
/// and splits the work: a fold over what came before it, another over what came after, so
/// the steer is never folded away and each fold says what its stretch did. The turn opens
/// as one.
fn turn_rows(
    built: &mut Built,
    turn: TurnId,
    items: &[Item],
    run: Range<usize>,
    open: bool,
    groups: Option<&HashSet<ItemId>>,
) {
    let answer = items.iter().rposition(|i| matches!(i.body, ItemBody::Text(_)));
    let work = |ix: usize, i: &Item| {
        Some(ix) != answer && !matches!(i.body, ItemBody::User(_)) && !plan(i)
    };
    let mut part = 0_u32;
    let mut start = 0_usize;
    let mut folded = false;
    let mut ix = 0_usize;
    while let Some(item) = items.get(ix) {
        let at = run.start.saturating_add(ix);
        if ix > 0 && matches!(item.body, ItemBody::User(_)) {
            part = part.saturating_add(1);
            start = ix;
            folded = false;
        }
        if !work(ix, item) {
            built.push(row_of(item), at..at.saturating_add(1));
            ix = ix.saturating_add(1);
            continue;
        }
        if !folded {
            // The stretch runs to the next steer, or the turn's end.
            let len = items
                .get(start.saturating_add(1)..)
                .unwrap_or_default()
                .iter()
                .take_while(|i| !matches!(i.body, ItemBody::User(_)))
                .count()
                .saturating_add(1);
            let stretch = run.start.saturating_add(start)
                ..run.start.saturating_add(start.saturating_add(len));
            built.push(Row::Fold { turn, part, open }, stretch);
            folded = true;
        }
        if !open {
            ix = ix.saturating_add(1);
            continue;
        }
        let len = groups.map_or(0, |_| {
            let rest = items.get(ix..).unwrap_or_default().iter();
            rest.take_while(|i| groupable(i)).count()
        });
        match groups {
            Some(opened) if len >= GROUP => {
                let shown = opened.contains(&item.id);
                let span = at..at.saturating_add(len);
                built.push(Row::Group { first: item.id.clone(), open: shown }, span);
                if shown {
                    for (k, call) in
                        items.get(ix..ix.saturating_add(len)).unwrap_or_default().iter().enumerate()
                    {
                        let at = at.saturating_add(k);
                        built.push(row_of(call), at..at.saturating_add(1));
                    }
                }
                ix = ix.saturating_add(len);
            }
            _ => {
                built.push(row_of(item), at..at.saturating_add(1));
                ix = ix.saturating_add(1);
            }
        }
    }
}

/// The fewest quiet calls in a row that make a group.
const GROUP: usize = 2;

/// Whether `item` is a call that only looked and is done: a group's kind of call.
fn groupable(item: &Item) -> bool {
    if plan(item) {
        return false;
    }
    match &item.body {
        ItemBody::Tool(call) => {
            quiet(call)
                && matches!(call.state, ToolState::Completed)
                && call.child.is_none()
                && call.images.is_empty()
        }
        _ => false,
    }
}

/// Whether `call` only looks (a read, a search, a fetch) and asks nothing: a quiet line, not a
/// card.
#[must_use]
pub fn quiet(call: &ToolCall) -> bool {
    !matches!(call.kind.as_str(), kind::EDIT | kind::WRITE | kind::EXEC)
        && !matches!(call.state, ToolState::Pending { .. })
}

/// The live turn's rows, each item its own but quiet calls in a row as one group.
fn live_rows(built: &mut Built, items: &[Item], start: usize, open: &HashSet<ItemId>) {
    let mut ix = 0_usize;
    while let Some(item) = items.get(ix) {
        let at = start.saturating_add(ix);
        let len = items.get(ix..).unwrap_or_default().iter().take_while(|i| groupable(i)).count();
        if len < GROUP {
            built.push(row_of(item), at..at.saturating_add(1));
            ix = ix.saturating_add(1);
            continue;
        }
        let shown = open.contains(&item.id);
        built.push(Row::Group { first: item.id.clone(), open: shown }, at..at.saturating_add(len));
        if shown {
            for (k, call) in
                items.get(ix..ix.saturating_add(len)).unwrap_or_default().iter().enumerate()
            {
                let at = at.saturating_add(k);
                built.push(row_of(call), at..at.saturating_add(1));
            }
        }
        ix = ix.saturating_add(len);
    }
}

fn row_of(item: &Item) -> Row {
    let id = item.id.clone();
    match item.body {
        ItemBody::User(_) => Row::User { item: id },
        ItemBody::Text(_) => Row::Text { item: id },
        ItemBody::Reasoning(_) => Row::Reasoning { item: id },
        ItemBody::Tool(_) => Row::Tool { item: id },
        ItemBody::Compaction(_)
        | ItemBody::Notice(_)
        | ItemBody::Review { .. }
        | ItemBody::Extra { .. } => Row::Note { item: id },
    }
}

/// What a settled turn's work adds up to.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Fold {
    /// The turn is still under way: its line says what it did so far.
    pub running: bool,
    /// How it ended.
    pub ended: Ended,
    /// From its start to its end, when both are known.
    pub took: Option<Duration>,
    /// Tool calls.
    pub steps: u32,
    /// Commands run.
    pub ran: u32,
    /// Files read.
    pub read: u32,
    /// Searches of the files or the web, and pages fetched.
    pub searched: u32,
    /// Subagents started.
    pub delegated: u32,
    /// The files edited or written, each once, in the order first changed.
    pub edited: Vec<String>,
    /// Edit and write calls, a file edited twice counting twice.
    pub edit_calls: u32,
    /// Lines added by its edits.
    pub added: u32,
    /// Lines removed.
    pub removed: u32,
    /// Calls that failed.
    pub failed: u32,
}

/// How a turn ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Ended {
    /// It ran to its end.
    #[default]
    Complete,
    /// The person stopped it.
    Stopped,
    /// It failed.
    Failed,
}

impl Fold {
    /// What `turn`'s `items` add up to.
    #[must_use]
    pub fn of(turn: &Turn, items: &[Item]) -> Self {
        let mut fold = Self {
            ended: match turn.state {
                TurnState::Active | TurnState::Complete => Ended::Complete,
                TurnState::Interrupted => Ended::Stopped,
                TurnState::Failed { .. } => Ended::Failed,
            },
            took: turn
                .ended_ms
                .filter(|end| !turn.started_ms.is_zero() && *end > turn.started_ms)
                .map(|end| Duration::from_millis(end.millis_since(turn.started_ms))),
            added: turn.changed.added,
            removed: turn.changed.removed,
            ..Self::default()
        };
        fold.count(items);
        fold
    }

    /// What a group of calls adds up to: its kinds of work, untimed.
    #[must_use]
    pub fn of_calls(items: &[Item]) -> Self {
        let mut fold = Self::default();
        fold.count(items);
        fold
    }

    fn count(&mut self, items: &[Item]) {
        let fold = self;
        let bump = |n: &mut u32| *n = n.saturating_add(1);
        for item in items.iter().filter(|i| !plan(i)) {
            let ItemBody::Tool(call) = &item.body else { continue };
            bump(&mut fold.steps);
            if matches!(call.state, ToolState::Failed) {
                bump(&mut fold.failed);
            }
            match (&call.detail, call.kind.as_str()) {
                (Some(ToolDetail::Edit(d)), _) => fold.edit(&d.path),
                (Some(ToolDetail::Write(d)), _) => fold.edit(&d.path),
                (Some(ToolDetail::Exec(_)), _) | (None, kind::EXEC) => bump(&mut fold.ran),
                (Some(ToolDetail::Read(_)), _) | (None, kind::READ) => bump(&mut fold.read),
                (
                    Some(ToolDetail::Search(_) | ToolDetail::Fetch(_) | ToolDetail::WebSearch(_)),
                    _,
                )
                | (None, kind::SEARCH | kind::FETCH | kind::WEB_SEARCH) => {
                    bump(&mut fold.searched);
                }
                (Some(ToolDetail::Agent(_)), _) | (None, kind::AGENT) => bump(&mut fold.delegated),
                _ => {}
            }
        }
    }

    fn edit(&mut self, path: &str) {
        self.edit_calls = self.edit_calls.saturating_add(1);
        let name = path.rsplit('/').next().unwrap_or(path);
        if !self.edited.iter().any(|e| e == name) {
            self.edited.push(name.to_owned());
        }
    }

    /// "Worked 55 s", "Stopped after 12 s", "Failed after 3 s", or the verb alone; "Working"
    /// while it runs.
    #[must_use]
    pub fn lead(&self) -> String {
        if self.running {
            return "Working".to_owned();
        }
        let took = self.took.map(crate::kit::duration);
        match (self.ended, took) {
            (Ended::Complete, Some(t)) => format!("Worked {t}"),
            (Ended::Stopped, Some(t)) => format!("Stopped after {t}"),
            (Ended::Failed, Some(t)) => format!("Failed after {t}"),
            (Ended::Complete, None) => "Worked".to_owned(),
            (Ended::Stopped, None) => "Stopped".to_owned(),
            (Ended::Failed, None) => "Failed".to_owned(),
        }
    }

    /// What the work was, the weightiest first: "Ran a command", "Read 3 files", then
    /// "2 more" for the steps not named.
    #[must_use]
    pub fn what(&self) -> Vec<String> {
        let say = |n: u32, one: &str, many: &str| {
            if n == 1 { one.to_owned() } else { many.replace('#', &n.to_string()) }
        };
        let edited = u32::try_from(self.edited.len()).unwrap_or(u32::MAX);
        let kinds = [
            (
                edited,
                match self.edited.as_slice() {
                    [one] => format!("Edited {one}"),
                    _ => format!("Edited {edited} files"),
                },
                self.edit_calls,
            ),
            (self.ran, say(self.ran, "Ran a command", "Ran # commands"), self.ran),
            (self.read, say(self.read, "Read a file", "Read # files"), self.read),
            (self.searched, say(self.searched, "Searched once", "Searched # times"), self.searched),
            (
                self.delegated,
                say(self.delegated, "Started a subagent", "Started # subagents"),
                self.delegated,
            ),
        ];
        let named: Vec<_> = kinds.into_iter().filter(|(n, ..)| *n > 0).take(NAMED).collect();
        let steps_named = named.iter().map(|(.., steps)| *steps).fold(0, u32::saturating_add);
        let rest = self.steps.saturating_sub(steps_named);
        let mut out: Vec<String> = named.into_iter().map(|(_, words, _)| words).collect();
        match (out.is_empty(), rest) {
            (_, 0) => {}
            (true, n) => out.push(say(n, "1 step", "# steps")),
            (false, n) => out.push(format!("{n} more")),
        }
        out
    }

    /// The fold's line, what the work was: "Edited notes.txt · Read 3 files · 2 more"; the
    /// lead alone for a turn that called nothing.
    #[must_use]
    pub fn line(&self) -> String {
        let what = self.what();
        if what.is_empty() { self.lead() } else { what.join(" \u{b7} ") }
    }

    /// The fold as one sentence, for whoever reads it aloud: "Worked 55 s: Edited b.rs · Ran 2
    /// commands".
    #[must_use]
    pub fn label(&self) -> String {
        let what = self.what();
        if what.is_empty() {
            self.lead()
        } else {
            format!("{}: {}", self.lead(), what.join(" \u{b7} "))
        }
    }

    /// What stands at the fold's right, apart from its line: how long the turn took ("55 s"),
    /// or how it ended short ("Stopped after 12 s"); nothing when the line says it already.
    #[must_use]
    pub fn when(&self) -> Option<String> {
        if self.running || self.what().is_empty() {
            return None;
        }
        match (self.ended, self.took) {
            (Ended::Complete, took) => took.map(crate::kit::duration),
            _ => Some(self.lead()),
        }
    }
}

#[cfg(test)]
mod tests {
    use slopty_core::WallMs;
    use slopty_proto::thread::detail::{EditDetail, ExecDetail, ExecStatus, ReadDetail};
    use slopty_proto::thread::wire::Outcome;
    use slopty_proto::thread::{Changed, Clipped, Patch, ToolCall, Usage, UserMessage};

    use super::*;
    use crate::conversation::thread::fixtures;

    fn item(id: &str, turn: u32, body: ItemBody) -> Item {
        Item { id: ItemId(id.to_owned()), turn: TurnId(turn), at_ms: WallMs::ZERO, body }
    }

    fn user(id: &str, turn: u32) -> Item {
        item(
            id,
            turn,
            ItemBody::User(UserMessage {
                text: Clipped::whole("Fix it"),
                images: Vec::new(),
                command: None,
                intent: None,
            }),
        )
    }

    fn text(id: &str, turn: u32) -> Item {
        item(id, turn, ItemBody::Text(Clipped::whole("Done.")))
    }

    fn tool(id: &str, turn: u32, kind: &str, detail: Option<ToolDetail>) -> Item {
        item(
            id,
            turn,
            ItemBody::Tool(Box::new(ToolCall {
                name: kind.to_owned(),
                kind: kind.to_owned(),
                title: String::new(),
                input: Clipped::default(),
                state: ToolState::Completed,
                output: None,
                images: Vec::new(),
                detail,
                child: None,
                ended_ms: None,
            })),
        )
    }

    fn exec(id: &str, turn: u32) -> Item {
        tool(
            id,
            turn,
            kind::EXEC,
            Some(ToolDetail::Exec(ExecDetail {
                command: Clipped::whole("cargo test"),
                description: None,
                cwd: None,
                background: false,
                task: None,
                status: ExecStatus::Done,
                exit_code: Some(0),
                stderr: None,
                duration_ms: None,
            })),
        )
    }

    fn read(id: &str, turn: u32) -> Item {
        tool(
            id,
            turn,
            kind::READ,
            Some(ToolDetail::Read(ReadDetail {
                path: "src/lib.rs".to_owned(),
                offset: None,
                limit: None,
                lines: None,
                total_lines: None,
            })),
        )
    }

    fn edit(id: &str, turn: u32, path: &str) -> Item {
        tool(
            id,
            turn,
            kind::EDIT,
            Some(ToolDetail::Edit(EditDetail {
                path: path.to_owned(),
                edits: 1,
                replace_all: false,
                patch: Patch::default(),
            })),
        )
    }

    fn turn(id: u32, state: TurnState, secs: u64) -> Turn {
        Turn {
            id: TurnId(id),
            input: None,
            state,
            started_ms: WallMs::from_millis(1_000),
            ended_ms: Some(WallMs::from_millis(secs.saturating_mul(1_000).saturating_add(1_000))),
            usage: Usage::default(),
            models: Vec::new(),
            changed: Changed::default(),
            before: None,
            after: None,
        }
    }

    fn state(turns: Vec<Turn>, items: Vec<Item>) -> ThreadState {
        let mut state = fixtures::empty();
        state.turns = turns;
        state.items = items;
        state
    }

    #[test]
    fn a_settled_turn_folds_its_work_under_its_message_and_over_its_answer() {
        let state = state(
            vec![turn(1, TurnState::Complete, 55)],
            vec![user("u", 1), exec("x", 1), text("mid", 1), read("r", 1), text("end", 1)],
        );
        let mut open = HashSet::new();
        let rows = build(Input {
            state: &state,
            unshown: &[],
            open: &open,
            shut: &HashSet::new(),
            kept: &HashSet::new(),
            groups: &HashSet::new(),
        });
        let id = |s: &str| ItemId(s.to_owned());
        assert_eq!(
            rows,
            [
                Row::User { item: id("u") },
                Row::Fold { turn: TurnId(1), part: 0, open: false },
                Row::Text { item: id("end") },
            ]
        );
        open.insert(TurnId(1));
        let rows = build(Input {
            state: &state,
            unshown: &[],
            open: &open,
            shut: &HashSet::new(),
            kept: &HashSet::new(),
            groups: &HashSet::new(),
        });
        assert_eq!(
            rows,
            [
                Row::User { item: id("u") },
                Row::Fold { turn: TurnId(1), part: 0, open: true },
                Row::Tool { item: id("x") },
                Row::Text { item: id("mid") },
                Row::Tool { item: id("r") },
                Row::Text { item: id("end") },
            ],
            "opened, the work shows in its order"
        );
    }

    /// A message the person sent into a running turn stands where it was sent and splits the
    /// turn's work into a fold before it and one after; the turn opens as one.
    #[test]
    fn a_steer_splits_the_fold_where_it_was_sent() {
        let state = state(
            vec![turn(1, TurnState::Complete, 55)],
            vec![user("u", 1), exec("x", 1), user("steer", 1), read("r", 1), text("end", 1)],
        );
        let mut open = HashSet::new();
        let built = build_spans(Input {
            state: &state,
            unshown: &[],
            open: &open,
            shut: &HashSet::new(),
            kept: &HashSet::new(),
            groups: &HashSet::new(),
        });
        let id = |s: &str| ItemId(s.to_owned());
        assert_eq!(
            built.rows,
            [
                Row::User { item: id("u") },
                Row::Fold { turn: TurnId(1), part: 0, open: false },
                Row::User { item: id("steer") },
                Row::Fold { turn: TurnId(1), part: 1, open: false },
                Row::Text { item: id("end") },
            ]
        );
        assert_eq!(built.spans.get(1), Some(&(0..2)), "the stretch before the steer");
        assert_eq!(built.spans.get(3), Some(&(2..5)), "and after it");
        let key = |ix: usize| built.rows.get(ix).map(Row::key);
        assert_ne!(key(1), key(3), "two rows, two keys");
        open.insert(TurnId(1));
        let rows = build(Input {
            state: &state,
            unshown: &[],
            open: &open,
            shut: &HashSet::new(),
            kept: &HashSet::new(),
            groups: &HashSet::new(),
        });
        assert_eq!(
            rows,
            [
                Row::User { item: id("u") },
                Row::Fold { turn: TurnId(1), part: 0, open: true },
                Row::Tool { item: id("x") },
                Row::User { item: id("steer") },
                Row::Fold { turn: TurnId(1), part: 1, open: true },
                Row::Tool { item: id("r") },
                Row::Text { item: id("end") },
            ]
        );
    }

    /// A plan is read as an answer is: it stands outside a settled turn's fold where it was
    /// proposed, is not one of the fold's steps, and joins no group of quiet calls.
    #[test]
    fn a_plan_stands_outside_the_fold_and_out_of_a_group() {
        let plan = |id: &str, turn: u32| {
            tool(id, turn, kind::PLAN, Some(ToolDetail::Plan { text: Clipped::whole("1. Do") }))
        };
        let settled = state(
            vec![turn(1, TurnState::Complete, 5)],
            vec![user("u", 1), read("r", 1), plan("p", 1), text("end", 1)],
        );
        let none = HashSet::new();
        let rows = build(Input {
            state: &settled,
            unshown: &[],
            open: &none,
            shut: &HashSet::new(),
            kept: &HashSet::new(),
            groups: &HashSet::new(),
        });
        let id = |s: &str| ItemId(s.to_owned());
        assert_eq!(
            rows,
            [
                Row::User { item: id("u") },
                Row::Fold { turn: TurnId(1), part: 0, open: false },
                Row::Tool { item: id("p") },
                Row::Text { item: id("end") },
            ]
        );
        let t = turn(1, TurnState::Complete, 5);
        assert_eq!(Fold::of(&t, &settled.items).steps, 1, "the read, not the plan");

        let mut live = turn(1, TurnState::Active, 0);
        live.ended_ms = None;
        let mut working =
            state(vec![live], vec![user("u", 1), read("r1", 1), plan("p", 1), read("r2", 1)]);
        working.status.phase = Phase::Working;
        let rows = build(Input {
            state: &working,
            unshown: &[],
            open: &none,
            shut: &HashSet::new(),
            kept: &HashSet::new(),
            groups: &HashSet::new(),
        });
        assert!(rows.contains(&Row::Tool { item: id("p") }), "{rows:?}");
        assert!(!rows.iter().any(|r| matches!(r, Row::Group { .. })), "{rows:?}");
    }

    /// The turn under way shows its work under its line, open, as it comes; the reader may
    /// fold it, and it says it works at its foot either way.
    #[test]
    fn the_live_turns_work_is_open_under_its_line_and_it_says_it_works() {
        let mut live = turn(2, TurnState::Active, 0);
        live.ended_ms = None;
        let mut state = state(
            vec![turn(1, TurnState::Complete, 3), live],
            vec![user("u1", 1), text("a1", 1), user("u2", 2), exec("x", 2), text("a2", 2)],
        );
        state.status.phase = Phase::Working;
        let rows = build(Input {
            state: &state,
            unshown: &[],
            open: &HashSet::new(),
            shut: &HashSet::new(),
            kept: &HashSet::new(),
            groups: &HashSet::new(),
        });
        let id = |s: &str| ItemId(s.to_owned());
        assert_eq!(
            rows,
            [
                Row::User { item: id("u1") },
                Row::Text { item: id("a1") },
                Row::User { item: id("u2") },
                Row::Fold { turn: TurnId(2), part: 0, open: true },
                Row::Tool { item: id("x") },
                Row::Text { item: id("a2") },
                Row::Working { turn: TurnId(2) },
            ],
            "a turn with nothing but its answer has no fold"
        );
        let shut = HashSet::from([TurnId(2)]);
        let rows = build(Input {
            state: &state,
            unshown: &[],
            open: &HashSet::new(),
            shut: &shut,
            kept: &HashSet::new(),
            groups: &HashSet::new(),
        });
        assert_eq!(
            rows.get(2..),
            Some(
                &[
                    Row::User { item: id("u2") },
                    Row::Fold { turn: TurnId(2), part: 0, open: false },
                    Row::Text { item: id("a2") },
                    Row::Working { turn: TurnId(2) },
                ][..]
            ),
            "folded by the reader, its answer so far stays"
        );
        state.status.phase = Phase::Idle;
        let rows = build(Input {
            state: &state,
            unshown: &[],
            open: &HashSet::new(),
            shut: &HashSet::new(),
            kept: &HashSet::new(),
            groups: &HashSet::new(),
        });
        assert!(
            !rows.iter().any(|r| matches!(r, Row::Working { .. })),
            "an agent that says it is idle is not working on a turn it never ended"
        );
    }

    #[test]
    fn a_message_on_its_way_is_a_bubble_and_a_queued_one_waits_in_the_bar() {
        let state = state(Vec::new(), Vec::new());
        let thread = state.meta.id;
        let sent = |delivery, outcome| Sent {
            id: IntentId::new(),
            thread,
            intent: Intent::Send { text: "hi".to_owned(), delivery, attachments: vec![] },
            outcome,
        };
        let now = sent(Delivery::Steer, None);
        let queued = sent(Delivery::Queue, None);
        let refused = sent(Delivery::Queue, Some(Outcome::Refused { reason: "No".to_owned() }));
        let unshown = [&now, &queued, &refused];
        let rows = build(Input {
            state: &state,
            unshown: &unshown,
            open: &HashSet::new(),
            shut: &HashSet::new(),
            kept: &HashSet::new(),
            groups: &HashSet::new(),
        });
        assert_eq!(rows, [Row::Sending { intent: now.id }, Row::Sending { intent: refused.id }]);
    }

    #[test]
    fn a_fold_names_two_kinds_of_work_and_counts_the_rest() {
        let t = turn(1, TurnState::Complete, 55);
        let items =
            [exec("x", 1), read("r", 1), read("r2", 1), edit("e", 1, "a/b.rs"), exec("y", 1)];
        let fold = Fold::of(&t, &items);
        assert_eq!(fold.line(), "Edited b.rs \u{b7} Ran 2 commands \u{b7} 2 more");
        assert_eq!(fold.when().as_deref(), Some("55 s"));
        assert_eq!(fold.label(), "Worked 55 s: Edited b.rs \u{b7} Ran 2 commands \u{b7} 2 more");
        let fold = Fold::of(&t, &[exec("x", 1), read("r", 1), tool("m", 1, kind::MCP, None)]);
        assert_eq!(fold.line(), "Ran a command \u{b7} Read a file \u{b7} 1 more");
        let stopped =
            Fold::of(&turn(1, TurnState::Interrupted, 12), &[tool("m", 1, "other", None)]);
        assert_eq!(stopped.line(), "1 step");
        assert_eq!(stopped.when().as_deref(), Some("Stopped after 12 s"));
        let mut untimed = turn(1, TurnState::Complete, 0);
        untimed.ended_ms = None;
        let quiet = Fold::of(&untimed, &[]);
        assert_eq!((quiet.line().as_str(), quiet.when()), ("Worked", None));
    }

    /// A turn whose end never came folds once a newer turn began.
    #[test]
    fn a_turn_left_open_folds_once_the_next_begins() {
        let state = state(
            vec![turn(1, TurnState::Active, 0), turn(2, TurnState::Active, 0)],
            vec![user("u1", 1), exec("x", 1), text("a1", 1), user("u2", 2), read("r", 2)],
        );
        let rows = build(Input {
            state: &state,
            unshown: &[],
            open: &HashSet::new(),
            shut: &HashSet::new(),
            kept: &HashSet::new(),
            groups: &HashSet::new(),
        });
        let id = |s: &str| ItemId(s.to_owned());
        assert_eq!(
            rows,
            [
                Row::User { item: id("u1") },
                Row::Fold { turn: TurnId(1), part: 0, open: false },
                Row::Text { item: id("a1") },
                Row::User { item: id("u2") },
                Row::Fold { turn: TurnId(2), part: 0, open: true },
                Row::Tool { item: id("r") },
            ],
            "the last is still under way, its work open"
        );
    }

    /// In the live turn's open work, quiet calls done one after another are one line, which
    /// opens to show them; a lone one, and a call that acts, stay rows of their own.
    #[test]
    fn quiet_calls_in_a_row_are_one_line_in_the_live_turn() {
        let state = state(
            vec![turn(1, TurnState::Active, 0)],
            vec![user("u", 1), read("r1", 1), read("r2", 1), exec("x", 1), read("r3", 1)],
        );
        let id = |s: &str| ItemId(s.to_owned());
        let mut groups = HashSet::new();
        let built = |groups: &HashSet<ItemId>| {
            let none = HashSet::new();
            build(Input {
                state: &state,
                unshown: &[],
                open: &none,
                shut: &none,
                kept: &HashSet::new(),
                groups,
            })
        };
        assert_eq!(
            built(&groups),
            [
                Row::User { item: id("u") },
                Row::Fold { turn: TurnId(1), part: 0, open: true },
                Row::Group { first: id("r1"), open: false },
                Row::Tool { item: id("x") },
                Row::Tool { item: id("r3") },
            ]
        );
        groups.insert(id("r1"));
        assert_eq!(
            built(&groups).get(2..5),
            Some(
                &[
                    Row::Group { first: id("r1"), open: true },
                    Row::Tool { item: id("r1") },
                    Row::Tool { item: id("r2") },
                ][..]
            )
        );
        let items = state.items.get(1..3).unwrap_or_default();
        assert_eq!(Fold::of_calls(items).line(), "Read 2 files");
    }

    #[test]
    fn a_recorded_session_builds_its_rows_with_every_settled_turn_folded() {
        let state = fixtures::thread("tools");
        let rows = build(Input {
            state: &state,
            unshown: &[],
            open: &HashSet::new(),
            shut: &HashSet::new(),
            kept: &HashSet::new(),
            groups: &HashSet::new(),
        });
        assert!(rows.iter().any(|r| matches!(r, Row::Fold { .. })), "{rows:?}");
        assert!(!rows.iter().any(|r| matches!(r, Row::Tool { .. })), "every call is folded");
        let keys: HashSet<u64> = rows.iter().map(Row::key).collect();
        assert_eq!(keys.len(), rows.len(), "every row has its own key");
    }
}

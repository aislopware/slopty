//! The pure reducers: a thread's state and a worker's thread table, as the worker keeps them
//! and every client mirrors them.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use slopty_core::WallMs;

use super::wire::{Draft, Page, PullSeen, RequestCard, TableFrame, ThreadRow, TurnEnded};
use super::{
    Action, AgentScreen, BackgroundTask, Changed, Clipped, Command, Cursor, Edge, Goal, Item,
    ItemBody, ItemId, Meters, PartKey, Pending, Plan, Request, Status, ThreadId, ThreadMeta,
    ToolState, Turn, TurnId, TurnState,
};

/// Settled requests a thread keeps, newest last, so a client that comes back still sees who
/// answered what.
pub const RESOLVED_KEPT: usize = 32;

/// The longest last line a [`ThreadRow`] carries, in characters.
const LAST_LINE_CHARS: usize = 160;

/// A thread as far as its actions have brought it.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ThreadState {
    /// What it is.
    pub meta: ThreadMeta,
    /// Where it is.
    pub status: Status,
    /// Its turns, by number: all of them, or the last ones when `older`.
    pub turns: Vec<Turn>,
    /// Their items, in order.
    pub items: Vec<Item>,
    /// There are turns before the first one here ([`super::wire::ThreadRequest::Page`]).
    pub older: bool,
    /// Its open requests, then up to [`RESOLVED_KEPT`] settled ones, in the order they opened.
    pub requests: Vec<Request>,
    /// The messages waiting to go.
    pub pending: Vec<Pending>,
    /// The plan.
    pub plan: Option<Plan>,
    /// The background work.
    pub tasks: Vec<BackgroundTask>,
    /// The meters.
    pub meters: Meters,
    /// The commands the composer offers.
    pub commands: Vec<Command>,
    /// Whether its tree holds changes the person has not kept ([`Action::ToReview`]).
    pub to_review: bool,
    /// The goal its agent works toward ([`Action::GoalSet`]).
    pub goal: Option<Goal>,
    /// The windows and displays its agent drives ([`Action::ScreensSet`]).
    pub screens: Vec<AgentScreen>,
    /// Its branch's pull request ([`Action::PullSeen`]).
    pub pull: Option<PullSeen>,
    /// The last turn the person has seen, on any device ([`Action::Seen`]).
    pub seen: TurnId,
    /// What the person was writing to it and has not sent ([`Action::DraftSet`]).
    pub draft: Option<Draft>,
}

impl ThreadState {
    /// A thread with nothing in it yet.
    #[must_use]
    pub fn new(meta: ThreadMeta) -> Self {
        Self {
            meta,
            status: Status::default(),
            turns: Vec::new(),
            items: Vec::new(),
            older: false,
            requests: Vec::new(),
            pending: Vec::new(),
            plan: None,
            tasks: Vec::new(),
            meters: Meters::default(),
            commands: Vec::new(),
            to_review: false,
            goal: None,
            screens: Vec::new(),
            pull: None,
            seen: TurnId::BEFORE,
            draft: None,
        }
    }

    /// Apply one action. Pure: the same actions in the same order give the same state on the
    /// worker and on every client. An action about a turn or an item this state does not hold
    /// (one paged out) changes nothing.
    pub fn apply(&mut self, action: &Action) {
        match action {
            Action::Meta(meta) => self.meta = (**meta).clone(),
            Action::Status(status) => self.status = status.clone(),
            Action::TurnStarted(turn) => self.put_turn(turn.clone()),
            Action::TurnEnded { turn, state, usage, ended_ms } => {
                if let Some(t) = self.turn_mut(*turn) {
                    t.state = state.clone();
                    t.usage = usage.clone();
                    t.ended_ms = Some(*ended_ms);
                }
            }
            Action::ItemStarted(item) | Action::ItemCompleted(item) => self.put_item(item.clone()),
            Action::ItemUpdated(item) => {
                let mut item = item.clone();
                if let Some(old) = self.item(&item.id) {
                    hold_final(old, &mut item);
                }
                self.put_item(item);
            }
            Action::Append { item, part, text } => {
                if let Some(at) = self.item_index(item)
                    && let Some(found) = self.items.get_mut(at)
                {
                    append(&mut found.body, *part, text);
                }
            }
            Action::ItemRemoved { item } => {
                if let Some(at) = self.item_index(item) {
                    self.items.remove(at);
                }
            }
            Action::RequestOpened(request) => {
                match self.requests.iter_mut().find(|r| r.id == request.id) {
                    Some(have) => *have = (**request).clone(),
                    None => self.requests.push((**request).clone()),
                }
                self.trim_requests();
            }
            Action::RequestResolved { id, state } => {
                if let Some(have) = self.requests.iter_mut().find(|r| r.id == *id) {
                    have.state = state.clone();
                }
                self.trim_requests();
            }
            Action::PendingSet(pending) => self.pending.clone_from(pending),
            Action::PlanSet(plan) => self.plan.clone_from(plan),
            Action::TasksSet(tasks) => self.tasks.clone_from(tasks),
            Action::MetersSet(meters) => self.meters = meters.clone(),
            Action::CommandsSet(commands) => self.commands.clone_from(commands),
            Action::Truncated { after } => {
                let kept = |turn: TurnId| after.is_some_and(|last| turn <= last);
                self.turns.retain(|t| kept(t.id));
                self.items.retain(|i| kept(i.turn));
            }
            Action::Snapshot { turn, edge, tree } => {
                if let Some(t) = self.turn_mut(*turn) {
                    match edge {
                        Edge::Before => t.before = Some(tree.clone()),
                        Edge::After => t.after = Some(tree.clone()),
                    }
                }
            }
            Action::ToReview(to_review) => self.to_review = *to_review,
            Action::GoalSet(goal) => self.goal.clone_from(goal),
            Action::ScreensSet(screens) => self.screens.clone_from(screens),
            Action::PullSeen(pull) => self.pull.clone_from(pull),
            Action::Seen(turn) => self.seen = self.seen.max(*turn),
            Action::DraftSet(draft) => self.draft = Some(draft.clone()),
        }
    }

    /// Put older turns in front of what this state holds.
    pub fn prepend(&mut self, page: &Page) {
        let first = self.turns.first().map(|t| t.id);
        let older = |id: TurnId| first.is_none_or(|first| id < first);
        let mut turns: Vec<Turn> = page.turns.iter().filter(|t| older(t.id)).cloned().collect();
        let mut items: Vec<Item> = page
            .items
            .iter()
            .filter(|i| older(i.turn) && self.item_index(&i.id).is_none())
            .cloned()
            .collect();
        turns.append(&mut self.turns);
        items.append(&mut self.items);
        self.turns = turns;
        self.items = items;
        self.older = page.older;
    }

    /// This state cut to its last `turns` turns: what a snapshot carries.
    #[must_use]
    pub fn window(&self, turns: u32) -> Self {
        let keep = usize::try_from(turns).unwrap_or(usize::MAX);
        let skip = self.turns.len().saturating_sub(keep);
        if skip == 0 {
            return self.clone();
        }
        let from = self.turns.get(skip).map_or(TurnId(u32::MAX), |t| t.id);
        let mut out = self.clone();
        out.turns = self.turns.iter().skip(skip).cloned().collect();
        out.items = self.items.iter().filter(|i| i.turn >= from).cloned().collect();
        out.older = true;
        out
    }

    /// Up to `turns` turns before `before`, with their items: the page a client asked for.
    #[must_use]
    pub fn page(&self, before: TurnId, turns: u32) -> Page {
        let earlier: Vec<&Turn> = self.turns.iter().filter(|t| t.id < before).collect();
        let keep = usize::try_from(turns).unwrap_or(usize::MAX);
        let skip = earlier.len().saturating_sub(keep);
        let taken: Vec<Turn> = earlier.iter().skip(skip).map(|t| (*t).clone()).collect();
        let from = if skip == 0 && !self.older {
            TurnId::BEFORE
        } else {
            taken.first().map_or(before, |t| t.id)
        };
        let items =
            self.items.iter().filter(|i| i.turn >= from && i.turn < before).cloned().collect();
        Page { turns: taken, items, older: skip > 0 || self.older }
    }

    /// The item `id`.
    #[must_use]
    pub fn item(&self, id: &ItemId) -> Option<&Item> {
        self.item_index(id).and_then(|at| self.items.get(at))
    }

    /// The turn `id`.
    #[must_use]
    pub fn turn(&self, id: TurnId) -> Option<&Turn> {
        self.turns.binary_search_by_key(&id, |t| t.id).ok().and_then(|at| self.turns.get(at))
    }

    /// The last turn.
    #[must_use]
    pub fn last_turn(&self) -> Option<&Turn> {
        self.turns.last()
    }

    /// The requests still open.
    pub fn open_requests(&self) -> impl Iterator<Item = &Request> {
        self.requests.iter().filter(|r| r.is_open())
    }

    /// Its latest turn, once it ended ([`ThreadRow::ended`]); `None` while a turn is under way
    /// or before the first.
    #[must_use]
    pub fn ended(&self) -> Option<TurnEnded> {
        let last = self.last_turn()?;
        let at_ms = last.ended_ms?;
        Some(TurnEnded {
            turn: last.id,
            at_ms,
            ran_ms: at_ms.as_millis().saturating_sub(last.started_ms.as_millis()),
            answered: matches!(last.state, TurnState::Complete),
        })
    }

    /// The thread's row in its worker's table.
    #[must_use]
    pub fn row(&self, updated_ms: WallMs) -> ThreadRow {
        let changed = self.turns.iter().fold(Changed::default(), |sum, t| Changed {
            added: sum.added.saturating_add(t.changed.added),
            removed: sum.removed.saturating_add(t.changed.removed),
        });
        ThreadRow {
            id: self.meta.id,
            agent: self.meta.agent.clone(),
            title: self.meta.title.clone(),
            status: self.status.clone(),
            requests: self
                .open_requests()
                .map(|r| RequestCard {
                    id: r.id.clone(),
                    item: r.item.clone(),
                    kind: r.kind.clone(),
                    title: r.title.clone(),
                    options: r.options.clone(),
                    buttons: super::wire::NoteChoice::of(r),
                    opened_ms: r.opened_ms,
                })
                .collect(),
            last_line: self.last_line(),
            doing: self.doing(),
            changed,
            terminal: self.meta.terminal,
            cwd: Some(self.meta.cwd.clone()).filter(|cwd| !cwd.is_empty()),
            repo: None,
            repo_id: None,
            parent: self.meta.parent.clone(),
            drive: self.meta.drive.clone(),
            caps: self.meta.caps.clone(),
            facts: self.meta.facts.clone(),
            to_review: self.to_review,
            pull: self.pull.clone(),
            meters: self.meters.clone(),
            ended: self.ended(),
            seen: self.seen,
            draft: self.draft.clone(),
            updated_ms,
        }
    }

    fn doing(&self) -> Option<String> {
        self.items.iter().rev().find_map(|item| match &item.body {
            ItemBody::Tool(call)
                if matches!(call.state, ToolState::Running | ToolState::Pending { .. }) =>
            {
                Some(call.title.clone())
            }
            _ => None,
        })
    }

    fn last_line(&self) -> Option<String> {
        self.items.iter().rev().find_map(|item| match &item.body {
            ItemBody::Text(text) => text
                .text
                .lines()
                .rev()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .map(|line| line.chars().take(LAST_LINE_CHARS).collect()),
            _ => None,
        })
    }

    fn turn_mut(&mut self, id: TurnId) -> Option<&mut Turn> {
        self.turns.binary_search_by_key(&id, |t| t.id).ok().and_then(|at| self.turns.get_mut(at))
    }

    /// A turn the agent tells again keeps the snapshots the worker took of it, since an
    /// adapter's turn never carries them ([`Action::Snapshot`] does).
    fn put_turn(&mut self, mut turn: Turn) {
        match self.turns.binary_search_by_key(&turn.id, |t| t.id) {
            Ok(at) => {
                if let Some(have) = self.turns.get_mut(at) {
                    turn.before = turn.before.or_else(|| have.before.take());
                    turn.after = turn.after.or_else(|| have.after.take());
                    *have = turn;
                }
            }
            Err(at) => self.turns.insert(at, turn),
        }
    }

    /// Searched from the end, since the items that change are the newest.
    fn item_index(&self, id: &ItemId) -> Option<usize> {
        self.items.iter().rposition(|i| i.id == *id)
    }

    fn put_item(&mut self, item: Item) {
        match self.item_index(&item.id).and_then(|at| self.items.get_mut(at)) {
            Some(have) => *have = item,
            None => self.items.push(item),
        }
    }

    fn trim_requests(&mut self) {
        let settled = self.requests.iter().filter(|r| !r.is_open()).count();
        let mut over = settled.saturating_sub(RESOLVED_KEPT);
        self.requests.retain(|r| {
            if over > 0 && !r.is_open() {
                over = over.saturating_sub(1);
                return false;
            }
            true
        });
    }
}

/// A tool call that is over stays over under an update; only its completion replaces it.
fn hold_final(old: &Item, new: &mut Item) {
    if let (ItemBody::Tool(was), ItemBody::Tool(now)) = (&old.body, &mut new.body)
        && was.state != now.state
        && !was.state.may_become(&now.state)
    {
        now.state = was.state.clone();
    }
}

fn append(body: &mut ItemBody, part: PartKey, text: &str) {
    let target: Option<&mut Clipped> = match (body, part) {
        (ItemBody::Text(c) | ItemBody::Reasoning(c), PartKey::Body) => Some(c),
        (ItemBody::User(m), PartKey::Body) => Some(&mut m.text),
        (ItemBody::Notice(n), PartKey::Body) => Some(&mut n.text),
        (ItemBody::Tool(call), PartKey::Input) => Some(&mut call.input),
        (ItemBody::Tool(call), PartKey::Output) => Some(call.output.get_or_insert_default()),
        _ => None,
    };
    if let Some(target) = target {
        target.append(text);
    }
}

/// A worker's thread table as a client mirrors it.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct TableState {
    /// Where it stands.
    pub cursor: Cursor,
    /// The rows, by thread.
    pub rows: BTreeMap<ThreadId, ThreadRow>,
}

impl TableState {
    /// Apply a frame of the table.
    pub fn apply(&mut self, frame: &TableFrame) {
        match frame {
            TableFrame::Snapshot { cursor, rows } => {
                self.cursor = *cursor;
                self.rows = rows.iter().map(|r| (r.id, r.clone())).collect();
            }
            TableFrame::Delta { cursor, rows, removed } => {
                self.cursor = *cursor;
                for row in rows {
                    self.rows.insert(row.id, row.clone());
                }
                for id in removed {
                    self.rows.remove(id);
                }
            }
        }
    }
}

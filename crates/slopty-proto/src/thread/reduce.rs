//! The pure reducers: a thread's state and a worker's thread table, as the worker keeps them
//! and every client mirrors them.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use slopty_core::WallMs;

use super::wire::{Page, TableFrame, ThreadRow, TurnEnded};
use super::{
    Action, AgentScreen, BackgroundTask, CallNote, Changed, Clipped, Cursor, Draft, Item, ItemBody,
    ItemId, PartKey, Plan, Status, ThreadId, ThreadMeta, ToolState, Turn, TurnId, TurnState,
};

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
    /// The plan.
    pub plan: Option<Plan>,
    /// The background work.
    pub tasks: Vec<BackgroundTask>,
    /// The windows and displays its agent drives ([`Action::ScreensSet`]).
    pub screens: Vec<AgentScreen>,
    /// The last turn the person has seen, on any device ([`Action::Seen`]).
    pub seen: TurnId,
    /// What its agent's own prompt box holds as the person types ([`Action::DraftSet`]).
    pub draft: Option<Draft>,
    /// The prompts queued in its agent's own TUI behind the turn, in the order they go
    /// ([`Action::QueuedSet`]).
    pub queued: Vec<String>,
    /// The person's own name for it, over the agent's ([`Action::NameSet`]); empty for none.
    pub named: String,
    /// When the worker last heard its agent alive during a turn ([`Action::Lease`]).
    pub lease_ms: Option<WallMs>,
    /// What the agent says of its calls beside them, by call ([`Action::CallNoted`]): only for
    /// calls this state holds.
    pub calls: BTreeMap<ItemId, CallNote>,
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
            plan: None,
            tasks: Vec::new(),
            screens: Vec::new(),
            seen: TurnId::BEFORE,
            draft: None,
            queued: Vec::new(),
            named: String::new(),
            lease_ms: None,
            calls: BTreeMap::new(),
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
                self.calls.remove(item);
            }
            Action::QueuedSet(queued) => self.queued.clone_from(queued),
            Action::PlanSet(plan) => self.plan.clone_from(plan),
            Action::TasksSet(tasks) => self.tasks.clone_from(tasks),
            Action::Truncated { after } => {
                let kept = |turn: TurnId| after.is_some_and(|last| turn <= last);
                self.turns.retain(|t| kept(t.id));
                self.items.retain(|i| kept(i.turn));
                self.keep_calls();
            }
            Action::ScreensSet(screens) => self.screens.clone_from(screens),
            Action::Seen(turn) => self.seen = self.seen.max(*turn),
            Action::DraftSet(draft) => self.draft.clone_from(draft),
            Action::NameSet(name) => self.named.clone_from(name),
            Action::Lease(at) => {
                self.lease_ms = Some(self.lease_ms.map_or(*at, |had| had.max(*at)));
            }
            Action::CallNoted { item, note } => {
                if self.item_index(item).is_some() {
                    let note = match self.calls.get(item) {
                        Some(had) => note.clone().over(had),
                        None => note.clone(),
                    };
                    self.calls.insert(item.clone(), note);
                }
            }
        }
    }

    /// The call notes of calls this state no longer holds go.
    fn keep_calls(&mut self) {
        let held: std::collections::BTreeSet<&ItemId> = self.items.iter().map(|i| &i.id).collect();
        self.calls.retain(|id, _| held.contains(id));
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
        for (id, note) in &page.calls {
            self.calls.entry(id.clone()).or_insert_with(|| note.clone());
        }
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
        out.keep_calls();
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
        let items: Vec<Item> =
            self.items.iter().filter(|i| i.turn >= from && i.turn < before).cloned().collect();
        let calls = self
            .calls
            .iter()
            .filter(|(id, _)| items.iter().any(|i| i.id == **id))
            .map(|(id, note)| (id.clone(), note.clone()))
            .collect();
        Page { turns: taken, items, older: skip > 0 || self.older, calls }
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
            native: self.meta.native.clone(),
            title: if self.named.is_empty() { self.meta.title.clone() } else { self.named.clone() },
            status: self.status.clone(),
            last_line: self.last_line(),
            doing: self.doing(),
            changed,
            cwd: Some(self.meta.cwd.clone()).filter(|cwd| !cwd.is_empty()),
            repo: None,
            repo_id: None,
            parent: self.meta.parent.clone(),
            drive: self.meta.drive.clone(),
            facts: self.meta.facts.clone(),
            ended: self.ended(),
            began_ms: self
                .turns
                .last()
                .filter(|turn| turn.state == TurnState::Active)
                .map(|turn| turn.started_ms),
            seen: self.seen,
            lease_ms: self.lease_ms,
            created_ms: self.meta.created_ms,
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

    /// A turn told again replaces the one held.
    fn put_turn(&mut self, turn: Turn) {
        match self.turns.binary_search_by_key(&turn.id, |t| t.id) {
            Ok(at) => {
                if let Some(have) = self.turns.get_mut(at) {
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
    /// Apply a frame of the table. A delta's removals go before its rows: a thread removed and
    /// made again under the same id since the client's cursor is in both, and is there after.
    pub fn apply(&mut self, frame: &TableFrame) {
        match frame {
            TableFrame::Snapshot { cursor, rows } => {
                self.cursor = *cursor;
                self.rows = rows.iter().map(|r| (r.id, r.clone())).collect();
            }
            TableFrame::Delta { cursor, rows, removed } => {
                self.cursor = *cursor;
                for id in removed {
                    self.rows.remove(id);
                }
                for row in rows {
                    self.rows.insert(row.id, row.clone());
                }
            }
        }
    }
}

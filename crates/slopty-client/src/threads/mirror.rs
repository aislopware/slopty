//! One followed thread as this client holds it: the worker's state, carried forward frame by
//! frame from a cursor, and how often each item changed, so a view measures again only the
//! rows whose items moved.

use std::collections::HashMap;

use slopty_proto::thread::wire::ThreadFrame;
use slopty_proto::thread::{Action, Cursor, ItemId, ThreadState};

/// A thread's state and the cursor it stands at.
#[derive(Clone, Debug, Default)]
pub struct Mirror {
    state: Option<ThreadState>,
    cursor: Option<Cursor>,
    live: bool,
    /// How many times each item changed since the state was last replaced.
    revs: HashMap<ItemId, u32>,
    /// How many times the state was replaced whole.
    generation: u32,
}

/// What a frame did to a mirror.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Took {
    /// The state moved on.
    Moved,
    /// The frame did not follow on from the cursor: the stream must start again from it.
    Gap,
    /// Not a frame of the state (an expansion), handed back.
    Aside,
}

impl Mirror {
    /// A mirror of what a cache kept: drawn at once, and caught up from its cursor.
    #[must_use]
    pub(crate) fn cached(state: ThreadState, cursor: Cursor) -> Self {
        Self {
            state: Some(state),
            cursor: Some(cursor),
            live: false,
            revs: HashMap::new(),
            generation: 0,
        }
    }

    /// The revision of `item`: it changes whenever the item does, and whenever the whole state
    /// is replaced.
    #[must_use]
    pub fn rev(&self, item: &ItemId) -> u64 {
        let changed = self.revs.get(item).copied().unwrap_or(0);
        (u64::from(self.generation) << 32) | u64::from(changed)
    }

    fn touched(&mut self, action: &Action) {
        let item = match action {
            Action::ItemStarted(item) | Action::ItemUpdated(item) | Action::ItemCompleted(item) => {
                &item.id
            }
            Action::Append { item, .. } | Action::ItemRemoved { item } => item,
            _ => return,
        };
        let rev = self.revs.entry(item.clone()).or_default();
        *rev = rev.wrapping_add(1);
    }

    /// The state, once a snapshot or the cache brought one.
    #[must_use]
    pub const fn state(&self) -> Option<&ThreadState> {
        self.state.as_ref()
    }

    /// Where the state stands in the worker's log.
    #[must_use]
    pub const fn cursor(&self) -> Option<Cursor> {
        self.cursor
    }

    /// Whether the worker's stream has caught this mirror up since the link last came: a mirror
    /// that is not live shows what it last knew.
    #[must_use]
    pub const fn live(&self) -> bool {
        self.live
    }

    /// The link went: what is held is what was last known.
    pub(crate) const fn lost(&mut self) {
        self.live = false;
    }

    /// Take one frame of the thread's stream.
    pub(crate) fn take(&mut self, frame: ThreadFrame) -> Took {
        match frame {
            ThreadFrame::Snapshot { cursor, state } => {
                self.state = Some(*state);
                self.cursor = Some(cursor);
                self.revs.clear();
                self.generation = self.generation.wrapping_add(1);
                self.live = true;
                Took::Moved
            }
            ThreadFrame::Actions { epoch, first, next, actions } => {
                let follows = self.cursor == Some(Cursor { epoch, seq: first });
                let Some(state) = self.state.as_mut().filter(|_| follows) else {
                    self.live = false;
                    return Took::Gap;
                };
                for action in &actions {
                    state.apply(action);
                }
                for action in &actions {
                    self.touched(action);
                }
                self.cursor = Some(Cursor { epoch, seq: next });
                self.live = true;
                Took::Moved
            }
            ThreadFrame::Page(page) => match self.state.as_mut() {
                Some(state) => {
                    state.prepend(&page);
                    Took::Moved
                }
                None => Took::Aside,
            },
            ThreadFrame::Expanded { .. } | ThreadFrame::Review(_) => Took::Aside,
        }
    }
}

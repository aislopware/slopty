//! Going to a turn: a line's author opened the thread at the turn that wrote it.
//!
//! The view scrolls to the turn's first row, the person's message, and stops following the
//! tail. A turn older than the ones held is paged back to, a page at a time, until it comes or
//! the thread has nothing older.

use gpui::{Context, FollowMode, ListOffset, px};
use slopty_proto::thread::TurnId;

use super::ThreadView;
use crate::conversation::thread::find;

impl ThreadView {
    /// Show turn `turn` from its start, paging back to it when it is older than those held.
    pub fn go_to_turn(&mut self, turn: TurnId, cx: &mut Context<Self>) {
        self.going = Some((turn, None));
        self.go_on(cx);
    }

    /// The turn being gone to, while it is.
    #[must_use]
    pub const fn going(&self) -> Option<TurnId> {
        match self.going {
            Some((turn, _)) => Some(turn),
            None => None,
        }
    }

    /// Go on to the turn asked, now that the thread may hold it.
    pub(super) fn go_on(&mut self, cx: &mut Context<Self>) {
        let Some((turn, paged_from)) = self.going else { return };
        let Some(state) = self.state(cx) else { return };
        let first = state.turns.first().map(|t| t.id);
        let older = state.older;
        let Some(at) = state.items.iter().position(|i| i.turn == turn) else {
            let before_held = first.is_some_and(|f| turn < f);
            if before_held && older && paged_from != first {
                self.going = Some((turn, first));
                self.older(cx);
            } else if !before_held || !older {
                // Not in the thread: nothing to go to.
                self.going = None;
            }
            return;
        };
        // Rows not built yet for it: gone on with once they are.
        let Some(ix) = find::row_of(&self.rows, &self.spans, at) else { return };
        self.going = None;
        self.list.set_follow_mode(FollowMode::Normal);
        self.list.scroll_to(ListOffset { item_ix: ix, offset_in_item: px(0.0) });
        cx.notify();
    }
}

#[cfg(test)]
impl ThreadView {
    /// The row at the top of the list.
    pub(crate) fn top_row(&self) -> usize {
        self.list.logical_scroll_top().item_ix
    }
}

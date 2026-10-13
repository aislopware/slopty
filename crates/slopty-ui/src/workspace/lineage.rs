//! Which agent started which: an agent another started carries its parent's thread as a fact
//! on its row, and the navigator sets its tile in under its parent's.

use slopty_client::layout::TileRef;
use slopty_proto::items::ItemKind;
use slopty_proto::thread::ThreadId;

use super::WorkspaceView;

/// The row fact that names the thread which started this one.
const PARENT_FACT: &str = "slopty.parent";

impl WorkspaceView {
    /// The tile `tile`'s agent was started from: its parent thread's, while that is here.
    pub(super) fn lead_of(&self, tile: TileRef) -> Option<TileRef> {
        let thread = match self.item(tile)?.kind {
            ItemKind::Thread { thread } => thread,
            ItemKind::Terminal { session } => self.session_thread(session)?,
            _ => return None,
        };
        let parent: ThreadId = self.thread_facts(thread)?.get(PARENT_FACT)?.parse().ok()?;
        self.tile_of_thread(parent).filter(|lead| *lead != tile)
    }
}

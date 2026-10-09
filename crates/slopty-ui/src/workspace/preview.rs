//! The preview tab: a file opened in passing (from a thread's call, a folder's tree, a review, a
//! path in a shell, a search hit) opens as a preview, its name set in italics. The next file
//! opened that way takes its place, in its pane and at its tab, so reading through a turn's
//! files leaves one tab rather than a row of them. An edit, a double-click on its name or a
//! rename keeps it: it is then a tab like any other. A file picked on purpose (the palette, a
//! new note, a handoff) opens kept, and keeps a preview it lands on.
//!
//! Only a preview in the tab on show is taken over, so opening a file never reaches into a tab
//! out of sight; a preview elsewhere stays where it is and the new one opens beside the focus.
//! `MonoCode`'s `layout.ts` `preview` (audit row 14).

use gpui::{App, Context};
use slopty_client::layout::{Pos, TileRef};
use slopty_core::ItemId;
use slopty_proto::items::ItemKind;

use super::WorkspaceView;

/// The preview and the file on its way to take its place.
#[derive(Debug, Default)]
pub(super) struct Preview {
    /// The file tile open as the preview, while it is one.
    tile: Option<TileRef>,
    /// A preview proposed and not come yet, and where the one it replaces stood.
    into: Option<(ItemId, Pos)>,
}

impl WorkspaceView {
    /// Whether `tile` is the preview.
    pub(super) fn is_preview(&self, tile: TileRef) -> bool {
        self.preview.tile == Some(tile)
    }

    /// Keep `tile`: a preview becomes a tab like any other. Nothing for any other tile.
    pub(super) fn keep_preview(&mut self, tile: TileRef, cx: &mut App) {
        if self.preview.tile.take_if(|t| *t == tile).is_some() {
            self.panes_news(cx);
        }
    }

    /// The preview a new one takes the place of, and where it stands: in the tab on show and
    /// with no edit of its own.
    pub(super) fn preview_to_replace(&self) -> Option<(TileRef, Pos)> {
        let old = self.preview.tile?;
        let shown = self.layout.shown_tab().is_some_and(|tab| tab.pane_of(old).is_some());
        let file = self.item(old).is_some_and(|i| matches!(i.kind, ItemKind::File { .. }));
        if !shown || !file || self.file_facts(old.item).unsaved {
            return None;
        }
        Some((old, self.layout.position(old)?))
    }

    /// `tile`, just proposed, is the preview now; it takes `replacing`'s place when it comes.
    pub(super) fn preview_opened(&mut self, tile: TileRef, replacing: Option<Pos>) {
        self.preview.tile = Some(tile);
        self.preview.into = replacing.map(|at| (tile.item, at));
    }

    /// Where `id`, come now, goes if it is the preview taking another's place.
    pub(super) fn preview_place(&mut self, id: ItemId) -> Option<Pos> {
        self.preview.into.take_if(|(item, _)| *item == id).map(|(_, at)| at)
    }

    /// A file tile's edit keeps it, once the edit is there.
    pub(super) fn keep_edited(&mut self, id: ItemId, cx: &mut Context<Self>) {
        if let Some(tile) = self.preview.tile.filter(|t| t.item == id)
            && self.file_facts(id).unsaved
        {
            self.keep_preview(tile, cx);
        }
    }

    /// A double-click on `tile`'s name: a preview is kept, any other is named.
    pub(super) fn keep_or_rename(
        &mut self,
        tile: TileRef,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_preview(tile) {
            self.keep_preview(tile, cx);
        } else {
            self.start_rename(tile, window, cx);
        }
    }
}

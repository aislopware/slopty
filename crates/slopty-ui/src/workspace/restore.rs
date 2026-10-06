//! What a relaunch puts back beside the arrangement the layout restores: the main window's
//! frame, the face or the TUI each agent's tile showed, and the tiles shown in windows of their
//! own. All three are saved in `layout.json` with the arrangement they frame.

use gpui::Context;
use slopty_client::layout::{Saved, SavedFace, SavedPopout, WindowFrame};
use slopty_proto::items::ItemKind;

use super::WorkspaceView;

/// What the last run left that waits for this one to catch up with it.
#[derive(Debug, Default)]
pub(super) struct Restore {
    /// The main window's frame, as it last stood.
    pub window: Option<WindowFrame>,
    /// The faces picked last run whose tiles' sessions are not attached yet.
    pub faces: Vec<SavedFace>,
    /// The tiles shown in their own windows last run whose streams are not open yet.
    pub popouts: Vec<SavedPopout>,
}

impl Restore {
    /// What `saved` left to put back.
    pub(super) fn of(saved: Option<&Saved>) -> Self {
        saved.map_or_else(Self::default, |saved| Self {
            window: saved.window.clone().filter(WindowFrame::sane),
            faces: saved.faces.clone(),
            popouts: saved.popouts.iter().filter(|p| p.frame.sane()).cloned().collect(),
        })
    }
}

impl WorkspaceView {
    /// Where the main window stood when it last moved, this run or the last one: where it
    /// opens.
    #[must_use]
    pub const fn window_frame(&self) -> Option<&WindowFrame> {
        self.restore.window.as_ref()
    }

    /// The main window stands at `frame` now; saved with the layout.
    pub fn set_window_frame(&mut self, frame: Option<WindowFrame>, cx: &Context<Self>) {
        let Some(frame) = frame else { return };
        if self.restore.window.as_ref() != Some(&frame) {
            self.restore.window = Some(frame);
            self.layout_touched(cx);
        }
    }

    /// The layout as `layout.json` keeps it now: the arrangement, and beside it the window's
    /// frame, the faces picked on the tiles there and the tiles out in their own windows. What
    /// the last run left that this one has not caught up with yet is kept as it was.
    pub(super) fn to_save(&self) -> Saved {
        let mut saved = Saved {
            tiling: self.layout.save(),
            navigator: self.navigator.clone(),
            window: self.restore.window.clone(),
            ..Saved::default()
        };
        let tiles: Vec<_> = self.layout.tiles().collect();
        for tile in &tiles {
            let Some(ItemKind::Terminal { session }) = self.item(*tile).map(|i| &i.kind) else {
                continue;
            };
            if let Some(face) = self.faces.chosen.get(session) {
                saved.faces.push(SavedFace { tile: *tile, face: *face });
            }
        }
        let waiting =
            self.restore.faces.iter().filter(|f| {
                tiles.contains(&f.tile) && !saved.faces.iter().any(|s| s.tile == f.tile)
            });
        let waiting: Vec<SavedFace> = waiting.copied().collect();
        saved.faces.extend(waiting);
        for (item, frame) in self.popouts.frames() {
            if let Some(tile) = self.tile_of(item) {
                saved.popouts.push(SavedPopout { tile, frame: frame.clone() });
            }
        }
        let waiting =
            self.restore.popouts.iter().filter(|p| {
                tiles.contains(&p.tile) && !saved.popouts.iter().any(|s| s.tile == p.tile)
            });
        let waiting: Vec<SavedPopout> = waiting.cloned().collect();
        saved.popouts.extend(waiting);
        saved.frecency.clone_from(&self.frecency);
        saved.looked = self
            .projects_looked()
            .into_iter()
            .map(|(project, l)| slopty_client::layout::SavedLooked {
                project,
                seq: l.seq,
                at_ms: l.at_ms,
            })
            .collect();
        saved
    }

    /// The faces the last run picked, for the tiles whose sessions are attached now: the pick
    /// stands as if made this run.
    pub(super) fn restore_faces(&mut self) {
        let mut waiting = std::mem::take(&mut self.restore.faces);
        waiting.retain(|saved| {
            let Some(ItemKind::Terminal { session }) = self.item(saved.tile).map(|i| &i.kind)
            else {
                return true;
            };
            if !self.terminals.contains_key(session) {
                return true;
            }
            self.faces.chosen.entry(*session).or_insert(saved.face);
            false
        });
        self.restore.faces = waiting;
    }

    /// `item`'s stream is open: a tile that was out in its own window when the last run ended
    /// goes back out, where that window stood.
    pub(super) fn restore_popout(&mut self, item: slopty_core::ItemId, cx: &mut Context<Self>) {
        let Some(tile) = self.tile_of(item) else { return };
        let Some(at) = self.restore.popouts.iter().position(|p| p.tile == tile) else { return };
        let saved = self.restore.popouts.remove(at);
        let placed = crate::window_frame::bounds(&saved.frame, cx);
        self.pop_out_at(item, placed, cx);
    }

    /// Write the layout now if it changed: the app is quitting, and the half-second wait before
    /// a save would outlast it.
    pub fn save_layout_now(&mut self) {
        let Some(path) = self.layout_path.clone() else { return };
        let saved = self.to_save();
        if self.layout_saved.as_ref() == Some(&saved) {
            return;
        }
        super::write_layout(&path, &saved);
        self.layout_saved = Some(saved);
    }
}

//! Where the person is on this client, as the server is told it so a notice goes only where it
//! is wanted (`docs/decisions/ui.md`, "Notifications by presence").
//!
//! [`WorkspaceView::presence`] reads it off the workspace: what kind of seat this is, whether its
//! window is in front, the workspace there, the terminals on screen and the one with the
//! keyboard. The app sends it on every change, and lowers `active` once the person has been
//! away from the machine a while, which only the app can measure.

use gpui::Window;
use slopty_client::layout::TileRef;
use slopty_proto::items::ItemKind;
use slopty_proto::orchestration::TermRef;
use slopty_proto::thread::attention::{Presence, Seat};

use super::WorkspaceView;
use super::projects::worker_id;

impl WorkspaceView {
    /// Where the person is on this client, as the server is to be told.
    #[must_use]
    pub fn presence(&self, window: &Window) -> Presence {
        let mut showing: Vec<TermRef> = self
            .drawn
            .on_screen
            .borrow()
            .iter()
            .filter_map(|item| self.tile_of(*item))
            .filter_map(|tile| self.term_ref(tile))
            .collect();
        showing.sort_by_key(|t| (t.worker, t.session));
        let workspace = self.layout.shown_index().map(|ix| self.project_name_at(ix));
        Presence {
            seat: self.seat(),
            active: self.app_active && window.is_window_active(),
            workspace,
            showing,
            focus: self.focused().and_then(|tile| self.term_ref(tile)),
            listening: true,
        }
    }

    /// A phone, or a tablet with no keyboard on it, is carried; anything else is sat at.
    fn seat(&self) -> Seat {
        let touch = self.theme.density == slopty_theme::Density::TOUCH;
        if self.layout.is_phone() || (touch && !self.hardware_keyboard) {
            Seat::Handheld
        } else {
            Seat::Desk
        }
    }

    /// The terminal `tile` shows, by the server's names for it.
    fn term_ref(&self, tile: TileRef) -> Option<TermRef> {
        let ItemKind::Terminal { session } = self.item(tile)?.kind else { return None };
        Some(TermRef { worker: worker_id(tile.worker)?, session })
    }
}

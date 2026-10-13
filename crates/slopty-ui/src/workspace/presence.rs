//! Where the person is on this client, as the server is told it so a notice goes only where it
//! is wanted (`docs/decisions/ui.md`, "Notifications by presence").
//!
//! [`WorkspaceView::presence`] reads it off the workspace: what kind of seat this is and whether
//! its window is in front. The app sends it on every change, and lowers `active` once the person
//! has been away from the machine a while, which only the app can measure.

use gpui::Window;
use slopty_proto::thread::attention::{Presence, Seat};

use super::WorkspaceView;

impl WorkspaceView {
    /// Where the person is on this client, as the server is to be told: whether its window is
    /// in front, on a phone or at a desk. The shell's own live views say nothing of themselves
    /// yet, so no terminal is named as on screen or focused.
    #[must_use]
    pub fn presence(&self, window: &Window) -> Presence {
        Presence {
            seat: if cfg!(target_os = "ios") { Seat::Handheld } else { Seat::Desk },
            active: self.app_active && window.is_window_active(),
            showing: Vec::new(),
            focus: None,
            listening: true,
        }
    }
}

//! slopty: the right pane's closed tabs, opened again (⌘⇧T, as a browser does). The window keeps
//! the last twenty it closed, in the form the pane's tabs are kept across launches
//! (`kept_tabs`), each with the pane it was in; the key brings back the newest closed in the pane
//! shown. A terminal is not kept: closing it ends its shell, which cannot come back.

use gpui::{App, Context, Window};

use super::{RightSurface, Shell};

/// How many closed tabs a window keeps.
const CLOSED_KEPT: usize = 20;

impl Shell {
    /// Keep `surface` of the pane `key` as closed, before it goes.
    pub(super) fn remember_closed_tab(&mut self, key: &str, surface: RightSurface, cx: &App) {
        if matches!(surface, RightSurface::Terminal(_)) {
            return;
        }
        let Some(kept) = self.kept_surface(key, surface, cx) else { return };
        self.closed_tabs.push((key.to_owned(), kept));
        if self.closed_tabs.len() > CLOSED_KEPT {
            self.closed_tabs.remove(0);
        }
    }

    /// Open the newest tab closed in the shown chat's pane again, in front, the pane open:
    /// whether there was one.
    pub(super) fn reopen_closed_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.active_chat.is_empty() {
            return false;
        }
        let key = self.panel_key(cx);
        let Some(at) = self.closed_tabs.iter().rposition(|(pane, _)| *pane == key) else {
            return false;
        };
        let (_, saved) = self.closed_tabs.remove(at);
        let made = self.restore_surface(&key, saved, window, cx);
        self.set_surfaces_open(true, cx);
        if let Some(made) = made {
            self.set_right_active(made, cx);
        }
        cx.notify();
        made.is_some()
    }
}


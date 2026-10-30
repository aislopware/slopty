//! The views closed last, opened again (⇧⌘T, as a browser does). The window keeps the
//! last twenty it closed, in the form a session's panes are kept across launches (`kept_tabs`),
//! each with the session it stood beside; the key brings back the newest closed beside the
//! session shown, docked at its right with the keyboard. A shell is not kept: closing it ends
//! its terminal, which cannot come back.

use gpui::{App, Context, Window};

use super::panes::Pane;
use super::{RightSurface, Shell};

/// How many closed views a window keeps.
const CLOSED_KEPT: usize = 20;

impl Shell {
    /// Keep `view` of the session `key` as closed, before it goes.
    pub(super) fn remember_closed_tab(&mut self, key: &str, view: RightSurface, cx: &App) {
        let Some(kept) = self.kept_pane(Pane::View(view), cx) else { return };
        self.closed_tabs.push((key.to_owned(), kept));
        if self.closed_tabs.len() > CLOSED_KEPT {
            self.closed_tabs.remove(0);
        }
    }

    /// Open the newest view closed beside the shown session again, with the keyboard: whether
    /// there was one.
    pub(super) fn reopen_closed_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.active_chat.is_empty() {
            return false;
        }
        let key = self.panel_key(cx);
        let Some(at) = self.closed_tabs.iter().rposition(|(session, _)| *session == key) else {
            return false;
        };
        let (_, saved) = self.closed_tabs.remove(at);
        let made = self.restore_pane(&key, saved, window, cx);
        if let Some(made) = made {
            self.focus_pane(made, cx);
        }
        cx.notify();
        made.is_some()
    }
}

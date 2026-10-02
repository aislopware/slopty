//! Secure keyboard entry ([`slopty_platform::secure_input`]): what is typed into the app is
//! kept from every other program on this Mac while the focused tile takes a password (a
//! terminal whose program reads one, a remote window whose password field has the keyboard),
//! or while any app window is in front when the person asks for it always, and never when
//! they turn it off (`[terminal] secure_keyboard_entry`).
//!
//! It follows what it depends on: the workspace's render (focus, the window coming to the
//! front or going), a terminal's changes (a prompt that stops echoing) and a stream's (the
//! worker's field). Tests hold a stand-in switch, so no test turns the Mac's on.

use gpui::{App, Context};
use slopty_platform::secure_input::{SecureInput, Switch, System};
use slopty_proto::items::ItemKind;
use slopty_theme::SecureEntry;

use super::WorkspaceView;

/// The workspace's hold on secure event input.
pub(super) type Secure = SecureInput<Box<dyn Switch>>;

/// The Mac's own switch, or in tests one that reaches nothing.
pub(super) fn secure() -> Secure {
    let switch: Box<dyn Switch> = if cfg!(test) { Box::new(Unswitched) } else { Box::new(System) };
    SecureInput::new(switch)
}

/// A switch that reaches nothing: a test must not turn the Mac's on, so it reads the hold's
/// state instead.
struct Unswitched;

impl Switch for Unswitched {
    fn enable(&self) {}

    fn disable(&self) {}
}

impl WorkspaceView {
    /// Whether what is typed now is to be kept from other programs on this Mac.
    fn wants_secure_input(&self, cx: &App) -> bool {
        let front = cx.active_window().is_some();
        match self.theme.behaviour.secure_entry {
            SecureEntry::Never => false,
            SecureEntry::Always => front,
            SecureEntry::Passwords => front && self.focused_takes_password(cx),
        }
    }

    /// Whether the focused tile takes a password now.
    fn focused_takes_password(&self, cx: &App) -> bool {
        let Some(tile) = self.focused() else { return false };
        match self.item(tile).map(|item| &item.kind) {
            Some(ItemKind::Terminal { session }) => {
                self.terminals.get(session).is_some_and(|v| v.read(cx).at_password_prompt())
            }
            Some(ItemKind::Window { .. } | ItemKind::Display { .. }) => {
                self.screens.get(&tile.item).is_some_and(|v| v.read(cx).in_password_field())
            }
            _ => false,
        }
    }

    /// Turn secure keyboard entry on or off as what it depends on now says.
    pub(super) fn follow_secure_input(&mut self, cx: &Context<Self>) {
        let on = self.wants_secure_input(cx);
        if on != self.secure.is_on() {
            tracing::info!(on, "secure keyboard entry");
            self.secure.set(on);
        }
    }

    /// Whether secure keyboard entry is on.
    #[cfg(test)]
    pub(crate) const fn secure_input_on(&self) -> bool {
        self.secure.is_on()
    }
}

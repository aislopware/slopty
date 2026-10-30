//! Whether the person looks at the window: the one read behind a session marked seen as it is
//! drawn, the sync nudge on coming back and the invariants' watch.
//!
//! The OS says it through the window's focus. The e2e socket holds it instead ([`hold`]): a
//! guest's window is in front or not as the guest's other apps decide, so a session the test has
//! open read as seen on one run and unread on the next, and a frame taken of it differed by its
//! sidebar row.

use gpui::{App, Global, Window};

/// Held: the window counts as looked at whatever the OS says of its focus.
struct Held;

impl Global for Held {}

/// Count the window as looked at from now on, whatever the OS says of its focus.
pub fn hold(cx: &mut App) {
    cx.set_global(Held);
}

/// Whether the person looks at `window`: it is the active window, or [`hold`] says so.
pub fn looking(window: &Window, cx: &App) -> bool {
    window.is_window_active() || cx.has_global::<Held>()
}


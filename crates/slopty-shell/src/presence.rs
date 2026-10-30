//! Whether the person looks at the window: the one read behind a session marked seen as it is
//! drawn, the sync nudge on coming back and the invariants' watch.
//!
//! And what they look at ([`OnScreen`]): the session on screen, whose moments never notify
//! (`docs/decisions/layout.md`, "Attention is an inbox and a jump, never a feed").
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

/// The chat whose session is on screen while the person looks at the window, else none: the
/// app's notes stay quiet for it and take its note back (`slopty_ui`'s `Attention::set_on_screen`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OnScreen(pub Option<String>);

impl Global for OnScreen {}

/// The shell says what the person looks at as it draws: the selected chat while [`looking`],
/// else none. Only a change is set, so a watcher hears each once.
pub fn show(chat: Option<String>, cx: &mut App) {
    if cx.try_global::<OnScreen>().map(|shown| &shown.0) != Some(&chat) {
        cx.set_global(OnScreen(chat));
    }
}

/// The chat on screen, as [`show`] last said.
#[must_use]
pub fn on_screen(cx: &App) -> Option<&str> {
    cx.try_global::<OnScreen>().and_then(|shown| shown.0.as_deref())
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    /// What is on screen is set once per change, so the app's notes hear each change once.
    #[gpui::test]
    fn what_is_on_screen_is_said_once_per_change(cx: &TestAppContext) {
        let heard = std::rc::Rc::new(std::cell::Cell::new(0_u32));
        let _watch = cx.update(|cx| {
            let heard = std::rc::Rc::clone(&heard);
            cx.observe_global::<super::OnScreen>(move |_| heard.set(heard.get().saturating_add(1)))
        });
        cx.update(|cx| super::show(Some("a".into()), cx));
        cx.update(|cx| super::show(Some("a".into()), cx));
        assert_eq!(heard.get(), 1, "the same chat again is no change");
        assert_eq!(cx.update(|cx| super::on_screen(cx).map(str::to_owned)).as_deref(), Some("a"));
        cx.update(|cx| super::show(None, cx));
        assert_eq!(heard.get(), 2, "the window put away is one");
        assert_eq!(cx.update(|cx| super::on_screen(cx).map(str::to_owned)), None);
    }
}

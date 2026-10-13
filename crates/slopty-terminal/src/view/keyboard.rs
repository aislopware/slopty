//! Whether this terminal has the person's keyboard, told to its worker
//! (`TermRequest::Focus`): the program hears focus in and out when it asked to (DEC 1004), a
//! page or an edit a program in the session hands over goes to the client looking at it first,
//! and while any client looks at it Claude Code sends no phone push for it.
//!
//! The keyboard is this view's while its focus handle, or something inside it (its find bar),
//! has the focus of a window in front. It is told on each change, and again on a new link,
//! where the worker starts from nobody looking.

use gpui::{AnyWindowHandle, Context, Subscription, Window};
use slopty_proto::terminal::TermRequest;

use super::TerminalView;

/// The window the view watches its focus in, and what it watches there.
pub(super) type Watch = Option<(AnyWindowHandle, [Subscription; 3])>;

impl TerminalView {
    /// Whether this view has the keyboard in the window in front, as last told to its worker.
    #[must_use]
    pub const fn has_keyboard(&self) -> bool {
        self.keyboard
    }

    /// Watch the focus in `window`, once per window the view is drawn in.
    pub(super) fn watch_keyboard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let here = window.window_handle();
        if self.keyboard_watch.as_ref().is_some_and(|(at, _)| *at == here) {
            return;
        }
        let focus = self.focus.clone();
        let came = cx.on_focus_in(&focus, window, |this, window, cx| {
            this.tell_keyboard(window.is_window_active(), cx);
        });
        let went = cx.on_focus_out(&focus, window, |this, _event, _window, cx| {
            this.tell_keyboard(false, cx);
        });
        let front = cx.observe_window_activation(window, |this, window, cx| {
            let held = window.is_window_active() && this.focus.contains_focused(window, cx);
            this.tell_keyboard(held, cx);
        });
        self.keyboard_watch = Some((here, [came, went, front]));
        let held = window.is_window_active() && self.focus.contains_focused(window, cx);
        self.tell_keyboard(held, cx);
    }

    /// The keyboard came or went: the worker hears it, on a change only.
    fn tell_keyboard(&mut self, held: bool, cx: &Context<Self>) {
        if self.keyboard != held {
            self.keyboard = held;
            self.post(TermRequest::Focus { focused: held }, cx);
        }
    }

    /// A new link starts from nobody looking: it hears the keyboard again if this view has it.
    pub(super) fn keyboard_relinked(&mut self, cx: &Context<Self>) {
        if self.keyboard {
            self.post(TermRequest::Focus { focused: true }, cx);
        }
    }
}

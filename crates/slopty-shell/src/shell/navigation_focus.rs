// Derived from zeron (MIT, github.com/zeronsh/zeron @ 75b7260), see THIRD_PARTY_NOTICES.md
//! Where the keyboard goes back to: a press in the main column keeps it there, and a focus that
//! goes away (an element unmounting) comes back to the pane with the keyboard.
use super::*;

/// Capture establishes a fallback before child listeners run. Inputs can
/// claim focus during bubbling, including listeners that stop propagation.
/// Clicking within an already focused input must not blur it first.
fn focus_navigation_scope(scope: &FocusHandle, window: &mut Window, cx: &mut App) {
    if !scope.contains_focused(window, cx) {
        window.focus(scope, cx);
    }
    let scope = scope.clone();
    window.defer(cx, move |window, cx| {
        // ComposerInput explicitly blurs on mouse-down outside itself. Keep
        // that click in its pane without moving the caret back into an input.
        if window.focused(cx).is_none() {
            window.focus(&scope, cx);
        }
    });
}

/// Cmd/Ctrl+C for a transcript selection once the pane itself holds focus
/// (clicking the transcript blurs the composer, whose `Copy` handles this
/// otherwise). Bubble phase: focused inputs, editors and terminals see the
/// keystroke first, so this only answers when none of them consumed it.
pub(super) fn copy_transcript_selection(event: &gpui::KeyDownEvent, cx: &mut App) {
    let keystroke = &event.keystroke;
    if keystroke.key != "c"
        || !(keystroke.modifiers.platform || keystroke.modifiers.control)
        || keystroke.modifiers.shift
        || keystroke.modifiers.alt
    {
        return;
    }
    if let Some(text) = crate::markdown::selection::selected_text() {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        cx.stop_propagation();
    }
}

impl Shell {
    /// A press in the main column keeps the keyboard in it, before what it lands on claims it.
    pub(super) fn capture_navigation_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        focus_navigation_scope(&self.main_focus, window, cx);
    }

    /// Where a lost focus comes back to, and where it goes with nothing focused: Settings' own
    /// focus there; on a session's page the pane with the keyboard, its view or its terminal;
    /// on a touch screen with the session in front, nowhere a software keyboard would rise.
    pub(super) fn navigation_focus_fallback(
        &mut self,
        cx: &mut Context<Self>,
    ) -> (FocusHandle, FocusHandle) {
        if matches!(self.route, Route::Settings(_)) {
            return (self.settings_focus.clone(), self.unfocused.clone());
        }
        let shell = match self.panes_now(cx).focused() {
            super::panes::Pane::View(view) => {
                let handle = self.view_handle(view, cx);
                return (handle.clone(), handle);
            }
            super::panes::Pane::Shell(n) => {
                self.pane_shells.get(&n).map(|shell| shell.read(cx).focus_handle())
            }
            super::panes::Pane::Session => None,
        };
        if let Some(shell) = shell {
            (shell, self.unfocused.clone())
        } else if crate::shell::touch_screen() {
            // On a touch screen the keyboard waits for a tap in the terminal
            // (`docs/decisions/ui.md`): focus recovered after a tap elsewhere, a sidebar row's,
            // goes where no software keyboard rises.
            (self.unfocused.clone(), self.unfocused.clone())
        } else {
            (self.session_terminal.read(cx).focus_handle(), self.unfocused.clone())
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext, TestAppContext};

    use super::*;

    struct FocusHost {
        root: FocusHandle,
        main: FocusHandle,
        right: FocusHandle,
        input: Entity<ComposerInput>,
        show_input: bool,
    }

    impl Render for FocusHost {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .track_focus(&self.root)
                .flex()
                .child(
                    div()
                        .id("main")
                        .w(px(200.))
                        .h_full()
                        .track_focus(&self.main)
                        .capture_any_mouse_down(cx.listener(|this, _, window, cx| {
                            focus_navigation_scope(&this.main, window, cx);
                        })),
                )
                .child(
                    div()
                        .id("right")
                        .w(px(200.))
                        .h_full()
                        .track_focus(&self.right)
                        .capture_any_mouse_down(cx.listener(|this, _, window, cx| {
                            focus_navigation_scope(&this.right, window, cx);
                        }))
                        .when(self.show_input, |el| el.child(self.input.clone())),
                )
        }
    }

    #[gpui::test]
    fn clicking_read_only_content_keeps_focus_in_its_pane(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_global(Theme::default()));
        let (host, cx) = cx.add_window_view(|_, cx| FocusHost {
            root: cx.focus_handle(),
            main: cx.focus_handle(),
            right: cx.focus_handle(),
            input: cx.new(|cx| ComposerInput::new("Message", cx)),
            show_input: true,
        });
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            let input = host.read(cx).input.focus_handle(cx);
            window.focus(&input, cx);
        });
        // Real input click-away blurs during capture. The pane fallback must
        // preserve keyboard navigation without re-entering the input.
        cx.simulate_click(gpui::point(px(300.), px(250.)), gpui::Modifiers::default());
        cx.update(|window, cx| {
            assert!(host.read(cx).right.is_focused(window));
        });
        cx.simulate_click(gpui::point(px(100.), px(250.)), gpui::Modifiers::default());
        cx.update(|window, cx| {
            assert!(host.read(cx).main.is_focused(window));
        });
    }
}

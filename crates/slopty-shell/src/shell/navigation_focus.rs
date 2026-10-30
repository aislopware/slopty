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

/// Whether the shell may give a pane the keyboard on its own, as a session landed on or switched
/// to, the panes a boot restores, a pane or a view closed and a lost focus recovered do: off a
/// touch screen, and on one only with a hardware keyboard, where a focused terminal raises no
/// soft keyboard. Without one the soft keyboard stays down and the keyboard waits for a tap
/// (`docs/decisions/ui.md`, "On a touch screen the shell never raises the soft keyboard").
pub(super) fn keyboard_moves_unasked(cx: &App) -> bool {
    !touch_screen() || crate::live::key_row::hardware_keyboard(cx)
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
    pub(super) fn capture_navigation_focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        focus_navigation_scope(&self.main_focus, window, cx);
    }

    /// Where a lost focus comes back to, and where it goes with nothing focused: Settings' own
    /// focus there; on a session's page the pane with the keyboard, its view or its terminal;
    /// where the shell may not move the keyboard on its own ([`keyboard_moves_unasked`]),
    /// nowhere a software keyboard would rise, whichever pane is in front.
    pub(super) fn navigation_focus_fallback(
        &mut self,
        cx: &Context<Self>,
    ) -> (FocusHandle, FocusHandle) {
        if matches!(self.route, Route::Settings(_)) {
            return (self.settings_focus.clone(), self.unfocused.clone());
        }
        if !keyboard_moves_unasked(cx) {
            // On a touch screen the keyboard waits for a tap (`docs/decisions/ui.md`): a focus
            // lost to a tap elsewhere (a sidebar row's) or to a pane or a view closing goes
            // where no software keyboard rises.
            return (self.unfocused.clone(), self.unfocused.clone());
        }
        let shell = match self.panes_now(cx).focused() {
            panes::Pane::View(view) => {
                let handle = self.view_handle(view, cx);
                return (handle.clone(), handle);
            }
            panes::Pane::Shell(n) => {
                self.pane_shells.get(&n).map(|shell| shell.read(cx).focus_handle())
            }
            panes::Pane::Session => None,
        };
        let shell = shell.unwrap_or_else(|| self.session_terminal.read(cx).focus_handle());
        (shell, self.unfocused.clone())
    }
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

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

    /// Lays the shell out and lets what that set off run, twice: a focus handed on lands once
    /// the pane it goes to draws.
    fn settle(cx: &mut gpui::VisualTestContext) {
        for _ in 0..2 {
            cx.run_until_parked();
            cx.update(|window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
            });
        }
        cx.run_until_parked();
    }

    /// Whether `pane` of `shell`'s view holds the keyboard, which on a device would raise the
    /// soft keyboard over it.
    fn holds(shell: &Entity<Shell>, pane: panes::Pane, cx: &mut gpui::VisualTestContext) -> bool {
        cx.update(|window, cx| shell.read(cx).pane_holds_keyboard(pane, window, cx))
    }

    /// On a touch screen a session opened leaves the soft keyboard down: neither the landing nor
    /// the panes it shows as it left them hand its terminal the keyboard, which comes when the
    /// person taps it (`docs/decisions/ui.md`, "On a touch screen the shell never raises the
    /// soft keyboard"). With a hardware keyboard, which raises none, its terminal takes it.
    #[gpui::test]
    fn a_session_opened_on_a_touch_screen_leaves_the_keyboard_down(cx: &mut TestAppContext) {
        use super::super::retention_tests::{PHONE, launch};
        let (shell, cx) = launch(cx, PHONE);
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, cx| {
                let terminals = [("a", "t-a"), ("b", "t-b")];
                state.apply_session_terminals(
                    terminals.map(|(chat, at)| (chat.to_owned(), at.to_owned())).into(),
                );
                cx.notify();
            });
            shell.open_chat("a".into(), cx);
        });
        settle(cx);
        assert!(
            shell.read_with(cx, |shell, cx| shell.state.read(cx).selected_chat.is_some()),
            "the session is open"
        );
        assert!(!holds(&shell, panes::Pane::Session, cx), "the session waits for a tap");

        cx.update(|_, cx| crate::live::key_row::set_hardware_keyboard(true, cx));
        shell.update(cx, |shell, cx| shell.open_chat("b".into(), cx));
        settle(cx);
        assert!(holds(&shell, panes::Pane::Session, cx), "a hardware keyboard's: it lands");
    }

    /// On an iPad's touch screen a split closed leaves the soft keyboard down: the session's
    /// terminal beside it is not handed the keyboard the split's shell had. With a hardware
    /// keyboard, which raises none, it is, as on the Mac.
    #[gpui::test]
    fn a_split_closed_on_a_touch_screen_leaves_the_keyboard_down(cx: &mut TestAppContext) {
        use super::super::panes::{Direction, Pane};
        use super::super::retention_tests::{Screen, launch};
        let ipad = Screen { size: (1366.0, 1024.0), scale: 2.0, touch: true, reduced: true };
        let (shell, cx) = launch(cx, ipad);
        shell.update(cx, |shell, cx| {
            shell.state.update(cx, |state, cx| {
                state.apply_session_terminals([("a".to_owned(), "t-a".to_owned())].into());
                cx.notify();
            });
            shell.open_chat("a".into(), cx);
        });
        settle(cx);
        for hardware in [false, true] {
            cx.update(|_, cx| crate::live::key_row::set_hardware_keyboard(hardware, cx));
            cx.update(|_, cx| {
                shell.update(cx, |shell, cx| shell.split_pane(Some(Direction::Right), cx));
            });
            settle(cx);
            let split = shell.read_with(cx, |shell, cx| shell.panes_now(cx).focused());
            assert!(matches!(split, Pane::Shell(_)), "an iPad splits: {split:?}");
            assert!(holds(&shell, split, cx), "the split takes the keyboard");
            assert!(shell.update(cx, Shell::close_focused_pane), "⌘W closes it");
            settle(cx);
            let left = shell.read_with(cx, |shell, cx| shell.panes_now(cx).focused());
            assert_eq!(left, Pane::Session, "the session alone");
            assert_eq!(
                holds(&shell, Pane::Session, cx),
                hardware,
                "the session's terminal holds the keyboard only with a hardware keyboard"
            );
        }
    }
}

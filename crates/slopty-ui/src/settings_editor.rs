//! The in-app settings editor: `settings.toml` in a dialog, saved on ⌘↩.
//!
//! The phone has no editor to hand the file to, and on the Mac a small change should not
//! need one either. The dialog holds the file's text (the commented defaults when there is
//! none) in a monospace field; ⌘↩ or the Save button hands the text back to the app, which
//! parses it and either writes it and applies it, or puts the parse error under the field and
//! keeps the dialog open so the typo can be fixed. Escape or a click outside discards. On the
//! Mac an "Open in editor" button keeps the old path to the default `.toml` editor.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px,
};
use gpui_kit::component::input::{Enter, Escape, InputEvent, Textarea, TextareaState};
use slopty_theme::{Theme, alpha};

use crate::a11y::tab_stop;
use crate::colors::{hsla, hsla_alpha};

/// Key context of the dialog.
pub const CTX: &str = "SettingsEditor";

/// What the user asked of the editor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SettingsEditorEvent {
    /// Parse this text, and when it holds, write it and apply it.
    Save(String),
    /// Hand the file to the system's editor instead.
    OpenExternally,
    /// Closed without saving.
    Dismiss,
}

/// The dialog and its field.
#[derive(Debug)]
pub struct SettingsEditor {
    text: Entity<TextareaState>,
    /// Where the file lives, shown under the title.
    path: SharedString,
    /// Offer "Open in editor" (the Mac; the phone has nothing to open it with).
    external: bool,
    /// Why the last save was refused.
    error: Option<String>,
    theme: Theme,
    _subscription: Subscription,
}

impl SettingsEditor {
    /// An editor over `text`, the file at `path`.
    pub fn new(
        text: &str,
        path: &str,
        external: bool,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let state = cx.new(|cx| TextareaState::new(window, cx).default_value(text.to_owned()));
        let subscription = cx.subscribe(&state, |this, _state, event, cx| match event {
            // Typing past an error is the fix in progress; the line goes on the next save.
            InputEvent::Change => {
                if this.error.is_some() {
                    this.error = None;
                    cx.notify();
                }
            }
            InputEvent::PressEnter { .. } | InputEvent::Focus | InputEvent::Blur => {}
        });
        Self {
            text: state,
            path: SharedString::from(path.to_owned()),
            external,
            error: None,
            theme,
            _subscription: subscription,
        }
    }

    /// The field's text.
    #[must_use]
    pub fn text(&self, cx: &gpui::App) -> String {
        self.text.read(cx).value().to_string()
    }

    /// Put the caret at the end of the field and give it the keyboard.
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.text.update(cx, |state, cx| {
            let end = state.text().len();
            state.set_selected_range(end..end, cx);
            state.focus(window, cx);
        });
    }

    /// The app refused the text: show why, keep editing.
    pub fn set_error(&mut self, error: String, cx: &mut Context<Self>) {
        self.error = Some(error);
        cx.notify();
    }

    /// The error under the field, if any.
    #[must_use]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// New tokens.
    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        if self.theme != theme {
            self.theme = theme;
            cx.notify();
        }
    }

    fn save(&self, cx: &mut Context<Self>) {
        cx.emit(SettingsEditorEvent::Save(self.text(cx)));
    }

    /// Escape from a button (Tab moved the focus there): the field's own Escape action
    /// covers it while it is focused, captured below.
    fn key_down(this: &mut Self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if ev.keystroke.key == "escape" && !this.text.read(cx).focus_handle(cx).is_focused(window) {
            cx.emit(SettingsEditorEvent::Dismiss);
            cx.stop_propagation();
        }
    }
}

impl EventEmitter<SettingsEditorEvent> for SettingsEditor {}

impl Focusable for SettingsEditor {
    fn focus_handle(&self, cx: &gpui::App) -> FocusHandle {
        self.text.read(cx).focus_handle(cx)
    }
}

impl Render for SettingsEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme.clone();
        let (s, spacing, radii) = (theme.surfaces, theme.spacing, theme.radii);
        let small = theme.typography.small();
        let button = move |id: &'static str, label: &'static str, hint: &'static str, accent| {
            let row = div()
                .id(id)
                .debug_selector(move || id.to_owned())
                .role(gpui::accesskit::Role::Button)
                .aria_label(label)
                .flex()
                .items_center()
                .gap(px(spacing.xs))
                .px(px(spacing.sm))
                .py(px(spacing.xxs))
                .rounded(px(radii.xs))
                .cursor_pointer()
                .when(accent, |el| el.bg(hsla(s.accent)).text_color(hsla(s.accent_fg)))
                .when(!accent, |el| {
                    el.text_color(hsla(s.text_secondary))
                        .hover(move |el| el.bg(hsla_alpha(s.text, alpha::HOVER)))
                })
                .child(label)
                .when(!hint.is_empty(), |el| {
                    el.child(
                        div()
                            .text_size(px(small))
                            .text_color(hsla(if accent { s.accent_fg } else { s.text_muted }))
                            .child(hint),
                    )
                });
            tab_stop(row, s.accent)
        };
        let error = self.error.clone().map(|error| {
            div()
                .id("settings-error")
                .debug_selector(|| "settings-error".to_owned())
                .role(gpui::accesskit::Role::Alert)
                .aria_label(error.clone())
                .px(px(spacing.md))
                .py(px(spacing.xs))
                .text_size(px(theme.typography.small()))
                .text_color(hsla(s.warn))
                .child(SharedString::from(error))
        });
        div()
            .id("settings-backdrop")
            .key_context(CTX)
            .absolute()
            .inset_0()
            .flex()
            .items_start()
            .justify_center()
            .pt(px(spacing.xl * 2.0))
            .bg(hsla_alpha(s.canvas, alpha::SCRIM))
            .on_key_down(cx.listener(Self::key_down))
            .capture_action(cx.listener(|_this, _: &Escape, _window, cx| {
                cx.emit(SettingsEditorEvent::Dismiss);
                cx.stop_propagation();
            }))
            // ⌘↩ saves (the field's `secondary-enter`: ⌘ on macOS and iOS alike); captured so
            // the field does not also break the line.
            .capture_action(cx.listener(|this, enter: &Enter, _window, cx| {
                if enter.secondary {
                    this.save(cx);
                    cx.stop_propagation();
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_this, _ev, _w, cx| {
                    cx.emit(SettingsEditorEvent::Dismiss);
                    cx.stop_propagation();
                }),
            )
            .child(
                div()
                    .id("settings-editor")
                    .debug_selector(|| "settings-editor".to_owned())
                    .role(gpui::accesskit::Role::Dialog)
                    .aria_label("Settings")
                    .w_full()
                    .max_w(px(640.0))
                    .mx(px(spacing.md))
                    .h(gpui::relative(0.8))
                    .max_h(px(720.0))
                    .flex()
                    .flex_col()
                    .rounded(px(radii.md))
                    .border_1()
                    .border_color(hsla(s.border))
                    .bg(hsla(s.panel))
                    .shadow_sm()
                    .text_size(px(theme.typography.ui_size))
                    .font_family(theme.typography.ui_family.clone())
                    .text_color(hsla(s.text))
                    .on_mouse_down(MouseButton::Left, |_ev, _w, cx| cx.stop_propagation())
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap(px(spacing.sm))
                            .px(px(spacing.md))
                            .py(px(spacing.sm))
                            .border_b_1()
                            .border_color(hsla(s.border))
                            .child("Settings")
                            .child(
                                div()
                                    .text_size(px(theme.typography.small()))
                                    .text_color(hsla(s.text_muted))
                                    .child(self.path.clone()),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_h(px(0.0))
                            .p(px(spacing.sm))
                            .font_family(
                                theme.typography.mono_families.first().cloned().unwrap_or_default(),
                            )
                            .child(
                                Textarea::new(&self.text)
                                    .appearance(false)
                                    .bordered(false)
                                    .aria_label("settings.toml")
                                    .h(gpui::relative(1.0)),
                            ),
                    )
                    .children(error)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_end()
                            .gap(px(spacing.xs))
                            .px(px(spacing.md))
                            .py(px(spacing.sm))
                            .border_t_1()
                            .border_color(hsla(s.border))
                            .when(self.external, |el| {
                                el.child(
                                    button("settings-open-external", "Open in editor", "", false)
                                        .on_click(cx.listener(|_this, _ev, _w, cx| {
                                            cx.emit(SettingsEditorEvent::OpenExternally);
                                        })),
                                )
                            })
                            .child(button("settings-cancel", "Cancel", "esc", false).on_click(
                                cx.listener(|_this, _ev, _w, cx| {
                                    cx.emit(SettingsEditorEvent::Dismiss);
                                }),
                            ))
                            .child(
                                button("settings-save", "Save", "⌘↩", true)
                                    .on_click(cx.listener(|this, _ev, _w, cx| this.save(cx))),
                            ),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use gpui::{TestAppContext, VisualTestContext};

    use super::*;

    fn editor<'a>(
        cx: &'a mut TestAppContext,
        text: &str,
        external: bool,
    ) -> (Entity<SettingsEditor>, Rc<RefCell<Vec<SettingsEditorEvent>>>, &'a mut VisualTestContext)
    {
        cx.update(gpui_kit::init);
        let events = Rc::new(RefCell::new(Vec::new()));
        let seen = Rc::clone(&events);
        let (view, cx) = cx.add_window_view(|window, cx| {
            let view = SettingsEditor::new(
                text,
                "~/settings.toml",
                external,
                Theme::default(),
                window,
                cx,
            );
            view.focus(window, cx);
            view
        });
        cx.update(|_window, cx| {
            cx.subscribe(&view, move |_view, event: &SettingsEditorEvent, _cx| {
                seen.borrow_mut().push(event.clone());
            })
            .detach();
        });
        cx.update(|window, _cx| window.set_a11y_active(true));
        cx.run_until_parked();
        (view, events, cx)
    }

    /// The dialog reads as one to a screen reader, the field holds the file, ⌘↩ hands the
    /// edited text back, and a refused save shows its reason until the next keystroke.
    #[gpui::test]
    fn the_editor_saves_on_command_enter_and_shows_a_refusal(cx: &mut TestAppContext) {
        let (view, events, cx) = editor(cx, "[font]\nmono_size = 13\n", true);
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        assert!(tree.iter().any(|n| n.is("Dialog", Some("Settings"))), "{tree:#?}");
        for label in ["Open in editor", "Cancel", "Save"] {
            assert!(tree.iter().any(|n| n.is("Button", Some(label))), "{label}: {tree:#?}");
        }
        cx.simulate_keystrokes("enter [ t e r m i n a l ]");
        cx.simulate_keystrokes("cmd-enter");
        cx.run_until_parked();
        assert_eq!(
            events.borrow().as_slice(),
            [SettingsEditorEvent::Save("[font]\nmono_size = 13\n\n[terminal]".to_owned())],
            "⌘↩ saves without breaking the line"
        );
        view.update(cx, |v, cx| v.set_error("bad key".to_owned(), cx));
        cx.run_until_parked();
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        assert!(tree.iter().any(|n| n.is("Alert", Some("bad key"))), "{tree:#?}");
        cx.simulate_keystrokes("x");
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |v, _| v.error().map(str::to_owned)), None);
        assert!(view.read_with(cx, |v, cx| v.text(cx).ends_with("[terminal]x")));
    }

    /// Escape and the Cancel button discard; the phone's dialog has no external editor.
    #[gpui::test]
    fn escape_and_cancel_dismiss_and_the_phone_has_no_external_editor(cx: &mut TestAppContext) {
        let (_view, events, cx) = editor(cx, "", false);
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        assert!(!tree.iter().any(|n| n.is("Button", Some("Open in editor"))), "{tree:#?}");
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        let cancel = cx.debug_bounds("settings-cancel").expect("the cancel button");
        cx.simulate_click(cancel.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(
            events.borrow().as_slice(),
            [SettingsEditorEvent::Dismiss, SettingsEditorEvent::Dismiss]
        );
    }
}

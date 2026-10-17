// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! Settings → Notifications: independently configurable session chimes plus
//! desktop banners on the same status transitions (`shell::on_state_changed`).
//!
//! The ShortcutsPage arrangement: the page holds a working copy, every flip
//! emits [`NotificationsEvent::Changed`], and the shell persists it. Nothing
//! here talks RPC — all preferences are device-local UI settings.

use gpui::{Context, EventEmitter, Window, div, prelude::*, px}; // slopty: no Agent updates.

use crate::popover;
use crate::settings::widgets;
use crate::theme::Theme;

#[derive(Debug, Clone)]
pub enum NotificationsEvent {
    /// A toggle flipped — persist the complete notification preference set.
    Changed {
        sound: bool,
        completion_sound: bool,
        input_sound: bool,
        attention_sound: bool,
        desktop: bool,
        background_only: bool,
    },
}

pub struct NotificationsPage {
    scroll: widgets::PageScroll,
    sound: bool,
    completion_sound: bool,
    input_sound: bool,
    attention_sound: bool,
    desktop: bool,
    background_only: bool,
}

impl EventEmitter<NotificationsEvent> for NotificationsPage {}

#[derive(Clone, Copy)]
enum NotificationPreference {
    Sound,
    CompletionSound,
    InputSound,
    AttentionSound,
    Desktop,
    // slopty: no Agent updates, no background-only switch.
}

fn is_switch_activation(key: &str, is_held: bool) -> bool {
    !is_held && matches!(key, "enter" | "space")
}

fn interactive_switch(
    element: gpui::Stateful<gpui::Div>,
    ring: gpui::Outline,
    preference: NotificationPreference,
    cx: &mut Context<NotificationsPage>,
) -> gpui::Stateful<gpui::Div> {
    element
        .tab_index(0)
        .focus_visible(move |style| style.outline_ring(ring))
        .cursor_pointer()
        .on_click(cx.listener(move |this, _, _, cx| {
            this.toggle(preference, cx);
        }))
        .on_key_down(cx.listener(move |this, event: &gpui::KeyDownEvent, _, cx| {
            if is_switch_activation(&event.keystroke.key, event.is_held) {
                cx.stop_propagation();
                this.toggle(preference, cx);
            }
        }))
}

impl NotificationsPage {
    pub fn new(
        sound: bool,
        completion_sound: bool,
        input_sound: bool,
        attention_sound: bool,
        desktop: bool,
        background_only: bool,
        _cx: &mut Context<Self>,
    ) -> Self {
        Self {
            scroll: widgets::PageScroll::default(),
            sound,
            completion_sound,
            input_sound,
            attention_sound,
            desktop,
            background_only,
        }
    }

    fn emit(&self, cx: &mut Context<Self>) {
        cx.emit(NotificationsEvent::Changed {
            sound: self.sound,
            completion_sound: self.completion_sound,
            input_sound: self.input_sound,
            attention_sound: self.attention_sound,
            desktop: self.desktop,
            background_only: self.background_only,
        });
    }

    fn value(&mut self, preference: NotificationPreference) -> &mut bool {
        match preference {
            NotificationPreference::Sound => &mut self.sound,
            NotificationPreference::CompletionSound => &mut self.completion_sound,
            NotificationPreference::InputSound => &mut self.input_sound,
            NotificationPreference::AttentionSound => &mut self.attention_sound,
            NotificationPreference::Desktop => &mut self.desktop,
        }
    }

    fn toggle(&mut self, preference: NotificationPreference, cx: &mut Context<Self>) {
        let value = self.value(preference);
        *value = !*value;
        self.emit(cx);
        cx.notify();
    }

    // slopty: every preference is on by default; a row that is off offers "Reset", and the page
    // restores them all (M10).
    fn reset(&mut self, preference: NotificationPreference, cx: &mut Context<Self>) {
        *self.value(preference) = true;
        self.emit(cx);
        cx.notify();
    }

    /// Whether any preference differs from its default (all on).
    fn any_changed(&self) -> bool {
        !(self.sound
            && self.completion_sound
            && self.input_sound
            && self.attention_sound
            && self.desktop)
    }

    fn restore_defaults(&mut self, cx: &mut Context<Self>) {
        self.sound = true;
        self.completion_sound = true;
        self.input_sound = true;
        self.attention_sound = true;
        self.desktop = true;
        self.emit(cx);
        cx.notify();
    }

    fn reset_button(
        &self,
        id: &'static str,
        label: &str,
        preference: NotificationPreference,
        is_default: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Stateful<gpui::Div>> {
        widgets::reset_action(theme, id, label, is_default).map(|button| {
            button.on_click(cx.listener(move |this, _, _, cx| this.reset(preference, cx)))
        })
    }

    fn on_scroll_hovered(&mut self, hovered: &bool, _: &mut Window, cx: &mut Context<Self>) {
        if self.scroll.set_list_hovered(*hovered) {
            cx.notify();
        }
    }
}

impl popover::ScrollRailHost for NotificationsPage {
    fn rail_bar(&mut self) -> &mut popover::MenuScrollbarState {
        self.scroll.rail_bar()
    }

    fn rail_scroll(&self) -> Option<gpui::ScrollHandle> {
        self.scroll.rail_scroll()
    }
}

impl Render for NotificationsPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).for_settings_surface();
        let ring = crate::surface_chrome::ring(&theme, crate::surface_chrome::RING_OFFSET);
        let sound = self.sound;
        let completion_sound = self.completion_sound;
        let input_sound = self.input_sound;
        let attention_sound = self.attention_sound;
        let desktop = self.desktop;
        let any_changed = self.any_changed();
        let toggle = |id: &'static str, label: &'static str, enabled: bool, interactive: bool| {
            // slopty: the switch is the stop, so its ring hugs the pill; the margin keeps the
            // row's 40 pt. Disabled subordinate controls remain named switches in the
            // accessibility tree, but have no focus or input handlers.
            widgets::toggle_switch(&theme, enabled, id)
                .id(id)
                .my(px((40.0 - widgets::SWITCH_TRACK_HEIGHT) / 2.0))
                .role(gpui::Role::Switch)
                .aria_label(label)
                .aria_toggled(if enabled {
                    gpui::Toggled::True
                } else {
                    gpui::Toggled::False
                })
                .when(!interactive, |el| {
                    el.aria_description("Unavailable while its parent setting is off")
                })
        };
        let card = widgets::section_card(&theme)
            .mt_0()
            .child(
                widgets::card_row(&theme, true)
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(160.0))
                            .flex()
                            .flex_col()
                            .child(widgets::row_title(&theme, "Chat sounds")),
                    )
                    .children(self.reset_button(
                        "notifications-sound-reset",
                        "Chat sounds",
                        NotificationPreference::Sound,
                        sound,
                        &theme,
                        cx,
                    ))
                    .child(interactive_switch(
                        toggle("notifications-sound-toggle", "Chat sounds", sound, true),
                        ring,
                        NotificationPreference::Sound,
                        cx,
                    )),
            )
            .child(
                widgets::card_row(&theme, false)
                    .when(!sound, |el| el.opacity(crate::theme::DISABLED_OPACITY))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(160.0))
                            .flex()
                            .flex_col()
                            .child(widgets::row_title(&theme, "Task completed")),
                    )
                    .children(
                        self.reset_button(
                            "notifications-completion-sound-reset",
                            "Task completed sound",
                            NotificationPreference::CompletionSound,
                            completion_sound || !sound,
                            &theme,
                            cx,
                        ),
                    )
                    .child(
                        toggle(
                            "notifications-completion-sound-toggle",
                            "Task completed sound",
                            completion_sound,
                            sound,
                        )
                        .when(sound, |el| {
                            interactive_switch(
                                el,
                                ring,
                                NotificationPreference::CompletionSound,
                                cx,
                            )
                        }),
                    ),
            )
            .child(
                widgets::card_row(&theme, false)
                    .when(!sound, |el| el.opacity(crate::theme::DISABLED_OPACITY))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(160.0))
                            .flex()
                            .flex_col()
                            .child(widgets::row_title(&theme, "Input required")),
                    )
                    .children(
                        self.reset_button(
                            "notifications-input-sound-reset",
                            "Input required sound",
                            NotificationPreference::InputSound,
                            input_sound || !sound,
                            &theme,
                            cx,
                        ),
                    )
                    .child(
                        toggle(
                            "notifications-input-sound-toggle",
                            "Input required sound",
                            input_sound,
                            sound,
                        )
                        .when(sound, |el| {
                            interactive_switch(el, ring, NotificationPreference::InputSound, cx)
                        }),
                    ),
            )
            .child(
                widgets::card_row(&theme, false)
                    .when(!sound, |el| el.opacity(crate::theme::DISABLED_OPACITY))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(160.0))
                            .flex()
                            .flex_col()
                            .child(widgets::row_title(&theme, "Errors and disconnections")),
                    )
                    .children(
                        self.reset_button(
                            "notifications-attention-sound-reset",
                            "Errors and disconnections sound",
                            NotificationPreference::AttentionSound,
                            attention_sound || !sound,
                            &theme,
                            cx,
                        ),
                    )
                    .child(
                        toggle(
                            "notifications-attention-sound-toggle",
                            "Errors and disconnections sound",
                            attention_sound,
                            sound,
                        )
                        .when(sound, |el| {
                            interactive_switch(
                                el,
                                ring,
                                NotificationPreference::AttentionSound,
                                cx,
                            )
                        }),
                    ),
            );
        let desktop_card = widgets::section_card(&theme)
            .mt_0()
            .child(
                widgets::card_row(&theme, true)
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(160.0))
                            .flex()
                            .flex_col()
                            .child(widgets::row_title(&theme, "Desktop notifications")),
                    )
                    .children(self.reset_button(
                        "notifications-desktop-reset",
                        "Desktop notifications",
                        NotificationPreference::Desktop,
                        desktop,
                        &theme,
                        cx,
                    ))
                    .child(interactive_switch(
                        toggle(
                            "notifications-desktop-toggle",
                            "Desktop notifications",
                            desktop,
                            true,
                        ),
                        ring,
                        NotificationPreference::Desktop,
                        cx,
                    )),
            );
        // slopty: no Agent updates: each agent updates itself on its machine.
        // slopty: no "Only when in the background": Slopty's notes post only while the
        // person is away from the app.

        let scrollbar = popover::rail(self, "notifications-page-scrollbar", &theme, cx);
        div()
            .id("notifications-page-host")
            .relative()
            .size_full()
            .on_hover(cx.listener(Self::on_scroll_hovered))
            .child(
                crate::edge_fade::edge_faded(
                    16.0,
                    true,
                    true,
                    div()
                        .id("notifications-page")
                        .size_full()
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll.scroll)
                        .child(
                            widgets::page_column()
                                .child(
                                    widgets::page_header(&theme, "Notifications", None)
                                        .items_center()
                                        .child(div().flex_1())
                                        .child(
                                            widgets::restore_defaults(
                                                &theme,
                                                "notifications-restore-defaults",
                                                any_changed,
                                            )
                                            .when(any_changed, |button| {
                                                button.on_click(cx.listener(|this, _, _, cx| {
                                                    this.restore_defaults(cx)
                                                }))
                                            }),
                                        ),
                                )
                                .child(
                                    widgets::section(&theme, "Desktop", desktop_card).mt(px(24.0)),
                                )
                                .child(widgets::section(&theme, "Sounds", card)),
                        ),
                )
                .fade_overflow_y(&self.scroll.scroll),
            )
            .children(scrollbar)
    }
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use super::*;

    // slopty: every preference is on by default: an off one offers Reset, and Restore defaults
    // turns them all back on (M10).
    #[gpui::test]
    fn restore_defaults_turns_every_preference_back_on(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_global(Theme::default()));
        let (page, cx) = cx
            .add_window_view(|_, cx| NotificationsPage::new(true, false, true, true, false, true, cx));
        let draw = |cx: &mut gpui::VisualTestContext| {
            cx.update(|window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
            });
        };
        draw(cx);
        assert!(cx.debug_bounds("notifications-completion-sound-reset").is_some());
        assert!(cx.debug_bounds("notifications-desktop-reset").is_some());
        assert!(cx.debug_bounds("notifications-sound-reset").is_none());
        let restore = cx.debug_bounds("notifications-restore-defaults").unwrap().center();
        cx.simulate_click(restore, gpui::Modifiers::default());
        draw(cx);
        page.read_with(cx, |page, _| assert!(!page.any_changed()));
        assert!(cx.debug_bounds("notifications-desktop-reset").is_none());
    }

    #[test]
    fn switches_accept_enter_or_space_once_per_press() {
        assert!(is_switch_activation("enter", false));
        assert!(is_switch_activation("space", false));
        assert!(!is_switch_activation("escape", false));
        assert!(!is_switch_activation("space", true));
    }
}

// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! Settings → Notifications: one switch, the system's notes for a session in the background
//! that needs the person, failed or finished (`slopty_ui`'s `workspace::attention` decides them).
//!
//! None makes a sound and none comes for the session on screen, so there is nothing else to
//! choose (`docs/decisions/layout.md`, "Attention is an inbox and a jump, never a feed"). The
//! page holds a working copy, a flip emits [`NotificationsEvent::Changed`], and the shell keeps
//! it: a device-local UI setting.
#![allow(
    missing_copy_implementations,
    missing_debug_implementations,
    missing_docs,
    clippy::clone_on_ref_ptr,
    clippy::trivially_copy_pass_by_ref,
    reason = "shell lint migration: slice D"
)]

use gpui::prelude::*;
use gpui::{Context, EventEmitter, Window, div, px};

use crate::popover;
use crate::settings::widgets;
use crate::theme::Theme;

#[derive(Debug, Clone)]
pub enum NotificationsEvent {
    /// The switch flipped: keep its new state.
    Changed { desktop: bool },
}

pub struct NotificationsPage {
    scroll: widgets::PageScroll,
    desktop: bool,
}

impl EventEmitter<NotificationsEvent> for NotificationsPage {}

fn is_switch_activation(key: &str, is_held: bool) -> bool {
    !is_held && matches!(key, "enter" | "space")
}

impl NotificationsPage {
    pub fn new(desktop: bool, _cx: &mut Context<Self>) -> Self {
        Self { scroll: widgets::PageScroll::default(), desktop }
    }

    fn toggle(&mut self, cx: &mut Context<Self>) {
        self.desktop = !self.desktop;
        cx.emit(NotificationsEvent::Changed { desktop: self.desktop });
        cx.notify();
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
        let desktop = self.desktop;
        // The switch is the stop, so its ring hugs the pill; the margin keeps the row's
        // 40 pt.
        let switch = widgets::toggle_switch(&theme, desktop, "notifications-desktop-toggle")
            .id("notifications-desktop-toggle")
            .debug_selector(|| "notifications-desktop-toggle".into())
            .my(px((40.0 - widgets::SWITCH_TRACK_HEIGHT) / 2.0))
            .role(gpui::Role::Switch)
            .aria_label("Desktop notifications")
            .aria_toggled(if desktop { gpui::Toggled::True } else { gpui::Toggled::False })
            .tab_index(0)
            .focus_visible(move |style| style.outline_ring(ring))
            .cursor_pointer()
            .on_click(cx.listener(|this, _, _, cx| this.toggle(cx)))
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                if is_switch_activation(&event.keystroke.key, event.is_held) {
                    cx.stop_propagation();
                    this.toggle(cx);
                }
            }));
        let card = widgets::section_card(&theme).mt_0().child(
            widgets::card_row(&theme, true)
                .child(
                    div()
                        .flex_1()
                        .min_w(px(160.0))
                        .flex()
                        .flex_col()
                        .child(widgets::row_title(&theme, "Desktop notifications"))
                        .child(widgets::details_label(
                            &theme,
                            "When a session in the background needs you, fails or finishes. Never a sound.",
                        )),
                )
                .child(switch),
        );
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
                                // One page head (`widgets::page_head`).
                                .child(widgets::page_head(
                                    &theme,
                                    "Notifications",
                                    None,
                                    "What Slopty tells you while you are elsewhere.",
                                    None,
                                ))
                                .child(widgets::section(&theme, "Desktop", card).mt(px(24.0))),
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

    /// The one switch flips the notes and says its new state, by a press or by Space.
    #[gpui::test]
    fn the_switch_flips_the_notes_and_says_so(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_global(Theme::default()));
        let (page, cx) = cx.add_window_view(|_, cx| NotificationsPage::new(true, cx));
        let said = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        cx.update(|_, cx| {
            let said = said.clone();
            cx.subscribe(&page, move |_, event: &NotificationsEvent, _| {
                let NotificationsEvent::Changed { desktop } = event;
                said.borrow_mut().push(*desktop);
            })
            .detach();
        });
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        let switch = cx.debug_bounds("notifications-desktop-toggle").expect("the switch").center();
        cx.simulate_click(switch, gpui::Modifiers::default());
        assert_eq!(*said.borrow(), vec![false], "off, and said");
        page.read_with(cx, |page, _| assert!(!page.desktop, "the page holds it"));
    }

    #[test]
    fn switches_accept_enter_or_space_once_per_press() {
        assert!(is_switch_activation("enter", false));
        assert!(is_switch_activation("space", false));
        assert!(!is_switch_activation("escape", false));
        assert!(!is_switch_activation("space", true));
    }
}

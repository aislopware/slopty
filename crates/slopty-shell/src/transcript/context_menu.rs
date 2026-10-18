// slopty: Slopty's own file (not in zeron).
//! Right-click in the chat opens a menu where people expect one: on selected text, Copy and
//! Quote in reply; on a code block, its header's actions (Copy code, Fit content or Use
//! horizontal scrolling, Run in terminal). The menu is the house's floating menu
//! (`popover::menu_at`) at the pointer, driven by the keyboard as every menu is.

use std::rc::Rc;

use gpui::{AnyElement, App, Context, FocusHandle, Pixels, Point, SharedString, Window, prelude::*, px};

use super::Transcript;
use crate::markdown::render::MenuChoice;
use crate::theme::Theme;

/// What the context menu keeps between frames.
#[derive(Default)]
pub(super) struct ContextMenuState {
    menu: crate::popover::Popup<ContextMenu>,
    focus: Option<FocusHandle>,
}

struct ContextMenu {
    /// What the menu is about, its accessible name.
    label: &'static str,
    position: Point<Pixels>,
    choices: Vec<MenuChoice>,
    active: Option<usize>,
}

/// The menu of a selection of `words`: Copy, and Quote in reply.
pub(super) fn selection_choices(words: &str) -> Vec<MenuChoice> {
    let copied = words.to_owned();
    let quoted = words.to_owned();
    vec![
        MenuChoice {
            label: "Copy".into(),
            keys: Some("mod-c"),
            act: Rc::new(move |_: &mut Window, cx: &mut App| {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(copied.clone()));
            }),
        },
        MenuChoice {
            label: "Quote in reply".into(),
            keys: None,
            act: Rc::new(move |window: &mut Window, cx: &mut App| {
                crate::composer::insert_quote(&quoted, window, cx);
            }),
        },
    ]
}

impl Transcript {
    /// Open the menu `label` of `choices` at `position`, the keyboard in it.
    pub(super) fn open_context_menu(
        &mut self,
        label: &'static str,
        choices: Vec<MenuChoice>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if choices.is_empty() {
            return;
        }
        self.withdraw_quote(cx);
        self.context_menu.menu.open(ContextMenu { label, position, choices, active: None });
        let focus = self.context_menu.focus.get_or_insert_with(|| cx.focus_handle()).clone();
        window.focus(&focus, cx);
        cx.notify();
    }

    /// The code blocks' right-click: their menu opens here.
    pub(super) fn code_menu_ui(&self, cx: &mut Context<Self>) -> Rc<crate::markdown::render::MenuFn> {
        let entity = cx.weak_entity();
        Rc::new(move |label: &'static str, choices: Vec<MenuChoice>, position: Point<Pixels>, window: &mut Window, cx: &mut App| {
            entity
                .update(cx, |this, cx| this.open_context_menu(label, choices, position, window, cx))
                .ok();
        })
    }

    /// A right-click over the chat: the selection's menu, when text is selected.
    pub(super) fn on_context_click(
        &mut self,
        event: &gpui::MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let words = crate::markdown::selection::selected_text()
            .map(|text| text.trim().to_owned())
            .filter(|text| !text.is_empty());
        if let Some(words) = words {
            cx.stop_propagation();
            self.open_context_menu("Selection", selection_choices(&words), event.position, window, cx);
        }
    }

    fn close_context_menu(&mut self, cx: &mut Context<Self>) {
        if self.context_menu.menu.begin_close(crate::clock::now(cx)) {
            crate::popover::reap_popup(cx, |this: &mut Self| &mut this.context_menu.menu);
            cx.notify();
        }
    }

    /// Do the menu's `nth` choice, then close it.
    fn pick_context_choice(&mut self, nth: usize, window: &mut Window, cx: &mut Context<Self>) {
        let act = self
            .context_menu
            .menu
            .as_open()
            .and_then(|menu| menu.choices.get(nth))
            .map(|choice| choice.act.clone());
        self.close_context_menu(cx);
        if let Some(act) = act {
            act(window, cx);
        }
    }

    fn context_menu_key(&mut self, event: &gpui::KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        use crate::popover::MenuKey;
        let Some(menu) = self.context_menu.menu.open_mut() else {
            return;
        };
        let count = menu.choices.len();
        match crate::popover::classify_key(
            &event.keystroke.key,
            event.keystroke.modifiers.platform,
            event.keystroke.modifiers.control,
        ) {
            MenuKey::Escape => self.close_context_menu(cx),
            MenuKey::Up => menu.active = crate::popover::menu_step(menu.active, count, -1),
            MenuKey::Down => menu.active = crate::popover::menu_step(menu.active, count, 1),
            MenuKey::Enter => {
                if let Some(active) = menu.active {
                    self.pick_context_choice(active, window, cx);
                }
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// The context menu, while open.
    pub(super) fn render_context_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.context_menu.menu.get()?;
        let focus = self.context_menu.focus.clone()?;
        let theme = Theme::of(cx).for_popup();
        let mut card = crate::popover::popover_card(&theme)
            .id("transcript-context-menu-card")
            .track_focus(&focus)
            .role(gpui::Role::Menu)
            .aria_label(menu.label)
            .min_w(px(180.0))
            .flex()
            .flex_col()
            .on_key_down(cx.listener(|this, event, window, cx| this.context_menu_key(event, window, cx)))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_context_menu(cx)))
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation());
        for (nth, choice) in menu.choices.iter().enumerate() {
            let id = SharedString::from(format!("transcript-context-menu-{nth}"));
            card = card.child(
                crate::popover::menu_row(&theme, menu.active == Some(nth), id.clone())
                    .id(id)
                    .role(gpui::Role::MenuItem)
                    .aria_label(choice.label.clone())
                    .when_some(choice.keys, |row, keys| row.aria_keyshortcuts(crate::keys::aria_combo(keys)))
                    .on_click(cx.listener(move |this, _, window, cx| this.pick_context_choice(nth, window, cx)))
                    .child(div_label(choice.label.clone()))
                    .when_some(choice.keys, |row, keys| {
                        row.child(crate::popover::kbd_hint(&theme, &crate::keys::combo_text(keys)))
                    }),
            );
        }
        Some(crate::popover::menu_at(
            "transcript-context-menu",
            menu.position,
            card.into_any_element(),
            self.context_menu.menu.closing_at(crate::clock::now(cx)),
        ))
    }
}

/// A row's words, taking the row's room so its key sits at the end.
fn div_label(label: SharedString) -> gpui::Div {
    gpui::div().flex_1().min_w_0().truncate().child(label)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A selection's menu copies it and quotes it, Copy saying its key.
    #[test]
    fn a_selection_offers_copy_and_quote() {
        let choices = selection_choices("the words");
        let labels: Vec<&str> = choices.iter().map(|c| c.label.as_ref()).collect();
        assert_eq!(labels, ["Copy", "Quote in reply"]);
        assert_eq!(choices[0].keys, Some("mod-c"));
        assert_eq!(choices[1].keys, None);
    }
}

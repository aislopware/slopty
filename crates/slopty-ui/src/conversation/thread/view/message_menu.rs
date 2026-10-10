//! A message's own menu, by a right click or a long press on it: what its quiet actions under it
//! do, and quoting it in the next message.
//!
//! - **Copy** puts its words on the clipboard and says "Copied" under it, as its copy does.
//! - **Quote in reply** puts its words in the draft, each line behind "> ", and gives the composer
//!   the keyboard. Not in a subagent's thread, which takes no messages.
//! - **Fork from here**, under one of the person's messages, starts a new thread from just before
//!   it, the message waiting in its composer (`super::fork`).
//! - **Ask aside** asks the draft of a fork of the whole thread in a sheet over it
//!   (`super::aside`).

use std::rc::Rc;

use gpui::{
    AnyElement, App, Context, FocusHandle, IntoElement as _, ParentElement as _, Pixels, Point,
    Window, px,
};
use slopty_proto::thread::{ItemId, TurnId};

use super::ThreadView;
use crate::kit;

/// The menu open over a message, where it was pressed.
#[derive(Clone, Debug)]
pub(super) struct MessageMenu {
    item: ItemId,
    words: String,
    /// The person's message's turn, which it forks from; none for the agent's.
    turn: Option<TurnId>,
    at: Point<Pixels>,
    /// What held the keyboard before it opened, which has it back when it closes.
    back_to: Option<FocusHandle>,
}

/// What a row of the menu does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Pick {
    Copy,
    Quote,
    Fork,
    Aside,
}

/// The menu's row that forks from a message of the person's.
pub(super) const FORK: &str = "Fork from here";

/// The menu's row that asks the draft aside.
pub(super) const ASIDE: &str = "Ask aside";

/// `words` quoted for a reply: each line behind "> ", a lone ">" for a blank one.
pub(super) fn quoted(words: &str) -> String {
    words
        .trim_end()
        .lines()
        .map(|line| if line.is_empty() { ">".to_owned() } else { format!("> {line}") })
        .collect::<Vec<_>>()
        .join("\n")
}

impl ThreadView {
    /// `el`, a message's row, opening its menu on a right click or a long press.
    pub(super) fn message_menu_press<E>(
        el: E,
        item: &ItemId,
        words: &str,
        turn: Option<TurnId>,
        cx: &Context<Self>,
    ) -> E
    where
        E: gpui::InteractiveElement + gpui::ParentElement + gpui::Styled,
    {
        let this = cx.entity().downgrade();
        let (item, words) = (item.clone(), words.to_owned());
        kit::menu_press(el, move |at, window, cx| {
            let menu =
                MessageMenu { item: item.clone(), words: words.clone(), turn, at, back_to: None };
            let _gone = this.update(cx, |v, cx| v.open_message_menu(menu, window, cx));
        })
    }

    fn open_message_menu(
        &mut self,
        mut menu: MessageMenu,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        menu.back_to =
            self.message_menu.take().and_then(|open| open.back_to).or_else(|| window.focused(cx));
        self.message_menu = Some(menu);
        cx.notify();
    }

    /// Close the menu, the keyboard back where it was: whether it was open.
    pub(super) fn close_message_menu(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(menu) = self.message_menu.take() else { return false };
        if let Some(back) = menu.back_to {
            window.focus(&back, cx);
        }
        cx.notify();
        true
    }

    fn message_pick(
        &mut self,
        pick: Pick,
        menu: MessageMenu,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match pick {
            Pick::Copy => self.copy(menu.item, menu.words, cx),
            Pick::Quote => self.quote_into_draft(&quoted(&menu.words), window, cx),
            Pick::Fork => {
                if let Some(turn) = menu.turn {
                    self.fork_from(turn, cx);
                }
            }
            Pick::Aside => self.ask_aside(window, cx),
        }
    }

    /// The open menu, drawn late where the press landed.
    pub(super) fn message_menu_panel(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let menu = self.message_menu.clone()?;
        let mut picks = vec![(Pick::Copy, "copy", "Copy")];
        if !self.in_subagent() {
            picks.push((Pick::Quote, "quote", "Quote in reply"));
        }
        if menu.turn.is_some_and(|turn| self.forks_from(turn, cx)) {
            picks.push((Pick::Fork, "fork", FORK));
        }
        if !self.in_subagent() && self.can_aside(cx) {
            picks.push((Pick::Aside, "aside", ASIDE));
        }
        let this = cx.entity().downgrade();
        let mut rows = kit::Menu::new();
        for (pick, key, label) in picks {
            let (this, menu) = (this.clone(), menu.clone());
            rows.push(kit::MenuItem::new(key, label, move |window, cx| {
                let _gone = this.update(cx, |v, cx| v.message_pick(pick, menu.clone(), window, cx));
            }));
        }
        let panel = kit::MenuPanel::new("message-menu", "Message", Rc::new(rows), &self.theme, {
            move |window: &mut Window, cx: &mut App| {
                let _gone = this.update(cx, |v, cx| v.close_message_menu(window, cx));
            }
        });
        Some(
            gpui::deferred(
                gpui::anchored()
                    .position(menu.at)
                    .snap_to_window_with_margin(px(self.theme.spacing.sm))
                    .child(panel),
            )
            .with_priority(crate::palette::Layer::Submenu.priority())
            .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::quoted;

    #[test]
    fn a_quote_marks_every_line_and_keeps_the_blank_ones() {
        assert_eq!(quoted("one line"), "> one line");
        assert_eq!(quoted("first\n\nthird\n"), "> first\n>\n> third");
    }
}

//! The find card, ghostty's (`docs/decisions/terminal.md`, "Find is ghostty's card"): a
//! floating card in a corner of the pane, its field 180 wide with the count inside it ("3/55",
//! "-/55" before one is picked), then ↑ ↓ and ×. ⏎ and the ↑ step go up the history to the next
//! hit, ⇧⏎ and ↓ back down; Esc in the field gives the keys back to the grid and keeps the hits
//! on show (Esc again, there or in the field emptied, closes it). The card drags to any of the
//! four corners. The query is the Mac's find pasteboard's, shared with every other app's find.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Context, Focusable as _, InteractiveElement as _, IntoElement as _, MouseButton,
    MouseDownEvent, ParentElement as _, Pixels, Point, SharedString, Size,
    StatefulInteractiveElement as _, Styled as _, Window, div, point, px,
};
use slopty_shell::live::chrome;
use slopty_shell::{icons, popover};

use super::find::{self, Tally, Toggle};
use super::{
    CloseFind, FindNext, FindPrev, SEARCH_CTX, SEARCH_MAX, Search, TerminalView, ToggleMatchCase,
    ToggleRegex, ToggleWholeWord,
};

/// The field's width for the text: ghostty's 180.
const FIELD_TEXT: f32 = 180.0;
/// The room at the field's end the count takes.
const FIELD_COUNT: f32 = 58.0;
/// Between the card's parts: ghostty's 4.
const GAP: f32 = 4.0;
/// Around the card's parts, and between the card and the pane's corner: ghostty's 8.
const INSET: f32 = 8.0;
/// A step's square.
const STEP: f32 = 24.0;
/// The field's height: ghostty's 6 above and below a 16 pt line.
const FIELD_HEIGHT: f32 = 28.0;

/// The card's size, fixed so where it lands never waits on a layout.
pub(super) const CARD: Size<Pixels> = Size {
    width: px(INSET * 2.0 + 8.0 + FIELD_TEXT + FIELD_COUNT + GAP * 3.0 + STEP * 3.0),
    height: px(INSET * 2.0 + FIELD_HEIGHT),
};

/// A corner of the pane the card stands in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Corner {
    /// Where a card opens: the top right, as ghostty's does; on a phone-wide terminal, which can
    /// be wider than the screen, the top left, the part that is on screen.
    pub(super) const START: Self =
        if cfg!(target_os = "ios") { Self::TopLeft } else { Self::TopRight };

    /// The corner nearest `centre` in a pane of `pane`: the quarter it lies in.
    pub(super) fn nearest(centre: Point<Pixels>, pane: Size<Pixels>) -> Self {
        let left = centre.x < pane.width * 0.5;
        let top = centre.y < pane.height * 0.5;
        match (top, left) {
            (true, true) => Self::TopLeft,
            (true, false) => Self::TopRight,
            (false, true) => Self::BottomLeft,
            (false, false) => Self::BottomRight,
        }
    }

    /// Where the card's top left stands in this corner of a pane of `pane`, [`INSET`] in.
    pub(super) fn origin(self, pane: Size<Pixels>) -> Point<Pixels> {
        let (left, top) = (px(INSET), px(INSET));
        let right = (pane.width - CARD.width - px(INSET)).max(left);
        let bottom = (pane.height - CARD.height - px(INSET)).max(top);
        match self {
            Self::TopLeft => point(left, top),
            Self::TopRight => point(right, top),
            Self::BottomLeft => point(left, bottom),
            Self::BottomRight => point(right, bottom),
        }
    }
}

/// The card on its way to another corner: where the press was and how far it has gone.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) struct Drag {
    from: Point<Pixels>,
    by: Point<Pixels>,
}

impl TerminalView {
    /// The pane's size, as the element last laid it out.
    fn pane(&self) -> Option<Size<Pixels>> {
        self.drawn_bounds.map(|bounds| bounds.size)
    }

    /// The card follows the pointer while it is dragged; true while it is.
    pub(super) fn drag_find(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) -> bool {
        let Some(drag) = &mut self.find_drag else { return false };
        drag.by = at - drag.from;
        cx.notify();
        true
    }

    /// The card let go: it lands in the corner nearest where its centre was dropped. True if
    /// it was being dragged.
    pub(super) fn drop_find(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(drag) = self.find_drag.take() else { return false };
        if let Some(pane) = self.pane() {
            let centre = self.find_corner.origin(pane)
                + point(CARD.width * 0.5, CARD.height * 0.5)
                + drag.by;
            self.find_corner = Corner::nearest(centre, pane);
        }
        cx.notify();
        true
    }

    /// Esc in the field: an empty query closes the card; else the keys go back to the grid and
    /// the hits stay on show, as ghostty's does.
    pub fn close_find(&mut self, _: &CloseFind, window: &mut Window, cx: &mut Context<Self>) {
        let Some(search) = &self.search else { return };
        if search.query.needle().is_empty() {
            self.end_find(window, cx);
        } else {
            self.focus.focus(window, cx);
            cx.notify();
        }
    }

    /// Close the card and give the keys back to the program.
    pub(super) fn end_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search.take().is_some() {
            self.find_drag = None;
            self.focus.focus(window, cx);
            cx.notify();
        }
    }

    /// The find pasteboard's query, when it holds one: what a card opens with, and what it
    /// takes up when the person comes back from finding in another app.
    #[cfg_attr(
        not(target_os = "macos"),
        expect(clippy::missing_const_for_fn, reason = "only a Mac has a find pasteboard")
    )]
    pub(super) fn shared_needle(
        #[cfg_attr(
            not(target_os = "macos"),
            expect(unused_variables, reason = "only a Mac has a find pasteboard")
        )]
        cx: &gpui::App,
    ) -> Option<String> {
        #[cfg(target_os = "macos")]
        {
            cx.read_from_find_pasteboard().and_then(|item| item.text()).filter(|t| !t.is_empty())
        }
        #[cfg(not(target_os = "macos"))]
        {
            None
        }
    }

    /// Share `needle` with every other app's find.
    #[cfg_attr(
        not(target_os = "macos"),
        expect(clippy::missing_const_for_fn, reason = "only a Mac has a find pasteboard")
    )]
    pub(super) fn share_needle(
        #[cfg_attr(
            not(target_os = "macos"),
            expect(unused_variables, reason = "only a Mac has a find pasteboard")
        )]
        needle: &str,
        #[cfg_attr(
            not(target_os = "macos"),
            expect(unused_variables, reason = "only a Mac has a find pasteboard")
        )]
        cx: &gpui::App,
    ) {
        #[cfg(target_os = "macos")]
        if !needle.is_empty() {
            cx.write_to_find_pasteboard(gpui::ClipboardItem::new_string(needle.to_owned()));
        }
    }

    /// The window came to the front: a query changed in another app's find meanwhile is the
    /// open card's now.
    pub(super) fn find_came_back(&self, cx: &mut Context<Self>) {
        let Some(search) = &self.search else { return };
        if let Some(needle) = Self::shared_needle(cx)
            && needle != search.query.needle()
        {
            search.input.update(cx, |input, cx| input.set_text(needle, cx));
        }
    }

    /// The card: the shell's floating card in its corner (or under the pointer while dragged),
    /// the field with the toggles that are on and the count inside it, then the steps and
    /// close; each control names its key.
    pub(super) fn render_find(&self, search: &Search, cx: &Context<Self>) -> gpui::AnyElement {
        use slopty_shell::settings::widgets::{ActionHint as _, Keys};
        let tally = if search.query.needle().is_empty() {
            Tally::Quiet
        } else if let Some(why) = &search.invalid {
            Tally::Bad(why.clone())
        } else {
            Tally::Found {
                // Counted from the newest, as the steps go up the history from it.
                at: search.current.map(|at| search.matches.len().saturating_sub(at)),
                total: usize::try_from(search.total).unwrap_or(usize::MAX),
                more: search.total > SEARCH_MAX,
            }
        };
        let theme = chrome::theme(cx).for_popup();
        let words = tally.words();
        let bad = matches!(tally, Tally::Bad(_));
        let found = matches!(tally, Tally::Found { total, .. } if total > 0);
        let value = match &tally {
            Tally::Bad(why) => SharedString::from(why.clone()),
            _ => SharedString::from(words.clone()),
        };
        let focus = search.input.focus_handle(cx);
        let keys = |action: Box<dyn gpui::Action>| Keys::ActionIn(action, focus.clone());
        // A press on the card anywhere but the field leaves the keys in the field, as ghostty's
        // buttons do: the field gives them up on any press outside it, and takes them back here.
        let keep_keys = |focus: gpui::FocusHandle| {
            move |_ev: &MouseDownEvent, window: &mut Window, cx: &mut gpui::App| {
                window.focus(&focus, cx);
                cx.stop_propagation();
            }
        };
        // A toggle that is on shows its face in the field; a click there turns it off. Off,
        // it is a key away (⌥⌘C, ⌥⌘W, ⌥⌘R) and in the palette.
        let toggle = |name: &str,
                      face: &'static str,
                      label: &'static str,
                      which: Toggle,
                      action: Box<dyn gpui::Action>| {
            let selector = format!("terminal-find-{name}");
            search.query.is_on(which).then(|| {
                div()
                    .id(SharedString::from(selector.clone()))
                    .debug_selector(move || selector)
                    .role(gpui::Role::Button)
                    .aria_toggled(gpui::Toggled::True)
                    .flex_none()
                    .px(px(3.0))
                    .rounded(px(4.0))
                    .bg(theme.ink(0.1))
                    .text_size(slopty_shell::typography::ui_rems(11.0))
                    .text_color(theme.text_muted)
                    .cursor_pointer()
                    .action_hint(label, keys(action), cx)
                    .on_mouse_down(MouseButton::Left, keep_keys(focus.clone()))
                    .on_click(cx.listener(move |this, _ev, _window, cx| {
                        this.toggle_search(which, cx);
                    }))
                    .child(face)
            })
        };
        // With nothing to go to, the steps dim and keep their names.
        let step = |name: &str, icon: &'static str, label: &'static str, by: i64, action| {
            let selector = format!("terminal-find-{name}");
            popover::find_step(SharedString::from(selector.clone()), icon, found, &theme)
                .debug_selector(move || selector)
                .action_hint(label, keys(action), cx)
                .on_mouse_down(MouseButton::Left, keep_keys(focus.clone()))
                .when(found, |el| {
                    el.on_click(cx.listener(move |this, _ev, _window, cx| this.step_match(by, cx)))
                })
        };
        let field_focus = focus.clone();
        let count = popover::find_tally(&theme, words)
            .debug_selector(|| "terminal-find-count".to_owned())
            .when(bad, |el| el.text_color(theme.danger))
            .aria_label("Matches")
            .aria_value(value);
        let field = div()
            .id("terminal-find-field")
            .debug_selector(|| "terminal-find-field".to_owned())
            .flex_none()
            .w(px(8.0 + FIELD_TEXT + FIELD_COUNT))
            .h(px(FIELD_HEIGHT))
            .pl(px(8.0))
            .pr(px(8.0))
            .rounded(px(6.0))
            .bg(theme.ink(0.1))
            .flex()
            .items_center()
            .gap(px(GAP))
            .overflow_hidden()
            .cursor_text()
            // A press beside the text puts the caret in it too.
            .on_mouse_down(MouseButton::Left, move |_ev, window, cx| {
                window.focus(&field_focus, cx);
                cx.stop_propagation();
            })
            .child(div().flex_1().min_w_0().child(search.input.clone()))
            .children(toggle(
                "case",
                find::CASE_FACE,
                find::MATCH_CASE,
                Toggle::MatchCase,
                Box::new(ToggleMatchCase),
            ))
            .children(toggle(
                "word",
                find::WORD_FACE,
                find::WHOLE_WORD,
                Toggle::WholeWord,
                Box::new(ToggleWholeWord),
            ))
            .children(toggle(
                "regex",
                find::REGEX_FACE,
                find::REGEX,
                Toggle::Regex,
                Box::new(ToggleRegex),
            ))
            .child(count);
        let close = popover::find_step("terminal-find-close", icons::CLOSE, true, &theme)
            .debug_selector(|| "terminal-find-close".to_owned())
            .action_hint("Close find", keys(Box::new(CloseFind)), cx)
            .on_mouse_down(MouseButton::Left, |_ev, _window, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, _ev, window, cx| this.end_find(window, cx)));
        let card = popover::popover_card(&theme)
            .id("terminal-find")
            .debug_selector(|| "terminal-find".to_owned())
            .role(gpui::Role::Search)
            .aria_label("Find")
            .w(CARD.width)
            .h(CARD.height)
            .max_w_full()
            .p(px(INSET))
            .rounded(px(8.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(GAP))
            .font_family(theme.font_sans.clone())
            // Over what it finds in: the pointer there is the card's, not the text's.
            .occlude()
            .cursor_grab()
            .on_mouse_down(MouseButton::Left, {
                let focus = focus.clone();
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.find_drag = Some(Drag { from: event.position, by: Point::default() });
                    window.focus(&focus, cx);
                    cx.stop_propagation();
                })
            })
            .child(field)
            .child(step("up", icons::ALT_ARROW_UP, "Next match up", -1, Box::new(FindNext)))
            .child(step("down", icons::ALT_ARROW_DOWN, "Next match down", 1, Box::new(FindPrev)))
            .child(close);
        let at = self.pane().map(|pane| {
            let at = self.find_corner.origin(pane);
            self.find_drag.map_or(at, |drag| at + drag.by)
        });
        div()
            .key_context(SEARCH_CTX)
            .absolute()
            .map(|el| match at {
                Some(at) => el.left(at.x).top(at.y),
                None => el.top(px(INSET)).right(px(INSET)),
            })
            .on_action(cx.listener(Self::close_find))
            .on_action(cx.listener(|this, _: &ToggleMatchCase, _, cx| {
                this.toggle_search(Toggle::MatchCase, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleWholeWord, _, cx| {
                this.toggle_search(Toggle::WholeWord, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleRegex, _, cx| {
                this.toggle_search(Toggle::Regex, cx);
            }))
            // ⇧↩ steps back down, as ↩ steps up: the field is one line, so its newline is the
            // step down, taken before the field would put it in.
            .capture_action(cx.listener(|this, _: &slopty_shell::composer::Newline, _, cx| {
                cx.stop_propagation();
                this.step_match(1, cx);
            }))
            .child(popover::find_card_in("terminal-find-in", card))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use gpui::size;

    use super::*;

    /// A card dropped lands in the corner of the quarter its centre is in, and stands inside
    /// the pane however small it is.
    #[test]
    fn a_dropped_card_lands_in_the_nearest_corner() {
        let pane = size(px(1000.0), px(600.0));
        assert_eq!(Corner::nearest(point(px(10.0), px(10.0)), pane), Corner::TopLeft);
        assert_eq!(Corner::nearest(point(px(990.0), px(10.0)), pane), Corner::TopRight);
        assert_eq!(Corner::nearest(point(px(10.0), px(590.0)), pane), Corner::BottomLeft);
        assert_eq!(Corner::nearest(point(px(501.0), px(301.0)), pane), Corner::BottomRight);
        let right = Corner::BottomRight.origin(pane);
        assert_eq!(
            right,
            point(pane.width - CARD.width - px(INSET), pane.height - CARD.height - px(INSET))
        );
        let tiny = size(px(100.0), px(20.0));
        assert_eq!(Corner::BottomRight.origin(tiny), point(px(INSET), px(INSET)));
    }
}

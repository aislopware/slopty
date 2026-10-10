//! The one look of a tab, the title bar's and a pane's alike, and of the rows and menu items
//! that choose as tabs do: `MonoCode`'s pill (`.research/monocode-anatomy-2026-10-10.md`, §0
//! and §1a).
//!
//! A row of tabs lies on its bar's own plane, the ground, with the one line along its foot.
//! Every tab is a 30 pt pill at [`slopty_theme::Radii::sm`] standing inside the bar with air
//! above and below it, in slots of one width that shrink alike. It has no border and no edge
//! of colour, and its words never change weight: the tab on show is the `selected` wash and
//! the text's full tone, the rest are bare words in `text_secondary` that take the hover wash
//! and the full tone under the pointer. A tab leads with one 14 pt mark, and ends in a 20 pt
//! close laid over its end.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Div, ElementId, InteractiveElement as _, ParentElement as _, Stateful,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use slopty_theme::{Theme, alpha};

use crate::colors::hsla;
use crate::icons::Mark;
use crate::kit;

/// Every tab's slot, in points: `MonoCode`'s `w-56`, the same for every tab whatever its words.
pub(super) const TAB_SLOT: f32 = 224.0;

/// The least a tab's slot shrinks to while the row runs out of room: `MonoCode`'s `min-w-28`.
pub(super) const TAB_FLOOR: f32 = 112.0;

/// The gap between a tab's lead, its title and what follows: `MonoCode`'s `gap-1.5`.
pub(super) const TAB_GAP: f32 = 6.0;

/// The side of the dot that says a pane of a split has the keyboard: `MonoCode`'s `size-2`.
pub(super) const FOCUS_DOT: f32 = 8.0;

/// The side of the dot that says a file has an edit not yet on disk: `MonoCode`'s `size-1.5`.
const EDITED_DOT: f32 = 6.0;

/// `row`, a row of tabs or a bar that tabs stand in: on its own plane, with no fill, and the
/// one line along its foot. The line is the row's first child and lies inside it, so what is
/// added after it paints over it.
#[must_use]
pub(super) fn row<E: gpui::Styled + gpui::ParentElement>(theme: &Theme, row: E) -> E {
    let foot =
        div().absolute().left_0().right_0().bottom_0().h(kit::HAIR).bg(hsla(theme.surfaces.stroke));
    row.relative().child(foot)
}

/// `el` laid out as a strip of tabs: `MonoCode`'s `pl-1.5 pr-2.5 gap-0.5`, the tabs centred in
/// the bar's height.
#[must_use]
pub(super) fn strip<E: gpui::Styled>(theme: &Theme, el: E) -> E {
    let spacing = theme.spacing;
    el.flex()
        .items_center()
        .gap(px(spacing.xxs))
        .pl(px(spacing.xs + spacing.xxs))
        .pr(px(spacing.sm + spacing.xxs))
}

/// Where a pane's header starts its lead with no tab to stand in: where a first tab's lead
/// stands, past the strip's start and the tab's own, so the mark keeps its place as a pane
/// gains its second tab or loses it.
#[must_use]
pub(super) const fn lone_inset(theme: &Theme) -> f32 {
    let spacing = theme.spacing;
    spacing.xs + spacing.xxs + spacing.sm
}

/// `tab` as a pill: [`slopty_theme::Density::tab`] tall at [`slopty_theme::Radii::sm`], in a
/// slot [`TAB_SLOT`] wide that gives way alike down to [`TAB_FLOOR`], its words at the chrome
/// role. `closable` leaves room at its end for the close [`close`] lays over it. The caller
/// gives it its identity and its children.
#[must_use]
pub(super) fn tab(theme: &Theme, tab: Stateful<Div>, shown: bool, closable: bool) -> Stateful<Div> {
    let s = theme.surfaces;
    let spacing = theme.spacing;
    let end = if closable {
        2.0_f32.mul_add(spacing.xs, kit::CLOSE_BOX)
    } else {
        spacing.sm + spacing.xxs
    };
    let tab = kit::typed(tab, theme.roles().chrome)
        .relative()
        .flex_initial()
        .flex_shrink(1.0)
        .w(px(TAB_SLOT))
        .min_w(px(TAB_FLOOR))
        .h(px(theme.density.tab))
        .flex()
        .items_center()
        .gap(px(TAB_GAP))
        .pl(px(spacing.sm))
        .pr(px(end))
        .rounded(px(theme.radii.sm))
        .whitespace_nowrap();
    let tab = kit::eased(tab);
    if shown {
        tab.bg(hsla(s.selected)).text_color(hsla(s.text))
    } else {
        tab.text_color(hsla(s.text_secondary))
            .hover(move |el| el.bg(hsla(s.hover)).text_color(hsla(s.text)))
    }
}

/// A pane's tab: [`tab`], a level under the title bar's. The one on show takes the hover's
/// wash, where a title tab takes `selected`, so a tab of several tiles never reads as the same
/// pill twice, its title tab over the pane tab of its focused tile. Words, close and slots are
/// a title tab's.
#[must_use]
pub(super) fn pane_tab(
    theme: &Theme,
    el: Stateful<Div>,
    shown: bool,
    closable: bool,
) -> Stateful<Div> {
    let wash = theme.surfaces.hover;
    let el = tab(theme, el, shown, closable);
    if shown { el.bg(hsla(wash)) } else { el }
}

/// A tab's lead: `mark` in one 14 pt slot, in the text's tone on the tab on show and at
/// [`alpha::STRONG`] of `ink` on the rest, so a resting tab's mark recedes with its words and
/// still reads AA.
#[must_use]
pub(super) fn lead(theme: &Theme, mark: impl Into<Mark>, shown: bool) -> Stateful<Div> {
    let s = theme.surfaces;
    let ink = if shown { s.text } else { s.text_secondary };
    crate::palette::lead_mark(theme, mark, hsla(ink), crate::icons::IconSize::Inline.slot(theme))
        .when(!shown, |el| el.opacity(alpha::STRONG))
}

/// `close`, a tab's [`kit::close_box`], laid over the tab's end, [`slopty_theme::Spacing::xs`]
/// in and centred: shown at rest when `at_rest`, else only while the pointer is on the tab of
/// hover group `group`.
#[must_use]
pub(super) fn close(
    theme: &Theme,
    close: Stateful<Div>,
    at_rest: bool,
    group: &'static str,
) -> Div {
    let close =
        if at_rest { close } else { close.invisible().group_hover(group, gpui::Styled::visible) };
    div()
        .absolute()
        .top_0()
        .bottom_0()
        .right(px(theme.spacing.xs))
        .flex()
        .items_center()
        .child(close)
}

/// The dot after a file's title that says it has an edit not yet on disk, named "Edited" to a
/// screen reader: `MonoCode`'s dirty dot, in `text_secondary`.
#[must_use]
pub(super) fn edited(theme: &Theme, id: impl Into<ElementId>) -> Stateful<Div> {
    div()
        .id(id)
        .role(gpui::accesskit::Role::Label)
        .aria_label(super::tile::EDITED)
        .flex_none()
        .size(px(EDITED_DOT))
        .rounded(px(theme.radii.full))
        .bg(hsla(theme.surfaces.text_secondary))
}

/// The dot that leads a pane's row while the tab on show holds two panes or more: the accent's
/// mark on the pane with the keyboard, empty elsewhere, keeping its place so nothing after it
/// moves as the keyboard goes between panes (`MonoCode`'s split header).
#[must_use]
pub(super) fn focus_dot(theme: &Theme, focused: bool) -> Div {
    div()
        .flex_none()
        .size(px(FOCUS_DOT))
        .rounded(px(theme.radii.full))
        .when(focused, |el| el.bg(hsla(theme.surfaces.accent_fill)))
}

/// A pane's one tile's title where it has no tab to stand in (a split's pane, or a pane that
/// waits on its worker): no pill, but in a split the focus dot, then `mark` in its 14 pt slot
/// and `title`, `MonoCode`'s split header. `focused` is whether the pane has the keyboard and
/// `shared` whether the tab on show holds two panes or more.
#[must_use]
pub(super) fn lone(
    theme: &Theme,
    mark: impl Into<Mark>,
    title: impl gpui::IntoElement,
    focused: bool,
    shared: bool,
) -> Stateful<Div> {
    let ink = if focused { theme.surfaces.text } else { theme.surfaces.text_secondary };
    let slot = crate::icons::IconSize::Inline.slot(theme);
    div()
        .id("lone-title")
        .min_w_0()
        .h_full()
        .flex()
        .items_center()
        .gap(px(TAB_GAP))
        .pl(px(lone_inset(theme)))
        .children(shared.then(|| focus_dot(theme, focused)))
        .child(crate::palette::lead_mark(theme, mark, hsla(ink), slot))
        .child(div().min_w_0().overflow_hidden().child(title))
}

#[cfg(test)]
mod tests {
    use gpui::{InteractiveElement as _, Styled as _, div, px};
    use slopty_theme::{Theme, Variant};

    use super::{TAB_FLOOR, TAB_SLOT, pane_tab, tab};
    use crate::colors::hsla;

    /// The shown tab is a selected pill in the text's tone; the rest are bare words in the
    /// secondary tone. Both are the density's tab tall at the small radius, in a slot of one
    /// width, with no border and the regular weight.
    #[test]
    fn the_shown_tab_is_a_selected_pill_and_the_rest_bare() {
        for variant in [Variant::Dark, Variant::Light] {
            let theme = Theme::new(variant);
            let s = theme.surfaces;
            let mut shown = tab(&theme, div().id("a"), true, true);
            let style = shown.style();
            assert_eq!(style.background, Some(gpui::Fill::from(hsla(s.selected))), "{variant:?}");
            assert_eq!(style.text.color, Some(hsla(s.text)), "{variant:?}");
            assert_eq!(style.corner_radii.top_left, Some(px(theme.radii.sm).into()));
            assert_eq!(style.size.height, Some(px(theme.density.tab).into()));
            assert_eq!(style.size.width, Some(px(TAB_SLOT).into()), "one slot");
            assert_eq!(style.min_size.width, Some(px(TAB_FLOOR).into()), "one floor");
            assert!(style.border_widths.left.is_none(), "{variant:?}: no border");
            assert_eq!(style.text.font_weight, Some(gpui::FontWeight(400.0)), "never heavier");
            let mut rest = tab(&theme, div().id("b"), false, false);
            let style = rest.style();
            assert!(style.background.is_none(), "{variant:?}: bare");
            assert_eq!(style.text.color, Some(hsla(s.text_secondary)), "{variant:?}: receded");
            assert_eq!(style.text.font_weight, Some(gpui::FontWeight(400.0)));
        }
    }

    /// A pane's tab on show is a level under a title tab's: the hover's wash, not `selected`,
    /// in the text's full tone; one not on show is bare, as a title tab's is.
    #[test]
    fn a_pane_tab_on_show_is_a_level_under_a_title_tab() {
        for variant in [Variant::Dark, Variant::Light] {
            let theme = Theme::new(variant);
            let s = theme.surfaces;
            let mut shown = pane_tab(&theme, div().id("a"), true, true);
            let style = shown.style();
            assert_eq!(style.background, Some(gpui::Fill::from(hsla(s.hover))), "{variant:?}");
            assert_eq!(style.text.color, Some(hsla(s.text)), "{variant:?}");
            assert_ne!(s.hover, s.selected, "{variant:?}: two levels");
            let mut rest = pane_tab(&theme, div().id("b"), false, true);
            assert!(rest.style().background.is_none(), "{variant:?}: bare");
        }
    }
}

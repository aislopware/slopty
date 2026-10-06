//! The one look of a tab, the title bar's and a pane's alike: Zed's tab, squared
//! (`.research/zed-warp-system-2026-10-06.md`, item 3).
//!
//! A row of tabs lies on the chrome step with a sash line along its foot. The tab on show takes
//! the content's ground, draws a sash on either side and breaks the foot line, so it opens into
//! what it shows: the whole layout for a title tab, the pane's body for a pane's. The others are
//! bare and take the hover wash under the pointer. Every corner is square. A tab's close is a
//! small square box at the least radius.

use gpui::{Div, InteractiveElement as _, ParentElement as _, Stateful, Styled as _, div, px};
use slopty_theme::{Theme, stroke};

use crate::colors::hsla;
use crate::kit;

/// How a tab stands among its row's.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Look {
    /// It is the one on show.
    pub shown: bool,
    /// It is the first of a row that starts at its pane's own edge, where the sash on its left
    /// would draw the edge twice.
    pub first: bool,
    /// It carries the focus green's top edge: the shown tab of the focused pane, while the tab
    /// on show holds two panes or more.
    pub marked: bool,
}

/// `row`, a row of tabs or a bar that tabs stand in: on the chrome step, the sash along its
/// foot. The line is the row's first child and lies inside it, so a shown tab laid over it to
/// the row's foot covers it and opens into what it shows. Children added after it paint over it.
#[must_use]
pub(super) fn row<E: gpui::Styled + gpui::ParentElement>(theme: &Theme, row: E) -> E {
    let s = &theme.surfaces;
    let foot = div().absolute().left_0().right_0().bottom_0().h(kit::HAIR).bg(hsla(s.sash));
    row.relative().bg(hsla(s.chrome)).child(foot)
}

/// `tab` drawn as `look` says, its row's height and square: it reaches the row's foot, so the
/// shown one's ground covers the foot line ([`row`]). The caller gives it its identity, its
/// width, its padding and its children.
#[must_use]
pub(super) fn tab(theme: &Theme, tab: Stateful<Div>, look: Look) -> Stateful<Div> {
    let s = &theme.surfaces;
    let tab = tab.relative().h_full().flex().items_center();
    if !look.shown {
        let hover = hsla(s.hover);
        return tab.hover(move |el| el.bg(hover));
    }
    let mark = look
        .marked
        .then(|| div().absolute().left_0().right_0().top_0().h(px(stroke::MARK)).bg(hsla(s.focus)));
    let tab = tab.bg(hsla(theme.content())).border_r(kit::HAIR).border_color(hsla(s.sash));
    let tab = if look.first { tab } else { tab.border_l(kit::HAIR) };
    tab.children(mark)
}

/// A tab's close ([`kit::close_box`]): shown at rest on the tab on show, else only while the
/// pointer is on the tab of hover group `group`.
#[must_use]
pub(super) fn close(
    theme: &Theme,
    id: String,
    label: &'static str,
    shown: bool,
    group: &'static str,
    k: f32,
) -> Stateful<Div> {
    let close = kit::close_box(theme, id, label, k);
    if shown { close } else { close.invisible().group_hover(group, gpui::Styled::visible) }
}

#[cfg(test)]
mod tests {
    use gpui::{InteractiveElement as _, Styled as _, div};
    use slopty_theme::{Theme, Variant};

    use super::{Look, tab};
    use crate::colors::hsla;

    /// The shown tab stands on the content's ground between two sash lines; the others are
    /// bare, and none is rounded.
    #[test]
    fn the_shown_tab_takes_the_ground_between_two_sashes() {
        for variant in [Variant::Dark, Variant::Light] {
            let theme = Theme::new(variant);
            let s = theme.surfaces;
            let look = Look { shown: true, ..Look::default() };
            let mut shown = tab(&theme, div().id("a"), look);
            let style = shown.style();
            let ground = hsla(theme.content());
            assert_eq!(style.background, Some(gpui::Fill::from(ground)), "{variant:?}");
            assert_eq!(style.border_color, Some(hsla(s.sash)), "{variant:?}");
            assert!(style.border_widths.left.is_some() && style.border_widths.right.is_some());
            assert!(style.corner_radii.top_left.is_none(), "{variant:?}: square");
            let mut rest = tab(&theme, div().id("b"), Look::default());
            let style = rest.style();
            assert!(style.background.is_none(), "{variant:?}: bare");
            assert!(style.border_widths.left.is_none(), "{variant:?}: no sash");
        }
    }
}

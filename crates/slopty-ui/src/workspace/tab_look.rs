//! The one look of a tab: Zed's tab (`.research/zed-clone-2026-10-11.md` §3.4 and §5.1). Pane
//! tab bars are the only tabs; a tile alone in its pane has one cell.
//!
//! A row of tabs is Zed's tab bar: the tab bar's plane with the `stroke` line along its foot.
//! Every tab is a square cell the bar's full height, as wide as its title between
//! [`TAB_FLOOR`] and [`TAB_SLOT`]. The tab on show is the work's ground, the `stroke` line on
//! its two sides and none under it, so it opens into the pane below; the rest are bare words
//! in `text_secondary` that take the hover wash and the full tone under the pointer. Its words
//! never change weight. A tab leads with one 16 pt mark, and ends in a close laid over its end.
//!
//! A row's tabs move alike ([`Motion`]): one that opens grows from nothing to its slot over
//! 200 ms on [`Curve::TAB`] (`MonoCode`'s `tab-opening`), and tabs put in another order slide to
//! their places over 160 ms. Under Reduce Motion each lands at once.

use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Bounds, Div, ElementId, InteractiveElement as _, ParentElement as _, Pixels, Stateful,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use slopty_kit::colors::hsla;
use slopty_kit::icons::Mark;
use slopty_kit::kit::{self, Pace};
use slopty_theme::{Curve, Theme, stroke};

/// The widest a tab grows with its title: Zed's tabs end near 240.
pub(super) const TAB_SLOT: f32 = 240.0;

/// The least a tab gives way to while the row runs out of room.
pub(super) const TAB_FLOOR: f32 = 120.0;

/// The gap between a tab's lead, its title and what follows: Zed's tab's `gap_1`.
pub(super) const TAB_GAP: f32 = 4.0;

/// How far a tab's title fades out over its end while it is cut.
pub(super) const TITLE_FADE: f32 = 40.0;

/// The side of the dot that says a pane of a split has the keyboard: Zed's indicator dot.
pub(super) const FOCUS_DOT: f32 = 6.0;

/// The side of the dot that says a file has an edit not yet on disk: Zed's indicator dot.
const EDITED_DOT: f32 = 6.0;

/// `row`, a row of tabs or a bar that tabs stand in, with the one structural `stroke` line along
/// its foot, as every bar's; the caller lays its plane (the tab bar's where tabs stand). The line
/// is the row's first child and lies inside it, so what is added after it (the tab on show) paints
/// over it.
#[must_use]
pub(super) fn row<E: gpui::Styled + gpui::ParentElement>(theme: &Theme, row: E) -> E {
    let s = theme.surfaces;
    let foot = div().absolute().left_0().right_0().bottom_0().h(kit::HAIR).bg(hsla(s.stroke));
    row.relative().child(foot)
}

/// `el` laid out as a strip of tabs: Zed's, the cells edge to edge from the bar's start, the
/// full height of the bar.
#[must_use]
pub(super) fn strip<E: gpui::Styled>(_theme: &Theme, el: E) -> E {
    el.flex().items_stretch()
}

/// How far in a tab's lead stands from the cell's start: the pane's 8 pt edge, so the first
/// tab's glyph sits on the edge the pane's content keeps.
#[must_use]
pub(super) const fn cell_inset(theme: &Theme) -> f32 {
    theme.spacing.sm
}

/// `tab` as Zed's tab: a square cell its bar's full height, as wide as its title
/// between [`TAB_FLOOR`] and [`TAB_SLOT`], its words at the row role (Delta's header title,
/// 12, measured from the signed-in app). On show it is the
/// work's ground with the `stroke` line on its two sides and none under it, so it opens into
/// the pane; at rest it is bare, in `text_secondary`. Its lead stands [`cell_inset`] in, past
/// its side line. `closable` leaves room at its end for the close
/// [`close`] lays over it. The caller gives it its identity and its children.
#[must_use]
pub(super) fn tab(theme: &Theme, tab: Stateful<Div>, shown: bool, closable: bool) -> Stateful<Div> {
    let s = theme.surfaces;
    let spacing = theme.spacing;
    let end = if closable { 2.0_f32.mul_add(spacing.xs, kit::CLOSE_BOX) } else { spacing.sm };
    let tab = kit::typed(tab, theme.roles().row)
        .relative()
        .flex_initial()
        .flex_shrink(1.0)
        .max_w(px(TAB_SLOT))
        .min_w(px(TAB_FLOOR))
        .h_full()
        .flex()
        .items_center()
        .gap(px(TAB_GAP))
        .pl(px(cell_inset(theme) - stroke::LINE))
        .pr(px(end))
        .border_l(kit::HAIR)
        .border_r(kit::HAIR)
        .whitespace_nowrap();
    let tab = kit::eased(tab);
    if shown {
        tab.bg(hsla(s.ground)).border_color(hsla(s.stroke)).text_color(hsla(s.text))
    } else {
        tab.border_color(gpui::transparent_black())
            .text_color(hsla(s.text_secondary))
            .hover(move |el| el.bg(hsla(s.hover)).text_color(hsla(s.text)))
    }
}

/// A pane's tab: [`tab`], as Zed draws a pane's tabs.
#[must_use]
pub(super) fn pane_tab(
    theme: &Theme,
    el: Stateful<Div>,
    shown: bool,
    closable: bool,
) -> Stateful<Div> {
    tab(theme, el, shown, closable)
}

/// A tab's lead: `mark` in one 16 pt slot, in `icon` on the tab on show and the leads'
/// `icon_muted` on the rest, as every lead is (`docs/decisions/ui.md`, "The chrome's icons are
/// Octicons at 14").
#[must_use]
pub(super) fn lead(theme: &Theme, mark: impl Into<Mark>, shown: bool) -> Stateful<Div> {
    let s = theme.surfaces;
    let ink = if shown { s.icon } else { s.icon_muted };
    crate::palette::lead_mark(theme, mark, hsla(ink), theme.icon())
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
/// screen reader: Zed's dirty indicator, 6 pt in the accent's fill.
#[must_use]
pub(super) fn edited(theme: &Theme, id: impl Into<ElementId>) -> Stateful<Div> {
    div()
        .id(id)
        .role(gpui::accesskit::Role::Label)
        .aria_label(super::tile::EDITED)
        .flex_none()
        .size(px(EDITED_DOT))
        .rounded(px(theme.radii.full))
        .bg(hsla(theme.surfaces.accent_fill))
}

/// The dot that leads the tab on show in each pane while the tab holds two panes or more: Zed's
/// 6 pt indicator in the accent on the pane with the keyboard, empty elsewhere, keeping its
/// place so nothing after it moves as the keyboard goes between panes.
#[must_use]
pub(super) fn focus_dot(theme: &Theme, focused: bool) -> Div {
    div()
        .flex_none()
        .size(px(FOCUS_DOT))
        .rounded(px(theme.radii.full))
        .when(focused, |el| el.bg(hsla(theme.surfaces.accent_fill)))
}

/// The row whose tabs are on show, as a number its key hashes to (a project's, a pane's), and
/// the instant the frame stands for; `moves` off (Reduce Motion, a test's frame) lands every
/// tab at once.
#[derive(Clone, Copy, Debug)]
pub(super) struct Clock {
    pub row: u64,
    pub now: Instant,
    pub moves: bool,
}

/// How a row's tabs move, worked out as the row is built from the tabs it drew last, while
/// motion is on: one new since grows from nothing to its slot, and tabs whose order changed
/// slide from where they stood to their new place. Another row's tabs shown anew move nothing.
#[derive(Debug)]
pub(super) struct Motion<K> {
    row: Option<u64>,
    last: Vec<K>,
    /// Tabs opened, and when.
    opening: Vec<(K, Instant)>,
    /// Tabs sliding to a new place: how far left of it they stood, and since when.
    slides: Vec<(K, Pixels, Instant)>,
}

impl<K> Default for Motion<K> {
    fn default() -> Self {
        Self { row: None, last: Vec::new(), opening: Vec::new(), slides: Vec::new() }
    }
}

/// How a row's tabs move in one frame ([`Motion::step`]).
#[derive(Debug)]
pub(super) struct Moves<K> {
    /// The tabs still growing, and how far along (0 to 1).
    pub opening: Vec<(K, f32)>,
    /// The tabs still sliding, and how far left of their place they stand.
    pub slides: Vec<(K, Pixels)>,
}

impl<K> Default for Moves<K> {
    fn default() -> Self {
        Self { opening: Vec::new(), slides: Vec::new() }
    }
}

impl<K: Copy + PartialEq> Moves<K> {
    /// Whether anything moves, so the next frame is drawn.
    pub const fn any(&self) -> bool {
        !self.opening.is_empty() || !self.slides.is_empty()
    }

    /// How far tab `id` stands left of its place, while it slides.
    pub fn slide(&self, id: K) -> Option<Pixels> {
        self.slides.iter().find(|(t, _)| *t == id).map(|(_, dx)| *dx)
    }

    /// How far tab `id` has grown, while it grows.
    pub fn growth(&self, id: K) -> Option<f32> {
        self.opening.iter().find(|(t, _)| *t == id).map(|(_, t)| *t)
    }
}

impl<K: Copy + PartialEq> Motion<K> {
    /// How the row moves at `clock.now` with tabs `now`, `place` saying where each lay last
    /// frame (inside its two side lines).
    pub fn step(
        &mut self,
        now: &[K],
        place: impl Fn(K) -> Option<Bounds<Pixels>>,
        clock: Clock,
    ) -> Moves<K> {
        if self.row == Some(clock.row) && clock.moves {
            self.start(now, place, clock.now);
        } else {
            self.opening.clear();
            self.slides.clear();
        }
        self.row = Some(clock.row);
        self.last.clear();
        self.last.extend_from_slice(now);
        let (grow, slide) = (Pace::Sheet.duration(), Pace::Settle.duration());
        let live = |start: Instant, length| clock.now.saturating_duration_since(start) < length;
        self.opening.retain(|(_, start)| live(*start, grow));
        self.slides.retain(|(.., start)| live(*start, slide));
        let along = |start: Instant, length: Duration| {
            let gone = clock.now.saturating_duration_since(start);
            (gone.as_secs_f32() / length.as_secs_f32()).min(1.0)
        };
        let settle = Pace::Settle.curve();
        Moves {
            opening: self
                .opening
                .iter()
                .map(|(id, start)| (*id, Curve::TAB.at(along(*start, grow))))
                .collect(),
            slides: self
                .slides
                .iter()
                .map(|(id, dx, start)| (*id, *dx * (1.0 - settle.at(along(*start, slide)))))
                .collect(),
        }
    }

    /// Start what `now` moves against the tabs drawn last: a tab new since grows, and when the
    /// tabs on both lists stand in another order, each that moved slides from where it stood
    /// (by `place`, last frame's) to where it lands: the row's first place, then each tab
    /// before it at its own width, as tabs are as wide as their titles.
    fn start(&mut self, now: &[K], place: impl Fn(K) -> Option<Bounds<Pixels>>, at: Instant) {
        let had = |id: K| self.last.contains(&id);
        // A first frame has nothing it grew from.
        if !self.last.is_empty() {
            let new: Vec<K> = now.iter().copied().filter(|t| !had(*t)).collect();
            self.opening.extend(new.into_iter().map(|id| (id, at)));
        }
        let before: Vec<K> = self.last.iter().copied().filter(|t| now.contains(t)).collect();
        let after: Vec<K> = now.iter().copied().filter(|t| had(*t)).collect();
        if before == after {
            return;
        }
        let first = before.iter().filter_map(|t| place(*t)).map(|b| b.left()).reduce(Pixels::min);
        let Some(mut lands) = first else { return };
        for id in &after {
            let Some(from) = place(*id) else { continue };
            let dx = from.left() - lands;
            // The next lands past this one's cell: its spot and its two side lines.
            lands += from.size.width + kit::HAIR * 2.0;
            if dx != px(0.0) {
                self.slides.retain(|(t, ..)| t != id);
                self.slides.push((*id, dx, at));
            }
        }
    }
}

/// `el`, a tab opened `growth` of the way (0 to 1): inside a slot whose ceiling widens from
/// nothing to [`TAB_SLOT`], its floor growing with it, while the tab keeps its own width
/// inside; the slot stops at the tab's own width.
#[must_use]
pub(super) fn growing(el: impl gpui::IntoElement, growth: f32) -> Div {
    div()
        .flex()
        .flex_initial()
        .flex_shrink(1.0)
        .max_w(px(TAB_SLOT * growth))
        .min_w(px(TAB_FLOOR * growth))
        .h_full()
        .overflow_hidden()
        .child(el)
}

#[cfg(test)]
mod tests {
    use gpui::{InteractiveElement as _, Styled as _, div, px};
    use slopty_kit::colors::hsla;
    use slopty_theme::{Theme, Variant};

    use super::{TAB_FLOOR, TAB_SLOT, pane_tab, tab};

    /// A way to draw a tab: [`tab`] or [`pane_tab`].
    type Look = fn(&Theme, gpui::Stateful<gpui::Div>, bool, bool) -> gpui::Stateful<gpui::Div>;

    /// Zed's tab: the one on show is the work's ground with the `stroke` line on its sides,
    /// the rest bare words in the secondary tone. Both are square, the density's tab tall, as
    /// wide as their words within one floor and one ceiling, at the regular weight; a pane's
    /// tab is the same.
    #[test]
    fn the_shown_tab_opens_into_its_pane_and_the_rest_are_bare() {
        for variant in [Variant::Dark, Variant::Light] {
            let theme = Theme::new(variant);
            let s = theme.surfaces;
            let draws: [Look; 2] = [tab, pane_tab];
            for draw in draws {
                let mut shown = draw(&theme, div().id("a"), true, true);
                let style = shown.style();
                assert_eq!(style.background, Some(gpui::Fill::from(hsla(s.ground))), "{variant:?}");
                assert_eq!(style.text.color, Some(hsla(s.text)), "{variant:?}");
                assert_eq!(style.border_color, Some(hsla(s.stroke)), "{variant:?}: its sides");
                assert!(style.border_widths.bottom.is_none(), "{variant:?}: open below");
                assert!(style.corner_radii.top_left.is_none(), "{variant:?}: square");
                assert_eq!(style.size.height, Some(gpui::relative(1.0).into()), "its bar's height");
                assert_eq!(style.max_size.width, Some(px(TAB_SLOT).into()), "one ceiling");
                assert_eq!(style.min_size.width, Some(px(TAB_FLOOR).into()), "one floor");
                assert_eq!(style.text.font_weight, Some(gpui::FontWeight(400.0)), "never heavier");
                let mut rest = draw(&theme, div().id("b"), false, false);
                let style = rest.style();
                assert!(style.background.is_none(), "{variant:?}: bare");
                assert_eq!(style.text.color, Some(hsla(s.text_secondary)), "{variant:?}: receded");
                assert_eq!(style.text.font_weight, Some(gpui::FontWeight(400.0)));
            }
        }
    }
}

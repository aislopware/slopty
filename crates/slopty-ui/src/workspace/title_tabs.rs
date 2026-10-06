//! The title bar's tabs: the tabs of the project on show, each a tiling layout
//! ([`slopty_client::layout::tiling`]).
//!
//! A tab says the title of its focused work, then a mark for each agent in it that works or
//! has finished (`MonoCode`'s `TabHarnesses`), then its close. They are drawn as every tab is
//! ([`super::tab_look`]): the one on show opens into the layout under the bar, the rest are
//! bare words that take the hover wash. Tabs that do not fit scroll, and chevrons at the row's
//! ends step it a tab's width at a time while there is more past them.
//!
//! A tab pressed and moved is carried ([`super::area`]): along the row it moves, onto a
//! project's row in the navigator it goes to that project. While something carried is over
//! the row, a mark stands where it would land ([`render`]'s `drop_at`).

use std::rc::Rc;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    Context, FontWeight, InteractiveElement as _, IntoElement as _, MouseButton, MouseDownEvent,
    ParentElement as _, ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _,
    Window, div, px,
};
use slopty_client::layout::tiling::TabId;
use slopty_theme::{Theme, Typography, stroke};

use super::area::{self, DropSpots};
use super::tab_look::{self, Look};
use crate::colors::hsla;
use crate::draw::Draw;
use crate::icons::{Status, Symbol};
use crate::kit;

/// A tab's width bounds, in points: room for a short title, and no more than a long one needs.
const TAB_MIN: f32 = 96.0;
const TAB_MAX: f32 = 220.0;

/// The hover group of one tab, so its close shows with the pointer on it.
const TAB_GROUP: &str = "title-tab";

/// What the close button says.
pub(super) const CLOSE_TAB: &str = "Close tab";

/// One title tab, as the row draws it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct TitleTab {
    /// Which.
    pub id: TabId,
    /// The title of its focused work.
    pub title: SharedString,
    /// One mark for each agent in it that works or has finished, in pane order.
    pub marks: Vec<Status>,
    /// It is the one on show.
    pub shown: bool,
}

/// What the row asks of its host.
pub(super) trait TitleTabsHost: Sized + 'static {
    /// Show tab `id`.
    fn show_title_tab(&mut self, id: TabId, cx: &mut Context<Self>);

    /// Tab `id` was pressed at `ev`: a move from here carries it.
    fn carry_title_tab(&mut self, id: TabId, ev: &MouseDownEvent);

    /// Close tab `id`, and what is in it.
    fn close_title_tab(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>);
}

/// What [`render`] draws besides the tabs: where the row takes a drop, and where a drop
/// would land, before the tab at that index (past the last, after it).
pub(super) struct Drops<'a> {
    pub spots: &'a Rc<DropSpots>,
    pub at: Option<usize>,
}

/// The row of `tabs`, scrolled by `scroll`, writing where it and each tab lie to `drops`.
pub(super) fn render<V: TitleTabsHost>(
    theme: &Theme,
    tabs: &[TitleTab],
    scroll: &ScrollHandle,
    drops: &Drops<'_>,
    cx: &Draw<'_, V>,
) -> gpui::AnyElement {
    let s = &theme.surfaces;
    drops.spots.tabs.borrow_mut().clear();
    let last = tabs.len().saturating_sub(1);
    let items: Vec<gpui::AnyElement> =
        tabs.iter()
            .enumerate()
            .map(|(i, tab)| {
                let id = tab.id;
                let spots = Rc::clone(drops.spots);
                let spot = area::spot(move |b| spots.put_tab(id, b));
                // The mark of a drop: at the tab's leading edge, or the last one's trailing.
                let mark = (drops.at == Some(i) || (i == last && drops.at == Some(tabs.len())))
                    .then(|| {
                        let bar = div()
                            .debug_selector(|| "title-tabs-drop".to_owned())
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .w(px(stroke::MARK))
                            .bg(hsla(s.focus));
                        if drops.at == Some(i) { bar.left_0() } else { bar.right_0() }
                    });
                let n = id.get();
                let ink = if tab.shown { s.text } else { s.text_secondary };
                // Each mark under an id of its own: every one is a "status" image to the a11y tree.
                let marks = tab.marks.iter().enumerate().map(|(i, st)| {
                    div()
                        .id(("title-tab-mark", i))
                        .flex_none()
                        .debug_selector(move || format!("title-tab-mark-{n}-{i}"))
                        .child(crate::icons::status_mark(theme, Some(*st)))
                        .into_any_element()
                });
                let id_close = format!("title-tab-close-{n}");
                let close = tab_look::close(theme, id_close, CLOSE_TAB, tab.shown, TAB_GROUP)
                    .on_click(cx.listener(move |this: &mut V, _ev, window, cx| {
                        this.close_title_tab(id, window, cx);
                    }));
                let look = Look { shown: tab.shown, ..Look::default() };
                tab_look::tab(theme, div().id(("title-tab", n)), look)
                    .debug_selector(move || format!("title-tab-{n}"))
                    .group(TAB_GROUP)
                    .role(Role::Tab)
                    .aria_label(tab.title.clone())
                    .aria_selected(tab.shown)
                    .flex_initial()
                    .min_w(px(TAB_MIN))
                    .max_w(px(TAB_MAX))
                    .gap(px(theme.spacing.xs))
                    .pl(px(theme.spacing.sm))
                    .pr(px(theme.spacing.xs))
                    .text_color(hsla(ink))
                    .when(tab.shown, |el| el.font_weight(FontWeight(Typography::MEDIUM_WEIGHT)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this: &mut V, ev: &MouseDownEvent, _window, cx| {
                            this.show_title_tab(id, cx);
                            this.carry_title_tab(id, ev);
                            cx.stop_propagation();
                        }),
                    )
                    .child(
                        div()
                            .flex_auto()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(tab.title.clone()),
                    )
                    .children(marks)
                    .child(close)
                    .child(spot)
                    .children(mark)
                    .into_any_element()
            })
            .collect();
    // The chevrons show only while tabs lie past that end.
    let (offset, most) = (scroll.offset().x, scroll.max_offset().x);
    let back = offset < px(0.0);
    let on = most > px(0.0) && offset > -most;
    let step = px(TAB_MIN);
    let chevron = |name: &'static str, symbol: Symbol, label: &'static str, by: gpui::Pixels| {
        let scroll = scroll.clone();
        kit::icon_button(theme, name, symbol, label).on_click(move |_ev, window, _cx| {
            let at = scroll.offset();
            let x = (at.x + by).min(px(0.0)).max(-scroll.max_offset().x);
            scroll.set_offset(gpui::point(x, at.y));
            window.refresh();
        })
    };
    let strip = div()
        .id("title-tabs")
        .debug_selector(|| "title-tabs".to_owned())
        .role(Role::TabList)
        .flex_initial()
        .min_w_0()
        .h_full()
        .flex()
        .overflow_x_scroll()
        .track_scroll(scroll)
        .children(items);
    let spots = Rc::clone(drops.spots);
    let spot = area::spot(move |b| spots.strip.set(Some(b)));
    // As wide as its tabs and no wider, so what is left of the bar stays its empty span.
    div()
        .relative()
        .flex_initial()
        .min_w_0()
        .h_full()
        .flex()
        .items_center()
        .when(back, |el| {
            el.child(chevron("title-tabs-back", Symbol::ChevronLeft, "Earlier tabs", step))
        })
        .child(strip)
        .when(on, |el| {
            el.child(chevron("title-tabs-on", Symbol::ChevronRight, "Later tabs", -step))
        })
        .child(spot)
        .into_any_element()
}

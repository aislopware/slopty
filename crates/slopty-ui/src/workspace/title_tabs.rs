//! The title bar's tabs: the tabs of the project on show, each a tiling layout
//! ([`slopty_client::layout::tiling`]).
//!
//! A tab says the title of its focused work, then a mark for each agent in it that works or
//! has finished (`MonoCode`'s `TabHarnesses`), then its close. The one on show rests on the hover
//! fill; the rest are quiet words that take it under the pointer. Tabs that do not fit scroll,
//! and chevrons at the strip's ends step it a tab's width at a time while there is more past
//! them.

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    Context, FontWeight, InteractiveElement as _, IntoElement as _, MouseButton, MouseDownEvent,
    ParentElement as _, ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _,
    Window, div, px,
};
use slopty_client::layout::tiling::TabId;
use slopty_theme::{Theme, Typography};

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

/// One title tab, as the strip draws it.
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

/// What the strip asks of its host.
pub(super) trait TitleTabsHost: Sized + 'static {
    /// Show tab `id`.
    fn show_title_tab(&mut self, id: TabId, cx: &mut Context<Self>);

    /// Close tab `id`, and what is in it.
    fn close_title_tab(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>);
}

/// The strip of `tabs`, scrolled by `scroll`.
pub(super) fn render<V: TitleTabsHost>(
    theme: &Theme,
    tabs: &[TitleTab],
    scroll: &ScrollHandle,
    cx: &Draw<'_, V>,
) -> gpui::AnyElement {
    let s = &theme.surfaces;
    let row = theme.density.row;
    let items: Vec<gpui::AnyElement> = tabs
        .iter()
        .map(|tab| {
            let id = tab.id;
            let n = id.get();
            let ink = if tab.shown { s.text } else { s.text_secondary };
            // Each mark under an id of its own: every one is a "status" image to the a11y tree.
            let marks = tab.marks.iter().enumerate().map(|(i, st)| {
                div()
                    .id(("title-tab-mark", i))
                    .flex_none()
                    .debug_selector(move || format!("title-tab-mark-{n}-{i}"))
                    .child(crate::icons::status_mark(theme, Some(*st), 1.0))
                    .into_any_element()
            });
            let close =
                kit::icon_button(theme, format!("title-tab-close-{n}"), Symbol::Xmark, CLOSE_TAB)
                    .on_click(cx.listener(move |this: &mut V, _ev, window, cx| {
                        this.close_title_tab(id, window, cx);
                    }));
            let close = if tab.shown {
                close.into_any_element()
            } else {
                close.invisible().group_hover(TAB_GROUP, gpui::Styled::visible).into_any_element()
            };
            div()
                .id(("title-tab", n))
                .debug_selector(move || format!("title-tab-{n}"))
                .group(TAB_GROUP)
                .role(Role::Tab)
                .aria_label(tab.title.clone())
                .aria_selected(tab.shown)
                .flex_initial()
                .min_w(px(TAB_MIN))
                .max_w(px(TAB_MAX))
                .h(px(row))
                .flex()
                .items_center()
                .gap(px(theme.spacing.xs))
                .pl(px(theme.spacing.sm))
                .pr(px(theme.spacing.xxs))
                .rounded(px(theme.radii.sm))
                .text_color(hsla(ink))
                .when(tab.shown, |el| {
                    el.bg(hsla(s.hover)).font_weight(FontWeight(Typography::MEDIUM_WEIGHT))
                })
                .when(!tab.shown, |el| el.hover(move |el| el.bg(hsla(s.hover))))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this: &mut V, _ev: &MouseDownEvent, _window, cx| {
                        this.show_title_tab(id, cx);
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
        .items_center()
        .gap(px(theme.spacing.xxs))
        .overflow_x_scroll()
        .track_scroll(scroll)
        .children(items);
    // As wide as its tabs and no wider, so what is left of the bar stays its empty span.
    div()
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
        .into_any_element()
}

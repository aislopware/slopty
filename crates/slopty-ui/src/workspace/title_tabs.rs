//! The title bar's tabs: the tabs of the project on show, each a tiling layout
//! ([`slopty_client::layout::tiling`]).
//!
//! A tab says the title of its focused work, then a mark for each agent in it that works or
//! has finished (`MonoCode`'s `TabHarnesses`), then its close. They are drawn as every tab is
//! ([`super::tab_look`]): the one on show opens into the layout under the bar, the rest are
//! bare words that take the hover wash. Tabs that do not fit scroll, and an end past which tabs
//! lie fades out as deep as they run past it.
//!
//! A tab that closes folds its width away over 200 ms (`MonoCode`'s tab close, [`Pace::Sheet`])
//! while the tabs after it close the gap ([`Closing`]); under Reduce Motion it is gone at once.
//!
//! A right click or a long press on a tab opens its menu: close it, the others, those to its
//! right or to its left.
//!
//! A tab pressed and moved is carried ([`super::area`]): along the row it moves, onto a
//! project's row in the navigator it goes to that project. While something carried is over
//! the row, a mark stands where it would land ([`render`]'s `drop_at`).

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use gpui::accesskit::Role;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    Context, InteractiveElement as _, IntoElement as _, MouseButton, MouseDownEvent,
    ParentElement as _, Pixels, Point, ScrollHandle, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, div, px,
};
use slopty_client::layout::tiling::TabId;
use slopty_theme::{Theme, stroke};

use super::area::{self, DropSpots};
use super::tab_look::{self, Look};
use crate::colors::hsla;
use crate::draw::Draw;
use crate::icons::Status;
use crate::kit::{self, Pace};

/// A tab's width bounds, in points: room for a short title, and no more than a long one needs.
const TAB_MIN: f32 = 96.0;
const TAB_MAX: f32 = 220.0;

/// The widest the tab on show grows while it says where its one tile is too, or names it.
const TAB_WIDE: f32 = 340.0;

/// How much faster a tab's place shrinks than its title.
const PLACE_SHRINK: f32 = 1000.0;

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
    /// Where its one tile is, when it holds one alone and the bar is its header
    /// ([`super::tile_strip`]): a file's folder, beside the title, giving way first.
    pub place: Option<SharedString>,
    /// Its one tile is a file with an edit not yet on disk, said after the title as a
    /// document's title bar says it, when the bar is the tile's header.
    pub edited: bool,
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

    /// Tab `id` was pressed twice: its focused tile is named, or a preview kept.
    fn name_title_tab(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>);

    /// Close tab `id`, and what is in it.
    fn close_title_tab(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>);

    /// Tab `id` was pressed for its menu (a right click, a long press) at `at`.
    fn title_tab_menu(
        &mut self,
        id: TabId,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    );
}

/// What [`render`] draws besides the tabs: where the row takes a drop, where a drop would
/// land, before the tab at that index (past the last, after it), the tabs closing, and the
/// field that names the tab on show's one tile, in its title's place while it is open.
pub(super) struct Drops<'a> {
    pub spots: &'a Rc<DropSpots>,
    pub at: Option<usize>,
    pub closing: Option<(&'a Closing, Clock)>,
    pub field: Option<gpui::AnyElement>,
}

/// The project whose tabs are on show, as a number its key hashes to, and the instant the frame
/// stands for; `moves` off (Reduce Motion, a test's frame) closes a tab at once.
#[derive(Clone, Copy, Debug)]
pub(super) struct Clock {
    pub project: u64,
    pub now: Instant,
    pub moves: bool,
}

/// The title tabs that closed and still fold away, worked out as the row is built from the tabs
/// it drew last: one gone from the same project since, while motion is on, leaves a bare ghost
/// of its title at its last width, which narrows to nothing. Another project's tabs shown anew
/// close nothing. Read and written while the row is built, which never writes its host.
#[derive(Debug, Default)]
pub(super) struct Closing(RefCell<Folding>);

#[derive(Debug, Default)]
struct Folding {
    project: Option<u64>,
    last: Vec<(TabId, SharedString)>,
    ghosts: Vec<Ghost>,
}

/// A closed tab folding away: the tab it stood before (none past the last), its title, the
/// width it had, and when it closed.
#[derive(Clone, Debug)]
struct Ghost {
    before: Option<TabId>,
    title: SharedString,
    width: Pixels,
    start: Instant,
}

impl Folding {
    /// The ghosts at `clock.now`, each with its width then, from `tabs` and where `widths` says
    /// each tab lay last frame.
    fn step(
        &mut self,
        tabs: &[TitleTab],
        widths: &[(TabId, gpui::Bounds<Pixels>)],
        clock: Clock,
    ) -> Vec<(Option<TabId>, SharedString, Pixels)> {
        let now: Vec<(TabId, SharedString)> =
            tabs.iter().map(|t| (t.id, t.title.clone())).collect();
        let stays = |id: TabId| tabs.iter().any(|t| t.id == id);
        if self.project == Some(clock.project) && clock.moves {
            for (i, (id, title)) in self.last.iter().enumerate() {
                if stays(*id) {
                    continue;
                }
                let Some((_, at)) = widths.iter().find(|(t, _)| t == id) else { continue };
                let before = self.last.iter().skip(i).map(|(t, _)| *t).find(|t| stays(*t));
                let (title, width) = (title.clone(), at.size.width);
                self.ghosts.push(Ghost { before, title, width, start: clock.now });
            }
        } else {
            self.ghosts.clear();
        }
        self.project = Some(clock.project);
        self.last = now;
        let length = Pace::Sheet.duration();
        let curve = Pace::Sheet.curve();
        self.ghosts
            .retain(|g| clock.moves && clock.now.saturating_duration_since(g.start) < length);
        self.ghosts
            .iter()
            .map(|g| {
                let gone = clock.now.saturating_duration_since(g.start);
                let t = curve.at((gone.as_secs_f32() / length.as_secs_f32()).min(1.0));
                (g.before, g.title.clone(), g.width * (1.0 - t))
            })
            .collect()
    }
}

/// A closed tab's ghost at `width`: its title, bare, cut by the narrowing width.
fn ghost(theme: &Theme, title: SharedString, width: Pixels) -> gpui::AnyElement {
    let s = &theme.surfaces;
    kit::typed(div(), theme.roles().chrome)
        .debug_selector(|| "title-tab-closing".to_owned())
        .flex_none()
        .w(width)
        .h_full()
        .overflow_hidden()
        .flex()
        .items_center()
        .pl(px(theme.spacing.sm))
        .text_color(hsla(s.text_secondary))
        .child(div().flex_none().whitespace_nowrap().child(title))
        .into_any_element()
}

/// The row of `tabs`, scrolled by `scroll`, writing where it and each tab lie to `drops`.
pub(super) fn render<V: TitleTabsHost>(
    theme: &Theme,
    tabs: &[TitleTab],
    scroll: &ScrollHandle,
    drops: Drops<'_>,
    window: &Window,
    cx: &Draw<'_, V>,
) -> gpui::AnyElement {
    let s = &theme.surfaces;
    let mut field = drops.field;
    // Where each tab lay last frame is the width a closed one folds from.
    let ghosts = drops.closing.map_or_else(Vec::new, |(closing, clock)| {
        closing.0.borrow_mut().step(tabs, &drops.spots.tabs.borrow(), clock)
    });
    if !ghosts.is_empty() {
        window.request_animation_frame();
    }
    let ghosts_before = |before: Option<TabId>| -> Vec<gpui::AnyElement> {
        ghosts
            .iter()
            .filter(|(b, ..)| *b == before)
            .map(|(_, title, width)| ghost(theme, title.clone(), *width))
            .collect()
    };
    drops.spots.tabs.borrow_mut().clear();
    let last = tabs.len().saturating_sub(1);
    let mut items: Vec<gpui::AnyElement> =
        tabs.iter()
            .enumerate()
            .flat_map(|(i, tab)| {
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
                let host = cx.weak_entity();
                let el = kit::menu_press(div().id(("title-tab", n)), move |at, window, cx| {
                    let _gone = host.update(cx, |this, cx| this.title_tab_menu(id, at, window, cx));
                });
                // The chrome's own size, its words at the action role while it is on show: the
                // tab is a control's words, as the breadcrumb beside it is.
                let roles = theme.roles();
                let role = if tab.shown { roles.action } else { roles.chrome };
                let named = if tab.shown { field.take() } else { None };
                let widened = named.is_some() || tab.place.is_some() || tab.edited;
                let edited = tab.edited.then(|| {
                    kit::typed(div(), roles.metadata)
                        .id(("title-tab-edited", n))
                        .debug_selector(move || format!("title-tab-edited-{n}"))
                        .role(Role::Label)
                        .aria_label(super::tile::EDITED)
                        .flex_none()
                        .whitespace_nowrap()
                        .text_color(hsla(s.text_muted))
                        .child(super::tile::EDITED)
                });
                let title = div()
                    .debug_selector(move || format!("title-tab-text-{n}"))
                    .flex_auto()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(tab.title.clone());
                let place = tab.place.clone().map(|place| {
                    let mut el = div();
                    // Gives way long before the title does.
                    el.style().flex_shrink = Some(PLACE_SHRINK);
                    kit::typed(el, roles.metadata)
                        .debug_selector(move || format!("title-tab-place-{n}"))
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_color(hsla(s.text_muted))
                        .child(place)
                });
                let el = kit::typed(tab_look::tab(theme, el, look), role)
                    .debug_selector(move || format!("title-tab-{n}"))
                    .group(TAB_GROUP)
                    .role(Role::Tab)
                    .aria_label(tab.title.clone())
                    .aria_selected(tab.shown)
                    .flex_initial()
                    .min_w(px(TAB_MIN))
                    .max_w(px(if widened { TAB_WIDE } else { TAB_MAX }))
                    .gap(px(theme.spacing.xs))
                    .pl(px(theme.spacing.sm))
                    .pr(px(theme.spacing.xs))
                    .text_color(hsla(ink))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this: &mut V, ev: &MouseDownEvent, window, cx| {
                            if ev.click_count == 2 {
                                this.name_title_tab(id, window, cx);
                            } else {
                                this.show_title_tab(id, cx);
                                this.carry_title_tab(id, ev);
                            }
                            cx.stop_propagation();
                        }),
                    )
                    .map(|el| match named {
                        Some(field) => el.child(field),
                        None => el.child(title).children(edited).children(place),
                    })
                    .children(marks)
                    .child(close)
                    .child(spot)
                    .children(mark)
                    .into_any_element();
                // A tab closed just before this one folds away in its place.
                let mut here = ghosts_before(Some(id));
                here.push(el);
                here
            })
            .collect();
    items.extend(ghosts_before(None));
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
    // An end past which tabs lie fades out per pixel, as deep as they run past it, as a pane's
    // tabs do (`tile::render_tabs`). How far they run is known only once the strip is laid
    // out, so the fade reads it as the strip prepaints, in the frame that lays it out.
    let strip =
        gpui::edge_fade(strip, gpui::EdgeFade::x(px(theme.spacing.xl))).hidden_by_scroll(scroll);
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
        .child(strip)
        .child(spot)
        .into_any_element()
}

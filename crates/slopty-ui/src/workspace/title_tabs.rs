//! The title bar's tabs: the tabs of the project on show, each a tiling layout
//! ([`slopty_client::layout::tiling`]).
//!
//! A tab leads with one mark: a mark for each agent in it that works or has finished
//! (`MonoCode`'s `TabHarnesses`, up to three, then a count), else what its focused work is. Then
//! the title of its focused work, and its close under the pointer. They are drawn as every tab
//! is ([`super::tab_look`]): pills of one width on the bar's ground, the one on show on the
//! selected wash. Tabs that do not fit scroll, sideways under a vertical wheel too, and a
//! chevron floats over each end past which tabs lie, a press on it scrolling the row by most of
//! its width (`MonoCode`'s title strip).
//!
//! A tab that closes folds its width away over 200 ms (`MonoCode`'s tab close, on
//! [`Curve::TAB`]) while the tabs after it close the gap ([`Closing`]); under Reduce Motion it
//! is gone at once.
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
use slopty_theme::{Curve, Theme, stroke};

use super::area::{self, DropSpots};
use super::tab_look;
use crate::colors::hsla;
use crate::draw::Draw;
use crate::icons::{Mark, Status, Symbol};
use crate::kit::{self, Pace};

/// The most agents' marks a tab's lead shows before it counts the rest: `MonoCode`'s three.
const MARKS_SHOWN: usize = 3;

/// How far a tab's agents' marks overlap each other, in points: `MonoCode`'s `-space-x-0.5`.
const MARK_OVERLAP: f32 = 2.0;

/// The least a chevron scrolls the row by, in points: a tab's floor.
const SCROLL_LEAST: f32 = tab_look::TAB_FLOOR;

/// The share of the row's width a chevron scrolls it by.
const SCROLL_SHARE: f32 = 0.6;

/// The leading of a two-line tab's lines, over their size: `MonoCode`'s `leading-tight`.
const TWO_LINE_LEADING: f32 = 1.25;

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
    /// What its focused work is: its kind's glyph or its agent's mark, its lead while no agent
    /// in it works or has finished.
    pub lead: Mark,
    /// Where its one tile is, when it holds one alone and the bar is its header
    /// ([`super::tile_strip`]): a file's folder, beside the title, giving way first.
    pub place: Option<SharedString>,
    /// Its one tile is a file with an edit not yet on disk, a dot after the title, when the bar
    /// is the tile's header.
    pub edited: bool,
    /// What else it holds, when it holds more than its focused tile: the other's title, or how
    /// many tiles it holds. The tab then says it on a second line under its title
    /// (`MonoCode`'s `tabCopy`), so it never reads as its focused tile's pane tab again.
    pub meta: Option<SharedString>,
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
        let curve = Curve::TAB;
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
        .h(px(theme.density.tab))
        .overflow_hidden()
        .flex()
        .items_center()
        .pl(px(theme.spacing.sm))
        .text_color(hsla(s.text_secondary))
        .child(div().flex_none().whitespace_nowrap().child(title))
        .into_any_element()
}

/// A tab's lead: up to [`MARKS_SHOWN`] marks of its agents that work or have finished,
/// overlapping, then how many more; else what its focused work is.
fn tab_lead(theme: &Theme, tab: &TitleTab) -> gpui::AnyElement {
    let n = tab.id.get();
    if tab.marks.is_empty() {
        return tab_look::lead(theme, tab.lead, tab.shown)
            .debug_selector(move || format!("title-tab-lead-{n}"))
            .into_any_element();
    }
    let slot = crate::icons::IconSize::Inline.slot(theme);
    // Each mark under an id of its own: every one is a "status" image to the a11y tree.
    let marks = tab.marks.iter().take(MARKS_SHOWN).enumerate().map(|(i, st)| {
        div()
            .id(("title-tab-mark", i))
            .flex_none()
            .size(px(slot))
            .flex()
            .items_center()
            .justify_center()
            .when(i > 0, |el| el.ml(px(-MARK_OVERLAP)))
            .debug_selector(move || format!("title-tab-mark-{n}-{i}"))
            .child(crate::icons::status_mark(theme, Some(*st)))
            .into_any_element()
    });
    let more = tab.marks.len().saturating_sub(MARKS_SHOWN);
    let more = (more > 0).then(|| {
        kit::typed(div(), theme.roles().caption)
            .flex_none()
            .pl(px(theme.spacing.xxs))
            .text_color(hsla(theme.surfaces.text_muted))
            .child(format!("+{more}"))
    });
    div()
        .flex_none()
        .flex()
        .items_center()
        .when(!tab.shown, |el| el.opacity(slopty_theme::alpha::STRONG))
        .children(marks)
        .children(more)
        .into_any_element()
}

/// The chevron over the strip's `end` while tabs lie past it: a 26 pt square on the strong
/// hover's wash that scrolls the row by [`SCROLL_SHARE`] of its width, never less than
/// [`SCROLL_LEAST`].
fn chevron<V: TitleTabsHost>(
    theme: &Theme,
    scroll: &ScrollHandle,
    forward: bool,
    cx: &Draw<'_, V>,
) -> gpui::AnyElement {
    let s = theme.surfaces;
    let (id, icon, label) = if forward {
        ("title-tabs-forward", Symbol::ChevronRight, "Scroll tabs forward")
    } else {
        ("title-tabs-back", Symbol::ChevronLeft, "Scroll tabs back")
    };
    let side = px(kit::icon_button_side(theme));
    let glyph = crate::icons::IconSize::Inline;
    let handle = scroll.clone();
    let button = div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .role(Role::Button)
        .aria_label(label)
        .size(side)
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(theme.radii.sm))
        .bg(hsla(s.hover_strong))
        .text_color(hsla(s.text_secondary))
        .cursor_pointer()
        .child(
            crate::icons::Drawn::new(theme, icon, glyph)
                .slot(px(glyph.slot(theme)), hsla(s.text_secondary)),
        )
        .hover(move |el| el.bg(hsla(s.pressed)).text_color(hsla(s.text)))
        .on_mouse_down(MouseButton::Left, |_ev, _w, cx| cx.stop_propagation())
        .on_click(cx.listener(move |_this: &mut V, _ev, _window, cx| {
            let width = f32::from(handle.bounds().size.width);
            let step = (width * SCROLL_SHARE).max(SCROLL_LEAST);
            let (at, max) = (handle.offset(), handle.max_offset());
            let x = if forward { f32::from(at.x) - step } else { f32::from(at.x) + step };
            let x = x.clamp(-f32::from(max.x), 0.0);
            handle.set_offset(gpui::point(px(x), at.y));
            cx.notify();
        }));
    let at = div().absolute().top_0().bottom_0().flex().items_center();
    let at = if forward { at.right(px(theme.spacing.xs)) } else { at.left(px(theme.spacing.xs)) };
    at.child(kit::eased(button)).into_any_element()
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
    let mut items: Vec<gpui::AnyElement> = tabs
        .iter()
        .enumerate()
        .flat_map(|(i, tab)| {
            let id = tab.id;
            let spots = Rc::clone(drops.spots);
            let spot = area::spot(move |b| spots.put_tab(id, b));
            // The mark of a drop: at the tab's leading edge, or the last one's trailing.
            let mark =
                (drops.at == Some(i) || (i == last && drops.at == Some(tabs.len()))).then(|| {
                    let bar = div()
                        .debug_selector(|| "title-tabs-drop".to_owned())
                        .absolute()
                        .top(px(theme.spacing.xs))
                        .bottom(px(theme.spacing.xs))
                        .w(px(stroke::FOCUS))
                        .rounded(px(theme.radii.full))
                        .bg(hsla(s.accent_fill));
                    if drops.at == Some(i) { bar.left_0() } else { bar.right_0() }
                });
            let n = id.get();
            let id_close = format!("title-tab-close-{n}");
            let close = kit::close_box(theme, id_close, CLOSE_TAB).on_click(cx.listener(
                move |this: &mut V, _ev, window, cx| {
                    this.close_title_tab(id, window, cx);
                },
            ));
            let close = tab_look::close(theme, close, false, TAB_GROUP);
            let host = cx.weak_entity();
            let el = kit::menu_press(div().id(("title-tab", n)), move |at, window, cx| {
                let _gone = host.update(cx, |this, cx| this.title_tab_menu(id, at, window, cx));
            });
            let roles = theme.roles();
            let named = if tab.shown { field.take() } else { None };
            let edited = tab.edited.then(|| {
                tab_look::edited(theme, ("title-tab-edited", n))
                    .debug_selector(move || format!("title-tab-edited-{n}"))
            });
            let title = div()
                .debug_selector(move || format!("title-tab-text-{n}"))
                .flex_initial()
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
            let label = match &tab.meta {
                Some(meta) => SharedString::from(format!("{}, {meta}", tab.title)),
                None => tab.title.clone(),
            };
            let el = tab_look::tab(theme, el, tab.shown, true)
                .debug_selector(move || format!("title-tab-{n}"))
                .group(TAB_GROUP)
                .role(Role::Tab)
                .aria_label(label)
                .aria_selected(tab.shown)
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
                .child(tab_lead(theme, tab))
                .map(|el| match (named, tab.meta.clone()) {
                    (Some(field), _) => el.child(field),
                    (None, Some(meta)) => el.child(two_lines(theme, title, meta, n)),
                    (None, None) => el.child(title).children(edited).children(place),
                })
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
        .overflow_x_scroll()
        .track_scroll(scroll)
        .on_scroll_wheel(cx.listener(|_this: &mut V, _ev, _window, cx| cx.notify()))
        .children(items);
    let strip = tab_look::strip(theme, strip);
    // A chevron floats over an end while tabs lie past it, as the row is laid out in this
    // frame ([`PastEnd`]); the wheel draws the row again, so it follows the row as it scrolls.
    let back = PastEnd {
        child: Some(chevron(theme, scroll, false, cx)),
        scroll: scroll.clone(),
        forward: false,
        shown: false,
    };
    let forward = PastEnd {
        child: Some(chevron(theme, scroll, true, cx)),
        scroll: scroll.clone(),
        forward: true,
        shown: false,
    };
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
        .child(back)
        .child(forward)
        .into_any_element()
}

/// A chevron over the strip's `forward` end (else its start) that is drawn, and takes the
/// pointer, only while tabs lie past that end as the strip is laid out in the same frame: the
/// strip is prepainted before it, so its scroll reads this frame's room, never last frame's.
struct PastEnd {
    child: Option<gpui::AnyElement>,
    scroll: ScrollHandle,
    forward: bool,
    shown: bool,
}

impl gpui::IntoElement for PastEnd {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl gpui::Element for PastEnd {
    type PrepaintState = ();
    type RequestLayoutState = ();

    fn id(&self) -> Option<gpui::ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&gpui::GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut gpui::App,
    ) -> (gpui::LayoutId, Self::RequestLayoutState) {
        let layout = match &mut self.child {
            Some(child) => child.request_layout(window, cx),
            None => window.request_layout(gpui::Style::default(), [], cx),
        };
        (layout, ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&gpui::GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        _bounds: gpui::Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut gpui::App,
    ) -> Self::PrepaintState {
        let (at, max) = (self.scroll.offset().x, self.scroll.max_offset().x);
        let past = px(0.5);
        self.shown = if self.forward { at > -max + past } else { at < -past };
        if self.shown
            && let Some(child) = &mut self.child
        {
            child.prepaint(window, cx);
        }
    }

    fn paint(
        &mut self,
        _id: Option<&gpui::GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        _bounds: gpui::Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut gpui::App,
    ) {
        if self.shown
            && let Some(child) = &mut self.child
        {
            child.paint(window, cx);
        }
    }
}

/// A tab's title over what else it holds, `MonoCode`'s two-line tab: both lines at the caption's
/// size on a tight line so the two fit the pill, the title at the medium weight in the tab's
/// tone and the rest in the muted one.
fn two_lines(theme: &Theme, title: gpui::Div, meta: SharedString, n: u64) -> gpui::Div {
    let size = theme.typography.caption();
    let line = px(size * TWO_LINE_LEADING);
    let meta = div()
        .debug_selector(move || format!("title-tab-meta-{n}"))
        .min_w_0()
        .overflow_hidden()
        .text_ellipsis()
        .whitespace_nowrap()
        .text_size(px(size))
        .line_height(line)
        .text_color(hsla(theme.surfaces.text_muted))
        .child(meta);
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .justify_center()
        .child(
            title
                .text_size(px(size))
                .line_height(line)
                .font_weight(gpui::FontWeight(slopty_theme::Typography::MEDIUM_WEIGHT)),
        )
        .child(meta)
}

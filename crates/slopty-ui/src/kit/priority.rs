//! A row of a title, facts and controls that fits the room it is given: each item measured,
//! the least needed leaving first, and the title kept to a floor so it is never the one that
//! goes (`docs/decisions/ui.md`, "How surfaces adapt to their room").
//!
//! Before it, a row of `flex_none` chips beside a shrinkable title gave the title all the
//! shrinking: a thread beside a board, 312 pt wide, showed its pull request and worktree and no
//! title at all. Here every item is laid out at its own width first
//! ([`AnyElement::layout_as_root`], from its shaped text, not a guess); then, while the row
//! overflows, the item of the lowest [`Priority`] leaves, the trailing one first among equals, as
//! `NSToolbar`'s `visibilityPriority` has it. What left is named in the row's [`Dropped`], so its
//! menu ("…", "More") can offer it, and the menu's button shows only while something has left. The
//! title takes what the rest leaves, down to its floor, and ends in its own ellipsis.
//!
//! An [`PriorityRow::overlay`] takes no room: hidden controls that show on hover are laid over
//! the trailing end instead of keeping a blank where they would be.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    AnyElement, App, AvailableSpace, Bounds, Element, ElementId, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, Pixels, Refineable as _, SharedString, Size, Style,
    StyleRefinement, Styled, Window, point, px, relative,
};

/// How much an item is needed, against the others in its row: the lower leaves first.
///
/// Open, so a surface can rank between the named steps.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct Priority(pub u8);

impl Priority {
    /// Never leaves: a lead glyph, a close button.
    pub const ESSENTIAL: Self = Self(u8::MAX);
    /// What the row is mostly for, after the title: its state, its main control.
    pub const HIGH: Self = Self(192);
    /// A fact that only adds: a worktree's name, a place.
    pub const LOW: Self = Self(64);
    /// A fact or control worth keeping while there is room: a pull request, the worker.
    pub const MEDIUM: Self = Self(128);
}

/// The keys of the items a [`PriorityRow`] left out at its last layout, for its menu.
///
/// Shared with the closure that opens the menu, and kept by the view across frames; it is
/// written each time the row is laid out, so it says what the row on screen leaves out.
#[derive(Clone, Default, Debug)]
pub struct Dropped(Rc<RefCell<Vec<SharedString>>>);

impl Dropped {
    /// The keys left out, in the row's order.
    #[must_use]
    pub fn keys(&self) -> Vec<SharedString> {
        self.0.borrow().clone()
    }

    /// How many items were left out.
    #[must_use]
    pub fn count(&self) -> usize {
        self.0.borrow().len()
    }

    /// Whether `key` was left out.
    #[must_use]
    pub fn contains(&self, key: &str) -> bool {
        self.0.borrow().iter().any(|k| k == key)
    }

    /// Notes what the last layout left out; whether that changed.
    fn set(&self, keys: Vec<SharedString>) -> bool {
        let changed = *self.0.borrow() != keys;
        *self.0.borrow_mut() = keys;
        changed
    }
}

/// Which end of the row an item keeps to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Side {
    Leading,
    Trailing,
}

/// One item of a row.
struct Item {
    key: SharedString,
    priority: Priority,
    side: Side,
    element: AnyElement,
}

/// A row that fits its room by leaving out what is least needed; [`priority_row`] makes one.
pub struct PriorityRow {
    id: ElementId,
    style: StyleRefinement,
    items: Vec<Item>,
    /// The title: after the leading items, taking what is left down to its floor.
    title: Option<(AnyElement, Pixels)>,
    /// Items under it leave before the title narrows.
    title_priority: Priority,
    /// The title takes all the room the rest leave, not only its own width.
    title_fills: bool,
    /// Sized by its content, as its parent allows, rather than by its style.
    fit_content: bool,
    /// Where the next item goes.
    side: Side,
    menu: Option<AnyElement>,
    overlay: Option<AnyElement>,
    dropped: Dropped,
}

/// A row, `id`, of items that fits its width, as wide as its parent unless styled otherwise.
/// Give it a height, in which its items are centred, and a `gap` between them.
#[must_use]
pub fn priority_row(id: impl Into<ElementId>) -> PriorityRow {
    let mut style = StyleRefinement::default();
    style.size.width = Some(relative(1.).into());
    PriorityRow {
        id: id.into(),
        style,
        items: Vec::new(),
        title: None,
        title_priority: Priority::MEDIUM,
        title_fills: false,
        fit_content: false,
        side: Side::Leading,
        menu: None,
        overlay: None,
        dropped: Dropped::default(),
    }
}

impl PriorityRow {
    /// `element`, named `key` in [`Dropped`], leaving at `priority`.
    #[must_use]
    pub fn item(
        mut self,
        key: impl Into<SharedString>,
        priority: Priority,
        element: impl IntoElement,
    ) -> Self {
        self.items.push(Item {
            key: key.into(),
            priority,
            side: self.side,
            element: element.into_any_element(),
        });
        self
    }

    /// The title: placed after the items given before it, it takes the room the others leave,
    /// never more than its own width and never less than `floor` (or its own width, if
    /// narrower). It should end in an ellipsis when narrowed (`ChromeText::fill`).
    #[must_use]
    pub fn title(mut self, element: impl IntoElement, floor: Pixels) -> Self {
        self.items.push(Item {
            key: SharedString::default(),
            priority: Priority::ESSENTIAL,
            side: Side::Leading,
            element: div_marker(),
        });
        // In a block of its own, which takes the width it is laid out at: a flex row laid out
        // as a root keeps its content's width whatever room it is offered.
        let title = gpui::ParentElement::child(gpui::div(), element).into_any_element();
        self.title = Some((title, floor));
        self
    }

    /// Items under `priority` leave before the title narrows at all ([`Priority::MEDIUM`] by
    /// default: a worktree's name goes before a word of the title, a pull request stays while
    /// the title narrows to its floor).
    #[must_use]
    pub const fn title_priority(mut self, priority: Priority) -> Self {
        self.title_priority = priority;
        self
    }

    /// The title takes all the room the others leave, as a field that is being typed in does.
    #[must_use]
    pub const fn title_fills(mut self, fills: bool) -> Self {
        self.title_fills = fills;
        self
    }

    /// As wide as its items, the title whole, as its parent allows: it shrinks from there as a
    /// flex item, and leaves out what no longer fits.
    #[must_use]
    pub const fn fit_content(mut self) -> Self {
        self.fit_content = true;
        self
    }

    /// The items given after this keep to the trailing end.
    #[must_use]
    pub const fn end(mut self) -> Self {
        self.side = Side::Trailing;
        self
    }

    /// The row's menu button, last at the trailing end, shown only while something has left;
    /// it reads what from [`Self::dropped`].
    #[must_use]
    pub fn menu(mut self, element: impl IntoElement) -> Self {
        self.menu = Some(element.into_any_element());
        self
    }

    /// Laid over the trailing end at its own width, taking no room: controls hidden at rest
    /// that show on hover over the readouts they replace.
    #[must_use]
    pub fn overlay(mut self, element: impl IntoElement) -> Self {
        self.overlay = Some(element.into_any_element());
        self
    }

    /// Where the row says what it left out, for its menu.
    #[must_use]
    pub fn dropped(mut self, dropped: &Dropped) -> Self {
        self.dropped = dropped.clone();
        self
    }
}

/// The title's place among the items: an empty element never drawn, replaced as laid out.
fn div_marker() -> AnyElement {
    gpui::Empty.into_any_element()
}

/// The title among a row's items, for [`fit_row`].
#[derive(Clone, Copy, Debug)]
pub struct TitleFit {
    /// Its place among the items.
    pub at: usize,
    /// Its own width.
    pub natural: Pixels,
    /// The least it is narrowed to.
    pub least: Pixels,
    /// Items under it leave before it is narrowed at all; the rest stay while it narrows to
    /// its floor, and only then leave.
    pub priority: Priority,
}

/// Which items stay so that they, their gaps and the menu fit `room`.
///
/// The menu (`menu`) counts while anything leaves. The lowest priority leaves first, the
/// trailing one first among equals, and [`Priority::ESSENTIAL`] never. The title stands among
/// the items: whole while items under its priority leave, then at its floor while the rest
/// do. Returns whether each stays.
#[must_use]
pub fn fit_row(
    items: &[(Priority, Pixels)],
    title: Option<TitleFit>,
    gap: Pixels,
    menu: Option<Pixels>,
    room: Pixels,
) -> Vec<bool> {
    let mut widths = items.to_vec();
    let mut stays = vec![true; items.len()];
    let leave = |widths: &[(Priority, Pixels)], stays: &mut Vec<bool>, under: Priority| {
        while needed(widths, stays, gap, menu) > room {
            // The least needed of those still shown, the trailing one first among equals.
            let next = widths
                .iter()
                .zip(stays.iter())
                .enumerate()
                .filter(|(ix, ((p, _), stays))| {
                    **stays && *p < under && title.is_none_or(|t| t.at != *ix)
                })
                .min_by(|(a_ix, ((a, _), _)), (b_ix, ((b, _), _))| a.cmp(b).then(b_ix.cmp(a_ix)))
                .map(|(ix, _)| ix);
            let Some(next) = next else { break };
            if let Some(stay) = stays.get_mut(next) {
                *stay = false;
            }
        }
    };
    if let Some(t) = title {
        if let Some(w) = widths.get_mut(t.at) {
            w.1 = t.natural;
        }
        leave(&widths, &mut stays, t.priority);
        if let Some(w) = widths.get_mut(t.at) {
            w.1 = t.least.min(t.natural);
        }
    }
    leave(&widths, &mut stays, Priority::ESSENTIAL);
    stays
}

/// The width the items that stay take with their gaps, and the menu while any leaves.
fn needed(
    items: &[(Priority, Pixels)],
    stays: &[bool],
    gap: Pixels,
    menu: Option<Pixels>,
) -> Pixels {
    let menu = menu.filter(|_| stays.iter().any(|s| !s));
    let shown = items.iter().zip(stays).filter(|(_, s)| **s).map(|((_, w), _)| *w).chain(menu);
    let (count, sum) = shown.fold((0_u16, px(0.0)), |(n, sum), w| (n.saturating_add(1), sum + w));
    sum + gap * f32::from(count.saturating_sub(1))
}

/// What a row measured of its items before it was laid out: each at its own width.
pub struct Measured {
    sizes: Vec<Size<Pixels>>,
    title: Option<(AnyElement, Size<Pixels>, Pixels)>,
    menu: Option<(AnyElement, Size<Pixels>)>,
}

impl PriorityRow {
    /// The text style it gives what it lays out, as a `div` gives its children.
    fn given_text(&self) -> Option<gpui::TextStyleRefinement> {
        let mut style = Style::default();
        style.refine(&self.style);
        style.text_style().cloned()
    }
}

impl Element for PriorityRow {
    /// What it laid out, to paint.
    type PrepaintState = Vec<AnyElement>;
    type RequestLayoutState = Measured;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Measured) {
        let text = self.given_text();
        window.with_text_style(text, |window| {
            let mut style = Style::default();
            style.refine(&self.style);
            // Every item at its own width, from its shaped text: before the row is laid out, so a
            // row sized by its content knows its width in the same frame.
            let natural =
                Size { width: AvailableSpace::MaxContent, height: AvailableSpace::MaxContent };
            let sizes: Vec<Size<Pixels>> = self
                .items
                .iter_mut()
                .map(|item| item.element.layout_as_root(natural, window, cx))
                .collect();
            let title = self.title.take().map(|(mut element, floor)| {
                let size = element.layout_as_root(natural, window, cx);
                (element, size, floor.min(size.width))
            });
            let menu = self.menu.take().map(|mut element| {
                let size = element.layout_as_root(natural, window, cx);
                (element, size)
            });
            if self.fit_content {
                // A gap given in ems or points; a fraction of a width that is not known yet is
                // none.
                let gap = style.gap.width.to_pixels(px(0.0).into(), window.rem_size());
                let widths: Vec<(Priority, Pixels)> = self
                    .items
                    .iter()
                    .zip(&sizes)
                    .map(|(item, size)| (item.priority, size.width))
                    .collect();
                let all = vec![true; widths.len()];
                let whole = needed(&widths, &all, gap, None)
                    + title.as_ref().map_or(px(0.0), |(_, size, _)| size.width);
                style.size.width = gpui::Length::Definite(whole.into());
                style.flex_shrink = 1.0;
                style.min_size.width = gpui::Length::Definite(px(0.0).into());
            }
            (window.request_layout(style, [], cx), Measured { sizes, title, menu })
        })
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        request_layout: &mut Measured,
        window: &mut Window,
        cx: &mut App,
    ) -> Vec<AnyElement> {
        let text = self.given_text();
        window.with_text_style(text, |window| {
            let high = bounds.size.height;
            let mut style = Style::default();
            style.refine(&self.style);
            let gap = style.gap.width.to_pixels(bounds.size.width.into(), window.rem_size());
            let items = std::mem::take(&mut self.items);
            let sizes = std::mem::take(&mut request_layout.sizes);
            let title = request_layout.title.take();
            let mut menu = request_layout.menu.take();
            // The title's place is an item that stays whatever happens.
            let measured: Vec<(Priority, Pixels)> = items
                .iter()
                .zip(&sizes)
                .map(|(item, size)| {
                    (item.priority, if item.key.is_empty() { px(0.0) } else { size.width })
                })
                .collect();
            let title_fit = title.as_ref().and_then(|(_, size, least)| {
                Some(TitleFit {
                    at: items.iter().position(|item| item.key.is_empty())?,
                    natural: size.width,
                    least: *least,
                    priority: self.title_priority,
                })
            });
            let menu_width = menu.as_ref().map(|(_, size)| size.width);
            let stays = fit_row(&measured, title_fit, gap, menu_width, bounds.size.width);
            let moved = self.dropped.set(
                items
                    .iter()
                    .zip(&stays)
                    .filter(|(item, stays)| !**stays && !item.key.is_empty())
                    .map(|(item, _)| item.key.clone())
                    .collect(),
            );
            // What a menu says follows what was left out now: draw once more if that moved.
            if moved && menu.is_some() {
                window.refresh();
            }
            let menu_shown = menu.is_some() && stays.iter().any(|s| !s);
            // What the title may take: the row less every other item shown, the menu and the gaps.
            let rest = needed(&measured, &stays, gap, menu_width);
            let mut title = title.map(|(mut element, size, least)| {
                let room = (bounds.size.width - rest).max(px(0.0));
                let width = if self.title_fills { room } else { size.width.min(room) };
                let width = width.max(least.min(room));
                let laid = element.layout_as_root(
                    Size {
                        width: AvailableSpace::Definite(width),
                        height: AvailableSpace::MaxContent,
                    },
                    window,
                    cx,
                );
                (element, Size { width, height: laid.height })
            });
            // Leading items from the left edge, trailing ones and the menu against the right.
            let trailing: Vec<Pixels> = items
                .iter()
                .zip(&stays)
                .zip(&sizes)
                .filter(|((item, s), _)| **s && item.side == Side::Trailing)
                .map(|(_, size)| size.width)
                .chain(menu.as_ref().filter(|_| menu_shown).map(|(_, size)| size.width))
                .collect();
            let trailing_width = trailing.iter().fold(px(0.0), |sum, w| sum + *w + gap);
            let centred = |size: Size<Pixels>| (high - size.height).max(px(0.0)) / 2.0;
            let mut x = bounds.origin.x;
            let mut trailing_x = bounds.origin.x + bounds.size.width - trailing_width + gap;
            let mut shown = Vec::new();
            for ((item, stay), size) in items.into_iter().zip(stays).zip(&sizes) {
                if !stay {
                    continue;
                }
                if item.key.is_empty() {
                    if let Some((mut element, size)) = title.take() {
                        element.prepaint_at(point(x, bounds.origin.y + centred(size)), window, cx);
                        x += size.width + gap;
                        shown.push(element);
                    }
                    continue;
                }
                let mut element = item.element;
                let at = match item.side {
                    Side::Leading => {
                        let at = x;
                        x += size.width + gap;
                        at
                    }
                    Side::Trailing => {
                        let at = trailing_x;
                        trailing_x += size.width + gap;
                        at
                    }
                };
                element.prepaint_at(point(at, bounds.origin.y + centred(*size)), window, cx);
                shown.push(element);
            }
            if let Some((mut element, size)) = menu.take().filter(|_| menu_shown) {
                element.prepaint_at(point(trailing_x, bounds.origin.y + centred(size)), window, cx);
                shown.push(element);
            }
            if let Some(mut element) = self.overlay.take() {
                let natural =
                    Size { width: AvailableSpace::MaxContent, height: AvailableSpace::MaxContent };
                let size = element.layout_as_root(natural, window, cx);
                let at = point(
                    bounds.origin.x + bounds.size.width - size.width,
                    bounds.origin.y + centred(size),
                );
                element.prepaint_at(at, window, cx);
                shown.push(element);
            }
            shown
        })
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Measured,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let text = self.given_text();
        window.with_text_style(text, |window| {
            for element in prepaint {
                element.paint(window, cx);
            }
        });
    }
}

impl std::fmt::Debug for Measured {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Measured").field("sizes", &self.sizes).finish_non_exhaustive()
    }
}

impl std::fmt::Debug for PriorityRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PriorityRow")
            .field("id", &self.id)
            .field("items", &self.items.iter().map(|i| (&i.key, i.priority)).collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl IntoElement for PriorityRow {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Styled for PriorityRow {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

#[cfg(test)]
mod tests {
    use gpui::{
        Context, InteractiveElement as _, ParentElement as _, Render, TestAppContext,
        VisualTestContext, div,
    };

    use super::*;

    /// A tile header's items as the row sees them: the lead, the title's place, a pull
    /// request, a worktree, then at the trailing end the state and close.
    fn header() -> Vec<(Priority, Pixels)> {
        vec![
            (Priority::ESSENTIAL, px(16.0)),
            (Priority::ESSENTIAL, px(0.0)),
            (Priority::MEDIUM, px(50.0)),
            (Priority::LOW, px(120.0)),
            (Priority::HIGH, px(16.0)),
            (Priority::ESSENTIAL, px(24.0)),
        ]
    }

    const TITLE: TitleFit =
        TitleFit { at: 1, natural: px(200.0), least: px(80.0), priority: Priority::MEDIUM };

    #[test]
    fn a_priority_row_drops_from_the_trailing_low_end_and_keeps_the_title_floor() {
        let gap = px(8.0);
        let fits = |room: f32| fit_row(&header(), Some(TITLE), gap, Some(px(24.0)), px(room));
        // Everything whole: 16 + 200 + 50 + 120 + 16 + 24 and five gaps.
        assert_eq!(fits(466.0), [true; 6]);
        // The worktree, under the title's priority, goes before a word of the title does; the
        // menu comes in its place.
        assert_eq!(fits(465.0), [true, true, true, false, true, true]);
        // Then the title narrows to its floor while the pull request stays...
        assert_eq!(
            fits(16.0 + 80.0 + 50.0 + 16.0 + 24.0 + 24.0 + 40.0),
            [true, true, true, false, true, true]
        );
        // ...and only past it does the pull request leave, then the state.
        assert_eq!(fits(249.0), [true, true, false, false, true, true]);
        assert_eq!(fits(150.0), [true, true, false, false, false, true]);
        // What never leaves stays at any room.
        assert_eq!(fits(10.0), [true, true, false, false, false, true]);
        // Among equals the trailing one leaves first.
        let equals = [(Priority::LOW, px(40.0)), (Priority::LOW, px(40.0))];
        assert_eq!(fit_row(&equals, None, gap, None, px(60.0)), [true, false]);
    }

    struct Row {
        width: f32,
        dropped: Dropped,
    }

    fn sized(id: &'static str, width: f32) -> gpui::Div {
        div().debug_selector(move || id.to_owned()).flex_none().w(px(width)).h(px(16.0))
    }

    impl Render for Row {
        fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            // A flex row, as a header's title is: it narrows to the width it is given.
            let words =
                div().debug_selector(|| "words".to_owned()).min_w_0().w(px(200.0)).h(px(16.0));
            let title = div()
                .debug_selector(|| "title".to_owned())
                .flex()
                .min_w_0()
                .overflow_hidden()
                .child(words);
            div().child(
                priority_row("row")
                    .w(px(self.width))
                    .h(px(24.0))
                    .gap(px(8.0))
                    .item("lead", Priority::ESSENTIAL, sized("lead", 16.0))
                    .title(title, px(80.0))
                    .item("pr", Priority::MEDIUM, sized("pr", 50.0))
                    .item("worktree", Priority::LOW, sized("worktree", 120.0))
                    .end()
                    .item("state", Priority::HIGH, sized("state", 16.0))
                    .item("close", Priority::ESSENTIAL, sized("close", 24.0))
                    .menu(sized("menu", 24.0))
                    .overlay(sized("controls", 60.0))
                    .dropped(&self.dropped),
            )
        }
    }

    fn laid(cx: &mut VisualTestContext, id: &'static str) -> Option<Bounds<Pixels>> {
        cx.debug_bounds(id)
    }

    /// Laid out for real: each item measured, what leaves named for the menu, the menu shown
    /// only then, the title between its floor and its width, the trailing items against the
    /// row's end and the overlay over them taking no room.
    #[gpui::test]
    fn a_row_lays_out_what_stays_and_names_what_left(cx: &mut TestAppContext) {
        let dropped = Dropped::default();
        let (view, cx) = cx.add_window_view(|_, _| Row { width: 600.0, dropped: dropped.clone() });
        cx.run_until_parked();
        assert!(dropped.keys().is_empty(), "{:?}", dropped.keys());
        assert!(laid(cx, "menu").is_none(), "no menu while all fit");
        let title = laid(cx, "title").expect("the title");
        assert_eq!(title.size.width, px(200.0), "whole while there is room");
        let close = laid(cx, "close").expect("close");
        assert_eq!(close.right(), px(600.0), "the trailing end");
        let controls = laid(cx, "controls").expect("the overlay");
        assert_eq!(controls.right(), px(600.0), "over the trailing end");
        let worktree = laid(cx, "worktree").expect("the worktree");
        assert!(worktree.left() > title.right(), "after the title");

        view.update(cx, |row, cx| {
            row.width = 240.0;
            cx.notify();
        });
        cx.run_until_parked();
        assert_eq!(dropped.keys(), ["pr", "worktree"]);
        assert!(laid(cx, "worktree").is_none() && laid(cx, "pr").is_none());
        let menu = laid(cx, "menu").expect("the menu, now something left");
        assert_eq!(menu.right(), px(240.0), "last at the end");
        let title = laid(cx, "title").expect("the title");
        assert_eq!(title.size.width, px(128.0), "what the rest leave: {title:?}");
        let words = laid(cx, "words").expect("the title's words");
        assert_eq!(words.size.width, px(128.0), "and its words narrow with it");
        let state = laid(cx, "state").expect("the state stays");
        assert!(title.right() <= state.left(), "nothing overlaps: {title:?} {state:?}");
    }

    /// Headers of text items drawn as priority rows or as a plain flex row.
    struct Headers {
        rows: usize,
        priority: bool,
    }

    impl Render for Headers {
        fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let word = |text: &'static str| div().flex_none().child(text);
            let rows = (0..self.rows).map(|ix| {
                let title = div().min_w_0().overflow_hidden().child("Read a pull request's checks");
                if self.priority {
                    priority_row(ix)
                        .h(px(40.0))
                        .gap(px(8.0))
                        .item("lead", Priority::ESSENTIAL, word("◇"))
                        .title(title, px(96.0))
                        .item("pr", Priority::MEDIUM, word("#42"))
                        .item("worktree", Priority::LOW, word("slopty-board-1"))
                        .item("worker", Priority::LOW, word("studio"))
                        .end()
                        .item("state", Priority::HIGH, word("●"))
                        .item("close", Priority::ESSENTIAL, word("×"))
                        .menu(word("…"))
                        .into_any_element()
                } else {
                    div()
                        .h(px(40.0))
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(word("◇"))
                        .child(title)
                        .child(word("#42"))
                        .child(word("slopty-board-1"))
                        .child(word("studio"))
                        .child(div().flex_1())
                        .child(word("●"))
                        .child(word("×"))
                        .into_any_element()
                }
            });
            div().w(px(360.0)).flex().flex_col().children(rows)
        }
    }

    /// What a header costs drawn as a priority row against a plain flex row: 24 headers of
    /// eight text items each, a notify and its draw. Run by hand, in release:
    /// `cargo test -p slopty-ui --release --lib priority_row_cost -- --ignored --nocapture`.
    #[gpui::test]
    #[ignore = "measurement, run by hand"]
    fn priority_row_cost(cx: &mut TestAppContext) {
        const FRAMES: usize = 400;
        const WARM: usize = 40;
        let mut line = Vec::new();
        for priority in [false, true] {
            let (view, cx) = cx.add_window_view(|_, _| Headers { rows: 24, priority });
            cx.run_until_parked();
            let mut samples = Vec::new();
            for n in 0..WARM + FRAMES {
                let started = std::time::Instant::now();
                view.update(cx, |_, cx| cx.notify());
                cx.run_until_parked();
                if n >= WARM {
                    samples.push(started.elapsed());
                }
            }
            samples.sort_unstable();
            let at = |q: usize| {
                let ix = samples.len().saturating_sub(1).saturating_mul(q) / 100;
                samples.get(ix).copied().unwrap_or_default()
            };
            line.push(format!(
                "{}: {:?} / {:?} / {:?}",
                if priority { "priority rows" } else { "flex rows" },
                at(50),
                at(95),
                at(99)
            ));
        }
        println!(
            "MEASURE 24 headers at 360 pt, a notify and its draw (p50 / p95 / p99): {}",
            line.join(" · ")
        );
    }
}

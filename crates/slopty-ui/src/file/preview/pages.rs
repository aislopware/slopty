//! A PDF in a file tile, read with the keyboard and the pointer.
//!
//! The keys scroll it as Preview does: the arrows a few lines, Page Down and Space a screen,
//! ← and → a page, Home and End to either end. A drag over the pages selects their text, a double
//! click a word and a triple click a line, as `PDFKit` reads them ([`super::super::pdf_text`]); ⌘C
//! copies it and ⌘A selects every page's. The keys bind in the tile's key context while it shows
//! a PDF ([`CTX`]), and the palette lists them ([`palette_items`]).

use gpui::{
    Bounds, Context, DispatchPhase, InteractiveElement as _, KeyBinding, ListOffset, MouseButton,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, Window, px,
};

use super::{Body, Pages, Preview};
use crate::file::FileView;
use crate::file::pdf_text::{Granularity, PdfText, Selected, Spot};
use crate::icons::IconName;
use crate::palette::PaletteItem;

#[expect(clippy::derive_partial_eq_without_eq, reason = "gpui::actions! derives PartialEq only")]
mod actions {
    gpui::actions!(
        pdf,
        [
            /// Scroll a PDF down a few lines.
            ScrollDown,
            /// Scroll a PDF up a few lines.
            ScrollUp,
            /// Scroll a PDF down a screen.
            NextScreen,
            /// Scroll a PDF up a screen.
            PreviousScreen,
            /// Go to the top of the PDF's next page.
            NextPage,
            /// Go to the top of the page in view, or the one before.
            PreviousPage,
            /// Go to the PDF's first page.
            FirstPage,
            /// Go to the PDF's last page.
            LastPage,
            /// Copy the text selected in a PDF.
            CopyText,
            /// Select every page's text.
            SelectAllText,
        ]
    );
}
pub use actions::{
    CopyText, FirstPage, LastPage, NextPage, NextScreen, PreviousPage, PreviousScreen, ScrollDown,
    ScrollUp, SelectAllText,
};

/// The key context a file tile adds while it shows a PDF, beside `FileEditor`.
pub const CTX: &str = "FilePages";

/// Lines of the tile's text an arrow scrolls.
const ARROW_LINES: f32 = 3.0;

/// Text selected, and how it grows while the pointer is down.
pub(in crate::file) struct Selection {
    /// Where the press was: a page and a spot on it.
    anchor: (usize, Spot),
    by: Granularity,
    /// What is selected now, if anything.
    selected: Option<Selected>,
    /// The pointer is still down.
    held: bool,
}

impl Selection {
    /// The rectangles selected on page `ix`, in fractions of it.
    pub(super) fn on_page(&self, ix: usize) -> Vec<crate::file::pdf_text::Area> {
        self.selected
            .iter()
            .flat_map(|s| s.areas.iter())
            .filter(|(page, _)| *page == ix)
            .map(|(_, area)| *area)
            .collect()
    }
}

/// How far a key scrolls.
#[derive(Clone, Copy)]
enum Scroll {
    Lines(f32),
    Screens(f32),
    Page(isize),
    Start,
    End,
}

/// The palette's lines for a PDF's keys, `bindings` giving their chords.
#[must_use]
pub fn palette_items(bindings: &[KeyBinding]) -> Vec<PaletteItem> {
    let line = |label: &str, icon: IconName, action: Box<dyn gpui::Action>| {
        PaletteItem::new(label, icon, action, bindings)
    };
    vec![
        line("Next page", IconName::ChevronDown, Box::new(NextPage)),
        line("Previous page", IconName::ChevronUp, Box::new(PreviousPage)),
        line("First page", IconName::ArrowUp, Box::new(FirstPage)),
        line("Last page", IconName::ArrowDown, Box::new(LastPage)),
        line("Copy selected text", IconName::Copy, Box::new(CopyText)),
        line("Select all text", IconName::Type, Box::new(SelectAllText)),
    ]
}

impl FileView {
    pub(super) fn pages_mut(&mut self) -> Option<&mut Pages> {
        match self.preview.as_mut() {
            Some(Preview { body: Body::Pdf(pages), .. }) => Some(pages),
            _ => None,
        }
    }

    /// Whether the tile shows a PDF's pages, so its keys apply.
    pub(in crate::file) fn shows_pages(&self) -> bool {
        matches!(&self.preview, Some(Preview { body: Body::Pdf(p), .. }) if p.doc.is_some())
    }

    /// The tile's handlers for a PDF's keys.
    pub(in crate::file) fn page_keys(
        el: gpui::Stateful<gpui::Div>,
        cx: &Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let lines = ARROW_LINES;
        el.on_action(cx.listener(move |this, _: &ScrollDown, _, cx| {
            this.scroll_pages(Scroll::Lines(lines), cx);
        }))
        .on_action(cx.listener(move |this, _: &ScrollUp, _, cx| {
            this.scroll_pages(Scroll::Lines(-lines), cx);
        }))
        .on_action(cx.listener(|this, _: &NextScreen, _, cx| {
            this.scroll_pages(Scroll::Screens(1.0), cx);
        }))
        .on_action(cx.listener(|this, _: &PreviousScreen, _, cx| {
            this.scroll_pages(Scroll::Screens(-1.0), cx);
        }))
        .on_action(cx.listener(|this, _: &NextPage, _, cx| this.scroll_pages(Scroll::Page(1), cx)))
        .on_action(
            cx.listener(|this, _: &PreviousPage, _, cx| this.scroll_pages(Scroll::Page(-1), cx)),
        )
        .on_action(cx.listener(|this, _: &FirstPage, _, cx| this.scroll_pages(Scroll::Start, cx)))
        .on_action(cx.listener(|this, _: &LastPage, _, cx| this.scroll_pages(Scroll::End, cx)))
        .on_action(cx.listener(|this, _: &CopyText, _, cx| this.copy_text(cx)))
        .on_action(cx.listener(|this, _: &SelectAllText, _, cx| this.select_all_text(cx)))
    }

    fn scroll_pages(&mut self, by: Scroll, cx: &mut Context<Self>) {
        let line = self.text_size * self.zoom * 1.3;
        let Some(pages) = self.pages_mut() else { return };
        let count = pages.sizes.len();
        if count == 0 {
            return;
        }
        let list = &pages.list;
        let top = list.logical_scroll_top();
        match by {
            Scroll::Lines(n) => list.scroll_by(px(line * n)),
            Scroll::Screens(n) => {
                // A screen less a line, so the line at the edge is read again.
                let screen = f32::from(list.viewport_bounds().size.height) - line;
                list.scroll_by(px(screen.max(line) * n));
            }
            Scroll::Page(step) => {
                // Back from inside a page goes to its top first, as Preview does.
                let from = if step < 0 && top.offset_in_item > px(1.0) {
                    top.item_ix.saturating_add(1)
                } else {
                    top.item_ix
                };
                let to = from.saturating_add_signed(step).min(count.saturating_sub(1));
                list.scroll_to(ListOffset { item_ix: to, offset_in_item: px(0.0) });
            }
            Scroll::Start => list.scroll_to(ListOffset::default()),
            Scroll::End => list.scroll_to_end(),
        }
        cx.notify();
    }

    /// The pointer's moves and release, registered as the pages paint: wherever it goes while
    /// held after a press on a page, the selection follows, and the release ends it.
    pub(super) fn follow_pointer(entity: &gpui::Entity<Self>, window: &mut Window) {
        let view = entity.clone();
        window.on_mouse_event(move |e: &MouseMoveEvent, phase, _window, cx| {
            if phase == DispatchPhase::Bubble && e.pressed_button == Some(MouseButton::Left) {
                view.update(cx, |this, cx| this.drag_pages(e.position, cx));
            }
        });
        let view = entity.clone();
        window.on_mouse_event(move |e: &MouseUpEvent, phase, _window, cx| {
            if phase == DispatchPhase::Bubble && e.button == MouseButton::Left {
                view.update(cx, |this, _| this.release_pages());
            }
        });
    }

    /// A press at `at`: select from there, by character, word or line as the clicks count.
    pub(in crate::file) fn press_pages(
        &mut self,
        at: Point<Pixels>,
        clicks: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus(window, cx);
        let shown = self.shown_pages();
        let Some(pages) = self.pages_mut() else { return };
        let Some(anchor) = spot_at(&shown, at, false) else {
            pages.selection = None;
            cx.notify();
            return;
        };
        let by = match clicks {
            0 | 1 => Granularity::Character,
            2 => Granularity::Word,
            _ => Granularity::Line,
        };
        let selected = pages.text().and_then(|t| t.select(anchor, anchor, by));
        pages.selection = Some(Selection { anchor, by, selected, held: true });
        cx.notify();
    }

    /// The pointer moved to `at` with the button down: the selection runs from its press to
    /// the nearest place on a page.
    pub(in crate::file) fn drag_pages(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        let shown = self.shown_pages();
        let Some(pages) = self.pages_mut() else { return };
        let Some(Selection { anchor, by, held: true, .. }) = pages.selection.as_ref() else {
            return;
        };
        let (anchor, by) = (*anchor, *by);
        let Some(here) = spot_at(&shown, at, true) else { return };
        let selected = pages.text().and_then(|t| t.select(anchor, here, by));
        if let Some(selection) = pages.selection.as_mut()
            && selection.selected != selected
        {
            selection.selected = selected;
            cx.notify();
        }
    }

    pub(in crate::file) fn release_pages(&mut self) {
        if let Some(selection) = self.pages_mut().and_then(|p| p.selection.as_mut()) {
            selection.held = false;
        }
    }

    /// Where the PDF's pages are scrolled to: the page at the top, and how far into it.
    #[must_use]
    pub fn pages_top(&self) -> Option<ListOffset> {
        match &self.preview {
            Some(Preview { body: Body::Pdf(p), .. }) => Some(p.list.logical_scroll_top()),
            _ => None,
        }
    }

    /// Where page `ix` is in the window, while it is on screen.
    #[must_use]
    pub fn page_bounds(&self, ix: usize) -> Option<Bounds<Pixels>> {
        self.shown_pages().into_iter().find(|(shown, _)| *shown == ix).map(|(_, b)| b)
    }

    /// The pages on screen and where each is in the window, from the list's layout. Painted
    /// bounds would not do: a scrolled list moves its layer without painting its pages again.
    fn shown_pages(&self) -> Vec<(usize, Bounds<Pixels>)> {
        let Some(Preview { body: Body::Pdf(p), .. }) = &self.preview else { return Vec::new() };
        let k = self.zoom;
        let side = px(self.pad * k);
        let bottom = p.list.viewport_bounds().bottom();
        let mut shown = Vec::new();
        for (ix, &(w, h)) in p.sizes.iter().enumerate().skip(p.list.logical_scroll_top().item_ix) {
            let Some(slot) = p.list.bounds_for_item(ix) else { break };
            if slot.top() >= bottom {
                break;
            }
            let above = if ix == 0 { self.pad } else { self.theme.spacing.sm };
            let width = (slot.size.width - side * 2.0).max(px(0.0));
            let origin = gpui::point(slot.left() + side, slot.top() + px(above * k));
            shown.push((ix, Bounds { origin, size: gpui::size(width, width * (h / w.max(1.0))) }));
        }
        shown
    }

    /// The text selected, if any.
    #[must_use]
    pub fn selected_text(&self) -> Option<String> {
        match &self.preview {
            Some(Preview { body: Body::Pdf(p), .. }) => {
                p.selection.as_ref()?.selected.as_ref().map(|s| s.text.clone())
            }
            _ => None,
        }
    }

    fn copy_text(&self, cx: &Context<Self>) {
        if let Some(text) = self.selected_text() {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        }
    }

    fn select_all_text(&mut self, cx: &mut Context<Self>) {
        let Some(pages) = self.pages_mut() else { return };
        let selected = pages.text().and_then(PdfText::all);
        let anchor = (0, (0.0, 0.0));
        pages.selection =
            Some(Selection { anchor, by: Granularity::Character, selected, held: false });
        cx.notify();
    }
}

impl Pages {
    /// `PDFKit`'s reading of the document, opened the first time it is asked for.
    fn text(&self) -> Option<&PdfText> {
        self.text
            .get_or_init(|| {
                let opened = PdfText::open(&self.bytes);
                if opened.is_none() {
                    tracing::info!("PDF text not readable by PDFKit");
                }
                opened
            })
            .as_ref()
    }
}

/// The page under `at`, and the spot on it, among the pages `shown`. With `nearest`, a point
/// off every page goes to the closest one, clamped onto it: a drag that leaves the page still
/// selects to its edge.
fn spot_at(
    shown: &[(usize, Bounds<Pixels>)],
    at: Point<Pixels>,
    nearest: bool,
) -> Option<(usize, Spot)> {
    let mut shown = shown.iter();
    let distance = |b: &Bounds<Pixels>| {
        let (top, bottom) = (f32::from(b.top()), f32::from(b.bottom()));
        let y = f32::from(at.y);
        if y < top {
            top - y
        } else if y > bottom {
            y - bottom
        } else {
            0.0
        }
    };
    let &(ix, b) = if nearest {
        shown.min_by(|(_, a), (_, b)| distance(a).total_cmp(&distance(b)))?
    } else {
        shown.find(|(_, b)| b.contains(&at))?
    };
    let fraction = |v: Pixels, from: Pixels, len: Pixels| {
        let len = f32::from(len).max(1.0);
        f64::from(((f32::from(v) - f32::from(from)) / len).clamp(0.0, 1.0))
    };
    Some((
        ix,
        (fraction(at.x, b.origin.x, b.size.width), fraction(at.y, b.origin.y, b.size.height)),
    ))
}

#[cfg(test)]
mod tests {
    use gpui::{point, size};

    use super::*;

    #[test]
    fn a_point_is_placed_on_its_page_or_the_nearest_one() {
        let page =
            |y: f32| Bounds { origin: point(px(10.0), px(y)), size: size(px(100.0), px(200.0)) };
        let shown = [(3, page(0.0)), (4, page(220.0))];
        assert_eq!(spot_at(&shown, point(px(60.0), px(100.0)), false), Some((3, (0.5, 0.5))));
        assert_eq!(spot_at(&shown, point(px(10.0), px(320.0)), false), Some((4, (0.0, 0.5))));
        assert_eq!(spot_at(&shown, point(px(60.0), px(210.0)), false), None, "between pages");
        assert_eq!(
            spot_at(&shown, point(px(500.0), px(500.0)), true),
            Some((4, (1.0, 1.0))),
            "past the last page shown, its far corner"
        );
        assert_eq!(spot_at(&[], point(px(0.0), px(0.0)), true), None, "no page on screen");
    }
}

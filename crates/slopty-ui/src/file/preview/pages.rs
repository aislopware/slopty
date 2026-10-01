//! A PDF in a file tile, read with the keyboard and the pointer.
//!
//! The keys scroll it as Preview does: the arrows a few lines, Page Down and Space a screen,
//! ← and → a page, Home and End to either end. A drag over the pages selects their text, a double
//! click a word and a triple click a line, as `PDFKit` reads them ([`super::super::pdf_text`]); ⌘C
//! copies it and ⌘A selects every page's. The keys bind in the tile's key context while it shows
//! a PDF ([`CTX`]), and the palette lists them ([`palette_items`]).

use std::collections::HashMap;

use gpui::{
    Bounds, Context, DispatchPhase, Hitbox, HitboxBehavior, InteractiveElement as _, KeyBinding,
    ListOffset, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Window,
    px,
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

    /// The pages' mouse handling, registered as they paint: a press starts a selection, a
    /// move with it held grows it wherever the pointer goes, a release ends it.
    pub(super) fn listen_to_pages(
        entity: &gpui::Entity<Self>,
        hitbox: &Hitbox,
        window: &mut Window,
    ) {
        window.set_cursor_style(gpui::CursorStyle::IBeam, hitbox);
        let (view, hitbox) = (entity.clone(), hitbox.clone());
        window.on_mouse_event(move |e: &MouseDownEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble
                && e.button == MouseButton::Left
                && hitbox.is_hovered(window)
            {
                view.update(cx, |this, cx| this.press_pages(e.position, e.click_count, window, cx));
            }
        });
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
        let Some(pages) = self.pages_mut() else { return };
        let Some(anchor) = spot_at(&pages.bounds, pages.clock, at, false) else {
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
        let Some(pages) = self.pages_mut() else { return };
        let Some(Selection { anchor, by, held: true, .. }) = pages.selection.as_ref() else {
            return;
        };
        let (anchor, by) = (*anchor, *by);
        let Some(here) = spot_at(&pages.bounds, pages.clock, at, true) else { return };
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

/// The page under `at`, and the spot on it, among the pages painted in layout `clock`. With
/// `nearest`, a point off every page goes to the closest one, clamped onto it: a drag that
/// leaves the page still selects to its edge.
fn spot_at(
    bounds: &HashMap<usize, (u64, Bounds<Pixels>)>,
    clock: u64,
    at: Point<Pixels>,
    nearest: bool,
) -> Option<(usize, Spot)> {
    let mut painted = bounds.iter().filter(|(_, (when, _))| *when == clock);
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
    let (ix, b) = if nearest {
        painted
            .min_by(|(_, (_, a)), (_, (_, b))| distance(a).total_cmp(&distance(b)))
            .map(|(ix, (_, b))| (*ix, *b))?
    } else {
        painted.find(|(_, (_, b))| b.contains(&at)).map(|(ix, (_, b))| (*ix, *b))?
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

/// A hitbox over the pages that does not keep the list under it from scrolling.
pub(super) fn pages_hitbox(bounds: Bounds<Pixels>, window: &mut Window) -> Hitbox {
    window.insert_hitbox(bounds, HitboxBehavior::Normal)
}

#[cfg(test)]
mod tests {
    use gpui::{point, size};

    use super::*;

    #[test]
    fn a_point_is_placed_on_its_page_or_the_nearest_one() {
        let page =
            |y: f32| Bounds { origin: point(px(10.0), px(y)), size: size(px(100.0), px(200.0)) };
        let bounds =
            HashMap::from([(0, (7, page(0.0))), (1, (7, page(220.0))), (2, (6, page(440.0)))]);
        assert_eq!(spot_at(&bounds, 7, point(px(60.0), px(100.0)), false), Some((0, (0.5, 0.5))));
        assert_eq!(spot_at(&bounds, 7, point(px(10.0), px(320.0)), false), Some((1, (0.0, 0.5))));
        assert_eq!(spot_at(&bounds, 7, point(px(60.0), px(210.0)), false), None, "between pages");
        assert_eq!(
            spot_at(&bounds, 7, point(px(500.0), px(500.0)), true),
            Some((1, (1.0, 1.0))),
            "a page painted in an older layout is not there any more"
        );
    }
}

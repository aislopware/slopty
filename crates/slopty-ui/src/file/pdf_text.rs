//! A PDF's text as `PDFKit` reads it: what a drag, a double click or a triple click over a page
//! selects, where the selection lies on each page, and its string for the clipboard.
//!
//! The pages are drawn by Core Graphics (`super::decode`); `PDFKit` is opened beside it only when
//! the text is first asked for, since a PDF that is only read costs nothing more. Positions go
//! in and out as fractions of a page as the tile shows it, from its top-left corner, so the
//! caller needs no page geometry: the crop box and the page's turn are applied here.

use objc2::AllocAnyThread as _;
use objc2::rc::Retained;
use objc2_core_foundation::{CGFloat, CGPoint, CGRect};
use objc2_foundation::NSData;
use objc2_pdf_kit::{PDFDisplayBox, PDFDocument, PDFPage, PDFSelection, PDFSelectionGranularity};

/// A place on a page as the tile shows it: fractions of its width and height from the top-left.
pub type Spot = (f64, f64);

/// A rectangle on a page as the tile shows it, in the same fractions: `(x, y, width, height)`.
pub type Area = (f64, f64, f64, f64);

/// What a selection grows by as the pointer moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Granularity {
    /// A drag: letter by letter.
    Character,
    /// A double click and its drag: whole words.
    Word,
    /// A triple click and its drag: whole lines.
    Line,
}

/// Text selected: its string, and its lines' rectangles, each with its page's index.
#[derive(Debug, Clone, PartialEq)]
pub struct Selected {
    /// The text, as it goes to the clipboard.
    pub text: String,
    /// Each line's rectangle on its page.
    pub areas: Vec<(usize, Area)>,
}

/// A PDF open in `PDFKit`. Not `Send`: it stays on the thread that opened it, the UI's.
#[derive(Debug)]
pub struct PdfText {
    doc: Retained<PDFDocument>,
}

impl PdfText {
    /// Open `bytes` (copied: `PDFKit` keeps its data as long as it likes); `None` when `PDFKit`
    /// cannot read them, or they are locked.
    #[must_use]
    pub fn open(bytes: &[u8]) -> Option<Self> {
        let data = NSData::with_bytes(bytes);
        // SAFETY: `-[PDFDocument initWithData:]` (PDFKit/PDFDocument.h) on a fresh allocation,
        // with data it retains.
        let doc = unsafe { PDFDocument::initWithData(PDFDocument::alloc(), &data) }?;
        // SAFETY: a document made above (PDFDocument.h).
        let locked = unsafe { doc.isLocked() };
        (!locked).then_some(Self { doc })
    }

    /// Its page count.
    #[must_use]
    pub fn pages(&self) -> usize {
        // SAFETY: as above.
        unsafe { self.doc.pageCount() }
    }

    /// The text from `from` to `to` (each a page and a spot on it), grown `by`; `None` where
    /// nothing is there to select.
    #[must_use]
    pub fn select(
        &self,
        from: (usize, Spot),
        to: (usize, Spot),
        by: Granularity,
    ) -> Option<Selected> {
        let (start, end) = (self.page(from.0)?, self.page(to.0)?);
        let granularity = match by {
            Granularity::Character => PDFSelectionGranularity::Character,
            Granularity::Word => PDFSelectionGranularity::Word,
            Granularity::Line => PDFSelectionGranularity::Line,
        };
        // SAFETY: `-[PDFDocument selectionFromPage:atPoint:toPage:atPoint:withGranularity:]`
        // (PDFDocument.h) with two of the document's own pages and points in their page space.
        let selection = unsafe {
            self.doc.selectionFromPage_atPoint_toPage_atPoint_withGranularity(
                &start,
                on_page(&start, from.1),
                &end,
                on_page(&end, to.1),
                granularity,
            )
        }?;
        self.selected(&selection)
    }

    /// Every page's text.
    #[must_use]
    pub fn all(&self) -> Option<Selected> {
        // SAFETY: `-[PDFDocument selectionForEntireDocument]` (PDFDocument.h).
        let selection = unsafe { self.doc.selectionForEntireDocument() }?;
        self.selected(&selection)
    }

    fn page(&self, index: usize) -> Option<Retained<PDFPage>> {
        // SAFETY: `-[PDFDocument pageAtIndex:]` (PDFDocument.h) below the page count; `nil`
        // past it all the same.
        (index < self.pages()).then(|| unsafe { self.doc.pageAtIndex(index) }).flatten()
    }

    /// A selection's text and lines, `None` when it holds no text.
    fn selected(&self, selection: &PDFSelection) -> Option<Selected> {
        // SAFETY: `-[PDFSelection string]` (PDFSelection.h).
        let text = unsafe { selection.string() }?.to_string();
        if text.is_empty() {
            return None;
        }
        let mut areas = Vec::new();
        // SAFETY: `-[PDFSelection selectionsByLine]` (PDFSelection.h).
        for line in unsafe { selection.selectionsByLine() } {
            // SAFETY: `-[PDFSelection pages]`, then each page's index in its own document and
            // the line's bounds on it (PDFSelection.h, PDFDocument.h).
            for page in unsafe { line.pages() } {
                // SAFETY: a page of the selection is one of this document's (PDFDocument.h).
                let index = unsafe { self.doc.indexForPage(&page) };
                // SAFETY: the line's bounds on a page it lies on (PDFSelection.h).
                let bounds = unsafe { line.boundsForPage(&page) };
                areas.push((index, shown(&page, bounds)));
            }
        }
        Some(Selected { text, areas })
    }
}

/// How a page shows: its crop box, and its turn clockwise in quarters.
fn geometry(page: &PDFPage) -> (CGRect, isize) {
    // SAFETY: `-[PDFPage boundsForBox:]` (PDFPage.h).
    let crop = unsafe { page.boundsForBox(PDFDisplayBox::CropBox) };
    // SAFETY: `-[PDFPage rotation]` (PDFPage.h), a multiple of 90.
    let turn = unsafe { page.rotation() };
    (crop, turn.rem_euclid(360) / 90)
}

/// Fractions of the page upright (from its bottom-left) for `spot` on it as shown.
const fn upright(spot: Spot, quarters: isize) -> (f64, f64) {
    let (x, y) = spot;
    match quarters {
        1 => (y, x),
        2 => (1.0 - x, y),
        3 => (1.0 - y, 1.0 - x),
        _ => (x, 1.0 - y),
    }
}

/// [`upright`] undone: fractions of the page as shown for `(u, v)` of it upright.
const fn as_shown(u: f64, v: f64, quarters: isize) -> Spot {
    match quarters {
        1 => (v, u),
        2 => (1.0 - u, v),
        3 => (1.0 - v, 1.0 - u),
        _ => (u, 1.0 - v),
    }
}

/// `spot` in `page`'s own space.
fn on_page(page: &PDFPage, spot: Spot) -> CGPoint {
    let (crop, quarters) = geometry(page);
    let (u, v) = upright(spot, quarters);
    CGPoint::new(
        u.mul_add(crop.size.width, crop.origin.x),
        v.mul_add(crop.size.height, crop.origin.y),
    )
}

/// A rectangle in `page`'s own space as the tile shows it.
fn shown(page: &PDFPage, rect: CGRect) -> Area {
    let (crop, quarters) = geometry(page);
    let (w, h) = (crop.size.width.max(1.0), crop.size.height.max(1.0));
    let corner = |x: CGFloat, y: CGFloat| {
        as_shown((x - crop.origin.x) / w, (y - crop.origin.y) / h, quarters)
    };
    let (x0, y0) = corner(rect.origin.x, rect.origin.y);
    let (x1, y1) = corner(rect.origin.x + rect.size.width, rect.origin.y + rect.size.height);
    (x0.min(x1), y0.min(y1), (x1 - x0).abs(), (y1 - y0).abs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spot_as_shown_and_back_is_the_same_at_every_turn() {
        for quarters in 0..4 {
            let spot = (0.2, 0.7);
            let (u, v) = upright(spot, quarters);
            let back = as_shown(u, v, quarters);
            assert!((back.0 - spot.0).abs() < 1e-9 && (back.1 - spot.1).abs() < 1e-9, "{quarters}");
        }
    }
}

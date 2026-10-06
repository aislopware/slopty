//! A PDF in a file tile, scrolled with the keyboard as well as the pointer.
//!
//! The arrows scroll it a few lines, Page Down and Space a screen, as Preview does. The keys
//! bind in the tile's key context while it shows a PDF ([`CTX`]). The pages are for reading
//! beside the work: their text is not selected or copied here, and they are not paged by key
//! (cut 2026-10-05, `docs/decisions/ui.md`).

use gpui::{Context, InteractiveElement as _, px};

use super::{Body, Pages, Preview};
use crate::file::FileView;

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
        ]
    );
}
pub use actions::{NextScreen, PreviousScreen, ScrollDown, ScrollUp};

/// The key context a file tile adds while it shows a PDF, beside `FileEditor`.
pub const CTX: &str = "FilePages";

/// Lines of the tile's text an arrow scrolls.
const ARROW_LINES: f32 = 3.0;

impl FileView {
    fn pages(&self) -> Option<&Pages> {
        match self.preview.as_ref() {
            Some(Preview { body: Body::Pdf(pages), .. }) => Some(pages),
            _ => None,
        }
    }

    /// Whether the tile shows a PDF's pages, so its keys apply.
    pub(in crate::file) fn shows_pages(&self) -> bool {
        self.pages().is_some_and(|p| p.doc.is_some())
    }

    /// The tile's handlers for a PDF's keys.
    pub(in crate::file) fn page_keys(
        el: gpui::Stateful<gpui::Div>,
        cx: &Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        el.on_action(cx.listener(|this, _: &ScrollDown, _, cx| this.scroll_lines(ARROW_LINES, cx)))
            .on_action(cx.listener(|this, _: &ScrollUp, _, cx| this.scroll_lines(-ARROW_LINES, cx)))
            .on_action(cx.listener(|this, _: &NextScreen, _, cx| this.scroll_screens(1.0, cx)))
            .on_action(cx.listener(|this, _: &PreviousScreen, _, cx| this.scroll_screens(-1.0, cx)))
    }

    /// The height of a line of the tile's text, as an arrow scrolls by.
    fn line_height(&self) -> f32 {
        self.text_size * 1.3
    }

    fn scroll_lines(&self, lines: f32, cx: &mut Context<Self>) {
        let line = self.line_height();
        if let Some(pages) = self.pages() {
            pages.list.scroll_by(px(line * lines));
            cx.notify();
        }
    }

    fn scroll_screens(&self, screens: f32, cx: &mut Context<Self>) {
        let line = self.line_height();
        if let Some(pages) = self.pages() {
            // A screen less a line, so the line at the edge is read again.
            let screen = f32::from(pages.list.viewport_bounds().size.height) - line;
            pages.list.scroll_by(px(screen.max(line) * screens));
            cx.notify();
        }
    }

    /// Where the PDF's pages are scrolled to: the page at the top, and how far into it.
    #[must_use]
    pub fn pages_top(&self) -> Option<gpui::ListOffset> {
        self.pages().map(|p| p.list.logical_scroll_top())
    }
}

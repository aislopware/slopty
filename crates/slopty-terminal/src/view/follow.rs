//! Output never scrolls the person away from what they read (`docs/decisions/terminal.md`,
//! "The bottom moves, the view holds"): the view follows the bottom only while it is at the
//! bottom, a keystroke takes it home ([`TerminalView::type_key`]), and a view scrolled away
//! counts the lines that arrived below it since, on a "↓ 12 new lines" pill a click on takes
//! home too. The scroller overlays the grid and shows only once the view is scrolled back.
//!
//! The client's mirror already holds a scrolled view still as output arrives
//! (`slopty_client::TermState`); what this adds is the count, read off where the bottom stood
//! when the view left it.

use gpui::{Context, SharedString};
use slopty_shell::icons;

use super::{FootPill, ScrollToBottom, TerminalView};

/// Where the output's bottom stood when the view left it: the screen's first line then, in the
/// numbering `epoch` names (a reflow or the other screen numbers lines anew).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Away {
    epoch: Option<u32>,
    bottom: u64,
}

impl TerminalView {
    /// The screen's first line in the stream's numbering: where the bottom of the output is,
    /// wherever the view is scrolled.
    const fn bottom(&self) -> u64 {
        self.state.index_at_row(0).0.saturating_add(self.state.view_offset())
    }

    /// Keep [`Self::away`] true to the viewport: none while it follows the output, and the
    /// bottom as it stands now from the moment it leaves (or the numbering changes under it).
    pub(super) fn note_away(&mut self) {
        if self.state.view_offset() == 0 {
            self.away = None;
            return;
        }
        let epoch = self.state.epoch();
        if self.away.is_none_or(|away| away.epoch != epoch) {
            self.away = Some(Away { epoch, bottom: self.bottom() });
        }
    }

    /// The lines that arrived below a view scrolled away since it left the bottom: 0 while it
    /// follows the output, and 0 when nothing came.
    #[must_use]
    pub fn new_lines(&self) -> u64 {
        if self.state.view_offset() == 0 {
            return 0;
        }
        self.away
            .filter(|away| away.epoch == self.state.epoch())
            .map_or(0, |away| self.bottom().saturating_sub(away.bottom))
    }

    /// The pill that says the bottom moved: "↓ 12 new lines" at the grid's foot, the whole pill
    /// one button home (as ⇧⇲ and any keystroke are). Nothing while the view follows the output
    /// or nothing arrived since it left.
    pub(super) fn render_new_lines(&self, cx: &Context<Self>) -> Option<gpui::AnyElement> {
        let count = self.new_lines();
        if count == 0 {
            return None;
        }
        let words = if count == 1 {
            SharedString::new_static("1 new line")
        } else {
            SharedString::from(format!("{count} new lines"))
        };
        let pill = FootPill { id: "new-lines", icon: icons::ALT_ARROW_DOWN, words, act: None };
        Some(self.render_foot_pill(&pill, cx, |this, window, cx| {
            this.scroll_to_bottom(&ScrollToBottom, window, cx);
        }))
    }
}

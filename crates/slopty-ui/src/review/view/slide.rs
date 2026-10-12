//! A review's lines do not wrap: they slide sideways together under a sideways scroll, their
//! numbers standing, as Zed's and Delta's diffs do (`.research/delta-signed-in-2026-10-12.md`
//! §2.8). A wrapped line of code reads as two, and its second half lost the column it was
//! indented to.

use gpui::{Context, ScrollWheelEvent, Window};

use super::ReviewView;
use crate::conversation::lines;

/// How wide a letter of the code face is, as a share of its size: Lilex's advance, measured
/// 9.0 at 15 in the signed-in app (`.research/delta-signed-in-2026-10-12.md` §2.4).
const ADVANCE: f32 = 0.6;

/// The widest line number a review's gutter is sized for here, in digits.
const DIGITS: f32 = 4.0;

impl ReviewView {
    /// The columns of the widest line on show, the blocks' and the opened context's.
    pub(super) fn widest_line(&self) -> usize {
        let mut widest = 0;
        for blocks in self.blocks.values() {
            for block in blocks.iter() {
                for line in &block.lines {
                    widest = widest.max(lines::columns(&line.text));
                }
            }
        }
        for opened in self.context.values() {
            for line in opened.iter() {
                widest = widest.max(lines::columns(&line.text));
            }
        }
        widest
    }

    /// How far the lines can slide: the widest one's run past the room the code has beside
    /// its gutter, and a base unit more so its last letter clears the edge.
    fn slide_room(&self) -> f32 {
        let theme = &self.theme;
        let size = theme.roles().code.size;
        let columns = f32::from(u16::try_from(self.widest).unwrap_or(u16::MAX));
        // The row's two pads, the gutter (a tabular figure about 0.62 of the size), its gap.
        let gutter = theme.spacing.sm.mul_add(3.0, size * 0.62 * DIGITS);
        (columns.mul_add(size * ADVANCE, theme.spacing.sm) - (self.width - gutter)).max(0.0)
    }

    /// How far the lines are slid now: as far as asked, within the room there is.
    pub(in crate::review) fn slid(&self) -> f32 {
        self.slide.clamp(0.0, self.slide_room())
    }

    /// A scroll over the diff: one more sideways than down slides the lines; the list scrolls
    /// the rest itself.
    pub(super) fn slide_by(
        &mut self,
        ev: &ScrollWheelEvent,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let delta = ev.delta.pixel_delta(window.line_height());
        let (dx, dy) = (f32::from(delta.x), f32::from(delta.y));
        if dx.abs() <= dy.abs() {
            return;
        }
        let to = (self.slid() - dx).clamp(0.0, self.slide_room());
        if (to - self.slide).abs() > f32::EPSILON {
            self.slide = to;
            cx.notify();
        }
    }
}

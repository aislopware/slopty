// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! The boxes a byte range of laid-out text covers, one per visual line.
//!
//! Inline code, search matches, links and selections paint their washes from these, under the
//! glyphs: a text run's own background is square and vanishes when clipped.

use std::ops::Range;

use gpui::{Bounds, Pixels, Point, TextLayout, point, px, size};

/// How many visual rows one range may cross before the walk gives up.
const ROW_GUARD: usize = 256;

/// The wash boxes for one byte range, in window coordinates from the laid-out text's own
/// geometry.
///
/// `pad_x` overhangs each box horizontally (inline code); `inset_y` shrinks it vertically. Both
/// are 0 for a selection wash, which wants full-line-height boxes that tile across wrapped rows.
#[must_use]
pub fn range_rects(
    layout: &TextLayout,
    range: &Range<usize>,
    pad_x: f32,
    inset_y: f32,
) -> Vec<Bounds<Pixels>> {
    range_rects_with_positions(layout.bounds(), layout.line_height(), range, pad_x, inset_y, |ix| {
        layout.position_for_index(ix)
    })
}

fn range_rects_with_positions(
    bounds: Bounds<Pixels>,
    line_height: Pixels,
    range: &Range<usize>,
    pad_x: f32,
    inset_y: f32,
    position_for_index: impl Fn(usize) -> Option<Point<Pixels>>,
) -> Vec<Bounds<Pixels>> {
    let mut rects = Vec::new();
    let mut cur = range.start;
    // Walk the range one visual row at a time: the furthest index still on the current row is
    // found by a binary search over glyph positions.
    for _ in 0..ROW_GUARD {
        if cur >= range.end {
            break;
        }
        let Some(mut p1) = position_for_index(cur) else { break };
        // GPUI gives a soft-wrap boundary upstream affinity: the boundary index is reported at
        // the end of the row before. When the next byte sits on a lower row, `cur` starts that
        // row; its downstream position starts the box.
        if let Some(after) = position_for_index(cur.saturating_add(1))
            && after.y > p1.y
        {
            p1 = point(bounds.left(), after.y);
        }
        // `seg_end` closes the wash on this row and `next` is the first index on the next. A
        // soft-wrap boundary belongs to both rows.
        let (seg_end, next) = match position_for_index(range.end) {
            Some(end) if end.y == p1.y => (range.end, range.end),
            _ => {
                let (mut lo, mut hi) = (cur, range.end);
                while hi.saturating_sub(lo) > 1 {
                    let mid = lo.saturating_add(hi.saturating_sub(lo) / 2);
                    match position_for_index(mid) {
                        Some(at) if at.y == p1.y => lo = mid,
                        _ => hi = mid,
                    }
                }
                (lo, lo)
            }
        };
        if let Some(p2) = position_for_index(seg_end)
            && p2.x > p1.x
        {
            rects.push(Bounds::new(
                point(p1.x - px(pad_x), p1.y + px(inset_y)),
                size(p2.x - p1.x + px(2.0 * pad_x), line_height - px(2.0 * inset_y)),
            ));
        }
        if next <= cur {
            break;
        }
        cur = next;
    }
    rects
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ten bytes a row, ten pixels a byte, rows twenty pixels apart.
    fn grid(ix: usize) -> Option<Point<Pixels>> {
        let ix = u16::try_from(ix).ok()?;
        Some(point(px(f32::from(ix % 10) * 10.0), px(f32::from(ix / 10) * 20.0)))
    }

    #[test]
    fn a_range_across_a_wrap_is_one_box_a_row() {
        let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(100.0), px(60.0)));
        let rects = range_rects_with_positions(bounds, px(20.0), &(5..25), 0.0, 0.0, grid);
        assert_eq!(rects.len(), 3, "{rects:?}");
        assert_eq!(rects[0].origin, point(px(50.0), px(0.0)), "starts mid-row");
        assert_eq!(rects[2].size.width, px(50.0), "ends mid-row");
    }
}

//! What the target under a drag would do, read off the system cursor.
//!
//! A drag source hears moves and the end, never the target's answer while it drags
//! (`NSDraggingSource`, `NSDraggingSession`), but the drag manager shows that answer as the
//! cursor: the copy cursor over a target that takes a copy, the link cursor over one that links,
//! and the arrow over one that refuses or over nothing (P0 (2): `diff=0` against AppKit's own
//! cursors, read as the worker reads the cursor for its streams). The helper's source allows Copy
//! alone, so those three are all it shows. A cursor that is none of AppKit's, an app's own, says
//! nothing, and is taken as a copy: the drop then decides.

use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2_app_kit::{NSCursor, NSImageRep};
use objc2_core_graphics::{CGDataProvider, CGImage, CGImageAlphaInfo, CGImageByteOrderInfo};
use slopty_capture::{AlphaAt, Layout, bgra_premultiplied};
use slopty_proto::drag::DragOp;
use slopty_proto::screen::CursorShape;

/// Which of AppKit's cursors the system cursor is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Class {
    /// The arrow: nothing takes the drop here.
    Arrow,
    /// The copy cursor, an arrow with a plus.
    Copy,
    /// The link cursor.
    Link,
    /// The not-allowed cursor.
    NotAllowed,
    /// The closed hand.
    ClosedHand,
    /// None of them.
    Other,
}

impl Class {
    /// What a drop does where the cursor is this.
    #[must_use]
    pub const fn op(self) -> DragOp {
        match self {
            Self::Copy | Self::Other => DragOp::Copy,
            Self::Link => DragOp::Link,
            Self::Arrow | Self::NotAllowed | Self::ClosedHand => DragOp::None,
        }
    }
}

/// A cursor's picture: width, height and premultiplied BGRA pixels.
pub type Picture = (u16, u16, Vec<u8>);

/// The most pixels of a cursor that may differ from a reference for it to be that reference: a
/// read is of the same picture, so 0 is usual, and a few allow for rounding at the edges.
const CLOSE: usize = 8;

/// AppKit's cursors as the window server draws them at one scale, to class the system cursor
/// by.
#[derive(Debug)]
pub struct Cursors {
    scale: u8,
    references: Vec<(Class, Picture)>,
}

impl Cursors {
    /// AppKit's arrow, copy, link, not-allowed and closed-hand cursors at `scale` pixels a
    /// point.
    #[must_use]
    pub fn at(scale: u8) -> Self {
        let references = [
            (Class::Arrow, NSCursor::arrowCursor()),
            (Class::Copy, NSCursor::dragCopyCursor()),
            (Class::Link, NSCursor::dragLinkCursor()),
            (Class::NotAllowed, NSCursor::operationNotAllowedCursor()),
            (Class::ClosedHand, NSCursor::closedHandCursor()),
        ]
        .into_iter()
        .filter_map(|(class, cursor)| Some((class, pixels(&cursor, scale)?)))
        .collect();
        Self::of(scale, references)
    }

    /// Cursors at `scale` from their pictures.
    #[must_use]
    pub const fn of(scale: u8, references: Vec<(Class, Picture)>) -> Self {
        Self { scale, references }
    }

    /// The scale the references are at.
    #[must_use]
    pub const fn scale(&self) -> u8 {
        self.scale
    }

    /// The reference of `shape`'s size with the fewest differing pixels, with how many differ,
    /// when one is that close; else [`Class::Other`] and the nearest's count.
    #[must_use]
    pub fn class(&self, shape: &CursorShape) -> (Class, Option<usize>) {
        let best = self
            .references
            .iter()
            .filter(|(_, (w, h, _))| (*w, *h) == (shape.w, shape.h))
            .map(|(class, (_, _, bgra))| (*class, differing(bgra, &shape.bgra)))
            .min_by_key(|(_, diff)| *diff);
        match best {
            Some((class, diff)) if diff <= CLOSE => (class, Some(diff)),
            Some((_, diff)) => (Class::Other, Some(diff)),
            None => (Class::Other, None),
        }
    }
}

/// How many of the drag manager's steps a drop of none waits for after one that took: the step
/// it entered the next target on and one more.
pub const NONE_STEPS: u64 = 2;

/// The longest a none waits for them, should the drag stop moving.
pub const NONE_WAIT: Duration = Duration::from_millis(50);

/// What a drop does, as said: each change of the cursor's operation, but a none that follows an
/// operation that took only once the drag manager has had its next step.
///
/// The drag manager leaves one target on a step of the drag and enters the next on a later one,
/// so between two targets that both take the drop the cursor is the arrow for a step: 4–61 ms,
/// median 26, in a guest whose drag manager steps every 30 ms (MEASUREMENTS.md, "the badge,
/// timed"). Said as it is, the client's badge would flicker to none at every crossing. Waiting
/// for [`NONE_STEPS`] steps (`draggingSession:movedToPoint:`) or [`NONE_WAIT`] hides that,
/// and a refusal still shows within a step or two.
#[derive(Clone, Copy, Debug, Default)]
pub struct Badge {
    said: Option<DragOp>,
    /// When the cursor turned none after a take: the drag's step then, and the time.
    held: Option<(u64, Instant)>,
}

impl Badge {
    /// The cursor's operation is `op` at the drag's step `steps`, at `now`: what to say, if
    /// anything.
    pub fn see(&mut self, op: DragOp, steps: u64, now: Instant) -> Option<DragOp> {
        if op.takes() || !self.said.is_some_and(DragOp::takes) {
            self.held = None;
            return (self.said.replace(op) != Some(op)).then_some(op);
        }
        let (since, at) = *self.held.get_or_insert((steps, now));
        let stepped = steps >= since.saturating_add(NONE_STEPS);
        if !stepped && now.saturating_duration_since(at) < NONE_WAIT {
            return None;
        }
        self.held = None;
        self.said = Some(op);
        Some(op)
    }
}

/// How many pixels of two pictures of one size differ by more than a quarter in any byte.
fn differing(a: &[u8], b: &[u8]) -> usize {
    a.as_chunks::<4>()
        .0
        .iter()
        .zip(b.as_chunks::<4>().0)
        .filter(|(p, q)| p.iter().zip(q.iter()).any(|(x, y)| x.abs_diff(*y) > 64))
        .count()
}

/// A cursor's picture as `slopty_capture::read_cursor` reads the system's: premultiplied BGRA at
/// `scale` pixels a point, from the representation nearest that size.
#[must_use]
pub fn pixels(cursor: &NSCursor, scale: u8) -> Option<Picture> {
    let image = cursor.image();
    let points = image.size();
    #[expect(clippy::cast_possible_truncation, reason = "a cursor is a few hundred pixels")]
    let wanted = (points.width * f64::from(scale)).round() as isize;
    let reps = image.representations();
    let rep: Retained<NSImageRep> = reps
        .iter()
        .filter(|rep| rep.pixelsWide() >= wanted)
        .min_by_key(|rep| rep.pixelsWide())
        .or_else(|| reps.iter().max_by_key(|rep| rep.pixelsWide()))?;
    // SAFETY: AppKit rule: a null proposed rect asks for the whole image at the
    // representation's own pixel size; no context and no hints is the documented way to read
    // its pixels.
    let cg = unsafe { rep.CGImageForProposedRect_context_hints(std::ptr::null_mut(), None, None) }?;
    let some = Some(&*cg);
    let (alpha, premultiplied, opaque) = match CGImage::alpha_info(some) {
        CGImageAlphaInfo::PremultipliedLast => (AlphaAt::Last, true, false),
        CGImageAlphaInfo::PremultipliedFirst => (AlphaAt::First, true, false),
        CGImageAlphaInfo::Last => (AlphaAt::Last, false, false),
        CGImageAlphaInfo::First => (AlphaAt::First, false, false),
        CGImageAlphaInfo::NoneSkipLast => (AlphaAt::Last, true, true),
        CGImageAlphaInfo::NoneSkipFirst => (AlphaAt::First, true, true),
        _ => return None,
    };
    let layout = Layout {
        width: u16::try_from(CGImage::width(some)).ok()?,
        height: u16::try_from(CGImage::height(some)).ok()?,
        bytes_per_row: CGImage::bytes_per_row(some),
        little_endian: CGImage::byte_order_info(some) == CGImageByteOrderInfo::Order32Little,
        alpha,
        premultiplied,
        opaque,
    };
    let provider = CGImage::data_provider(some)?;
    let data = CGDataProvider::data(Some(&provider))?.to_vec();
    Some((layout.width, layout.height, bgra_premultiplied(&layout, &data)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape((w, h, bgra): (u16, u16, Vec<u8>)) -> CursorShape {
        CursorShape { w, h, hot_x: 0, hot_y: 0, bgra, scale: 2 }
    }

    /// A 4×4 picture: transparent but for the pixels `on` lists, opaque black.
    fn picture(on: &[usize]) -> (u16, u16, Vec<u8>) {
        let mut bgra = vec![0_u8; 16 * 4];
        for (_, pixel) in
            bgra.as_chunks_mut::<4>().0.iter_mut().enumerate().filter(|(n, _)| on.contains(n))
        {
            *pixel = [0, 0, 0, 255];
        }
        (4, 4, bgra)
    }

    /// Each change is said once. A none after a take waits for the drag manager's next step
    /// and one more, or for [`NONE_WAIT`], and a take meanwhile hides it: crossing from one
    /// target to the next never flickers to none. A none with nothing taken before is said at
    /// once, and so is a take after a none.
    #[test]
    fn a_none_between_two_targets_is_not_said() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut badge = Badge::default();
        assert_eq!(badge.see(DragOp::None, 0, ms(0)), Some(DragOp::None), "none at first");
        assert_eq!(badge.see(DragOp::None, 0, ms(1)), None, "said once");
        assert_eq!(badge.see(DragOp::Copy, 1, ms(2)), Some(DragOp::Copy));
        assert_eq!(badge.see(DragOp::None, 2, ms(3)), None, "the step that left the first");
        assert_eq!(badge.see(DragOp::None, 3, ms(30)), None, "one step on");
        assert_eq!(badge.see(DragOp::Copy, 3, ms(31)), None, "the next took: no flicker");
        assert_eq!(badge.see(DragOp::None, 4, ms(40)), None);
        assert_eq!(badge.see(DragOp::None, 6, ms(41)), Some(DragOp::None), "two steps on");
        assert_eq!(badge.see(DragOp::Link, 6, ms(42)), Some(DragOp::Link), "a take at once");
        assert_eq!(badge.see(DragOp::None, 6, ms(43)), None, "the drag stopped moving");
        assert_eq!(badge.see(DragOp::None, 6, ms(92)), None);
        assert_eq!(badge.see(DragOp::None, 6, ms(93)), Some(DragOp::None), "after the wait");
    }

    /// The system cursor classes as the reference of its size it matches, and means that
    /// cursor's operation: copy and link take the drop, the arrow and the not-allowed cursor
    /// refuse it. A cursor near none of them, or of a size none has, is an app's own, taken as
    /// a copy so the drop decides. AppKit's own cursors match as read (P0 (2), live).
    #[test]
    fn cursor_shapes_map_to_operations() {
        let cursors = Cursors::of(
            2,
            vec![
                (Class::Arrow, picture(&[0, 1, 4])),
                (Class::Copy, picture(&[0, 1, 4, 15])),
                (Class::Link, picture(&[0, 1, 4, 12])),
                (Class::NotAllowed, picture(&[5, 6, 9, 10])),
            ],
        );
        assert_eq!(cursors.scale(), 2);
        for (on, class, op) in [
            (&[0, 1, 4][..], Class::Arrow, DragOp::None),
            (&[0, 1, 4, 15][..], Class::Copy, DragOp::Copy),
            (&[0, 1, 4, 12][..], Class::Link, DragOp::Link),
            (&[5, 6, 9, 10][..], Class::NotAllowed, DragOp::None),
        ] {
            assert_eq!(cursors.class(&shape(picture(on))), (class, Some(0)), "{class:?}");
            assert_eq!(class.op(), op);
        }
        let (class, diff) = cursors.class(&shape(picture(&[2, 3, 7, 8, 11, 13, 14])));
        assert_eq!(class, Class::Other, "{diff:?}");
        assert_eq!(class.op(), DragOp::Copy, "the drop decides");
        assert_eq!(cursors.class(&shape((3, 3, vec![0; 36]))), (Class::Other, None));
    }
}

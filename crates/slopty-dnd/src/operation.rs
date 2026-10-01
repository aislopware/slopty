//! What the target under a drag would do, read off the system cursor.
//!
//! A drag source hears moves and the end, never the target's answer while it drags
//! (`NSDraggingSource`, `NSDraggingSession`), but the drag manager shows that answer as the
//! cursor: the copy cursor over a target that takes a copy, the link cursor over one that links,
//! and the arrow over one that refuses or over nothing (P0 (2): `diff=0` against AppKit's own
//! cursors, read as the worker reads the cursor for its streams). The helper's source allows Copy
//! alone, so those three are all it shows. A cursor that is none of AppKit's, an app's own, says
//! nothing, and is taken as a copy: the drop then decides.

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

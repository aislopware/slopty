//! Where a stream's input last put the worker's pointer, for the stream's cursor samples.
//!
//! A display stream's events go through the HID tap and move the worker's own pointer, so the
//! real pointer is the one to show. A window stream's events go to the window's application and
//! leave the real pointer wherever the worker's own user left it, so the picture's pointer is
//! where the input put it, or nowhere before the first event.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// The sink's events move the real pointer. Both halves are NaN patterns, which no placed
/// point (finite by construction) packs to.
const REAL: u64 = u64::MAX;
/// Nothing placed yet.
const UNPLACED: u64 = u64::MAX - 1;

/// Where the worker's pointer is, as far as a stream's input is concerned.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Pointer {
    /// The input moves the worker's own pointer: read the real one.
    Real,
    /// The input leaves the worker's pointer alone: the last place it was put, in global display
    /// points, `None` before the first.
    Placed(Option<(f64, f64)>),
}

/// A [`Pointer`] one thread writes and any other reads, lock-free. Starts as `Placed(None)`.
#[derive(Clone, Debug)]
pub struct PointerWatch(Arc<AtomicU64>);

impl Default for PointerWatch {
    fn default() -> Self {
        Self(Arc::new(AtomicU64::new(UNPLACED)))
    }
}

impl PointerWatch {
    /// The pointer as last written.
    #[must_use]
    pub fn get(&self) -> Pointer {
        match self.0.load(Ordering::Relaxed) {
            REAL => Pointer::Real,
            UNPLACED => Pointer::Placed(None),
            bits => {
                let high = u32::try_from(bits >> 32).unwrap_or_default();
                let low = u32::try_from(bits & u64::from(u32::MAX)).unwrap_or_default();
                Pointer::Placed(Some((
                    f64::from(f32::from_bits(high)),
                    f64::from(f32::from_bits(low)),
                )))
            }
        }
    }

    /// The input moves the worker's own pointer.
    pub fn follow_real(&self) {
        self.0.store(REAL, Ordering::Relaxed);
    }

    /// The input put the pointer at `(x, y)` global display points. A point that is not finite
    /// was never posted anywhere and is ignored.
    pub fn place(&self, x: f64, y: f64) {
        #[expect(clippy::cast_possible_truncation, reason = "display points fit an f32 to 0.01")]
        let (x, y) = (x as f32, y as f32);
        if !x.is_finite() || !y.is_finite() {
            return;
        }
        let bits = (u64::from(x.to_bits()) << 32) | u64::from(y.to_bits());
        self.0.store(bits, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_placed_point_reads_back_and_nothing_else_looks_like_one() {
        let watch = PointerWatch::default();
        assert_eq!(watch.get(), Pointer::Placed(None));
        watch.place(-1512.5, 982.25);
        assert_eq!(watch.get(), Pointer::Placed(Some((-1512.5, 982.25))));
        watch.place(f64::NAN, 3.0);
        assert_eq!(watch.get(), Pointer::Placed(Some((-1512.5, 982.25))), "a NaN is not a place");
        watch.follow_real();
        assert_eq!(watch.get(), Pointer::Real);
    }
}

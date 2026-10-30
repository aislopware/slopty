//! The number conversions the transcript and its surfaces make between counts, pixels and
//! milliseconds, each saying once why its cast is sound.

/// A count or an index as an `f32`: rows, lines, items, steps, pixels of an image side.
#[expect(
    clippy::cast_precision_loss,
    reason = "exact below 2^24 (16.7 million), which no count of things a view lays out nears"
)]
pub const fn count(n: usize) -> f32 {
    n as f32
}

/// Float milliseconds as whole ones: rounded, with NaN and negatives at 0.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a float-to-int `as` saturates at the bounds and takes NaN to 0, the clamp wanted"
)]
pub const fn millis(ms: f32) -> u64 {
    ms.round() as u64
}

/// A span in whole microseconds, saturating past `u64::MAX` (half a million years).
pub fn micros(span: std::time::Duration) -> u64 {
    u64::try_from(span.as_micros()).unwrap_or(u64::MAX)
}

/// An image's side in pixels as an `f32`.
#[expect(
    clippy::cast_precision_loss,
    reason = "exact below 2^24 pixels; past that only the last bits of a scale or an aspect round"
)]
pub const fn side(pixels: u32) -> f32 {
    pixels as f32
}

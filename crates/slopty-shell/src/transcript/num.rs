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

/// A side worked out in floats as whole pixels: truncated, with NaN and negatives at 0 and
/// anything past `u32::MAX` at it.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a float-to-int `as` saturates at the bounds and takes NaN to 0, the clamp wanted"
)]
pub const fn whole_side(side: f64) -> u32 {
    side as u32
}

/// An image's side in pixels as an `f32`.
#[expect(
    clippy::cast_precision_loss,
    reason = "exact below 2^24 pixels; past that only the last bits of a scale or an aspect round"
)]
pub const fn side(pixels: u32) -> f32 {
    pixels as f32
}

/// A float worked out as a count or an index: truncated, with NaN and negatives at 0 and
/// anything past `usize::MAX` at it.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a float-to-int `as` saturates at the bounds and takes NaN to 0, the clamp wanted"
)]
pub const fn whole(n: f32) -> usize {
    n as usize
}

/// An `f64` worked out for a drawing (a ratio, a time, a coordinate) as the `f32` it is drawn
/// with.
#[expect(
    clippy::cast_possible_truncation,
    reason = "an f64-to-f32 `as` rounds to the nearest f32, all the precision a drawing holds; \
              past f32's range it is infinite, which no ratio, time or coordinate drawn nears"
)]
pub const fn narrow(x: f64) -> f32 {
    x as f32
}

/// A count of frames as an `f32`, for a shader's frame uniform.
#[expect(
    clippy::cast_precision_loss,
    reason = "exact below 2^24 frames (three days at 60 a second); past that it steps by two"
)]
pub const fn frames(n: u32) -> f32 {
    n as f32
}

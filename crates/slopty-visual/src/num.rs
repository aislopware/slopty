// Derived from zeron (MIT, github.com/zeronsh/zeron @ 3b2f780), see THIRD_PARTY_NOTICES.md
//! The number conversions a visual makes between Lua's `f64`, geometry's `f32`
//! and counts, each saying once why its cast is sound.

/// A Lua number as the `f32` geometry is kept in: the nearest one, or an
/// infinity past its range, which callers clamp or reject.
#[expect(
    clippy::cast_possible_truncation,
    reason = "geometry is `f32`; narrowing rounds to the nearest, as wanted"
)]
pub(crate) const fn narrow(value: f64) -> f32 {
    value as f32
}

/// A count or an index as an `f32`.
#[expect(
    clippy::cast_precision_loss,
    reason = "exact below 2^24, which counts here (segments, grid steps, pixels of an \
              8192-pixel side, faces under the cap) stay far under"
)]
pub(crate) const fn count(n: usize) -> f32 {
    n as f32
}

/// `value` truncated toward zero to a `usize`: a negative or NaN value gives
/// 0, and one past the range saturates.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "`as` truncates, saturates and maps NaN to 0, which is the conversion wanted"
)]
pub(crate) const fn whole(value: f32) -> usize {
    value as usize
}

/// [`whole`] into a `u8`, for numbers already clamped to a few units.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "`as` truncates, saturates and maps NaN to 0, which is the conversion wanted"
)]
pub(crate) const fn small(value: f64) -> u8 {
    value as u8
}

/// [`whole`] into a `u8`, for a color channel already scaled into 0..=255.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "`as` truncates, saturates and maps NaN to 0, which is the conversion wanted"
)]
pub(crate) const fn byte(value: f32) -> u8 {
    value as u8
}

/// A Lua integer as an `f32`, the nearest one.
#[expect(
    clippy::cast_precision_loss,
    reason = "a scene's numbers (sizes, angles, factors) are far below 2^24, where `f32` is exact"
)]
pub(crate) const fn integer(n: i64) -> f32 {
    n as f32
}

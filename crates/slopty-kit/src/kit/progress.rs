//! How far a thing has got, in one language wherever it shows: a program's report in its
//! terminal (`OSC 9;4`), which the shell draws.

/// How far a thing has got.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Progress {
    /// This share of it, 0 to 1.
    Share(f32),
    /// Under way, how far unknown.
    Busy,
    /// Held, at this share when one is known.
    Paused(Option<f32>),
    /// Stopped by a failure, at this share when one is known.
    Failed(Option<f32>),
}

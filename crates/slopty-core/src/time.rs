//! Monotonic time for telemetry and pacing.
//!
//! Wall clocks differ between host and client; every latency figure on the wire is expressed
//! as a delta or an echoed timestamp in the sender's own monotonic domain.

use core::{fmt, ops};

use serde::{Deserialize, Serialize};

/// Nanoseconds on a monotonic clock. Only comparable with values from the same process.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(transparent)]
pub struct MonoTime(u64);

impl MonoTime {
    /// The current monotonic time of this process.
    #[must_use]
    pub fn now() -> Self {
        // `Instant` has no public epoch; we keep our own so values are plain integers on the wire.
        static EPOCH: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        let epoch = EPOCH.get_or_init(std::time::Instant::now);
        let nanos = epoch.elapsed().as_nanos();
        Self(u64::try_from(nanos).unwrap_or(u64::MAX))
    }

    /// Construct from raw nanoseconds (for tests and deserialised telemetry).
    #[must_use]
    pub const fn from_nanos(nanos: u64) -> Self {
        Self(nanos)
    }

    /// Raw nanoseconds.
    #[must_use]
    pub const fn as_nanos(self) -> u64 {
        self.0
    }

    /// Time elapsed since `earlier`, saturating at zero if `earlier` is later.
    #[must_use]
    pub const fn since(self, earlier: Self) -> Duration {
        Duration(self.0.saturating_sub(earlier.0))
    }

    /// Elapsed time since this instant.
    #[must_use]
    pub fn elapsed(self) -> Duration {
        Self::now().since(self)
    }
}

impl fmt::Debug for MonoTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MonoTime({}ns)", self.0)
    }
}

impl ops::Add<Duration> for MonoTime {
    type Output = Self;

    fn add(self, rhs: Duration) -> Self {
        Self(self.0.saturating_add(rhs.0))
    }
}

/// A non-negative span of nanoseconds. Saturating arithmetic; never panics.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(transparent)]
pub struct Duration(u64);

impl Duration {
    /// Zero.
    pub const ZERO: Self = Self(0);

    /// From nanoseconds.
    #[must_use]
    pub const fn from_nanos(nanos: u64) -> Self {
        Self(nanos)
    }

    /// From microseconds.
    #[must_use]
    pub const fn from_micros(micros: u64) -> Self {
        Self(micros.saturating_mul(1_000))
    }

    /// From milliseconds.
    #[must_use]
    pub const fn from_millis(millis: u64) -> Self {
        Self(millis.saturating_mul(1_000_000))
    }

    /// From seconds.
    #[must_use]
    pub const fn from_secs(secs: u64) -> Self {
        Self(secs.saturating_mul(1_000_000_000))
    }

    /// Nanoseconds.
    #[must_use]
    pub const fn as_nanos(self) -> u64 {
        self.0
    }

    /// Microseconds, truncated.
    #[must_use]
    pub const fn as_micros(self) -> u64 {
        self.0 / 1_000
    }

    /// Milliseconds, truncated.
    #[must_use]
    pub const fn as_millis(self) -> u64 {
        self.0 / 1_000_000
    }

    /// Fractional milliseconds.
    #[must_use]
    pub fn as_millis_f64(self) -> f64 {
        // Precision loss above 2^53 ns (~104 days) is irrelevant for a latency figure.
        #[expect(clippy::cast_precision_loss, reason = "telemetry display only")]
        let nanos = self.0 as f64;
        nanos / 1_000_000.0
    }

    /// Convert to the standard library type.
    #[must_use]
    pub const fn to_std(self) -> std::time::Duration {
        std::time::Duration::from_nanos(self.0)
    }
}

impl From<std::time::Duration> for Duration {
    fn from(d: std::time::Duration) -> Self {
        Self(u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
    }
}

impl fmt::Debug for Duration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.3}ms", self.as_millis_f64())
    }
}

impl ops::Add for Duration {
    type Output = Self;

    fn add(self, rhs: Self) -> Self {
        Self(self.0.saturating_add(rhs.0))
    }
}

impl ops::Sub for Duration {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self {
        Self(self.0.saturating_sub(rhs.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monotonic_time_never_goes_backwards() {
        let a = MonoTime::now();
        let b = MonoTime::now();
        assert!(b >= a);
        assert_eq!(a.since(b), Duration::ZERO, "since() saturates instead of underflowing");
    }

    #[test]
    fn duration_conversions() {
        assert_eq!(Duration::from_millis(3).as_micros(), 3_000);
        assert_eq!(Duration::from_secs(1).as_millis(), 1_000);
        assert_eq!(Duration::from_millis(u64::MAX).as_nanos(), u64::MAX, "saturates");
        assert_eq!(format!("{:?}", Duration::from_micros(1500)), "1.500ms");
    }

    /// Nanoseconds go in and out unchanged, sums and differences saturate instead of
    /// wrapping, the standard duration converts, and the clock moves.
    #[test]
    fn the_arithmetic_saturates_and_the_conversions_round_trip() {
        let d = Duration::from_nanos;
        assert_eq!(MonoTime::from_nanos(5).as_nanos(), 5);
        assert_eq!((MonoTime::from_nanos(5) + d(7)).as_nanos(), 12);
        assert_eq!((MonoTime::from_nanos(u64::MAX) + d(1)).as_nanos(), u64::MAX);
        assert_eq!(d(3) + d(4), d(7));
        assert_eq!(d(u64::MAX) + d(1), d(u64::MAX));
        assert_eq!(d(4) - d(3), d(1));
        assert_eq!(d(3) - d(4), d(0), "saturates at zero");
        assert_eq!(Duration::from(std::time::Duration::from_millis(1)), d(1_000_000));
        assert_eq!(format!("{:?}", MonoTime::from_nanos(5)), "MonoTime(5ns)");
        assert_eq!(format!("{:?}", d(1_500_000)), "1.500ms");
        // The clock: a spin of 50 µs on the standard clock is at least that on ours.
        let started = MonoTime::now();
        let spin = std::time::Instant::now();
        while spin.elapsed() < std::time::Duration::from_micros(50) {
            std::hint::spin_loop();
        }
        assert!(started.elapsed() >= Duration::from_micros(50), "{:?}", started.elapsed());
        assert!(MonoTime::now().as_nanos() > started.as_nanos());
    }
}

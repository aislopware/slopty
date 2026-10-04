//! Monotonic time for telemetry and pacing, and wall time for what is held and relayed.
//!
//! Wall clocks differ between worker and client; every latency figure on the wire is expressed
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

/// Milliseconds since the Unix epoch, by the clock of the machine that read it.
///
/// For what is held and relayed (a file's modification time, when a session started, when the
/// server heard something), where an age measured at sending would be wrong by the time it is
/// read. Machines' clocks differ, so a latency is never one of these. Zero means unknown.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(transparent)]
pub struct WallMs(u64);

impl WallMs {
    /// Unknown: the Unix epoch itself.
    pub const ZERO: Self = Self(0);

    /// The wall clock now.
    #[must_use]
    pub fn now() -> Self {
        Self::of(std::time::SystemTime::now())
    }

    /// `time` in milliseconds; zero before the epoch.
    #[must_use]
    pub fn of(time: std::time::SystemTime) -> Self {
        let since = time.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        Self(u64::try_from(since.as_millis()).unwrap_or(u64::MAX))
    }

    /// From raw milliseconds since the epoch.
    #[must_use]
    pub const fn from_millis(ms: u64) -> Self {
        Self(ms)
    }

    /// Raw milliseconds since the epoch.
    #[must_use]
    pub const fn as_millis(self) -> u64 {
        self.0
    }

    /// Whether it says nothing.
    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }

    /// The instant it names; `None` for [`Self::ZERO`].
    #[must_use]
    pub fn to_system(self) -> Option<std::time::SystemTime> {
        if self.is_zero() {
            return None;
        }
        std::time::UNIX_EPOCH.checked_add(std::time::Duration::from_millis(self.0))
    }

    /// How long after `earlier` this is, saturating at zero.
    #[must_use]
    pub const fn since(self, earlier: Self) -> std::time::Duration {
        std::time::Duration::from_millis(self.0.saturating_sub(earlier.0))
    }

    /// `span` later, saturating at the end of time.
    #[must_use]
    pub fn saturating_add(self, span: std::time::Duration) -> Self {
        Self(self.0.saturating_add(u64::try_from(span.as_millis()).unwrap_or(u64::MAX)))
    }

    /// Milliseconds after `earlier`, saturating at zero.
    #[must_use]
    pub const fn millis_since(self, earlier: Self) -> u64 {
        self.0.saturating_sub(earlier.0)
    }

    /// The date and time of day it reads `offset_s` seconds east of UTC (a time zone's offset
    /// then), on the proleptic Gregorian calendar.
    #[must_use]
    pub fn civil(self, offset_s: i64) -> Option<Civil> {
        const DAY_S: i64 = 86_400;
        let seconds = i64::try_from(self.0.checked_div(1_000)?).ok()?.checked_add(offset_s)?;
        let (days, of_day) = (seconds.div_euclid(DAY_S), seconds.rem_euclid(DAY_S));
        let (year, month, day) = civil_from_days(days)?;
        let part = |n: i64| u8::try_from(n).ok();
        Some(Civil {
            year,
            month: part(month)?,
            day: part(day)?,
            hour: part(of_day.checked_div(3_600)?)?,
            minute: part(of_day.checked_rem(3_600)?.checked_div(60)?)?,
            second: part(of_day.checked_rem(60)?)?,
        })
    }
}

/// A wall time as a calendar and a clock read it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Civil {
    /// The year, proleptic Gregorian.
    pub year: i64,
    /// 1 to 12.
    pub month: u8,
    /// 1 to 31.
    pub day: u8,
    /// 0 to 23.
    pub hour: u8,
    /// 0 to 59.
    pub minute: u8,
    /// 0 to 59.
    pub second: u8,
}

/// The proleptic Gregorian date `days` after 1970-01-01 (Howard Hinnant's `civil_from_days`).
fn civil_from_days(days: i64) -> Option<(i64, i64, i64)> {
    let z = days.checked_add(719_468)?;
    let era = z.div_euclid(146_097);
    let day_of_era = z.checked_sub(era.checked_mul(146_097)?)?;
    let year_of_era = day_of_era
        .checked_sub(day_of_era.checked_div(1_460)?)?
        .checked_add(day_of_era.checked_div(36_524)?)?
        .checked_sub(day_of_era.checked_div(146_096)?)?
        .checked_div(365)?;
    let day_of_year = day_of_era.checked_sub(
        year_of_era
            .checked_mul(365)?
            .checked_add(year_of_era.checked_div(4)?)?
            .checked_sub(year_of_era.checked_div(100)?)?,
    )?;
    let shifted_month = day_of_year.checked_mul(5)?.checked_add(2)?.checked_div(153)?;
    let day = day_of_year
        .checked_sub(shifted_month.checked_mul(153)?.checked_add(2)?.checked_div(5)?)?
        .checked_add(1)?;
    let month = if shifted_month < 10 {
        shifted_month.checked_add(3)?
    } else {
        shifted_month.checked_sub(9)?
    };
    let year = year_of_era.checked_add(era.checked_mul(400)?)?;
    let year = if month <= 2 { year.checked_add(1)? } else { year };
    Some((year, month, day))
}

impl fmt::Debug for WallMs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "WallMs({})", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The calendar reads the epoch, a leap day and the turn of a year, east and west of UTC.
    #[test]
    fn a_wall_time_reads_as_its_calendar_date() {
        let civil = |ms: u64, offset: i64| {
            let c = WallMs::from_millis(ms).civil(offset).unwrap();
            (c.year, c.month, c.day, c.hour, c.minute, c.second)
        };
        assert_eq!(civil(0, 0), (1970, 1, 1, 0, 0, 0));
        // 2024-02-29 12:34:56 UTC.
        assert_eq!(civil(1_709_210_096_000, 0), (2024, 2, 29, 12, 34, 56));
        assert_eq!(civil(1_709_210_096_000, 7 * 3_600), (2024, 2, 29, 19, 34, 56));
        // 2026-01-01 00:30:00 UTC is still the old year five hours west.
        assert_eq!(civil(1_767_227_400_000, -5 * 3_600), (2025, 12, 31, 19, 30, 0));
    }

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

    /// Wall milliseconds go in and out unchanged, differences saturate, zero is unknown, and
    /// the clock reads past 2020.
    #[test]
    fn wall_milliseconds_round_trip_and_zero_is_unknown() {
        let at = WallMs::from_millis(1_500);
        assert_eq!(at.as_millis(), 1_500);
        assert_eq!(at.since(WallMs::from_millis(500)), std::time::Duration::from_secs(1));
        assert_eq!(WallMs::from_millis(500).millis_since(at), 0, "saturates");
        assert_eq!(WallMs::ZERO.to_system(), None);
        assert_eq!(WallMs::of(at.to_system().unwrap()), at);
        assert_eq!(
            at.saturating_add(std::time::Duration::from_millis(5)),
            WallMs::from_millis(1_505)
        );
        assert!(WallMs::now() > WallMs::from_millis(1_577_836_800_000));
        assert_eq!(serde_json::to_string(&at).unwrap(), "1500", "a bare number on the wire");
    }
}

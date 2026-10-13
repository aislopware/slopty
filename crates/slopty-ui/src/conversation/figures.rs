//! How a thread says things in words: a model's name as a person says it, and a time of day.
//!
//! Nothing here draws; the views set these words where they belong.

use slopty_core::WallMs;

/// A time of day from ms since the Unix epoch, in this machine's zone: "14:05". `None` for
/// a record with no stamp.
#[must_use]
pub fn clock(at: WallMs) -> Option<String> {
    if at.is_zero() {
        return None;
    }
    let secs = i64::try_from(at.as_millis() / 1_000).ok()?;
    let local = secs.checked_add(utc_offset(secs))?;
    let day = local.rem_euclid(86_400);
    Some(format!("{:02}:{:02}", day / 3_600, (day % 3_600) / 60))
}

/// When a message was written, said for a reader at `now`, in this machine's zone.
///
/// The time alone today ("14:05"), "Yesterday 14:05", the weekday within the week ("Mon
/// 14:05"), and the date past that ("3 Oct 14:05"). `None` for a record with no stamp.
#[must_use]
pub fn stamp(at: WallMs, now: WallMs) -> Option<String> {
    let time = clock(at)?;
    let local_day = |ms: WallMs| {
        let secs = i64::try_from(ms.as_millis() / 1_000).ok()?;
        Some(secs.checked_add(utc_offset(secs))?.div_euclid(86_400))
    };
    let (day, today) = (local_day(at)?, local_day(now)?);
    Some(day_words(day, today).map_or_else(|| time.clone(), |day| format!("{day} {time}")))
}

/// How `day` reads beside a time for a reader on `today`, both days since the Unix epoch:
/// nothing for today, and a day to come as one gone ("Tomorrow", "Mon", "6 Oct").
fn day_words(day: i64, today: i64) -> Option<String> {
    const WEEKDAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] =
        ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    match today.checked_sub(day)? {
        0 => None,
        1 => Some("Yesterday".to_owned()),
        -1 => Some("Tomorrow".to_owned()),
        2..=6 | -6..=-2 => {
            let weekday = usize::try_from(day.rem_euclid(7)).ok()?;
            WEEKDAYS.get(weekday).map(|&d| d.to_owned())
        }
        _ => {
            let (month, date) = month_and_date(day);
            Some(format!("{date} {}", MONTHS.get(month.checked_sub(1)?)?))
        }
    }
}
/// The month (1 to 12) and its day of `day`, days since the Unix epoch (Howard Hinnant's
/// civil-from-days).
fn month_and_date(day: i64) -> (usize, i64) {
    let z = day.saturating_add(719_468);
    let era = z.div_euclid(146_097);
    let doe = z.saturating_sub(era.saturating_mul(146_097));
    let yoe =
        doe.saturating_sub(doe / 1_460).saturating_add(doe / 36_524).saturating_sub(doe / 146_096)
            / 365;
    let doy = doe
        .saturating_sub(yoe.saturating_mul(365).saturating_add(yoe / 4).saturating_sub(yoe / 100));
    let mp = doy.saturating_mul(5).saturating_add(2) / 153;
    let date = doy.saturating_sub(mp.saturating_mul(153).saturating_add(2) / 5).saturating_add(1);
    let month = if mp < 10 { mp.saturating_add(3) } else { mp.saturating_sub(9) };
    (usize::try_from(month).unwrap_or(1), date)
}

/// Seconds this machine's zone is ahead of UTC at `secs` past the Unix epoch, kept per hour:
/// a list asks for every visible prompt's time on every frame.
fn utc_offset(secs: i64) -> i64 {
    use core_foundation::date::CFDate;
    use core_foundation::timezone::CFTimeZone;
    thread_local! {
        static HOUR: std::cell::Cell<Option<(i64, i64)>> = const { std::cell::Cell::new(None) };
    }
    /// The Unix epoch in Core Foundation's absolute time, which counts from 2001-01-01.
    const CF_EPOCH: i64 = 978_307_200;
    let hour = secs.div_euclid(3_600);
    if let Some((at, offset)) = HOUR.get()
        && at == hour
    {
        return offset;
    }
    #[expect(clippy::cast_precision_loss, reason = "seconds since 2001, well within f64")]
    let at = CFDate::new(secs.saturating_sub(CF_EPOCH) as f64);
    #[expect(clippy::cast_possible_truncation, reason = "a zone offset in whole seconds")]
    let offset = CFTimeZone::system().seconds_from_gmt(at).round() as i64;
    HOUR.set(Some((hour, offset)));
    offset
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A day reads as nothing today, "Yesterday", a weekday within the week, and a date past
    /// it; 2026-10-03 is a Saturday, day `20_729` since the epoch.
    #[test]
    fn a_message_s_day_reads_by_how_far_back_it_is() {
        let today = 20_729;
        assert_eq!(day_words(today, today), None);
        assert_eq!(day_words(today - 1, today).as_deref(), Some("Yesterday"));
        assert_eq!(day_words(today - 5, today).as_deref(), Some("Mon"));
        assert_eq!(day_words(today - 2, today).as_deref(), Some("Thu"));
        assert_eq!(day_words(today - 7, today).as_deref(), Some("26 Sep"));
        assert_eq!(day_words(0, today).as_deref(), Some("1 Jan"));
        assert_eq!(month_and_date(20_729), (10, 3));
        assert_eq!(month_and_date(19_782), (2, 29), "2024's leap day");
        assert_eq!(day_words(today + 1, today).as_deref(), Some("Tomorrow"));
        assert_eq!(day_words(today + 2, today).as_deref(), Some("Mon"), "a day to come");
        assert_eq!(day_words(today + 9, today).as_deref(), Some("12 Oct"));
    }
}

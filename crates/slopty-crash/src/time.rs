//! Wall-clock times: now, printed in UTC, and read from a `.ips` report.

use std::time::{SystemTime, UNIX_EPOCH};

const DAY_S: i64 = 86_400;

/// Milliseconds since the Unix epoch, now; zero before it.
pub(crate) fn now_ms() -> u64 {
    let since = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
}

/// `ms` as `2026-09-30 12:34:56 UTC`.
pub(crate) fn format_utc(ms: u64) -> String {
    let seconds = i64::try_from(ms / 1_000).unwrap_or(i64::MAX);
    let (days, of_day) = (seconds.div_euclid(DAY_S), seconds.rem_euclid(DAY_S));
    let Some((year, month, day)) = civil_from_days(days) else {
        return format!("{ms} ms");
    };
    let (hour, minute, second) = (of_day / 3_600, of_day % 3_600 / 60, of_day % 60);
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02} UTC")
}

/// A `.ips` capture time (`2026-09-30 00:47:38.2846 +0700`) in ms since the Unix epoch.
pub(crate) fn parse_ips(stamp: &str) -> Option<u64> {
    let mut parts = stamp.split_whitespace();
    let (date, clock, zone) = (parts.next()?, parts.next()?, parts.next()?);
    let mut ymd = date.splitn(3, '-').map(str::parse::<i64>);
    let (year, month, day) = (ymd.next()?.ok()?, ymd.next()?.ok()?, ymd.next()?.ok()?);
    let (clock, fraction) = clock.split_once('.').unwrap_or((clock, ""));
    let mut hms = clock.splitn(3, ':').map(str::parse::<i64>);
    let (hour, minute, second) = (hms.next()?.ok()?, hms.next()?.ok()?, hms.next()?.ok()?);
    let millis: i64 = format!("{:0<3}", fraction.get(..3.min(fraction.len()))?).parse().ok()?;
    let (sign, zone) = match zone.split_at_checked(1)? {
        ("+", rest) => (1, rest),
        ("-", rest) => (-1, rest),
        _ => return None,
    };
    let (zone_h, zone_m) = zone.split_at_checked(2)?;
    let offset = zone_h
        .parse::<i64>()
        .ok()?
        .checked_mul(3_600)?
        .checked_add(zone_m.parse::<i64>().ok()?.checked_mul(60)?)?
        .checked_mul(sign)?;
    let local = days_from_civil(year, month, day)?
        .checked_mul(DAY_S)?
        .checked_add(hour.checked_mul(3_600)?)?
        .checked_add(minute.checked_mul(60)?)?
        .checked_add(second)?;
    let utc = local.checked_sub(offset)?;
    u64::try_from(utc.checked_mul(1_000)?.checked_add(millis)?).ok()
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Howard Hinnant's `days_from_civil`).
fn days_from_civil(year: i64, month: i64, day: i64) -> Option<i64> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let year = if month <= 2 { year.checked_sub(1)? } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year.checked_sub(era.checked_mul(400)?)?;
    let shifted_month = if month > 2 { month.checked_sub(3)? } else { month.checked_add(9)? };
    let day_of_year = shifted_month
        .checked_mul(153)?
        .checked_add(2)?
        .checked_div(5)?
        .checked_add(day)?
        .checked_sub(1)?;
    let day_of_era = year_of_era
        .checked_mul(365)?
        .checked_add(year_of_era.checked_div(4)?)?
        .checked_sub(year_of_era.checked_div(100)?)?
        .checked_add(day_of_year)?;
    era.checked_mul(146_097)?.checked_add(day_of_era)?.checked_sub(719_468)
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

#[cfg(test)]
mod tests {
    use super::{civil_from_days, days_from_civil, format_utc, parse_ips};

    #[test]
    fn dates_round_trip_through_days() {
        for days in [-719_468, -1, 0, 1, 11_016, 20_726, 2_932_896] {
            let (y, m, d) = civil_from_days(days).unwrap();
            assert_eq!(days_from_civil(y, m, d), Some(days), "{y}-{m}-{d}");
        }
    }

    #[test]
    fn a_time_prints_in_utc() {
        assert_eq!(format_utc(0), "1970-01-01 00:00:00 UTC", "the epoch");
        assert_eq!(format_utc(1_790_764_058_284), "2026-09-30 10:27:38 UTC", "a crash");
    }

    #[test]
    fn an_ips_capture_time_reads_with_its_zone() {
        assert_eq!(
            parse_ips("2026-09-30 17:27:38.2846 +0700"),
            Some(1_790_764_058_284),
            "east of UTC"
        );
        assert_eq!(parse_ips("2026-09-30 05:27:38.28 -0500"), Some(1_790_764_058_280), "west");
        assert_eq!(parse_ips("2026-09-30 10:27:38 +0000"), Some(1_790_764_058_000), "no fraction");
        assert_eq!(parse_ips("yesterday"), None, "not a time");
    }
}

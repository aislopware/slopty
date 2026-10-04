//! When a project's schedule runs: a five-field cron rule, read in the person's time zone.
//!
//! The fields are minute (0-59), hour (0-23), day of the month (1-31), month (1-12 or `jan`
//! to `dec`) and day of the week (0-7 or `sun` to `sat`, 0 and 7 both Sunday). Each is `*`, a
//! value, a range `a-b`, any of those stepped with `/n`, or a list of them joined by commas.
//! When both day fields are restricted a day matching either runs, as cron has it. `@hourly`,
//! `@daily`, `@weekly`, `@monthly` and `@yearly` stand for their usual rules.
//!
//! A rule is read in an IANA time zone the client names, which need not be the server's: the
//! server may run in a container with no time zone database, so a name the system cannot
//! resolve is looked up in the copy built into the binary. A time a change to summer time
//! skips runs at the first moment after it, and one the change back repeats runs once.

use jiff::Timestamp;
use jiff::civil::{Date, DateTime};
use jiff::tz::{TimeZone, TimeZoneDatabase};
use slopty_proto::project::WHEN_MAX;

/// How far ahead the next run is looked for: a rule naming a day that comes this rarely, or
/// never (the 30th of February), has none.
const DAYS_AHEAD: u32 = 366 * 8;

const MONTHS: [&str; 12] =
    ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
const WEEKDAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

/// A rule, each field as the set of values it matches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct When {
    minutes: u64,
    hours: u32,
    days: u32,
    months: u16,
    weekdays: u8,
    /// Both day fields are restricted, so a day matching either runs.
    either: bool,
}

/// One field's bounds and its names, if it has any.
struct Field {
    name: &'static str,
    least: u8,
    most: u8,
    names: &'static [&'static str],
    /// Where the first name stands.
    first_named: u8,
}

const MINUTE: Field = Field { name: "minute", least: 0, most: 59, names: &[], first_named: 0 };
const HOUR: Field = Field { name: "hour", least: 0, most: 23, names: &[], first_named: 0 };
const DAY: Field =
    Field { name: "day of the month", least: 1, most: 31, names: &[], first_named: 0 };
const MONTH: Field = Field { name: "month", least: 1, most: 12, names: &MONTHS, first_named: 1 };
const WEEKDAY: Field =
    Field { name: "day of the week", least: 0, most: 7, names: &WEEKDAYS, first_named: 0 };

impl When {
    /// Read `text` as a rule.
    ///
    /// # Errors
    /// What is wrong with it, in words the person can act on.
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        if text.len() > WHEN_MAX {
            return Err(format!("a schedule's rule is at most {WHEN_MAX} bytes"));
        }
        let rule = match text.to_ascii_lowercase().as_str() {
            "@hourly" => "0 * * * *".to_owned(),
            "@daily" | "@midnight" => "0 0 * * *".to_owned(),
            "@weekly" => "0 0 * * 0".to_owned(),
            "@monthly" => "0 0 1 * *".to_owned(),
            "@yearly" | "@annually" => "0 0 1 1 *".to_owned(),
            other => other.to_owned(),
        };
        let fields: Vec<&str> = rule.split_whitespace().collect();
        let [minute, hour, day, month, weekday] = fields[..] else {
            return Err(format!(
                "{text:?} is not five fields (minute, hour, day of the month, month, day of the \
                 week), such as \"0 9 * * 1-5\" for 09:00 on weekdays"
            ));
        };
        let weekdays = set(weekday, &WEEKDAY)?;
        // Sunday is 0 and 7 alike.
        let weekdays = (weekdays | (weekdays >> 7)) & 0x7f;
        Ok(Self {
            minutes: set(minute, &MINUTE)?,
            hours: u32::try_from(set(hour, &HOUR)?).map_err(|e| e.to_string())?,
            days: u32::try_from(set(day, &DAY)?).map_err(|e| e.to_string())?,
            months: u16::try_from(set(month, &MONTH)?).map_err(|e| e.to_string())?,
            weekdays: u8::try_from(weekdays).map_err(|e| e.to_string())?,
            either: !day.starts_with('*') && !weekday.starts_with('*'),
        })
    }

    /// The first time after `after` the rule names in `zone`, if one comes within eight
    /// years.
    #[must_use]
    pub(crate) fn next(&self, after: Timestamp, zone: &TimeZone) -> Option<Timestamp> {
        let start = zone.to_datetime(after);
        let mut date = start.date();
        for _ in 0..DAYS_AHEAD {
            if self.runs_on(date) {
                for hour in bits(u64::from(self.hours)) {
                    for minute in bits(self.minutes) {
                        let at = DateTime::new(
                            date.year(),
                            date.month(),
                            date.day(),
                            i8::try_from(hour).ok()?,
                            i8::try_from(minute).ok()?,
                            0,
                            0,
                        )
                        .ok()?;
                        if at <= start {
                            continue;
                        }
                        let at = zone.to_ambiguous_timestamp(at).compatible().ok()?;
                        if at > after {
                            return Some(at);
                        }
                    }
                }
            }
            date = date.tomorrow().ok()?;
        }
        None
    }

    /// Whether the rule runs on `date`.
    fn runs_on(&self, date: Date) -> bool {
        let has = |set: u64, at: i8| u32::try_from(at).is_ok_and(|at| (set >> at) & 1 == 1);
        if !has(u64::from(self.months), date.month()) {
            return false;
        }
        let day = has(u64::from(self.days), date.day());
        let weekday = has(u64::from(self.weekdays), date.weekday().to_sunday_zero_offset());
        if self.either { day || weekday } else { day && weekday }
    }
}

/// The time zone named `name`, an IANA name such as `Europe/Berlin`: from the system's
/// database, else from the copy built in.
///
/// # Errors
/// When neither knows it.
pub(crate) fn zone(name: &str) -> Result<TimeZone, String> {
    let name = name.trim();
    jiff::tz::db().get(name).or_else(|_| TimeZoneDatabase::bundled().get(name)).map_err(|e| {
        format!(
            "{name:?} is no time zone this server knows ({e}); name one as Europe/Berlin is named"
        )
    })
}

/// The values set in `set`, least first.
fn bits(set: u64) -> impl Iterator<Item = u32> {
    (0..64).filter(move |at| (set >> at) & 1 == 1)
}

/// The values `text` matches in `field`, a bit each.
fn set(text: &str, field: &Field) -> Result<u64, String> {
    let mut all = 0_u64;
    for item in text.split(',') {
        let (range, step) = match item.split_once('/') {
            Some((range, step)) => {
                let step: u8 = step
                    .parse()
                    .ok()
                    .filter(|s| *s > 0)
                    .ok_or_else(|| format!("{item:?}: a step is a whole number above 0"))?;
                (range, step)
            }
            None => (item, 1),
        };
        let (from, to) = if range == "*" {
            (field.least, field.most)
        } else if let Some((from, to)) = range.split_once('-') {
            (value(from, field)?, value(to, field)?)
        } else {
            let from = value(range, field)?;
            // `5/15` runs from 5 to the end, stepping 15.
            (from, if item.contains('/') { field.most } else { from })
        };
        if from > to {
            return Err(format!("{item:?}: a {} range runs from the lesser", field.name));
        }
        let mut at = from;
        while at <= to {
            all |= 1 << at;
            let Some(next) = at.checked_add(step) else { break };
            at = next;
        }
    }
    Ok(all)
}

/// One value of `field`, as a number or a name.
fn value(text: &str, field: &Field) -> Result<u8, String> {
    let named = field
        .names
        .iter()
        .position(|n| *n == text)
        .and_then(|at| u8::try_from(at).ok().and_then(|at| at.checked_add(field.first_named)));
    named
        .or_else(|| text.parse().ok())
        .filter(|v| (field.least..=field.most).contains(v))
        .ok_or_else(|| format!("{text:?} is no {}: {} to {}", field.name, field.least, field.most))
}

#[cfg(test)]
mod tests {
    use jiff::civil::date;

    use super::*;

    fn at(zone: &TimeZone, d: Date, hour: i8, minute: i8) -> Timestamp {
        zone.to_ambiguous_timestamp(d.at(hour, minute, 0, 0)).compatible().unwrap()
    }

    fn local(zone: &TimeZone, ts: Timestamp) -> DateTime {
        zone.to_datetime(ts)
    }

    /// The bundled database resolves a zone wherever the server runs; a rule runs at its time
    /// in that zone, not the server's, and keeps to it across the change to summer time: a time
    /// the change skips runs at the first moment after it, and one the change back repeats
    /// runs once.
    #[test]
    fn a_rule_runs_in_the_person_s_zone_across_a_change_of_clocks() {
        let berlin = TimeZoneDatabase::bundled().get("Europe/Berlin").unwrap();
        assert_eq!(zone(" Europe/Berlin ").unwrap().iana_name(), Some("Europe/Berlin"));
        assert!(zone("Mars/Olympus").unwrap_err().contains("no time zone"));

        let nine = When::parse("0 9 * * *").unwrap();
        // Saturday 2026-03-28, 10:00 in Berlin: the next 09:00 is Sunday's, after the change.
        let saturday = at(&berlin, date(2026, 3, 28), 10, 0);
        let sunday = nine.next(saturday, &berlin).unwrap();
        assert_eq!(local(&berlin, sunday), date(2026, 3, 29).at(9, 0, 0, 0));
        assert_eq!(sunday.as_second() - saturday.as_second(), 22 * 3600, "an hour shorter");

        let skipped = When::parse("30 2 * * *").unwrap();
        let before = at(&berlin, date(2026, 3, 29), 1, 0);
        let ran = skipped.next(before, &berlin).unwrap();
        assert_eq!(local(&berlin, ran), date(2026, 3, 29).at(3, 30, 0, 0), "past the gap");

        let first = at(&berlin, date(2026, 10, 25), 1, 0);
        let once = skipped.next(first, &berlin).unwrap();
        assert_eq!(local(&berlin, once), date(2026, 10, 25).at(2, 30, 0, 0));
        let again = skipped.next(once, &berlin).unwrap();
        assert_eq!(local(&berlin, again), date(2026, 10, 26).at(2, 30, 0, 0), "once only");

        let utc = TimeZone::UTC;
        assert_eq!(
            local(&utc, nine.next(saturday, &utc).unwrap()),
            date(2026, 3, 29).at(9, 0, 0, 0)
        );
    }

    /// Fields take values, names, ranges, steps and lists; both day fields restricted run on
    /// either; a day that never comes has no next run; and anything else is refused saying
    /// what a rule is.
    #[test]
    fn a_rule_is_read_as_cron_reads_it() {
        let utc = TimeZone::UTC;
        let monday = at(&utc, date(2026, 10, 5), 8, 0);
        let weekdays = When::parse("*/20 9-17 * * mon-fri").unwrap();
        let runs: Vec<DateTime> =
            std::iter::successors(weekdays.next(monday, &utc), |t| weekdays.next(*t, &utc))
                .take(4)
                .map(|t| local(&utc, t))
                .collect();
        let day = date(2026, 10, 5);
        assert_eq!(
            runs,
            [day.at(9, 0, 0, 0), day.at(9, 20, 0, 0), day.at(9, 40, 0, 0), day.at(10, 0, 0, 0)]
        );

        let friday_evening = at(&utc, date(2026, 10, 9), 18, 0);
        let next = local(&utc, weekdays.next(friday_evening, &utc).unwrap());
        assert_eq!(next, date(2026, 10, 12).at(9, 0, 0, 0), "over the weekend");

        // The 1st, or any Sunday.
        let either = When::parse("0 0 1 * 7").unwrap();
        let next = local(&utc, either.next(monday, &utc).unwrap());
        assert_eq!(next, date(2026, 10, 11).at(0, 0, 0, 0));
        let monthly = When::parse("@monthly").unwrap();
        assert_eq!(
            local(&utc, monthly.next(monday, &utc).unwrap()),
            date(2026, 11, 1).at(0, 0, 0, 0)
        );
        let stepped = When::parse("5/30 0 1 jan,jul *").unwrap();
        assert_eq!(
            local(&utc, stepped.next(monday, &utc).unwrap()),
            date(2027, 1, 1).at(0, 5, 0, 0)
        );
        assert_eq!(When::parse("0 0 30 feb *").unwrap().next(monday, &utc), None);

        for (rule, says) in [
            ("0 9 * *", "five fields"),
            ("60 9 * * *", "no minute"),
            ("0 9 * * 8", "no day of the week"),
            ("0 17-9 * * *", "from the lesser"),
            ("*/0 * * * *", "above 0"),
            ("0 9 * smarch *", "no month"),
        ] {
            let refused = When::parse(rule).unwrap_err();
            assert!(refused.contains(says), "{rule}: {refused}");
        }
    }
}

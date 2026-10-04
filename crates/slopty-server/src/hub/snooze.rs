//! The person's snoozes, held here so every client and a restart agree
//! ([`slopty_proto::snooze`]).
//!
//! A preset is worked out in the person's zone when it is set, and kept as a wall time. The
//! delivery loop wakes at the earliest one to let it go, and the ladder lets a thread's go as
//! soon as it has news: a notice for it, whatever it is and wherever it goes.

use std::collections::HashMap;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use slopty_core::WallMs;
use slopty_proto::orchestration::{ErrorCode, Outcome, TermRef};
use slopty_proto::server::FromServer;
use slopty_proto::snooze::{
    EVENING_HOUR, EVENING_LEAD_MIN, MORNING_HOUR, SNOOZES_MAX, Snooze, SnoozeOf, Until,
};
use slopty_proto::thread::attention::{Ladder, Notice, Rung};
use tokio::sync::watch;

use super::{Hub, State, error};

/// Every snooze that holds, by what it hides.
#[derive(Debug, Default)]
pub(super) struct Snoozes {
    held: HashMap<SnoozeOf, Snooze>,
}

impl Snoozes {
    /// The snoozes a store kept, less those over by `now`.
    pub(super) fn restore(kept: Vec<Snooze>, now: WallMs) -> Self {
        Self { held: kept.into_iter().filter(|s| s.holds(now)).map(|s| (s.of, s)).collect() }
    }

    /// Every one, the soonest to end first.
    pub(super) fn list(&self) -> Vec<Snooze> {
        let mut list: Vec<Snooze> = self.held.values().copied().collect();
        list.sort_by_key(|s| (s.until_ms, s.since_ms));
        list
    }

    /// Hold `snooze`, in place of one on the same thing.
    ///
    /// # Errors
    /// [`SNOOZES_MAX`] are held, none of them on the same thing.
    pub(super) fn set(&mut self, snooze: Snooze) -> Result<(), String> {
        if self.held.len() >= SNOOZES_MAX && !self.held.contains_key(&snooze.of) {
            return Err(format!("{SNOOZES_MAX} things are snoozed already; wake one first"));
        }
        self.held.insert(snooze.of, snooze);
        Ok(())
    }

    /// Let `of`'s go; whether one held.
    pub(super) fn end(&mut self, of: SnoozeOf) -> bool {
        self.held.remove(&of).is_some()
    }

    /// Let go of every one over by `now`; whether any was.
    pub(super) fn expire(&mut self, now: WallMs) -> bool {
        let before = self.held.len();
        self.held.retain(|_, s| s.holds(now));
        self.held.len() != before
    }

    /// When the soonest ends.
    pub(super) fn next(&self) -> Option<WallMs> {
        self.held.values().map(|s| s.until_ms).min()
    }

    /// `notice`'s thread has news: let go of its snooze and its tile's; whether any held.
    pub(super) fn news(&mut self, notice: &Notice) -> bool {
        let thread = self.end(SnoozeOf::Thread(notice.thread));
        let tile = notice.tile.map(|session| TermRef { worker: notice.thread.worker, session });
        let tile = tile.is_some_and(|tile| self.end(SnoozeOf::Tile(tile)));
        thread || tile
    }
}

/// When `until` ends, set at `now` by a person in `zone`.
///
/// # Errors
/// "This evening" is less than [`EVENING_LEAD_MIN`] off or past, or a time picked is not
/// ahead.
pub(super) fn until_ms(until: Until, now: WallMs, zone: &TimeZone) -> Result<WallMs, String> {
    let ms = |at: Timestamp| WallMs::from_millis(u64::try_from(at.as_millisecond()).unwrap_or(0));
    let start = i64::try_from(now.as_millis())
        .ok()
        .and_then(|ms| Timestamp::from_millisecond(ms).ok())
        .ok_or_else(|| "the server's clock reads no time".to_owned())?;
    let here = start.to_zoned(zone.clone());
    let at_hour = |date: jiff::civil::Date, hour: i8| {
        date.at(hour, 0, 0, 0).to_zoned(zone.clone()).map(|z| z.timestamp())
    };
    let end = match until {
        Until::InAnHour => start.checked_add(jiff::SignedDuration::from_hours(1)),
        Until::ThisEvening => {
            let evening = at_hour(here.date(), EVENING_HOUR);
            let lead = jiff::SignedDuration::from_mins(EVENING_LEAD_MIN);
            match evening {
                Ok(evening) if evening.duration_since(start) > lead => Ok(evening),
                Ok(_) => return Err("evening is under an hour off; snooze until tomorrow".into()),
                Err(e) => Err(e),
            }
        }
        Until::Tomorrow => here.date().tomorrow().and_then(|d| at_hour(d, MORNING_HOUR)),
        Until::At(at) if at > now => return Ok(at),
        Until::At(_) => return Err("a snooze ends at a time still ahead".to_owned()),
    };
    end.map(ms).map_err(|e| format!("no such time in {}: {e}", zone.iana_name().unwrap_or("UTC")))
}

/// The rung `of` stands on in `ladder`, when the ladder knows it.
fn rung_of(ladder: &Ladder, of: SnoozeOf) -> Option<Rung> {
    match of {
        SnoozeOf::Thread(at) => ladder.rung(at),
        SnoozeOf::Tile(tile) => ladder.tile(tile).map(|(_, standing)| standing.rung),
    }
}

impl Hub {
    /// Every snooze that holds, the soonest to end first.
    #[must_use]
    pub fn snoozes(&self) -> Vec<Snooze> {
        self.inner.state.lock().snoozes.list()
    }

    /// Take up the snoozes a store kept, before any link is served.
    pub fn adopt_snoozes(&self, kept: Vec<Snooze>) {
        let mut state = self.inner.state.lock();
        state.snoozes = Snoozes::restore(kept, WallMs::now());
        self.inner.snoozes_kept.send_replace(state.snoozes.list());
    }

    /// The snoozes as they change, for the store ([`crate::store::SnoozeStore`]).
    #[must_use]
    pub fn snoozes_kept(&self) -> watch::Receiver<Vec<Snooze>> {
        self.inner.snoozes_kept.subscribe()
    }

    /// The person snoozes `of` until `until`, in `zone` (the server's own when none).
    pub(super) fn snooze(&self, of: SnoozeOf, until: Until, zone: Option<&str>) -> Outcome {
        let zone = match zone.map(str::trim).filter(|z| !z.is_empty()) {
            Some(name) => match crate::project::when::zone(name) {
                Ok(zone) => zone,
                Err(why) => return error(ErrorCode::Invalid, &why),
            },
            None => TimeZone::system(),
        };
        let now = WallMs::now();
        let until_ms = match until_ms(until, now, &zone) {
            Ok(at) => at,
            Err(why) => return error(ErrorCode::Invalid, &why),
        };
        let mut guard = self.inner.state.lock();
        let state = &mut *guard;
        if rung_of(state.board.published(), of) == Some(Rung::NeedsYou) {
            drop(guard);
            return error(
                ErrorCode::Conflict,
                "it waits on you, so it stays where you see it until you answer",
            );
        }
        let snooze = Snooze { of, until_ms, since_ms: now };
        if let Err(why) = state.snoozes.set(snooze) {
            drop(guard);
            return error(ErrorCode::Invalid, &why);
        }
        self.snoozes_moved(state);
        drop(guard);
        Outcome::Snoozed(snooze)
    }

    /// The person ends `of`'s snooze, if one holds.
    pub(super) fn unsnooze(&self, of: SnoozeOf) -> Outcome {
        let mut guard = self.inner.state.lock();
        let state = &mut *guard;
        if state.snoozes.end(of) {
            self.snoozes_moved(state);
        }
        drop(guard);
        Outcome::Done
    }

    /// Let go of the snoozes over by `wall`, and say when the next ends, by `now`'s clock.
    pub(super) fn snoozes_due(
        &self,
        state: &mut State,
        wall: WallMs,
        now: tokio::time::Instant,
    ) -> Option<tokio::time::Instant> {
        if state.snoozes.expire(wall) {
            self.snoozes_moved(state);
        }
        let next = state.snoozes.next()?;
        now.checked_add(std::time::Duration::from_millis(next.millis_since(wall)))
    }

    /// The ladder's `notices` are news of their threads: their snoozes end.
    pub(super) fn snoozes_heard(&self, state: &mut State, notices: &[Notice]) {
        let mut moved = false;
        for notice in notices {
            moved |= state.snoozes.news(notice);
        }
        if moved {
            self.snoozes_moved(state);
        }
    }

    /// Tell every client and the store, and wake the delivery loop for the soonest end.
    fn snoozes_moved(&self, state: &State) {
        let list = state.snoozes.list();
        self.inner.snoozes_kept.send_replace(list.clone());
        self.announce(FromServer::Snoozes(list));
        self.inner.deliver.notify_one();
    }
}

#[cfg(test)]
mod tests;

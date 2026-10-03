//! Each machine's plan windows: a subscription's five-hour and seven-day limits.
//!
//! The agents publish them about themselves: Claude Code through its status line, Codex through
//! its `account/rateLimits`. Every thread row a worker's table sends carries its agent's windows,
//! and the freshest reading per machine and agent is kept here for the status bar. Nothing is
//! asked of anyone to fill it, and an agent that publishes none (an API key) has none.

use std::collections::BTreeMap;
use std::time::Duration;

use slopty_core::WallMs;
use slopty_proto::thread::Limit;

use crate::layout::WorkerKey;

/// One agent's windows on one machine, and when they were last heard.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Reading {
    /// Its windows, as its agent named them.
    pub limits: Vec<Limit>,
    /// When it was last heard, by the worker's clock.
    pub heard: WallMs,
}

impl Reading {
    /// Its windows that still hold at `now`: one past its reset says nothing of the window
    /// that runs now.
    pub fn current(&self, now: WallMs) -> impl Iterator<Item = &Limit> {
        self.limits.iter().filter(move |l| l.resets_ms.is_none_or(|at| at > now))
    }

    /// How old it is at `now`.
    #[must_use]
    pub const fn age(&self, now: WallMs) -> Duration {
        Duration::from_millis(now.as_millis().saturating_sub(self.heard.as_millis()))
    }
}

/// The freshest reading per machine and agent.
#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub struct PlanMeters {
    readings: BTreeMap<(WorkerKey, String), Reading>,
}

impl PlanMeters {
    /// A thread row of `agent` on `worker` said `limits` at `at`. An empty list says nothing,
    /// and a reading older than the one kept is left.
    pub fn hear(&mut self, worker: WorkerKey, agent: &str, limits: &[Limit], at: WallMs) {
        if limits.is_empty() {
            return;
        }
        let kept = self
            .readings
            .entry((worker, agent.to_owned()))
            .or_insert_with(|| Reading { limits: limits.to_vec(), heard: at });
        if at >= kept.heard {
            kept.limits = limits.to_vec();
            kept.heard = at;
        }
    }

    /// `worker` is gone: its readings go with it.
    pub fn forget(&mut self, worker: WorkerKey) {
        self.readings.retain(|(w, _), _| *w != worker);
    }

    /// What the bar shows for `worker`: `agent`'s reading when it has one with a window still
    /// holding at `now`, else the freshest such reading on that machine.
    #[must_use]
    pub fn shown(
        &self,
        worker: WorkerKey,
        agent: Option<&str>,
        now: WallMs,
    ) -> Option<(&str, &Reading)> {
        let live = |(_, r): &(&(WorkerKey, String), &Reading)| r.current(now).next().is_some();
        let on: Vec<(&(WorkerKey, String), &Reading)> =
            self.readings.iter().filter(|((w, _), _)| *w == worker).filter(live).collect();
        let own = agent.and_then(|a| on.iter().find(|((_, name), _)| name == a));
        own.or_else(|| on.iter().max_by_key(|(_, r)| r.heard))
            .map(|((_, name), reading)| (name.as_str(), *reading))
    }

    /// Every reading with a window still holding at `now`, by machine then agent.
    pub fn all(&self, now: WallMs) -> impl Iterator<Item = (WorkerKey, &str, &Reading)> {
        self.readings
            .iter()
            .filter(move |(_, r)| r.current(now).next().is_some())
            .map(|((w, a), r)| (*w, a.as_str(), r))
    }
}

/// A window's name as a readout says it: `five-hour` is `5h`, `seven-day` `7d`, `90-minute`
/// `90m`; a name of any other shape as it is.
#[must_use]
pub fn short_name(name: &str) -> String {
    let words = [
        ("one", 1),
        ("two", 2),
        ("three", 3),
        ("four", 4),
        ("five", 5),
        ("six", 6),
        ("seven", 7),
        ("eight", 8),
        ("nine", 9),
        ("ten", 10),
    ];
    let Some((count, unit)) = name.split_once('-') else { return name.to_owned() };
    let count = count
        .parse::<u32>()
        .ok()
        .or_else(|| words.iter().find(|(w, _)| *w == count).map(|(_, n)| *n));
    let unit = match unit {
        "minute" => "m",
        "hour" => "h",
        "day" => "d",
        "week" => "w",
        _ => return name.to_owned(),
    };
    count.map_or_else(|| name.to_owned(), |n| format!("{n}{unit}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limit(name: &str, used_bp: u32, resets: Option<u64>) -> Limit {
        Limit { name: name.to_owned(), used_bp, resets_ms: resets.map(WallMs::from_millis) }
    }

    /// A window past its reset says nothing of the one that runs now: it is dropped, and a
    /// reading left with no window is not shown at all.
    #[test]
    fn a_reading_past_its_reset_is_dropped() {
        let studio = WorkerKey::new(1);
        let mut meters = PlanMeters::default();
        let limits =
            [limit("five-hour", 2_300, Some(1_000)), limit("seven-day", 4_100, Some(9_000))];
        meters.hear(studio, "claude-code", &limits, WallMs::from_millis(500));
        let (_, reading) = meters.shown(studio, None, WallMs::from_millis(2_000)).expect("shown");
        let names: Vec<&str> =
            reading.current(WallMs::from_millis(2_000)).map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["seven-day"], "the five-hour window reset");
        assert!(meters.shown(studio, None, WallMs::from_millis(10_000)).is_none(), "both reset");
        assert_eq!(meters.all(WallMs::from_millis(10_000)).count(), 0);
    }

    /// No reading, no meter: an agent that publishes no windows (an API key) shows none, and a
    /// machine gone takes its readings with it.
    #[test]
    fn no_reading_shows_no_meter() {
        let studio = WorkerKey::new(1);
        let mut meters = PlanMeters::default();
        meters.hear(studio, "claude-code", &[], WallMs::from_millis(1));
        assert!(meters.shown(studio, None, WallMs::from_millis(2)).is_none());
        meters.hear(studio, "codex", &[limit("five-hour", 100, None)], WallMs::from_millis(1));
        assert!(meters.shown(studio, None, WallMs::from_millis(2)).is_some());
        meters.forget(studio);
        assert!(meters.shown(studio, None, WallMs::from_millis(2)).is_none());
    }

    /// The focused agent's reading is shown where it has one, else the freshest on that machine;
    /// an older word never replaces a newer one.
    #[test]
    fn the_focused_agents_reading_leads_else_the_freshest() {
        let studio = WorkerKey::new(1);
        let mut meters = PlanMeters::default();
        meters.hear(
            studio,
            "claude-code",
            &[limit("five-hour", 100, None)],
            WallMs::from_millis(5),
        );
        meters.hear(studio, "codex", &[limit("five-hour", 200, None)], WallMs::from_millis(9));
        let now = WallMs::from_millis(10);
        assert_eq!(
            meters.shown(studio, Some("claude-code"), now).map(|s| s.0),
            Some("claude-code")
        );
        assert_eq!(meters.shown(studio, Some("pi"), now).map(|s| s.0), Some("codex"));
        assert_eq!(meters.shown(studio, None, now).map(|s| s.0), Some("codex"));
        meters.hear(studio, "codex", &[limit("five-hour", 900, None)], WallMs::from_millis(3));
        let (_, kept) = meters.shown(studio, Some("codex"), now).expect("kept");
        assert_eq!(kept.limits[0].used_bp, 200, "the older word is left");
        assert!(meters.shown(WorkerKey::new(2), None, now).is_none(), "another machine has none");
    }

    #[test]
    fn a_windows_name_is_said_short() {
        assert_eq!(short_name("five-hour"), "5h");
        assert_eq!(short_name("seven-day"), "7d");
        assert_eq!(short_name("90-minute"), "90m");
        assert_eq!(short_name("2-week"), "2w");
        assert_eq!(short_name("window"), "window");
        assert_eq!(short_name("many-hour"), "many-hour");
    }
}

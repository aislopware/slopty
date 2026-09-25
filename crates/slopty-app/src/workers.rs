//! The workers this app reaches, from the server's directory or added by address: one direct
//! link each, all of them feeding one workspace.

use std::time::{Duration, Instant};

use slopty_client::WorkerLink;
use slopty_client::layout::WorkerKey;
use slopty_core::WorkerId;
use slopty_ui::workspace::WorkerStatus;

/// GPUI actions for the workers.
pub mod actions {
    #![expect(
        clippy::derive_partial_eq_without_eq,
        reason = "gpui::actions! derives PartialEq only"
    )]
    use gpui::actions;

    actions!(
        workers,
        [
            /// Open the panel that adds a worker by address.
            AddWorker,
            /// Open the panel that connects to a server, whose directory lists the workers.
            ConnectServer,
            /// Stop using the server: its workers leave, the ones added by address stay.
            DisconnectServer,
        ]
    );
}

/// The workspace's key for a worker: its id's 128 bits.
#[must_use]
pub const fn worker_key(id: WorkerId) -> WorkerKey {
    WorkerKey::new(id.as_uuid().as_u128())
}

/// One worker as the app keeps it, beside what the workspace keeps.
pub struct WorkerSlot {
    /// Its identity (the directory's and the known-workers store's key).
    pub id: WorkerId,
    /// It was added by address, so it stays without the server.
    pub added: bool,
    /// Wakes its connect loop out of a wait: it came back online, or the server went away and
    /// its cached address is worth a try. While a link is up it wakes the link's [`Hearing`]
    /// check instead, which gives up a silent link at once.
    pub wake: std::sync::Arc<tokio::sync::Notify>,
    /// The workspace's key for it.
    pub key: WorkerKey,
    /// Display name (from the store, refreshed by each `HelloAck`).
    pub name: String,
    /// The live link, to abandon it when the worker is forgotten.
    pub link: Option<std::sync::Weak<WorkerLink>>,
}

impl std::fmt::Debug for WorkerSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerSlot")
            .field("id", &self.id)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl WorkerSlot {
    /// A slot for a worker, before its first connection attempt.
    #[must_use]
    pub fn new(id: WorkerId, name: String, added: bool) -> Self {
        let wake = std::sync::Arc::default();
        Self { id, added, wake, key: worker_key(id), name, link: None }
    }

    /// Cut a wait short.
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    /// The direct link is up.
    #[must_use]
    pub fn linked(&self) -> bool {
        self.link.as_ref().is_some_and(|l| l.strong_count() > 0)
    }
}

/// Nothing heard from a worker for this long shows as silent: three keep-alives missed.
pub const SILENCE_WARN: Duration = slopty_net::endpoint::KEEP_ALIVE.saturating_mul(3);
/// Nothing heard for this long and the link is given up, so the redial takes over instead of
/// the transport's idle timeout: five keep-alives missed.
pub const SILENCE_DROP: Duration = slopty_net::endpoint::KEEP_ALIVE.saturating_mul(5);
/// How often a link's liveness and RTT are sampled: twice a keep-alive, so a bar is crossed
/// within half a second of the silence reaching it.
pub const HEARING_TICK: Duration = match slopty_net::endpoint::KEEP_ALIVE.checked_div(2) {
    Some(half) => half,
    None => Duration::ZERO,
};

/// Whether a worker is heard, from its link's count of received datagrams sampled over time.
///
/// Keep-alives move the count every [`slopty_net::endpoint::KEEP_ALIVE`] on an idle link, so a
/// count that stands still is the worker gone or the path cut, long before the transport's
/// idle timeout says so.
#[derive(Clone, Copy, Debug)]
pub struct Hearing {
    count: u64,
    at: Instant,
}

/// What a [`Hearing`] sample says.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Heard {
    /// Something arrived within [`SILENCE_WARN`].
    Live,
    /// Nothing for this long, past [`SILENCE_WARN`].
    Silent(Duration),
    /// Nothing for this long, past [`SILENCE_DROP`]: give the link up.
    Lost(Duration),
}

impl Hearing {
    /// A link that came up at `now`.
    #[must_use]
    pub const fn new(now: Instant) -> Self {
        Self { count: 0, at: now }
    }

    /// The link had received `count` datagrams at `now`.
    pub fn sample(&mut self, count: u64, now: Instant) -> Heard {
        if count != self.count {
            self.count = count;
            self.at = now;
        }
        let gap = now.saturating_duration_since(self.at);
        if gap >= SILENCE_DROP {
            Heard::Lost(gap)
        } else if gap >= SILENCE_WARN {
            Heard::Silent(gap)
        } else {
            Heard::Live
        }
    }
}

/// What a link's tick does with what it heard.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Tick {
    /// Show this.
    Show(WorkerStatus),
    /// Ping the worker, since a restarted one answers with a reset that ends the link, and show
    /// this.
    Ping(WorkerStatus),
    /// Give the link up. `at_once`: redial now rather than after the backoff.
    GiveUp {
        /// Redial now.
        at_once: bool,
    },
}

impl Heard {
    /// What the tick does. `woken`: the server says the worker is back online or moved, so a
    /// link that is silent then is the dead one.
    #[must_use]
    pub const fn tick(self, woken: bool) -> Tick {
        match self {
            Self::Live => Tick::Show(WorkerStatus::Connected),
            Self::Silent(_) | Self::Lost(_) if woken => Tick::GiveUp { at_once: true },
            Self::Silent(gap) => Tick::Ping(WorkerStatus::Silent(gap.as_secs())),
            Self::Lost(_) => Tick::GiveUp { at_once: false },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_silence_bars_are_three_and_five_keep_alives() {
        assert_eq!(SILENCE_WARN, Duration::from_secs(3));
        assert_eq!(SILENCE_DROP, Duration::from_secs(5));
        assert_eq!(HEARING_TICK, Duration::from_millis(500));
    }

    #[test]
    fn a_link_is_heard_while_keep_alives_arrive_and_lost_five_seconds_after_they_stop() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let mut hearing = Hearing::new(start);
        // A keep-alive and its ACK a second apart, sampled every half second: always live.
        for tick in 1..=20_u64 {
            assert_eq!(hearing.sample(tick / 2, at(tick * 500)), Heard::Live, "tick {tick}");
        }
        // The worker dies after the last count, heard at 10 s.
        assert_eq!(hearing.sample(10, at(12_500)), Heard::Live);
        assert_eq!(hearing.sample(10, at(13_000)), Heard::Silent(Duration::from_secs(3)));
        assert_eq!(hearing.sample(10, at(14_500)), Heard::Silent(Duration::from_millis(4_500)));
        assert_eq!(hearing.sample(10, at(15_000)), Heard::Lost(Duration::from_secs(5)));
        // Anything at all that arrives makes it live again.
        assert_eq!(hearing.sample(11, at(15_500)), Heard::Live);
    }

    #[test]
    fn a_link_that_never_hears_anything_is_lost_from_when_it_came_up() {
        let start = Instant::now();
        let mut hearing = Hearing::new(start);
        assert_eq!(hearing.sample(0, start + SILENCE_WARN), Heard::Silent(SILENCE_WARN));
        assert_eq!(hearing.sample(0, start + SILENCE_DROP), Heard::Lost(SILENCE_DROP));
    }

    #[test]
    fn a_silent_link_pings_and_is_given_up_at_once_when_the_server_says_the_worker_is_back() {
        let silent = Heard::Silent(Duration::from_millis(3_500));
        assert_eq!(Heard::Live.tick(false), Tick::Show(WorkerStatus::Connected));
        assert_eq!(
            Heard::Live.tick(true),
            Tick::Show(WorkerStatus::Connected),
            "a live link stays"
        );
        assert_eq!(silent.tick(false), Tick::Ping(WorkerStatus::Silent(3)));
        assert_eq!(silent.tick(true), Tick::GiveUp { at_once: true });
        let lost = Heard::Lost(SILENCE_DROP);
        assert_eq!(lost.tick(false), Tick::GiveUp { at_once: false });
        assert_eq!(lost.tick(true), Tick::GiveUp { at_once: true });
    }
}

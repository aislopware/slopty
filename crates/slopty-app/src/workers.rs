//! The workers this app reaches, as the server's directory lists them: one direct link each, all
//! of them feeding one workspace.

use std::time::{Duration, Instant};

use slopty_client::{WorkerKey, WorkerLink};
use slopty_core::WorkerId;
use slopty_net::liveness::{Beat, Pulse};
use slopty_platform::resume::Resume;
use slopty_ui::workspace::WorkerStatus;
/// The workspace's key for a worker: its id's 128 bits.
pub use slopty_ui::workspace::worker_key;

/// One worker as the app keeps it, beside what the workspace keeps.
pub struct WorkerSlot {
    /// Its identity (the directory's key).
    pub id: WorkerId,
    /// Wakes its connect loop out of a wait: it came back online, or the server went away and
    /// its cached address is worth a try. While a link is up it wakes the link's ping
    /// supervisor instead, which pings at once and gives up a silent link.
    pub wake: std::sync::Arc<tokio::sync::Notify>,
    /// The workspace's key for it.
    pub key: WorkerKey,
    /// Display name (from the directory, refreshed by each `HelloAck`).
    pub name: String,
    /// The live link, to abandon it when the worker is forgotten.
    pub link: Option<std::sync::Weak<WorkerLink>>,
    /// Hands a resume to the live link's check, which probes it at once ([`Probe`]).
    pub resume: Option<tokio::sync::mpsc::UnboundedSender<Resume>>,
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
    pub fn new(id: WorkerId, name: String) -> Self {
        let wake = std::sync::Arc::default();
        Self { id, wake, key: worker_key(id), name, link: None, resume: None }
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

    /// Something may have killed its link ([`Resume`]): the live link's check probes it now,
    /// and a worker between links is dialled now rather than at the end of its backoff.
    pub fn resume(&self, resume: Resume) {
        // The check drops its end with its link, so a send that fails is a worker between links.
        let probed = self.resume.as_ref().is_some_and(|tx| tx.send(resume).is_ok());
        if !probed {
            self.wake();
        }
    }
}

/// The least a probe waits for its answer: a path a few milliseconds long answers in a
/// fraction of it, and a radio just back from sleep in less.
pub const PROBE_FLOOR: Duration = Duration::from_millis(250);
/// The most a probe waits: past this a link is as good as dead, and a new one is cheaper.
pub const PROBE_CEILING: Duration = Duration::from_secs(1);
/// How long a probe runs before a link that was not in doubt shows it: one frame.
pub const DOUBT_GRACE: Duration = Duration::from_millis(16);
/// How often a probe looks for the answer.
pub const PROBE_POLL: Duration = Duration::from_millis(2);

/// A probe of a live link after a [`Resume`], sized from the link's last round trip.
///
/// A QUIC PING goes out, and the link counts as alive the moment anything arrives from the
/// worker. How long it waits, and when the views show the doubt, come from the round trip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Probe {
    /// Past this with nothing heard, the link is given up and dialled again at once.
    pub deadline: Duration,
    /// When the views are set back: at once when the device was away or the path moved, else
    /// once the probe has run longer than a healthy link takes to answer.
    pub doubt_after: Duration,
}

impl Probe {
    /// The probe for `resume` over a link whose round trip was last `rtt`.
    #[must_use]
    pub fn new(resume: Resume, rtt: Option<Duration>) -> Self {
        let rtt = rtt.unwrap_or(PROBE_FLOOR);
        let deadline = rtt.saturating_mul(4).clamp(PROBE_FLOOR, PROBE_CEILING);
        let doubt_after = if resume.was_away() || resume.moved() {
            Duration::ZERO
        } else {
            rtt.saturating_mul(2).max(DOUBT_GRACE)
        };
        Self { deadline, doubt_after }
    }
}

/// A link came up for worker `id`: its slot takes the link and the answered name.
///
/// The workspace's key for it comes back, or `None` when the slot went while the dial was in
/// flight (the worker forgotten, or no longer listed): the link is to be dropped, not shown.
pub fn adopt(
    slots: &mut [WorkerSlot],
    id: WorkerId,
    link: std::sync::Weak<WorkerLink>,
    name: String,
) -> Option<WorkerKey> {
    let slot = slots.iter_mut().find(|w| w.id == id)?;
    slot.link = Some(link);
    slot.name = name;
    Some(slot.key)
}

/// One worker link's supervisor (`slopty_net::liveness`).
///
/// The link is alive while its control stream answers the pings it sends, whatever the
/// datagrams beside it say. It pings the stream every two seconds, shows the link silent once
/// one ping went unanswered, and relinks it once two did: six seconds after the last answer.
#[derive(Clone, Copy, Debug)]
pub struct Supervisor {
    beat: Beat,
    pongs: u64,
}

/// What a [`Supervisor`]'s look says to do.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Look {
    /// Ping the control stream now ([`slopty_client::WorkerLink::ping_stream`]).
    pub ping_stream: bool,
    /// And then this.
    pub tick: Tick,
}

/// What a link's look does with what it heard.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Tick {
    /// Show this.
    Show(WorkerStatus),
    /// Ping the connection too, since a restarted worker answers with a reset that ends the
    /// link, and show this.
    Ping(WorkerStatus),
    /// Give the link up and dial again at once, its views kept, set back, until the new link
    /// lands ([`WorkerStatus::Relinking`]).
    Relink,
}

impl Supervisor {
    /// A link that came up at `now`, its control stream answered `pongs` times so far.
    #[must_use]
    pub const fn new(now: Instant, pongs: u64) -> Self {
        Self { beat: Beat::new(now), pongs }
    }

    /// Look at the link at `now`, its stream answered `pongs` times so far. `woken`: the server
    /// says the worker is back online or moved, so a link in doubt then is the dead one.
    pub fn look(&mut self, pongs: u64, woken: bool, now: Instant) -> Look {
        if pongs != self.pongs {
            self.pongs = pongs;
            self.beat.heard(now);
        }
        let tick = match self.beat.pulse(now) {
            Pulse::Live => Tick::Show(WorkerStatus::Connected),
            Pulse::Silent(_) | Pulse::Dead(_) if woken => Tick::Relink,
            Pulse::Silent(gap) => Tick::Ping(WorkerStatus::Silent(gap.as_secs())),
            Pulse::Dead(_) => Tick::Relink,
        };
        let ping_stream = tick != Tick::Relink && self.beat.ping_due(now);
        Look { ping_stream, tick }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A probe waits four round trips, never less than a quarter second nor more than one; the
    /// views show the doubt at once after the device was away or the path moved, and otherwise
    /// only once the probe outlasts what a healthy link takes.
    #[test]
    fn a_probe_is_sized_from_the_round_trip() {
        let lan = Some(Duration::from_millis(3));
        let derp = Some(Duration::from_millis(120));
        let bad = Some(Duration::from_millis(900));
        assert_eq!(Probe::new(Resume::Woke, lan).deadline, PROBE_FLOOR);
        assert_eq!(Probe::new(Resume::Woke, derp).deadline, Duration::from_millis(480));
        assert_eq!(Probe::new(Resume::Woke, bad).deadline, PROBE_CEILING);
        assert_eq!(Probe::new(Resume::Woke, None).deadline, PROBE_CEILING);
        for away in [Resume::Woke, Resume::ScreensWoke, Resume::Unlocked, Resume::PathChanged] {
            assert_eq!(Probe::new(away, lan).doubt_after, Duration::ZERO, "{away:?}");
        }
        assert_eq!(Probe::new(Resume::Foreground, lan).doubt_after, DOUBT_GRACE);
        assert_eq!(Probe::new(Resume::Foreground, derp).doubt_after, Duration::from_millis(240));
    }

    /// A resume goes to a live link's check; a worker without one is dialled now.
    #[test]
    fn a_resume_probes_a_live_link_and_dials_a_dead_one() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut slot = WorkerSlot::new(WorkerId::new(), "studio".to_owned());
        slot.resume = Some(tx);
        slot.resume(Resume::Woke);
        assert_eq!(rx.try_recv().ok(), Some(Resume::Woke), "the live link probes");
        drop(rx);
        slot.resume(Resume::Woke);
        let woke = slot.wake.notified();
        tokio::pin!(woke);
        assert!(woke.as_mut().enable(), "its check is gone with its link: dialled now");
    }

    /// A worker forgotten while its dial was in flight gets no link and no key, so the
    /// workspace shows nothing for it; one still there takes the link and its answered name.
    #[test]
    fn a_link_is_adopted_only_by_a_slot_that_is_still_there() {
        let (kept, gone) = (WorkerId::new(), WorkerId::new());
        let mut slots = vec![WorkerSlot::new(kept, "stored".to_owned())];
        assert_eq!(adopt(&mut slots, gone, std::sync::Weak::new(), "ghost".to_owned()), None);
        assert_eq!(slots.len(), 1);
        assert!(slots[0].link.is_none(), "the other slot is untouched");
        let key = adopt(&mut slots, kept, std::sync::Weak::new(), "studio".to_owned());
        assert_eq!(key, Some(worker_key(kept)));
        assert_eq!(slots[0].name, "studio");
        assert!(slots[0].link.is_some());
    }

    /// Looked at every half second, a link whose stream answers each ping stays connected;
    /// one whose stream stops answering (its datagrams may still flow) shows silent after one
    /// missed ping and is relinked six seconds after its last answer.
    #[test]
    fn a_link_lives_by_its_pongs_and_is_relinked_six_seconds_after_the_last() {
        use slopty_net::liveness::{LOOK_EVERY, PING_EVERY, PONG_WITHIN, SILENT_AFTER};
        let start = Instant::now();
        let mut sup = Supervisor::new(start, 0);
        let first = sup.look(0, false, start);
        assert_eq!(first, Look { ping_stream: true, tick: Tick::Show(WorkerStatus::Connected) });
        let (mut now, mut pongs, mut pings) = (start, 0, 1_u64);
        for _ in 0..40 {
            now += LOOK_EVERY;
            // Every ping answered by the next look.
            pongs = pings;
            let look = sup.look(pongs, false, now);
            assert_eq!(look.tick, Tick::Show(WorkerStatus::Connected));
            pings += u64::from(look.ping_stream);
        }
        assert_eq!(pings, 11, "one ping every two seconds over twenty");
        let stalled = now;
        let mut silent = None;
        let relinked = loop {
            now += LOOK_EVERY;
            let look = sup.look(pongs, false, now);
            match look.tick {
                Tick::Relink => break now.duration_since(stalled),
                Tick::Ping(WorkerStatus::Silent(_)) => {
                    silent.get_or_insert_with(|| now.duration_since(stalled));
                }
                Tick::Show(_) | Tick::Ping(_) => {}
            }
        };
        // The stream stopped somewhere between two pings: counted from its last answer, it
        // shows silent four seconds on and is relinked six seconds on.
        let silent = silent.expect("silent before it is relinked");
        assert!(
            silent >= SILENT_AFTER.saturating_sub(PING_EVERY) && silent <= SILENT_AFTER,
            "{silent:?}"
        );
        assert!(
            relinked >= PONG_WITHIN.saturating_sub(PING_EVERY) && relinked <= PONG_WITHIN,
            "{relinked:?}"
        );
        assert_eq!(relinked.saturating_sub(silent), PONG_WITHIN.saturating_sub(SILENT_AFTER));
    }

    /// A link in doubt is relinked at once when the server says the worker is back or moved;
    /// a live one stays.
    #[test]
    fn a_link_in_doubt_is_relinked_at_once_when_the_server_says_the_worker_is_back() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let mut sup = Supervisor::new(start, 0);
        assert_eq!(sup.look(0, true, at(500)).tick, Tick::Show(WorkerStatus::Connected));
        for ms in [1_000, 1_500, 2_000, 2_500, 3_000, 3_500] {
            let _looked = sup.look(0, false, at(ms));
        }
        assert_eq!(sup.look(0, false, at(4_000)).tick, Tick::Ping(WorkerStatus::Silent(4)));
        assert_eq!(sup.look(0, true, at(4_500)).tick, Tick::Relink);
        assert_eq!(sup.look(1, false, at(5_000)).tick, Tick::Show(WorkerStatus::Connected));
    }

    /// An app that was away (the Mac asleep, the app suspended in the background on iOS) does
    /// not relink a link for the time it was away: it pings at once and waits for the answer.
    #[test]
    fn an_app_back_from_away_pings_before_it_judges() {
        let start = Instant::now();
        let mut sup = Supervisor::new(start, 0);
        let _looked = sup.look(0, false, start);
        let back = start + Duration::from_secs(600);
        let look = sup.look(0, false, back);
        assert_eq!(look, Look { ping_stream: true, tick: Tick::Show(WorkerStatus::Connected) });
    }
}

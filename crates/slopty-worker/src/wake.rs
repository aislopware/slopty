//! Sleep policy for the worker.
//!
//! The machine stays awake while a client is attached or an agent is working, and the display
//! stays on while a window or display is being streamed. A worker that idles to sleep drops
//! every session and stalls every agent mid-turn; a display that sleeps captures nothing.
//!
//! An agent keeps the machine up with nobody connected (a phone pocketed, a laptop lid closed)
//! only while it shows signs of work ([`Quiet`]). During a turn that is its terminal: Claude Code
//! repaints its spinner and timer throughout, so [`SILENT_CAP`] of silence means it hung or its
//! end was never heard. A turn paused on background work is quiet on screen, so there it is
//! also the processor time of the agent's descendant processes, the commands it left running:
//! a build computes, a dev server waiting for requests does not. A paused turn holds the
//! machine for [`PAUSED_CEILING`] at most, whatever it does, since a watcher that polls never
//! ends. Otherwise an unattended worker sleeps as its owner set it to. The person may narrow it
//! ([`Policy`], `[worker] keep_awake`): to attached clients only, or to nothing at all. The
//! policy is pure and counts; the assertions behind [`Holds`] are the daemon's (`NSProcessInfo`
//! activities through `slopty_platform::Activity`).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use slopty_core::SessionId;

/// How long a working agent may show no sign of work (its terminal printing, or for a paused
/// turn its commands computing) before it stops keeping the machine awake.
pub const SILENT_CAP: Duration = Duration::from_mins(15);

/// How long a turn paused on background work holds the machine at most. A build or a test run
/// is done well within it; a watcher that polls would otherwise hold the machine for good.
pub const PAUSED_CEILING: Duration = Duration::from_hours(2);

/// The two operating-system assertions the policy toggles.
pub trait Holds: Send {
    /// Hold (or release) the machine out of idle sleep.
    fn system(&mut self, hold: bool);
    /// Hold (or release) the display out of idle sleep.
    fn display(&mut self, hold: bool);
}

/// What the person lets keep the machine awake (`[worker] keep_awake`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Policy {
    /// A client attached, or an agent at work with nobody attached; a live stream's display.
    #[default]
    Working,
    /// A client attached only; a live stream's display.
    Attached,
    /// Nothing: the machine and its displays sleep as their owner set them to.
    Never,
}

/// Attached clients, working agents and live streams, and the holds they imply.
pub struct Wake<H> {
    clients: usize,
    streams: usize,
    agents: usize,
    system: bool,
    display: bool,
    policy: Policy,
    holds: H,
}

impl<H> std::fmt::Debug for Wake<H> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Wake")
            .field("clients", &self.clients)
            .field("streams", &self.streams)
            .field("agents", &self.agents)
            .finish_non_exhaustive()
    }
}

impl<H: Holds> Wake<H> {
    /// Nobody attached, nothing held, under [`Policy::Working`].
    pub const fn new(holds: H) -> Self {
        Self {
            clients: 0,
            streams: 0,
            agents: 0,
            system: false,
            display: false,
            policy: Policy::Working,
            holds,
        }
    }

    /// Hold only what `policy` lets hold.
    #[must_use]
    pub fn keeping(self, policy: Policy) -> Self {
        Self { policy, ..self }
    }

    /// A client connected: the first one holds the machine awake.
    pub fn client_joined(&mut self) {
        self.clients = self.clients.saturating_add(1);
        self.hold_system();
    }

    /// A client left: the last one lets the machine sleep again, unless an agent works.
    pub fn client_left(&mut self) {
        self.clients = self.clients.saturating_sub(1);
        self.hold_system();
    }

    /// How many agents are working now, not silent past [`SILENT_CAP`] ([`Quiet`]): the
    /// machine is held while it is not zero, as while a client is attached.
    pub fn agents(&mut self, working: usize) {
        self.agents = working;
        self.hold_system();
    }

    /// Hold the machine exactly while a client is attached or an agent works, as far as the
    /// policy lets them; only a change reaches the assertion.
    fn hold_system(&mut self) {
        let hold = match self.policy {
            Policy::Working => self.clients > 0 || self.agents > 0,
            Policy::Attached => self.clients > 0,
            Policy::Never => false,
        };
        if hold != self.system {
            self.system = hold;
            self.holds.system(hold);
        }
    }

    /// How many streams are live now; the display is held while it is not zero, unless the
    /// policy holds nothing.
    pub fn streams(&mut self, live: usize) {
        self.streams = live;
        let hold = live > 0 && self.policy != Policy::Never;
        if hold != self.display {
            self.display = hold;
            self.holds.display(hold);
        }
    }

    /// Whether working agents can hold the machine under the policy: when not, nobody need
    /// sample them.
    #[must_use]
    pub fn counts_agents(&self) -> bool {
        self.policy == Policy::Working
    }

    /// `(clients, streams)` right now.
    #[must_use]
    pub const fn counts(&self) -> (usize, usize) {
        (self.clients, self.streams)
    }

    /// What is held and why.
    #[must_use]
    pub const fn awake(&self) -> slopty_proto::ctl::Awake {
        slopty_proto::ctl::Awake {
            clients: self.clients,
            streams: self.streams,
            agents: self.agents,
            system: self.system,
            display: self.display,
        }
    }
}

/// One working agent's signs of work, sampled on a tick.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Signs {
    /// Its session.
    pub session: SessionId,
    /// Its terminal's output count (`session::Activity::output`).
    pub output: u64,
    /// For a turn paused on background work: how long it has been paused, and the processor
    /// time its descendant processes have used (`ports::descendants_cpu`).
    pub paused: Option<(Duration, u64)>,
}

/// Which working agents showed signs of work lately.
#[derive(Debug, Default)]
pub struct Quiet {
    /// Per working agent's session: its signs as last seen, and when they last moved.
    seen: HashMap<SessionId, ((u64, u64), Instant)>,
}

impl Quiet {
    /// The agents working now, at `now`: how many showed a sign of work within [`SILENT_CAP`]
    /// and, when paused, have been for less than [`PAUSED_CEILING`]. An agent that starts
    /// working counts as having just moved, and one that stops is forgotten.
    pub fn working(&mut self, agents: &[Signs], now: Instant) -> usize {
        self.seen.retain(|session, _seen| agents.iter().any(|a| a.session == *session));
        agents
            .iter()
            .filter(|agent| {
                let signs = (agent.output, agent.paused.map_or(0, |(_for, cpu)| cpu));
                let (last, moved) = self.seen.entry(agent.session).or_insert((signs, now));
                if *last != signs {
                    *last = signs;
                    *moved = now;
                }
                let ceiling = agent.paused.is_some_and(|(paused, _cpu)| paused >= PAUSED_CEILING);
                now.saturating_duration_since(*moved) < SILENT_CAP && !ceiling
            })
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Log(Vec<&'static str>);

    impl Holds for Log {
        fn system(&mut self, hold: bool) {
            self.0.push(if hold { "system on" } else { "system off" });
        }

        fn display(&mut self, hold: bool) {
            self.0.push(if hold { "display on" } else { "display off" });
        }
    }

    /// Only the edges toggle the assertion: a second client changes nothing, and the machine is
    /// released when the last one leaves, not before.
    #[test]
    fn the_first_client_holds_the_machine_and_the_last_releases_it() {
        let mut wake = Wake::new(Log::default());
        wake.client_joined();
        wake.client_joined();
        wake.client_left();
        assert_eq!(wake.holds.0, ["system on"]);
        wake.client_left();
        assert_eq!(wake.holds.0, ["system on", "system off"]);
        assert_eq!(wake.counts(), (0, 0));
    }

    /// The display follows the live-stream count from the registry, with the same edge rule.
    #[test]
    fn the_display_is_held_while_any_stream_is_live() {
        let mut wake = Wake::new(Log::default());
        wake.streams(1);
        wake.streams(2);
        wake.streams(1);
        assert_eq!(wake.holds.0, ["display on"]);
        wake.streams(0);
        wake.streams(0);
        assert_eq!(wake.holds.0, ["display on", "display off"]);
    }

    /// A stray extra `client_left` (a connection torn down twice) cannot underflow or release
    /// what a still-attached client holds.
    #[test]
    fn leaving_more_often_than_joining_is_harmless() {
        let mut wake = Wake::new(Log::default());
        wake.client_left();
        wake.client_joined();
        assert_eq!(wake.counts(), (1, 0));
        assert_eq!(wake.holds.0, ["system on"]);
    }

    /// With nobody attached, working agents hold the machine and the last one to stop lets
    /// go; a client and agents together hold it once, released when both are gone.
    #[test]
    fn a_working_agent_holds_the_machine_with_nobody_attached() {
        let mut wake = Wake::new(Log::default());
        wake.agents(2);
        wake.agents(1);
        assert_eq!(wake.holds.0, ["system on"]);
        assert!(wake.awake().system);
        wake.agents(0);
        assert_eq!(wake.holds.0, ["system on", "system off"]);
        wake.client_joined();
        wake.agents(1);
        wake.client_left();
        assert_eq!(wake.holds.0, ["system on", "system off", "system on"]);
        wake.agents(0);
        assert_eq!(wake.holds.0, ["system on", "system off", "system on", "system off"]);
        assert_eq!(wake.awake().agents, 0);
    }

    /// The person's policy decides what may hold: attached only lets a working agent sleep
    /// with the machine, and never holds nothing, a live stream's display included.
    #[test]
    fn the_policy_decides_what_keeps_the_machine_awake() {
        let mut attached = Wake::new(Log::default()).keeping(Policy::Attached);
        attached.agents(1);
        assert!(attached.holds.0.is_empty(), "an agent alone holds nothing");
        attached.client_joined();
        attached.streams(1);
        assert_eq!(attached.holds.0, ["system on", "display on"]);
        attached.client_left();
        assert_eq!(attached.holds.0, ["system on", "display on", "system off"]);

        let mut never = Wake::new(Log::default()).keeping(Policy::Never);
        never.client_joined();
        never.agents(2);
        never.streams(1);
        assert!(never.holds.0.is_empty(), "nothing held: {:?}", never.holds.0);
        let awake = never.awake();
        assert!(!awake.system && !awake.display && awake.clients == 1 && awake.streams == 1);
    }

    /// An agent whose terminal prints keeps counting; one silent for the cap stops, and
    /// counts again as soon as it prints; one that stops working is forgotten.
    #[test]
    fn the_cap_lets_a_silent_agent_go() {
        let (a, b) = (SessionId::new(), SessionId::new());
        let turn = |session, output| Signs { session, output, paused: None };
        let mut quiet = Quiet::default();
        let t0 = Instant::now();
        let at = |d: Duration| t0.checked_add(d).unwrap();
        assert_eq!(quiet.working(&[turn(a, 10), turn(b, 5)], t0), 2);
        let late = SILENT_CAP.checked_add(Duration::from_secs(1)).unwrap();
        assert_eq!(quiet.working(&[turn(a, 11), turn(b, 5)], at(Duration::from_mins(1))), 2);
        assert_eq!(quiet.working(&[turn(a, 12), turn(b, 5)], at(late)), 1, "b went silent");
        assert_eq!(quiet.working(&[turn(a, 12), turn(b, 6)], at(late)), 2, "b printed again");
        assert_eq!(quiet.working(&[turn(b, 6)], at(late)), 1);
        assert_eq!(quiet.working(&[turn(a, 12), turn(b, 6)], at(late)), 2, "a starts afresh");

        let mut wake = Wake::new(Log::default());
        let mut quiet = Quiet::default();
        wake.agents(quiet.working(&[turn(a, 1)], t0));
        wake.agents(quiet.working(&[turn(a, 1)], at(late)));
        assert_eq!(wake.holds.0, ["system on", "system off"], "a hung agent lets the Mac sleep");
    }

    /// A paused turn whose terminal is still counts while its commands compute, stops once
    /// they have been idle for the cap (a dev server waiting), and stops at the ceiling
    /// whatever they do (a watcher that polls).
    #[test]
    fn a_paused_turn_counts_while_its_commands_compute_up_to_the_ceiling() {
        let a = SessionId::new();
        let paused =
            |for_: Duration, cpu| Signs { session: a, output: 7, paused: Some((for_, cpu)) };
        let t0 = Instant::now();
        let at = |d: Duration| t0.checked_add(d).unwrap();
        let mut quiet = Quiet::default();
        let step = Duration::from_mins(10);
        assert_eq!(quiet.working(&[paused(Duration::ZERO, 100)], t0), 1);
        assert_eq!(quiet.working(&[paused(step, 900)], at(step)), 1, "a build computes");
        assert_eq!(quiet.working(&[paused(step * 2, 1700)], at(step * 2)), 1);
        assert_eq!(quiet.working(&[paused(step * 3, 1700)], at(step * 3)), 1, "idle 10 min");
        assert_eq!(quiet.working(&[paused(step * 4, 1700)], at(step * 4)), 0, "idle past the cap");
        assert_eq!(quiet.working(&[paused(step * 5, 1800)], at(step * 5)), 1, "computing again");

        let mut polling = Quiet::default();
        let mut cpu = 0;
        let mut counted = Vec::new();
        for minute in (0..=PAUSED_CEILING.as_secs() / 60 + 10).step_by(10) {
            cpu += 5;
            let since = Duration::from_mins(minute);
            counted.push(polling.working(&[paused(since, cpu)], at(since)));
        }
        let held = counted.iter().take_while(|n| **n == 1).count();
        assert_eq!(u64::try_from(held).unwrap() * 10, PAUSED_CEILING.as_secs() / 60, "{counted:?}");
    }
}

//! Sleep policy for the worker.
//!
//! The machine stays awake while a client is attached or an agent is working, and the display
//! stays on while a window or display is being streamed. A worker that idles to sleep drops
//! every session and stalls every agent mid-turn; a display that sleeps captures nothing.
//!
//! An agent keeps the machine up with nobody connected (a phone pocketed, a laptop lid closed)
//! while its thread is at work ([`Quiet`]), as the thread host holds it: the thread is
//! [`Phase::Working`] or [`Phase::Waiting`] on its own background work, and its agent's process
//! runs. During a turn it holds only while the thread shows progress ([`signs`]): [`SILENT_CAP`]
//! with nothing new from the agent means it hung or its end was never heard. A turn paused on
//! background work says nothing while its commands run, so it holds from the pause on for
//! [`PAUSED_CEILING`] at most, whatever they do, since a watcher that polls never ends.
//! Otherwise an unattended worker sleeps as its owner set it to. The person may narrow it
//! ([`Policy`], `[worker] keep_awake`, applied as the file changes): to attached clients only, or
//! to nothing at all. The policy is pure and counts; the assertions behind [`Holds`] are the
//! daemon's (`NSProcessInfo` activities through `slopty_platform::Activity`).

use std::collections::HashMap;
use std::hash::{Hash as _, Hasher as _};
use std::time::{Duration, Instant};

use slopty_core::WallMs;
use slopty_proto::thread::{ItemBody, Liveness, Phase, ThreadId, ThreadState};

/// How long a thread at work may show no progress before it stops keeping the machine awake.
///
/// A tool call that runs long (a build, a test run) streams nothing until it ends, so this is
/// an hour, as Claude desktop gives a turn without progress.
pub const SILENT_CAP: Duration = Duration::from_hours(1);

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

    /// The person changed the policy while the worker runs: hold from now on only what
    /// `policy` lets hold, releasing what it no longer does. Agents stop counting at once under
    /// a policy that does not count them, and count again from the next sample.
    pub fn set_policy(&mut self, policy: Policy) {
        self.policy = policy;
        if !self.counts_agents() {
            self.agents = 0;
        }
        self.hold_system();
        self.streams(self.streams);
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

    /// How many threads are at work now, as [`Quiet`] counts them: the machine is held while
    /// it is not zero, as while a client is attached.
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

/// One thread at work, as a tick samples it ([`signs`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Signs {
    /// The thread.
    pub thread: ThreadId,
    /// What it has said so far, folded to a number: it moves whenever the agent says anything
    /// new (an item, a word of its answer, a tool's state or output, a task's output), and
    /// never for a lease alone.
    pub progress: u64,
    /// For a turn paused on background work: how long it has been paused.
    pub paused: Option<Duration>,
}

/// What `state` says of its thread for the sleep policy at `now`.
///
/// `None` unless it is at work, [`Phase::Working`] or [`Phase::Waiting`] with its agent's
/// process running. A thread that waits on the person, or rests, holds nothing: the person is
/// not there.
#[must_use]
pub fn signs(state: &ThreadState, now: WallMs) -> Option<Signs> {
    if state.status.liveness != Liveness::Live {
        return None;
    }
    let paused = match state.status.phase {
        Phase::Working => None,
        Phase::Waiting => {
            let since = now.as_millis().saturating_sub(state.status.since_ms.as_millis());
            Some(Duration::from_millis(since))
        }
        _ => return None,
    };
    Some(Signs { thread: state.meta.id, progress: progress(state), paused })
}

/// What a thread has said so far, folded to a number ([`Signs::progress`]): counts and sizes,
/// never the text, so a tick reads nothing large.
fn progress(state: &ThreadState) -> u64 {
    let mut hash = std::hash::DefaultHasher::new();
    (state.turns.len(), state.items.len()).hash(&mut hash);
    if let Some(last) = state.items.last() {
        last.id.hash(&mut hash);
        match &last.body {
            ItemBody::Text(text) | ItemBody::Reasoning(text) => text.chars.hash(&mut hash),
            ItemBody::Tool(call) => {
                std::mem::discriminant(&call.state).hash(&mut hash);
                call.output.as_ref().map(|out| out.chars).hash(&mut hash);
            }
            _ => {}
        }
    }
    for task in &state.tasks {
        (&task.state, task.output.as_ref().map(|out| out.chars)).hash(&mut hash);
    }
    hash.finish()
}

/// Which threads at work showed progress lately.
#[derive(Debug, Default)]
pub struct Quiet {
    /// Per thread at work: its progress as last seen, and when it last moved.
    seen: HashMap<ThreadId, (u64, Instant)>,
}

impl Quiet {
    /// The threads at work now, at `now`: those in a turn that showed progress within
    /// [`SILENT_CAP`], and those paused for less than [`PAUSED_CEILING`]. A thread that starts
    /// working counts as having just moved, and one that stops is forgotten.
    pub fn working(&mut self, threads: &[Signs], now: Instant) -> usize {
        self.seen.retain(|thread, _seen| threads.iter().any(|t| t.thread == *thread));
        threads
            .iter()
            .filter(|thread| {
                let (last, moved) =
                    self.seen.entry(thread.thread).or_insert((thread.progress, now));
                if *last != thread.progress {
                    *last = thread.progress;
                    *moved = now;
                }
                match thread.paused {
                    Some(paused) => paused < PAUSED_CEILING,
                    None => now.saturating_duration_since(*moved) < SILENT_CAP,
                }
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

    /// A policy changed while the worker runs takes and releases the holds at once.
    #[test]
    fn a_policy_changed_while_running_applies_at_once() {
        let mut wake = Wake::new(Log::default());
        wake.agents(1);
        wake.streams(1);
        assert_eq!(wake.holds.0, ["system on", "display on"]);
        wake.set_policy(Policy::Attached);
        assert_eq!(wake.holds.0, ["system on", "display on", "system off"], "agents stop");
        assert_eq!(wake.awake().agents, 0, "and stop counting");
        wake.client_joined();
        wake.set_policy(Policy::Never);
        assert_eq!(
            wake.holds.0,
            ["system on", "display on", "system off", "system on", "system off", "display off"]
        );
        wake.set_policy(Policy::Working);
        assert_eq!(wake.holds.0.len(), 8, "the client and the stream hold again");
        assert!(wake.awake().system && wake.awake().display);
    }

    /// A Claude Code thread, at rest.
    fn thread() -> ThreadState {
        use slopty_proto::thread::{AgentId, Drive, ThreadMeta};
        ThreadState::new(ThreadMeta {
            id: ThreadId::new(),
            agent: AgentId::named(AgentId::CLAUDE_CODE),
            agent_version: "2.1.300".to_owned(),
            native: "s".to_owned(),
            cwd: "/w".to_owned(),
            title: "t".to_owned(),
            parent: None,
            origin: ThreadMeta::PERSON.to_owned(),
            drive: Drive::named(Drive::DRIVEN),
            facts: std::collections::BTreeMap::new(),
            created_ms: WallMs::ZERO,
        })
    }

    /// Only a thread at work whose agent runs is sampled: working, or paused on its own
    /// background work for as long as the pause has lasted. One waiting on the person, at rest,
    /// or whose process ended holds nothing. Its progress moves with what the agent says, and
    /// not with a lease.
    #[test]
    fn a_thread_at_work_is_sampled_and_its_progress_moves_with_what_it_says() {
        use slopty_proto::thread::{Action, Clipped, Item, ItemId, Status, TurnId};

        let now = WallMs::from_millis(10_000_000);
        let mut state = thread();
        assert_eq!(signs(&state, now), None, "at rest");
        state.status = Status { phase: Phase::Working, since_ms: now, ..Status::default() };
        let working = signs(&state, now).expect("working");
        assert_eq!((working.thread, working.paused), (state.meta.id, None));

        let said = |text: &str| Item {
            id: ItemId("answer".to_owned()),
            turn: TurnId(1),
            at_ms: now,
            body: ItemBody::Text(Clipped::whole(text)),
        };
        state.items.push(said("Rea"));
        let first = signs(&state, now).unwrap().progress;
        assert_ne!(first, working.progress, "an item");
        state.items[0] = said("Reading the tests");
        let more = signs(&state, now).unwrap().progress;
        assert_ne!(more, first, "a word more of the answer");
        state.apply(&Action::Lease(now));
        assert_eq!(signs(&state, now).unwrap().progress, more, "a lease is no progress");

        state.status.phase = Phase::Waiting;
        let later = WallMs::from_millis(now.as_millis().saturating_add(90_000));
        assert_eq!(signs(&state, later).unwrap().paused, Some(Duration::from_secs(90)));
        for phase in [Phase::NeedsYou, Phase::Idle, Phase::Done, Phase::Failed, Phase::Stopped] {
            state.status.phase = phase;
            assert_eq!(signs(&state, now), None, "{phase:?}");
        }
        state.status.phase = Phase::Working;
        state.status.liveness = Liveness::Exited { resumable: true };
        assert_eq!(signs(&state, now), None, "its process ended");
    }

    /// A thread whose progress moves keeps counting; one silent for the cap stops, and counts
    /// again as soon as it moves; one that stops working is forgotten.
    #[test]
    fn the_cap_lets_a_silent_thread_go() {
        let (a, b) = (ThreadId::new(), ThreadId::new());
        let turn = |thread, progress| Signs { thread, progress, paused: None };
        let mut quiet = Quiet::default();
        let t0 = Instant::now();
        let at = |d: Duration| t0.checked_add(d).unwrap();
        assert_eq!(quiet.working(&[turn(a, 10), turn(b, 5)], t0), 2);
        let late = SILENT_CAP.checked_add(Duration::from_secs(1)).unwrap();
        assert_eq!(quiet.working(&[turn(a, 11), turn(b, 5)], at(Duration::from_mins(1))), 2);
        assert_eq!(quiet.working(&[turn(a, 12), turn(b, 5)], at(late)), 1, "b went silent");
        assert_eq!(quiet.working(&[turn(a, 12), turn(b, 6)], at(late)), 2, "b moved again");
        assert_eq!(quiet.working(&[turn(b, 6)], at(late)), 1);
        assert_eq!(quiet.working(&[turn(a, 12), turn(b, 6)], at(late)), 2, "a starts afresh");

        let mut wake = Wake::new(Log::default());
        let mut quiet = Quiet::default();
        wake.agents(quiet.working(&[turn(a, 1)], t0));
        wake.agents(quiet.working(&[turn(a, 1)], at(late)));
        assert_eq!(wake.holds.0, ["system on", "system off"], "a hung turn lets the Mac sleep");
    }

    /// A paused turn counts whatever its commands do, until the ceiling: a watcher that polls
    /// would otherwise hold the machine for good.
    #[test]
    fn a_paused_turn_counts_up_to_the_ceiling() {
        let a = ThreadId::new();
        let paused = |for_: Duration| Signs { thread: a, progress: 7, paused: Some(for_) };
        let t0 = Instant::now();
        let at = |d: Duration| t0.checked_add(d).unwrap();
        let mut quiet = Quiet::default();
        let mut counted = Vec::new();
        for minute in (0..=PAUSED_CEILING.as_secs() / 60 + 30).step_by(10) {
            let since = Duration::from_mins(minute);
            counted.push(quiet.working(&[paused(since)], at(since)));
        }
        let held = counted.iter().take_while(|n| **n == 1).count();
        assert_eq!(u64::try_from(held).unwrap() * 10, PAUSED_CEILING.as_secs() / 60, "{counted:?}");
        assert!(counted[held..].iter().all(|n| *n == 0), "and none after: {counted:?}");
    }
}

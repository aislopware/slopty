//! Coding agents on the worker.
//!
//! Every agent Slopty runs is driven through its own published door, one adapter per agent, each
//! a sans-IO codec here and its IO on the worker: Claude Code over stream-json with the control
//! protocol ([`claude`]), Codex over its app-server ([`codex`]), pi over its RPC mode ([`pi`]),
//! and any ACP agent over the Agent Client Protocol ([`acp`]). They share the thread model's
//! pieces: the transcript decoder ([`conversation`]), the queue of messages that wait on the
//! worker ([`queue`]), the files sent with a message ([`attach`]), and Slopty's tools for an
//! agent it starts ([`mcp`]).
//!
//! An agent the person runs in a terminal by hand is that terminal's program. The worker knows
//! of it only what the terminal shows anyone ([`AgentTable`]): its [`detect`]ed foreground
//! process says an agent is there, and the [`title`] it paints tells a running turn from rest.
//! Nothing is typed into it and nothing of its own record is read.

#![forbid(unsafe_code)]
#![warn(unreachable_pub)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` is on, so an item shared from a private module is `pub(crate)`"
)]

pub mod acp;
pub mod attach;
pub mod claude;
pub mod codex;
pub mod commands;
pub mod conversation;
pub mod detect;
pub mod driven;
pub mod history;
pub mod managed;
pub mod mcp;
pub mod media;
pub mod permission;
pub mod pi;
pub mod queue;
pub mod reports;
pub mod shown;
pub mod status;
pub mod title;
pub mod transcript;
pub mod trust;
pub mod vouch;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::SystemTime;

use slopty_core::{SessionId, WallMs};

use crate::detect::Program;
use crate::status::{AgentEvent, AgentSource, AgentStatus};
use crate::title::TitleSignal;

/// What the worker can see of a session without any help from the agent: the program in the
/// foreground of its tty, the title that program paints, and the directory it runs in.
///
/// Everything here is a fact about the terminal, gathered by the worker on a timer; nothing in
/// it requires the agent to cooperate. A `program` of `None` means the platform would not say
/// (the lookup failed), which is not the same as "no agent runs here" and never clears one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Observation {
    /// The foreground process of the session's tty.
    pub program: Option<Program>,
    /// The terminal title (OSC 0/2), when the session set one.
    pub title: Option<String>,
    /// Where the agent runs: the foreground process's own working directory, else OSC 7.
    pub cwd: Option<PathBuf>,
    /// The foreground process's pid, when the platform said.
    pub pid: Option<i32>,
    /// When that process started, when the platform said. Together with `pid` this is the
    /// identity of the process: one `claude` replaced by another inside a tick is not the same
    /// agent, whatever the session it runs in.
    pub started: Option<SystemTime>,
}

/// One terminal's agent, as its foreground process and its title show it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tracker {
    status: AgentStatus,
    /// Which signal the current status came from.
    source: AgentSource,
    /// When the status last changed, what [`AgentEvent::since_ms`] says.
    since_ms: WallMs,
    /// The agent process this tracker follows (`pid`, start time), when the platform said.
    process: Option<(i32, Option<SystemTime>)>,
}

impl Default for Tracker {
    fn default() -> Self {
        Self {
            status: AgentStatus::None,
            source: AgentSource::Process,
            since_ms: WallMs::ZERO,
            process: None,
        }
    }
}

impl Tracker {
    /// Current status.
    #[must_use]
    pub const fn status(&self) -> &AgentStatus {
        &self.status
    }

    /// Which signal the current status came from.
    #[must_use]
    pub const fn source(&self) -> AgentSource {
        self.source
    }

    /// The event describing the current state (for a joining client).
    #[must_use]
    pub const fn event(&self, session: SessionId) -> AgentEvent {
        AgentEvent { session, status: self.status, source: self.source, since_ms: self.since_ms }
    }

    /// Fold in one round of what the worker can see; the event when the visible state changed.
    ///
    /// The process being there means [`AgentStatus::Idle`], and the title tells a running turn
    /// from rest. A process that is not the agent ends it; one replaced by another between two
    /// probes is a new agent.
    pub fn observe(&mut self, session: SessionId, obs: &Observation) -> Option<AgentEvent> {
        let Some(program) = obs.program.as_ref() else {
            // The platform would not say; that is not evidence of anything.
            return None;
        };
        if !program.is_claude() {
            if self.status == AgentStatus::None {
                return None;
            }
            *self = Self::default();
            return Some(self.event(session));
        }
        if let Some(pid) = obs.pid {
            let process = (pid, obs.started);
            if self.process.is_some_and(|known| known != process) {
                *self = Self::default();
            }
            self.process = Some(process);
        }
        let (status, source) = match obs.title.as_deref().and_then(title::signal) {
            Some(TitleSignal::Working) => (AgentStatus::Working, AgentSource::Title),
            Some(TitleSignal::Idle) => (AgentStatus::Idle, AgentSource::Title),
            // No title to read: the process being there is all we know.
            None => (AgentStatus::Idle, AgentSource::Process),
        };
        if status == self.status && source == self.source {
            return None;
        }
        if status != self.status {
            self.since_ms = WallMs::now();
        }
        self.status = status;
        self.source = source;
        Some(self.event(session))
    }
}

/// All sessions' agents.
#[derive(Debug, Default)]
pub struct AgentTable {
    sessions: HashMap<SessionId, Tracker>,
    /// Sessions whose agent ended and where none has started since ([`Self::ended`]).
    ended: HashSet<SessionId>,
}

impl AgentTable {
    /// Feed one round of what the worker can see of a session ([`Tracker::observe`]).
    pub fn observe(&mut self, session: SessionId, obs: &Observation) -> Option<AgentEvent> {
        // A session with nothing agent-like in it must not grow an entry on every tick.
        if !self.sessions.contains_key(&session)
            && !obs.program.as_ref().is_some_and(Program::is_claude)
        {
            return None;
        }
        let tracker = self.sessions.entry(session).or_default();
        let had_agent = tracker.status() != &AgentStatus::None;
        let event = tracker.observe(session, obs);
        let has_agent = tracker.status() != &AgentStatus::None;
        self.note_end(session, had_agent, has_agent);
        if !has_agent {
            self.sessions.remove(&session);
        }
        event
    }

    /// Keep [`Self::ended`] as a signal moved `session` from having an agent or not to having
    /// one or not.
    fn note_end(&mut self, session: SessionId, had_agent: bool, has_agent: bool) {
        if has_agent {
            self.ended.remove(&session);
        } else if had_agent {
            self.ended.insert(session);
        }
    }

    /// The program in `session` exited: the agent that ran there has ended. An exited terminal
    /// has no foreground process for [`Self::observe`] to miss, so this is how that end is heard
    /// at once. The event that says so; `None` when no agent ran there.
    pub fn exited(&mut self, session: SessionId) -> Option<AgentEvent> {
        let tracker = self.sessions.remove(&session)?;
        if tracker.status() == &AgentStatus::None {
            return None;
        }
        self.note_end(session, true, false);
        Some(Tracker::default().event(session))
    }

    /// Whether the agent that ran in `session` has ended and none has started there since:
    /// the terminal's shell, if it has one, is what reads its input now.
    #[must_use]
    pub fn ended(&self, session: SessionId) -> bool {
        self.ended.contains(&session)
    }

    /// Keep only the sessions in `live`: the events of the agents that went with the others.
    pub fn retain(&mut self, live: &[SessionId]) -> Vec<AgentEvent> {
        let mut gone = Vec::new();
        self.ended.retain(|session| live.contains(session));
        self.sessions.retain(|session, _tracker| {
            if live.contains(session) {
                return true;
            }
            gone.push(Tracker::default().event(*session));
            false
        });
        gone
    }

    /// Whether `event` still says what the table holds: an event computed before a newer one
    /// was told is not sent.
    #[must_use]
    pub fn is_current(&self, event: &AgentEvent) -> bool {
        self.sessions.get(&event.session).map_or(event.status == AgentStatus::None, |tracker| {
            tracker.status == event.status && tracker.source == event.source
        })
    }

    /// Forget `session` entirely.
    pub fn forget(&mut self, session: SessionId) {
        self.sessions.remove(&session);
        self.ended.remove(&session);
    }

    /// The pid of the agent in `session`, when the platform said.
    #[must_use]
    pub fn pid(&self, session: SessionId) -> Option<i32> {
        self.sessions.get(&session)?.process.map(|(pid, _started)| pid)
    }

    /// Every session's agent, as an event that raises no attention.
    #[must_use]
    pub fn snapshot(&self) -> Vec<AgentEvent> {
        self.sessions.iter().map(|(id, t)| t.event(*id)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the worker sees of a terminal running `program` with `title`, always the same
    /// process ([`process`] for another one).
    fn seen(program: &str, title: Option<&str>) -> Observation {
        process(program, title, 4321)
    }

    /// [`seen`] for a named process: `pid`, started at the same time.
    fn process(program: &str, title: Option<&str>, pid: i32) -> Observation {
        Observation {
            program: Some(Program::named(program)),
            title: title.map(str::to_owned),
            cwd: Some(PathBuf::from("/tmp/project")),
            pid: Some(pid),
            started: SystemTime::UNIX_EPOCH.checked_add(std::time::Duration::from_secs(1_000_000)),
        }
    }

    /// A `claude` the person started by hand is there by its process, working or at rest by its
    /// title, stamped as its phase changes, and gone with its process.
    #[test]
    fn a_hand_started_claude_is_attributed_from_the_process_and_the_title() {
        let sid = SessionId::new();
        let mut t = Tracker::default();
        assert_eq!(t.event(sid).since_ms, WallMs::ZERO, "no agent, no stamp");
        // A shell is not an agent, and an empty tracker has nothing to clear.
        assert_eq!(t.observe(sid, &seen("zsh", Some("~/project"))), None);

        let before = WallMs::now();
        let e = t.observe(sid, &seen("claude", None)).expect("the process");
        assert_eq!((e.status, e.source), (AgentStatus::Idle, AgentSource::Process));
        assert!(e.since_ms >= before);
        assert_eq!(t.observe(sid, &seen("claude", None)), None, "nothing changed");

        let e = t.observe(sid, &seen("claude", Some("◐ Claude Code"))).expect("the title");
        assert_eq!((e.status, e.source), (AgentStatus::Working, AgentSource::Title));
        let working = e.since_ms;
        let e = t.observe(sid, &seen("claude", Some("✳ fix the build"))).expect("back to idle");
        assert_eq!((e.status, e.source), (AgentStatus::Idle, AgentSource::Title));
        assert!(e.since_ms >= working, "rest is a new phase");
        let agent = status::SessionAgent::from(&t.event(sid));
        assert_eq!(agent.since_ms, e.since_ms, "orchestration reads the same stamp");

        let e = t.observe(sid, &seen("zsh", Some("~/project"))).expect("gone");
        assert_eq!((e.status, e.since_ms), (AgentStatus::None, WallMs::ZERO));
    }

    /// Between two probes the first `claude` exited and a second one started: the second is a
    /// new agent, and the same process seen again changes nothing. A lookup that says nothing
    /// changes nothing.
    #[test]
    fn a_second_claude_in_the_same_terminal_is_a_new_agent() {
        let sid = SessionId::new();
        let mut table = AgentTable::default();
        table.observe(sid, &process("claude", None, 100)).expect("the first agent");
        table.observe(sid, &process("claude", Some("◐ Claude Code"), 100)).expect("working");
        let e = table.observe(sid, &process("claude", None, 101)).expect("the second agent");
        assert_eq!((e.status, e.source), (AgentStatus::Idle, AgentSource::Process));
        assert_eq!(table.pid(sid), Some(101));
        assert_eq!(table.observe(sid, &process("claude", None, 101)), None);
        assert_eq!(table.observe(sid, &Observation::default()), None, "the platform said nothing");
    }

    /// The table keeps entries only for agents, says which events are still current, and lets
    /// an agent go with its terminal or its program.
    #[test]
    fn the_table_keeps_only_agents_and_lets_them_go_with_their_terminals() {
        let (a, b, shell) = (SessionId::new(), SessionId::new(), SessionId::new());
        let mut table = AgentTable::default();
        let stale = table.observe(a, &seen("claude", None)).expect("the process");
        assert!(table.is_current(&stale));
        table.observe(a, &seen("claude", Some("◐ Claude Code"))).expect("working");
        assert!(!table.is_current(&stale), "the title spoke since");
        table.observe(b, &seen("claude", None)).expect("another");
        assert_eq!(table.observe(shell, &seen("zsh", None)), None);
        assert_eq!(table.snapshot().len(), 2, "only the agents have entries");

        assert_eq!(table.retain(&[a, b]), Vec::new(), "both still run");
        let gone = table.retain(&[b]);
        assert_eq!(gone.len(), 1, "{gone:#?}");
        assert_eq!((gone[0].session, gone[0].status), (a, AgentStatus::None));

        let ended = table.exited(b).expect("an event");
        assert_eq!((ended.session, ended.status), (b, AgentStatus::None));
        assert!(table.ended(b), "its terminal's shell reads input now");
        assert!(table.is_current(&ended));
        assert!(table.exited(b).is_none(), "said once");
        assert!(table.exited(shell).is_none(), "no agent ran there");
        table.observe(b, &seen("claude", None)).expect("a new agent");
        assert!(!table.ended(b), "another started");
        table.observe(b, &seen("zsh", None)).expect("gone");
        assert!(table.ended(b), "ended by the process table");
        table.forget(b);
        assert!(!table.ended(b));
        assert_eq!(table.snapshot(), Vec::<AgentEvent>::new());
    }
}

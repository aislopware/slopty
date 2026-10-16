//! The coding agents a person runs in Slopty's terminals, as the worker sees them from outside.
//!
//! A terminal's agent is the person's own: Slopty drives the agents it starts through their own
//! doors (`slopty_worker::thread`), and of one typed into a terminal it knows only what the
//! terminal shows anyone:
//!
//! 1. **Foreground process.** `slopty_worker` reads the foreground process of each session's tty;
//!    when `slopty_agent::detect` says it is a coding agent, the session gets an agent at
//!    `AgentStatus::Idle`, and when it goes the agent goes with it.
//! 2. **Title.** The spinning circle Claude Code paints into the terminal title separates a running
//!    turn from an idle prompt (`slopty_agent::title`).
//!
//! Each tick also tells the sleep policy how many agents are working with their terminals still
//! printing (`slopty_worker::wake`).

use std::path::PathBuf;
use std::time::Duration;

use slopty_agent::Observation;
use slopty_agent::detect::Program;
use slopty_agent::status::{AgentEvent, AgentStatus};
use slopty_core::SessionId;
use slopty_worker::session::Probe;

use crate::Daemon;

/// How often every session's foreground process and title are read.
const TICK: Duration = Duration::from_millis(750);

/// Watch every session for an agent, until the daemon stops.
pub async fn watch(daemon: Daemon) -> ! {
    let mut quiet = slopty_worker::wake::Quiet::default();
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let live = daemon.worker.probe().await;
        // Sessions the worker no longer runs are gone whatever their last signal said.
        let ids: Vec<SessionId> = live.iter().map(|(id, _probe)| *id).collect();
        let unlooked = daemon.handoffs.lock().retain(&ids);
        for session in unlooked {
            let _sent = daemon.presence.send((session, false));
        }
        {
            let mut agents = daemon.agents.lock();
            let mut events = Vec::new();
            for (session, probe) in live {
                events.extend(agents.observe(session, &observation(probe)));
            }
            events.extend(agents.retain(&ids));
            broadcast(&daemon, &agents, events);
            drop(agents);
        }

        keep_awake(&daemon, &mut quiet);
    }
}

/// Tell the sleep policy how many agents are working that showed signs of work within its cap:
/// their terminals printing. Nothing is sampled while the person's policy lets no agent hold
/// the machine.
fn keep_awake(daemon: &Daemon, quiet: &mut slopty_worker::wake::Quiet) {
    if !daemon.wake.lock().counts_agents() {
        return;
    }
    // Taken whole, so the table's lock is let go before each terminal is asked.
    let agents = daemon.agents.lock().snapshot();
    let signs: Vec<slopty_worker::wake::Signs> = agents
        .into_iter()
        .filter(|event| event.status == AgentStatus::Working)
        .map(|event| event.session)
        .filter_map(|session| {
            let output = daemon.worker.get(session).ok()?.activity().borrow().output;
            Some(slopty_worker::wake::Signs { session, output, paused: None })
        })
        .collect();
    let at_work = quiet.working(&signs, std::time::Instant::now());
    daemon.wake.lock().agents(at_work);
}

/// Send what the poll found, dropping anything already overtaken.
fn broadcast(daemon: &Daemon, agents: &slopty_agent::AgentTable, events: Vec<AgentEvent>) {
    for event in events {
        if !agents.is_current(&event) {
            continue;
        }
        tracing::debug!(
            session = %event.session,
            status = ?event.status,
            source = ?event.source,
            "agent"
        );
        let _sent = daemon.heard.send(event);
    }
}

/// What a probe saw, taken apart rather than copied.
fn observation(probe: Probe) -> Observation {
    let Probe { foreground, title, cwd } = probe;
    let (program, fg_cwd, pid, started) = match foreground {
        Some(fg) => {
            (Some(Program { name: fg.name, argv: fg.argv }), fg.cwd, Some(fg.pid), fg.started)
        }
        None => (None, None, None, None),
    };
    Observation {
        program,
        title,
        // The agent's own working directory is the truth; OSC 7 from the shell that launched
        // it is the fallback when the process table would not say.
        cwd: fg_cwd.or_else(|| cwd.map(PathBuf::from)),
        pid,
        started,
    }
}

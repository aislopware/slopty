//! The coding agents a person runs in Slopty's terminals, as the worker sees them from outside.
//!
//! A terminal's agent is the person's own: Slopty drives the agents it starts through their own
//! doors (`slopty_worker::thread`), and of one typed into a terminal it knows only that it is
//! there. `slopty_worker` reads the foreground process of each session's tty; when
//! `slopty_agent::detect` says it is a coding agent, the session has an agent, and when it goes
//! the agent goes with it. Nothing of the terminal's screen is read.
//!
//! What it sees keeps orchestration from typing into the terminal (`orchestrate::may_type`).
//! The sleep policy counts the threads Slopty drives, as their host holds them
//! (`crate::threads::awake`).

use std::path::PathBuf;
use std::time::Duration;

use slopty_agent::Observation;
use slopty_agent::detect::Program;
use slopty_core::SessionId;
use slopty_worker::session::Probe;

use crate::Daemon;

/// How often every session's foreground process is read.
const TICK: Duration = Duration::from_millis(750);

/// Watch every session for an agent, until the daemon stops.
pub async fn watch(daemon: Daemon) -> ! {
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
        let mut agents = daemon.agents.lock();
        for (session, probe) in live {
            if let Some(event) = agents.observe(session, &observation(probe)) {
                tracing::debug!(session = %event.session, status = ?event.status, "agent");
            }
        }
        let _gone = agents.retain(&ids);
        drop(agents);
    }
}

/// What a probe saw, taken apart rather than copied.
fn observation(probe: Probe) -> Observation {
    let Probe { foreground, cwd } = probe;
    let (program, fg_cwd, pid, started) = match foreground {
        Some(fg) => {
            (Some(Program { name: fg.name, argv: fg.argv }), fg.cwd, Some(fg.pid), fg.started)
        }
        None => (None, None, None, None),
    };
    Observation {
        program,
        // The agent's own working directory is the truth; OSC 7 from the shell that launched
        // it is the fallback when the process table would not say.
        cwd: fg_cwd.or_else(|| cwd.map(PathBuf::from)),
        pid,
        started,
    }
}

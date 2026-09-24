//! Attributing agent sessions nobody registered hooks for.
//!
//! Hooks are precise but opt-in: they only fire for sessions whose Claude Code found
//! `slopty hook` in `~/.claude/settings.json`. This tick covers everything else — a `claude`
//! the human typed into a Slopty terminal, or one running since before the relay was
//! installed — from what the worker can see anyway:
//!
//! 1. **Foreground process.** `slopty_worker` reads the foreground process of each session's tty;
//!    when `slopty_agent::detect` says it is Claude Code, the session gets an agent at
//!    `AgentStatus::Idle`, and when it goes the agent goes with it.
//! 2. **Title.** The spinning circle Claude Code paints into the terminal title separates a running
//!    turn from an idle prompt (`slopty_agent::title`).
//! 3. **Transcript.** The JSONL file the agent writes is found from its working directory
//!    (`slopty_agent::discover`) and tailed off the blocking pool; its newest record says whether
//!    the turn is thinking, running a tool, or finished, and names it. The lookup is repeated every
//!    `REDISCOVER_EVERY` ticks, because `/clear` and `/resume` start a new file.
//!
//! Hooks outrank all three: once one has spoken for a session, this tick only fills gaps (the
//! transcript path a hook would have named, so ⌘⇧L works either way) and never touches the
//! status. Nothing here reads a transcript outside the project directory the session's own
//! working directory points at.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use slopty_agent::detect::Program;
use slopty_agent::transcript::Tail;
use slopty_agent::{Discovery, Observation};
use slopty_core::SessionId;
use slopty_proto::WorkerMsg;
use slopty_proto::agent::AgentEvent;

use crate::Daemon;

/// How often every session's foreground process and title are read.
const TICK: Duration = Duration::from_millis(750);

/// Every how many ticks an agent whose transcript is already known is looked up again:
/// `/clear` and `/resume` start a new file, and the tail has to move with it. Finding one for
/// the first time happens on every tick; this is the cost of a `read_dir` per known agent.
const REDISCOVER_EVERY: u64 = 8;

/// Watch every session for an agent nobody told us about, until the daemon stops.
pub async fn watch(daemon: Daemon) -> ! {
    let home = slopty_agent::hooks::home_dir();
    let mut tails: HashMap<SessionId, Tail> = HashMap::new();
    let mut ticks: u64 = 0;
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let live = daemon.worker.probe().await;
        // Sessions the worker no longer runs are gone whatever their last signal said: an agent
        // whose terminal exited never reports a `SessionEnd` hook.
        let ids: Vec<SessionId> = live.iter().map(|(id, _probe)| *id).collect();
        tails.retain(|session, _tail| ids.contains(session));
        {
            let mut agents = daemon.agents.lock();
            let mut events = Vec::new();
            for (session, probe) in &live {
                let observation = Observation {
                    program: probe
                        .foreground
                        .as_ref()
                        .map(|fg| Program { name: fg.name.clone(), argv: fg.argv.clone() }),
                    title: probe.title.clone(),
                    // The agent's own working directory is the truth; OSC 7 from the shell
                    // that launched it is the fallback when the process table would not say.
                    cwd: probe
                        .foreground
                        .as_ref()
                        .and_then(|fg| fg.cwd.clone())
                        .or_else(|| probe.cwd.clone().map(PathBuf::from)),
                    pid: probe.foreground.as_ref().map(|fg| fg.pid),
                    started: probe.foreground.as_ref().and_then(|fg| fg.started),
                };
                events.extend(agents.observe(*session, &observation));
            }
            events.extend(agents.retain(&ids));
            // Sent before anything is awaited: a hook arriving on the control socket while we
            // read files has already broadcast something newer.
            broadcast(&daemon, &agents, events);
            drop(agents);
        }

        ticks = ticks.wrapping_add(1);
        for (session, path) in
            discover(&daemon, &home, ticks.is_multiple_of(REDISCOVER_EVERY)).await
        {
            if daemon.agents.lock().set_transcript_path(session, &path) {
                // A conversation the human cleared or resumed: read the new file from its top.
                tails.remove(&session);
            }
        }
        follow(&daemon, &mut tails).await;
    }
}

/// Send what the poll found, dropping anything a hook has already overtaken.
fn broadcast(daemon: &Daemon, agents: &slopty_agent::AgentTable, events: Vec<AgentEvent>) {
    for event in events {
        if !agents.is_current(&event) {
            continue;
        }
        tracing::debug!(
            session = %event.session,
            status = ?event.status,
            source = ?event.source,
            "agent (no hooks)"
        );
        let _sent = daemon.events.send(WorkerMsg::Agent(event));
    }
}

/// Look for the transcript of every agent whose file is still unknown, and — when `again` —
/// of those that have one, in case the human cleared the conversation and Claude Code started
/// a new file. Only paths that differ from what is being read come back.
async fn discover(
    daemon: &Daemon,
    home: &std::path::Path,
    again: bool,
) -> Vec<(SessionId, PathBuf)> {
    let wanted: Vec<Discovery> = daemon
        .agents
        .lock()
        .discoveries()
        .into_iter()
        .filter(|d| again || d.current.is_none())
        .collect();
    if wanted.is_empty() {
        return Vec::new();
    }
    let home = home.to_path_buf();
    let found = tokio::task::spawn_blocking(move || {
        wanted
            .into_iter()
            .filter_map(|d| {
                let path = slopty_agent::discover::transcript_for(&home, &d.cwd, d.since)?;
                (d.current.as_deref() != Some(path.to_string_lossy().as_ref()))
                    .then_some((d.session, path))
            })
            .collect::<Vec<_>>()
    })
    .await;
    found.unwrap_or_default()
}

/// Read what is new in every unhooked agent's transcript, turn it into status and broadcast
/// it. Each session's event is sent before the next file is read, so a hook that arrives in
/// between is never overwritten.
async fn follow(daemon: &Daemon, tails: &mut HashMap<SessionId, Tail>) {
    let paths: Vec<(SessionId, PathBuf)> = {
        let agents = daemon.agents.lock();
        agents
            .sessions_with_agents()
            .into_iter()
            .filter_map(|session| Some((session, agents.transcript_path(session)?)))
            .collect()
    };
    for (session, path) in paths {
        let mut tail = tails.remove(&session).unwrap_or_default();
        let Ok((tail, read)) = tokio::task::spawn_blocking(move || {
            let read = tail.read(&path);
            (tail, read)
        })
        .await
        else {
            continue;
        };
        tails.insert(session, tail);
        let progress = match read {
            Ok(read) => read.progress,
            Err(e) => {
                tracing::debug!(%session, error = %e, "agent transcript read");
                continue;
            }
        };
        if let Some(progress) = progress {
            let mut agents = daemon.agents.lock();
            let events = agents.observe_progress(session, &progress).into_iter().collect();
            broadcast(daemon, &agents, events);
            drop(agents);
        }
    }
}

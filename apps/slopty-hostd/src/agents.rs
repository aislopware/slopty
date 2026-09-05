//! Attributing agent sessions nobody registered hooks for.
//!
//! Hooks are precise but opt-in: they only fire for sessions whose Claude Code found
//! `slopty hook` in `~/.claude/settings.json`. This tick covers everything else — a `claude`
//! the human typed into a Slopty terminal, or one running since before the relay was
//! installed — from what the host can see anyway:
//!
//! 1. **Foreground process.** `slopty_host` reads the foreground process of each session's tty;
//!    when `slopty_agent::detect` says it is Claude Code, the session gets an agent at
//!    `AgentStatus::Idle`, and when it goes the agent goes with it.
//! 2. **Title.** The sparkle Claude Code paints into the terminal title separates a running turn
//!    from an idle prompt (`slopty_agent::title`).
//! 3. **Transcript.** The JSONL file the agent writes is found from its working directory
//!    (`slopty_agent::discover`) and tailed off the blocking pool; its newest record says whether
//!    the turn is thinking, running a tool, or finished, and names it.
//!
//! Hooks outrank all three: once one has spoken for a session, this tick only fills gaps (the
//! transcript path a hook would have named, so ⌘⇧L works either way) and never touches the
//! status. Nothing here reads a transcript outside the project directory the session's own
//! working directory points at.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use slopty_agent::Observation;
use slopty_agent::detect::Program;
use slopty_agent::transcript::Tail;
use slopty_core::SessionId;
use slopty_proto::HostMsg;

use crate::Daemon;

/// How often every session's foreground process and title are read.
const TICK: Duration = Duration::from_millis(750);

/// Watch every session for an agent nobody told us about, until the daemon stops.
pub async fn watch(daemon: Daemon) -> ! {
    let home = slopty_agent::hooks::home_dir();
    let mut tails: HashMap<SessionId, Tail> = HashMap::new();
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let live = daemon.host.probe().await;
        let mut events = Vec::new();
        {
            let mut agents = daemon.agents.lock();
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
                };
                let started = probe
                    .foreground
                    .as_ref()
                    .and_then(|fg| fg.started)
                    .unwrap_or_else(SystemTime::now);
                events.extend(agents.observe(*session, &observation, started));
            }
        }
        // Sessions the host no longer runs are gone whatever their last signal said: an agent
        // whose terminal exited never reports a `SessionEnd` hook.
        let ids: Vec<SessionId> = live.iter().map(|(id, _probe)| *id).collect();
        tails.retain(|session, _tail| ids.contains(session));
        events.extend(daemon.agents.lock().retain(&ids));

        for (session, path) in discover(&daemon, &home).await {
            daemon.agents.lock().set_transcript_path(session, &path);
        }
        events.extend(follow(&daemon, &mut tails).await);
        for event in events {
            tracing::debug!(
                session = %event.session,
                status = ?event.status,
                source = ?event.source,
                "agent (no hooks)"
            );
            let _sent = daemon.events.send(HostMsg::Agent(event));
        }
    }
}

/// Look for the transcript of every agent whose file is still unknown.
async fn discover(daemon: &Daemon, home: &std::path::Path) -> Vec<(SessionId, PathBuf)> {
    let wanted = daemon.agents.lock().undiscovered();
    if wanted.is_empty() {
        return Vec::new();
    }
    let home = home.to_path_buf();
    let found = tokio::task::spawn_blocking(move || {
        wanted
            .into_iter()
            .filter_map(|(session, cwd, since)| {
                let path = slopty_agent::discover::transcript_for(&home, &cwd, since)?;
                Some((session, path))
            })
            .collect::<Vec<_>>()
    })
    .await;
    found.unwrap_or_default()
}

/// Read what is new in every unhooked agent's transcript and turn it into status.
async fn follow(
    daemon: &Daemon,
    tails: &mut HashMap<SessionId, Tail>,
) -> Vec<slopty_proto::agent::AgentEvent> {
    let paths: Vec<(SessionId, PathBuf)> = {
        let agents = daemon.agents.lock();
        agents
            .sessions_with_agents()
            .into_iter()
            .filter_map(|session| Some((session, agents.transcript_path(session)?)))
            .collect()
    };
    let mut events = Vec::new();
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
            events.extend(daemon.agents.lock().observe_progress(session, &progress));
        }
    }
    events
}

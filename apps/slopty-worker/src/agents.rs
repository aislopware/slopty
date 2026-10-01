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
//!
//! Each tick also hands the worker the conversation each session holds
//! (`AgentTable::resumable`), which it keeps so a reboot can resume it
//! (`slopty_worker::restore`), and tells the sleep policy how many agents are working with
//! their terminals still printing (`slopty_worker::wake`).
//!
//! Once, shortly after the daemon starts, the agents it found already running (a worker
//! restarted under its shells) get back what only hooks had said before the restart, from
//! Claude Code's own list of its sessions (`slopty_agent::roster`, `AgentTable::recover`).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use slopty_agent::detect::Program;
use slopty_agent::transcript::{self, Tail};
use slopty_agent::{Discovery, Observation};
use slopty_core::SessionId;
use slopty_proto::WorkerMsg;
use slopty_proto::agent::{AgentEvent, AgentStatus};
use slopty_worker::session::Probe;

use crate::Daemon;

/// How often every session's foreground process and title are read.
const TICK: Duration = Duration::from_millis(750);

/// The tick after which the agents already running are recovered from Claude Code's own list:
/// by then every session's foreground process has been read.
const RECOVER_AT_TICK: u64 = 2;

/// How long `claude agents --json` may take, login shell included.
const ROSTER_WAIT: Duration = Duration::from_secs(10);

/// Every how many ticks an agent whose transcript is already known is looked up again:
/// `/clear` and `/resume` start a new file, and the tail has to move with it. Finding one for
/// the first time happens on every tick; this is the cost of a `read_dir` per known agent.
const REDISCOVER_EVERY: u64 = 8;

/// Watch every session for an agent nobody told us about, until the daemon stops.
pub async fn watch(daemon: Daemon) -> ! {
    let home = slopty_platform::dirs::home();
    let mut tails: HashMap<SessionId, Tail> = HashMap::new();
    let mut ticks: u64 = 0;
    let mut quiet = slopty_worker::wake::Quiet::default();
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let live = daemon.worker.probe().await;
        // Sessions the worker no longer runs are gone whatever their last signal said: an agent
        // whose terminal exited never reports a `SessionEnd` hook.
        let ids: Vec<SessionId> = live.iter().map(|(id, _probe)| *id).collect();
        let running: HashSet<SessionId> = ids.iter().copied().collect();
        tails.retain(|session, _tail| running.contains(session));
        let unlooked = daemon.handoffs.lock().retain(&ids);
        for session in unlooked {
            let _sent = daemon.presence.send((session, false));
        }
        {
            let mut agents = daemon.agents.lock();
            let mut events = Vec::new();
            let mut loosened = Vec::new();
            for (session, probe) in live {
                events.extend(agents.observe(session, &observation(probe)));
                loosened.extend(agents.loosening_report(session));
            }
            for report in loosened {
                let _sent = daemon.reports.send(report);
            }
            events.extend(agents.retain(&ids));
            // Sent before anything is awaited: a hook arriving on the control socket while we
            // read files has already broadcast something newer.
            broadcast(&daemon, &agents, events);
            drop(agents);
        }

        keep_awake(&daemon, &mut quiet).await;
        ticks = ticks.wrapping_add(1);
        if ticks == RECOVER_AT_TICK && !daemon.agents.lock().sessions_with_agents().is_empty() {
            tokio::spawn(recover(daemon.clone(), home.clone()));
        }
        for (session, path) in
            discover(&daemon, &home, ticks.is_multiple_of(REDISCOVER_EVERY)).await
        {
            if daemon.agents.lock().set_transcript_path(session, &path) {
                // A conversation the human cleared or resumed: read the new file from its top.
                tails.remove(&session);
            }
        }
        // The conversation each session holds, kept for a reboot to resume.
        let resumable: Vec<_> = {
            let agents = daemon.agents.lock();
            ids.iter().map(|session| (*session, agents.resumable(*session))).collect()
        };
        for (session, agent) in resumable {
            daemon.worker.keep_agent(session, agent);
        }
        follow(&daemon, &mut tails).await;
    }
}

/// Tell the sleep policy how many agents are working (a turn, a tool, or background work out)
/// that showed signs of work within its cap: their terminals printing, and for a paused turn
/// the processor time of the commands it left running (read off the runtime's blocking pool).
async fn keep_awake(daemon: &Daemon, quiet: &mut slopty_worker::wake::Quiet) {
    let working: Vec<(AgentEvent, Option<i32>)> = {
        let agents = daemon.agents.lock();
        agents
            .snapshot()
            .into_iter()
            .filter(|event| match event.status {
                AgentStatus::Working | AgentStatus::Tool { .. } => true,
                AgentStatus::Waiting { tasks, .. } => tasks > 0,
                _ => false,
            })
            .map(|event| {
                let pid = agents.pid(event.session);
                (event, pid)
            })
            .collect()
    };
    let sampled = sample(working, slopty_core::WallMs::now()).await;
    let signs: Vec<slopty_worker::wake::Signs> = sampled
        .into_iter()
        .filter_map(|(session, paused)| {
            let output = daemon.worker.get(session).ok()?.activity().borrow().output;
            Some(slopty_worker::wake::Signs { session, output, paused })
        })
        .collect();
    let at_work = quiet.working(&signs, std::time::Instant::now());
    daemon.wake.lock().agents(at_work);
}

/// Each working agent's session, and for a paused turn how long it has waited and the processor
/// time of the commands it left running.
///
/// Only a paused turn's commands are read, on the blocking pool. With none, nothing goes there:
/// this runs every tick, and a trip each time kept every idle pool thread from reaching tokio's
/// keep-alive, so the pool held the most threads the worker ever needed at once.
async fn sample(
    working: Vec<(AgentEvent, Option<i32>)>,
    now: slopty_core::WallMs,
) -> Vec<(SessionId, Option<(Duration, u64)>)> {
    let paused =
        working.iter().any(|(event, _pid)| matches!(event.status, AgentStatus::Waiting { .. }));
    let read = move || {
        working
            .into_iter()
            .map(|(event, pid)| {
                let paused = matches!(event.status, AgentStatus::Waiting { .. }).then(|| {
                    let cpu = pid
                        .and_then(|pid| u32::try_from(pid).ok())
                        .map_or(0, slopty_worker::ports::descendants_cpu);
                    (now.since(event.since_ms), cpu)
                });
                (event.session, paused)
            })
            .collect::<Vec<_>>()
    };
    if paused { tokio::task::spawn_blocking(read).await.unwrap_or_default() } else { read() }
}

/// Put back what only the hooks had said of the agents already running when the daemon
/// started, from `claude agents --json`.
async fn recover(daemon: Daemon, home: PathBuf) {
    let Some(out) =
        slopty_worker::caps::agent_output("claude", &slopty_agent::roster::ARGS, ROSTER_WAIT).await
    else {
        tracing::debug!("claude agents --json gave nothing; agents recover from their next hook");
        return;
    };
    let listed = match slopty_agent::roster::parse(&String::from_utf8_lossy(&out)) {
        Ok(listed) => listed,
        Err(e) => {
            tracing::warn!(error = %e, "claude agents --json did not read");
            return;
        }
    };
    let settings = slopty_agent::hooks::settings_path(&home);
    let hooked = tokio::task::spawn_blocking(move || {
        slopty_agent::hooks::registered(&settings).is_ok_and(|events| !events.is_empty())
    })
    .await
    .unwrap_or(false);
    let mut agents = daemon.agents.lock();
    let events = agents.recover(&listed, hooked);
    tracing::info!(
        listed = listed.len(),
        recovered = events.len(),
        "agents recovered from Claude Code's list"
    );
    broadcast(&daemon, &agents, events);
    drop(agents);
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
/// it. The files are read together in one trip to the blocking pool; what they say is applied
/// under the table's lock, where a hook that arrived meanwhile outranks it.
async fn follow(daemon: &Daemon, tails: &mut HashMap<SessionId, Tail>) {
    let reads: Vec<(SessionId, PathBuf, Tail)> = {
        let agents = daemon.agents.lock();
        agents
            .sessions_with_agents()
            .into_iter()
            .filter_map(|session| {
                let path = agents.transcript_path(session)?;
                Some((session, path, tails.remove(&session).unwrap_or_default()))
            })
            .collect()
    };
    if reads.is_empty() {
        return;
    }
    let Ok(read) = tokio::task::spawn_blocking(move || read_tails(reads)).await else { return };
    let mut said = Vec::new();
    for (session, tail, read) in read {
        tails.insert(session, tail);
        match read {
            Ok(read) if read.progress.is_some() || read.mode.is_some() => {
                said.push((session, read));
            }
            Ok(_) => {}
            Err(e) => tracing::debug!(%session, error = %e, "agent transcript read"),
        }
    }
    if said.is_empty() {
        return;
    }
    let mut agents = daemon.agents.lock();
    let mut events = Vec::new();
    for (session, read) in &said {
        // The mode first, so a status event that follows carries it too.
        let moded = read.mode.as_deref().and_then(|mode| agents.hear_mode(*session, mode));
        let progressed = read.progress.as_ref().and_then(|p| agents.observe_progress(*session, p));
        events.extend(progressed.or(moded));
    }
    broadcast(daemon, &agents, events);
    drop(agents);
}

/// Read each transcript from where its tail left off: the tails back, beside what the new
/// records say. Blocking.
fn read_tails(
    reads: Vec<(SessionId, PathBuf, Tail)>,
) -> Vec<(SessionId, Tail, std::io::Result<transcript::Read>)> {
    reads
        .into_iter()
        .map(|(session, path, mut tail)| {
            let read = tail.read(&path);
            (session, tail, read)
        })
        .collect()
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

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    /// An agent of `status`, with no process known.
    fn at(status: AgentStatus) -> (AgentEvent, Option<i32>) {
        let event = AgentEvent {
            session: SessionId::new(),
            kind: slopty_proto::agent::AgentKind::ClaudeCode,
            status,
            agent_session: None,
            detail: None,
            attention: false,
            source: slopty_proto::agent::AgentSource::Transcript,
            since_ms: slopty_core::WallMs::ZERO,
            mode: None,
        };
        (event, None)
    }

    /// A tick with no paused turn starts no blocking-pool thread, and one with a paused turn
    /// reads its commands there. A runtime of one thread starts a thread only for the pool.
    #[test]
    fn only_a_paused_turn_takes_the_blocking_pool() {
        let started = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .on_thread_start({
                let started = std::sync::Arc::clone(&started);
                move || {
                    started.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            })
            .build()
            .unwrap();
        let now = slopty_core::WallMs::now();
        let quiet = runtime.block_on(sample(Vec::new(), now));
        let busy = runtime.block_on(sample(vec![at(AgentStatus::Working)], now));
        assert!(quiet.is_empty());
        assert_eq!(busy.first().map(|(_session, paused)| *paused), Some(None));
        assert_eq!(started.load(std::sync::atomic::Ordering::Relaxed), 0);

        let waiting = AgentStatus::Waiting { tasks: 1, crons: 0 };
        let paused = runtime.block_on(sample(vec![at(waiting)], now));
        assert!(paused.first().is_some_and(|(_session, paused)| paused.is_some()));
        assert_eq!(started.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    /// What a tick's transcript reads cost for `AGENTS` agents with nothing new to say: one
    /// trip to the blocking pool per agent, one after the other (the path this replaced),
    /// against one trip for them all.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "measurement"]
    async fn transcript_tick_cost() {
        const AGENTS: usize = 16;
        const TICKS: u32 = 2_000;
        let dir = tempfile::tempdir().unwrap();
        let record = r#"{"type":"assistant","uuid":"a1","message":{"content":[{"type":"text","text":"done"}]}}"#;
        let agents: Vec<(SessionId, PathBuf)> = (0..AGENTS)
            .map(|n| {
                let path = dir.path().join(format!("{n}.jsonl"));
                std::fs::write(&path, format!("{record}\n")).unwrap();
                (SessionId::new(), path)
            })
            .collect();
        let mut tails: HashMap<SessionId, Tail> = HashMap::new();
        for (session, path) in &agents {
            let mut tail = Tail::default();
            tail.read(path).unwrap();
            tails.insert(*session, tail);
        }
        for round in 1..=3 {
            let clock = Instant::now();
            for _ in 0..TICKS {
                for (session, path) in &agents {
                    let mut tail = tails.remove(session).unwrap_or_default();
                    let path = path.clone();
                    let (tail, _read) = tokio::task::spawn_blocking(move || {
                        let read = tail.read(&path);
                        (tail, read)
                    })
                    .await
                    .unwrap();
                    tails.insert(*session, tail);
                }
            }
            let one_each = clock.elapsed().as_secs_f64() * 1e6 / f64::from(TICKS);
            let clock = Instant::now();
            for _ in 0..TICKS {
                let reads = agents
                    .iter()
                    .map(|(s, p)| (*s, p.clone(), tails.remove(s).unwrap_or_default()))
                    .collect();
                let read = tokio::task::spawn_blocking(move || read_tails(reads)).await.unwrap();
                for (session, tail, _read) in read {
                    tails.insert(session, tail);
                }
            }
            let together = clock.elapsed().as_secs_f64() * 1e6 / f64::from(TICKS);
            eprintln!(
                "round {round}, {AGENTS} idle agents: a tick's reads {one_each:.1} -> {together:.1} µs"
            );
        }
    }
}

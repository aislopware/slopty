//! The TCP ports each session listens on, announced to every client when they change.
//!
//! [`slopty_host::ports::Trigger`] decides when a session is scanned; this task feeds it the
//! sessions' output hints and a look at every session each [`RESCAN`], and runs the scans.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use slopty_core::SessionId;
use slopty_host::ports::{RESCAN, listening};
use slopty_proto::HostMsg;

use crate::Daemon;

/// Scan sessions as the trigger says until the daemon stops.
pub async fn watch(daemon: Daemon) {
    let Some(mut hints) = daemon.host.take_port_hints() else { return };
    let mut look = tokio::time::interval(RESCAN);
    look.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // The process ptyd spawned for each session: the root of the tree scanned.
    let mut roots: HashMap<SessionId, u32> = HashMap::new();
    loop {
        let next = daemon.ports.lock().next_due();
        tokio::select! {
            hint = hints.recv() => {
                let Some(session) = hint else { return };
                daemon.ports.lock().hint(session, Instant::now());
            }
            _ = look.tick() => look_at_all(&daemon, &mut roots).await,
            () = until(next) => {}
        }
        let due = daemon.ports.lock().take_due(Instant::now());
        if due.is_empty() {
            continue;
        }
        if due.iter().any(|s| !roots.contains_key(s))
            && let Ok(pids) = daemon.host.pids().await
        {
            roots = pids.into_iter().collect();
        }
        let trees: Vec<(SessionId, u32)> =
            due.iter().filter_map(|s| Some((*s, *roots.get(s)?))).collect();
        let found = tokio::task::spawn_blocking(move || {
            trees.into_iter().map(|tree| (tree.0, listening(&[tree]))).collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
        for (session, ports) in found {
            let changed = daemon.ports.lock().scanned(session, ports);
            if let Some(ports) = changed {
                tracing::info!(%session, ports = ?ports.iter().map(|p| p.number).collect::<Vec<_>>(), "listening ports");
                let _sent = daemon.events.send(HostMsg::Ports { session, ports });
            }
        }
    }
}

/// Sleep until `at`, or forever when nothing is owed.
async fn until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at.into()).await,
        None => std::future::pending().await,
    }
}

/// Tell the trigger which sessions run a program in the foreground (anything but the process
/// ptyd spawned), and forget the sessions that are gone.
async fn look_at_all(daemon: &Daemon, roots: &mut HashMap<SessionId, u32>) {
    let probes = daemon.host.probe().await;
    let live: HashSet<SessionId> = probes.iter().map(|(s, _probe)| *s).collect();
    if live.iter().any(|s| !roots.contains_key(s))
        && let Ok(pids) = daemon.host.pids().await
    {
        *roots = pids.into_iter().collect();
    }
    roots.retain(|s, _pid| live.contains(s));
    let now = Instant::now();
    let mut trigger = daemon.ports.lock();
    trigger.retain(|s| live.contains(&s));
    for (session, probe) in probes {
        let root = roots.get(&session).copied();
        let busy = probe
            .foreground
            .as_ref()
            .zip(root)
            .is_some_and(|(fg, root)| u32::try_from(fg.pid).ok() != Some(root));
        trigger.look(session, busy, now);
    }
}

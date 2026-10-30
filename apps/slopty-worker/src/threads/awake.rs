//! The threads that keep the machine awake (`slopty_worker::wake`).
//!
//! The host's threads are looked over whenever its table changes (a turn begins or ends, an
//! agent's process goes), at most every [`GAP`], and every [`TICK`] besides, for the silence
//! that only time shows: each one at work, working or paused on its own background work with its
//! agent's process running, is sampled ([`wake::signs`]), and the sleep policy is told how many
//! count ([`wake::Quiet`]). Nothing is sampled while the person's
//! policy lets no agent hold the machine. The threads are the record Slopty keeps of the agents
//! it drives, so a turn of `claude -p`, Codex, pi or an ACP agent holds the machine with nobody
//! linked, as the server's tables already do for its own Mac (`slopty_server::hub::awake`).

use std::time::Duration;

use slopty_core::WallMs;
use slopty_worker::thread::Host;
use slopty_worker::wake;
use tokio::time::Instant;

use crate::Daemon;

/// How often the threads are looked over with nothing changed: a turn gone silent stops holding
/// the machine by time alone ([`wake::SILENT_CAP`]).
pub const TICK: Duration = Duration::from_secs(5);

/// The least time between two looks a change brought: a streaming turn changes the table at every
/// delta.
pub const GAP: Duration = Duration::from_millis(200);

/// Keep telling the sleep policy how many of `host`'s threads are at work, for as long as the
/// daemon runs.
pub async fn watch(daemon: Daemon, host: Host) -> ! {
    let mut quiet = wake::Quiet::default();
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut table = Some(host.table_watch());
    loop {
        tokio::select! {
            _ticked = tick.tick() => {}
            changed = async {
                match table.as_mut() {
                    Some(table) => table.changed().await,
                    None => std::future::pending().await,
                }
            } => {
                // A host whose table is gone changes no more: the tick alone goes on.
                if changed.is_err() {
                    table = None;
                }
                tokio::time::sleep(GAP).await;
            }
        }
        if !daemon.wake.lock().counts_agents() {
            quiet = wake::Quiet::default();
            continue;
        }
        let now = WallMs::now();
        let host = host.clone();
        let Ok(signs) =
            tokio::task::spawn_blocking(move || host.visit(|state| wake::signs(state, now))).await
        else {
            continue;
        };
        let at_work = quiet.working(&signs, Instant::now());
        daemon.wake.lock().agents(at_work);
    }
}

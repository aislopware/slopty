//! The threads that keep the machine awake (`slopty_worker::wake`).
//!
//! Every [`TICK`] the host's threads are looked over: each one at work, working or paused on its
//! own background work with its agent's process running, is sampled ([`wake::signs`]), and the
//! sleep policy is told how many count ([`wake::Quiet`]). Nothing is sampled while the person's
//! policy lets no agent hold the machine. The threads are the record Slopty keeps of the agents
//! it drives, so a turn of `claude -p`, Codex, pi or an ACP agent holds the machine with nobody
//! linked, as the server's tables already do for its own Mac (`slopty_server::hub::awake`).

use std::time::{Duration, Instant};

use slopty_core::WallMs;
use slopty_worker::thread::Host;
use slopty_worker::wake;

use crate::Daemon;

/// How often the threads are looked over. Idle sleep comes after minutes at the least, so a turn
/// that just began holds the machine long before it could sleep.
pub const TICK: Duration = Duration::from_secs(5);

/// Keep telling the sleep policy how many of `host`'s threads are at work, for as long as the
/// daemon runs.
pub async fn watch(daemon: Daemon, host: Host) -> ! {
    let mut quiet = wake::Quiet::default();
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
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

//! What a stack costs to bring up, which every case in this suite pays: the server, the
//! daemons and the app, until the app's link to the worker the server lists is up and its
//! first shell has a prompt; and the server's own share of it.

use std::time::{Duration, Instant};

use slopty_e2e::Stack;
use slopty_e2e::harness::ServerDaemon;

/// Stacks brought up in a row.
const RUNS: usize = 7;
/// How long the first shell's prompt may take.
const STEP: Duration = Duration::from_secs(20);

fn median(mut samples: Vec<Duration>) -> f64 {
    samples.sort_unstable();
    samples.get(samples.len() / 2).map_or(0.0, |d| d.as_secs_f64() * 1e3)
}

/// Prints the medians over [`RUNS`] stacks: to the link up, and to the first shell's prompt;
/// then over [`RUNS`] servers started alone, until they print where they listen.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_stack_comes_up_in_measured_time() {
    let (mut linked, mut prompted) = (Vec::new(), Vec::new());
    for _ in 0..RUNS {
        let started = Instant::now();
        let mut stack = Stack::launch("e2e-start").await.unwrap();
        linked.push(started.elapsed());
        stack
            .driver
            .wait_for("the first shell with a prompt", STEP, |d| {
                d.terminals.iter().any(|t| t.rows.iter().any(|r| !r.is_empty()))
            })
            .await
            .unwrap();
        prompted.push(started.elapsed());
        stack.shutdown().await;
    }
    let mut served = Vec::new();
    for _ in 0..RUNS {
        let dir = tempfile::tempdir().unwrap();
        let started = Instant::now();
        let mut server = ServerDaemon::start(dir.path(), "e2e-start", "warn").await.unwrap();
        served.push(started.elapsed());
        server.kill().await;
    }
    println!(
        "MEASURE stack start: linked {:.0} ms, first prompt {:.0} ms, the server alone {:.0} ms \
         (medians of {RUNS})",
        median(linked),
        median(prompted),
        median(served)
    );
}

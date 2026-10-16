//! A link whose control stream stalls while its connection lives: the real app, server and
//! worker, the worker built with its stall seam (`SLOPTY_E2E_STALL_CONTROL`), which stops every
//! control stream of the worker from reading or writing while a file exists, and leaves the
//! connection, its keep-alives and its datagrams flowing. Only the links' pings can tell
//! (`slopty_net::liveness`): the app relinks the worker, and the server ends its lease, within
//! six seconds of the last answer; once the stall lifts the worker is connected again at once.
//!
//! Timed from the stall to the relink and from the lift to the next live link, over a few
//! stalls; `docs/MEASUREMENTS.md` has the numbers.

use std::time::{Duration, Instant};

use slopty_e2e::{Driver, Dump, Stack};

/// The worker's seam: control streams stall while the file it names exists.
const STALL_ENV: &str = "SLOPTY_E2E_STALL_CONTROL";
/// How long anything may take before the test fails.
const STEP: Duration = Duration::from_secs(20);
/// How often the timing loop reads the app.
const QUICK: Duration = Duration::from_millis(5);
/// Stalls timed.
const STALLS: usize = 3;
/// The most a relink may take from the stall: six seconds from the last answer, which came
/// before the stall, plus a look and a read of the app.
const RELINK_BOUND: Duration = Duration::from_millis(6_800);
/// The most the server may take to list the worker unreachable from the stall: its own six
/// seconds and a look, and a read of the CLI.
const SERVER_BOUND: Duration = Duration::from_millis(7_500);
/// The most the link may take to be live again once the stall lifts.
const BACK_BOUND: Duration = Duration::from_secs(2);

fn status(d: &Dump) -> &str {
    d.workers.first().map_or("", |w| w.status.as_str())
}

fn links(d: &Dump) -> u64 {
    d.workers.first().map_or(0, |w| w.links)
}

/// Read the app until `pred` holds, at most `bound` after `since`: how long from `since`.
async fn until(
    drv: &mut Driver,
    what: &str,
    since: Instant,
    bound: Duration,
    pred: impl Fn(&Dump) -> bool,
) -> Duration {
    let deadline = since.checked_add(bound).unwrap();
    loop {
        let d = drv.dump().await.unwrap();
        if pred(&d) {
            return since.elapsed();
        }
        assert!(Instant::now() < deadline, "{what} not within {bound:?}; last state:\n{d:#?}");
        tokio::time::sleep(QUICK).await;
    }
}

/// From the stall: when the app first showed the link silent, if it did before it gave the link
/// up (a server that ended the lease first relinks it at once), and when it gave it up.
async fn until_relinked(drv: &mut Driver, stalled: Instant) -> (Option<Duration>, Duration) {
    let deadline = stalled.checked_add(STEP).unwrap();
    let mut silent = None;
    loop {
        let d = drv.dump().await.unwrap();
        let now = status(&d);
        if now.starts_with("silent") {
            silent.get_or_insert_with(|| stalled.elapsed());
        } else if now != "connected" {
            return (silent, stalled.elapsed());
        }
        assert!(Instant::now() < deadline, "never relinked; last state:\n{d:#?}");
        tokio::time::sleep(QUICK).await;
    }
}

fn worst(took: &[Duration]) -> Duration {
    took.iter().copied().max().unwrap_or_default()
}

fn report(what: &str, took: &[Duration]) {
    let ms: Vec<u128> = took.iter().map(Duration::as_millis).collect();
    println!("MEASURE {what}, {} stalls: {ms:?} ms, worst {:?}", took.len(), worst(took));
}

/// A stalled control stream under a connection that still carries datagrams is found by its
/// pings: the app shows the worker silent, then relinks it within six seconds of the last
/// answer, and the server marks it unreachable as fast. Once the stall lifts the worker is
/// connected again on a new link within two seconds, every time.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn link_with_datagrams_but_a_stalled_stream() {
    let stall_dir = tempfile::tempdir().unwrap();
    let stall = stall_dir.path().join("stall");
    let stall_path = stall.to_string_lossy().into_owned();
    let mut stack = Stack::launch_with("e2e-worker", &[(STALL_ENV, &stall_path)]).await.unwrap();
    stack.driver.wait_for("the worker", STEP, |d| status(d) == "connected").await.unwrap();

    let (mut silent, mut relinked, mut back) = (Vec::new(), Vec::new(), Vec::new());
    for n in 0..STALLS {
        let drv = &mut stack.driver;
        let seen = links(&drv.dump().await.unwrap());
        std::fs::write(&stall, b"").unwrap();
        let stalled = Instant::now();
        // Relinking, or held as the server has it once it ended the lease: either way no
        // longer the link that stalled.
        let (shown_silent, gone) = until_relinked(drv, stalled).await;
        silent.extend(shown_silent);
        relinked.push(gone);
        if n == 0 {
            stack.server_lists_worker("unreachable", SERVER_BOUND).await.unwrap();
            let lease = stalled.elapsed();
            println!("MEASURE a stalled worker stream, the server's lease ended within {lease:?}");
            assert!(lease <= SERVER_BOUND, "the server ended the lease after {lease:?}");
        }
        std::fs::remove_file(&stall).unwrap();
        let lifted = Instant::now();
        let drv = &mut stack.driver;
        let live = |d: &Dump| links(d) > seen && status(d) == "connected";
        back.push(until(drv, "connected again", lifted, STEP, live).await);
        // Settled before the next stall: the pings run on the new link.
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    report("a stalled control stream shown silent", &silent);
    report("a stalled control stream relinked", &relinked);
    report("a lifted stall connected again", &back);
    assert!(worst(&relinked) <= RELINK_BOUND, "relinked within six seconds: {relinked:?}");
    assert!(!silent.is_empty(), "the bar said silent before a relink at least once");
    assert!(worst(&back) <= BACK_BOUND, "connected again within two seconds: {back:?}");
    stack.shutdown().await;
}

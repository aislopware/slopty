//! A link whose control stream stalls while its connection lives: the real app, server and
//! worker, the worker built with its stall seam (`SLOPTY_E2E_STALL_CONTROL`), which stops every
//! control stream of the worker from reading or writing while a file exists, and leaves the
//! connection, its keep-alives and its datagrams flowing. Only the links' pings can tell
//! (`slopty_net::liveness`): the app relinks the worker, and the server ends its lease, within
//! six seconds of the last answer; once the stall lifts the worker is connected again at once.
//!
//! Timed from the stall to the relink and from the lift to the next live link, over a few
//! stalls; `docs/MEASUREMENTS.md` has the numbers.
//!
//! Each change is seen by a look (a dump of the app, a run of the CLI), and on a starved machine
//! a look takes seconds of its own. So each time is a span: after the start of the last look that
//! still showed the old state, by the end of the first that showed the new. A bound fails only
//! when the change was certainly late, its span starting past it: the product's time, not the
//! looks'. Both ends are reported.

use std::time::{Duration, Instant};

use slopty_e2e::harness::slopty_json;
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

/// When a change happened, from some start: after `after` (the start of the last look that did
/// not see it) and by `by` (the end of the first that did).
#[derive(Clone, Copy, Debug)]
struct Span {
    after: Duration,
    by: Duration,
}

/// Read the app until `pred` holds, at most `bound` after `since`: when it came to hold.
async fn until(
    drv: &mut Driver,
    what: &str,
    since: Instant,
    bound: Duration,
    pred: impl Fn(&Dump) -> bool,
) -> Span {
    let deadline = since.checked_add(bound).unwrap();
    let mut after = Duration::ZERO;
    loop {
        let started = since.elapsed();
        let d = drv.dump().await.unwrap();
        if pred(&d) {
            return Span { after, by: since.elapsed() };
        }
        after = started;
        assert!(Instant::now() < deadline, "{what} not within {bound:?}; last state:\n{d:#?}");
        tokio::time::sleep(QUICK).await;
    }
}

/// Run the CLI until the server lists the worker `liveness`, at most `bound` after `since`: when
/// it came to.
async fn until_listed(stack: &Stack, liveness: &str, since: Instant, bound: Duration) -> Span {
    let cli = stack.dir.path().join("cli");
    let deadline = since.checked_add(bound).unwrap();
    let mut after = Duration::ZERO;
    loop {
        let started = since.elapsed();
        let workers = slopty_json(stack.server.address(), &cli, &["workers"], b"").await.unwrap();
        if workers.as_array().is_some_and(|all| all.iter().any(|w| w["liveness"] == liveness)) {
            return Span { after, by: since.elapsed() };
        }
        after = started;
        assert!(Instant::now() < deadline, "no worker {liveness} within {bound:?}: {workers}");
        tokio::time::sleep(QUICK).await;
    }
}

/// From the stall: when the app first showed the link silent, if it did before it gave the link
/// up (a server that ended the lease first relinks it at once), and when it gave it up.
async fn until_relinked(drv: &mut Driver, stalled: Instant) -> (Option<Span>, Span) {
    let deadline = stalled.checked_add(STEP).unwrap();
    let mut silent = None;
    let mut after = Duration::ZERO;
    loop {
        let started = stalled.elapsed();
        let d = drv.dump().await.unwrap();
        let now = status(&d);
        if now.starts_with("silent") {
            silent.get_or_insert_with(|| Span { after, by: stalled.elapsed() });
        } else if now != "connected" {
            return (silent, Span { after, by: stalled.elapsed() });
        }
        after = started;
        assert!(Instant::now() < deadline, "never relinked; last state:\n{d:#?}");
        tokio::time::sleep(QUICK).await;
    }
}

/// The worst of `spans`: the latest `by`, as the looks saw it, and the latest `after`, which is
/// what a bound holds the product to.
fn worst(spans: &[Span]) -> (Duration, Duration) {
    let by = spans.iter().map(|s| s.by).max().unwrap_or_default();
    let after = spans.iter().map(|s| s.after).max().unwrap_or_default();
    (by, after)
}

fn report(what: &str, spans: &[Span]) {
    let ms: Vec<String> =
        spans.iter().map(|s| format!("{}-{}", s.after.as_millis(), s.by.as_millis())).collect();
    let (by, after) = worst(spans);
    let n = spans.len();
    println!("MEASURE {what}, {n} stalls (after-by ms): {ms:?}, worst by {by:?}, after {after:?}");
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
            let lease = until_listed(&stack, "unreachable", stalled, STEP).await;
            println!(
                "MEASURE a stalled worker stream, the server's lease ended after {:?}, by {:?}",
                lease.after, lease.by
            );
            assert!(lease.after <= SERVER_BOUND, "the server ended the lease after {lease:?}");
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
    assert!(worst(&relinked).1 <= RELINK_BOUND, "relinked within six seconds: {relinked:?}");
    assert!(!silent.is_empty(), "the bar said silent before a relink at least once");
    assert!(worst(&back).1 <= BACK_BOUND, "connected again within two seconds: {back:?}");
    stack.shutdown().await;
}

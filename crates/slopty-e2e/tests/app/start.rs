//! What a stack costs to bring up, which every case in this suite pays: the server, the
//! daemons and the app, until the app's link to the worker the server lists is up and its
//! first shell has a prompt; and the server's own share of it. Then the two budgets of an app
//! that stays open all day: a relaunch onto a day's layout, to its last tile live, and what the
//! app costs at rest.

use std::time::{Duration, Instant};

use slopty_e2e::harness::{SecondWorker, ServerDaemon};
use slopty_e2e::{Dump, Stack};

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

/// Shells on each of the two workers: 20 tiles, a day's layout (`SLOPTY_E2E_PER_WORKER` to
/// compare another).
fn per_worker() -> usize {
    let asked = std::env::var("SLOPTY_E2E_PER_WORKER").ok().and_then(|s| s.parse().ok());
    asked.unwrap_or(10).max(1)
}
/// Relaunches timed.
const RELAUNCHES: usize = 5;
/// How long every tile of a relaunch may take to be live.
const LIVE: Duration = Duration::from_secs(60);
/// How long the app is left before its rest is read: links settled, every shell at its prompt.
const SETTLE: Duration = Duration::from_secs(10);
/// How long its rest is read over (`SLOPTY_E2E_REST_SECS`, 60 by default).
fn rest() -> Duration {
    let secs = std::env::var("SLOPTY_E2E_REST_SECS").ok().and_then(|s| s.parse().ok());
    Duration::from_secs(secs.unwrap_or(60))
}

/// The relaunch budget: about three times the median measured on the Mac Studio, release
/// (312 ms; `docs/MEASUREMENTS.md`, "the daily budgets"). A debug build is held to it too.
const RELAUNCH_BUDGET: Duration = Duration::from_secs(1);
/// Wakeups a second of the release app at rest: 11.4 to 11.8 measured, now that the display
/// link stops while the window wants no frames (it was 72 while the link ran at rest), so the
/// budget is about two and a half times that.
const WAKEUPS_BUDGET: f64 = 30.0;
/// CPU power of the release app at rest, in milliwatts: about three times the 0.6 to 1.7
/// measured.
const IDLE_MW_BUDGET: f64 = 5.0;
/// The release app's footprint at rest, in megabytes: 62 to 71 measured, so a launch's
/// leftovers growing back toward the 96 the symbols' prewarm once left fail it.
const FOOTPRINT_BUDGET_MB: u64 = 85;

/// Terminal tiles drawing a frame their worker sent, with both workers' links up: a tile
/// showing only what this device kept of it is not live yet.
fn live(d: &Dump) -> usize {
    if d.workers.iter().filter(|w| w.status == "connected").count() < 2 {
        return 0;
    }
    d.terminals.iter().filter(|t| t.epoch.is_some() && t.rows.iter().any(|r| !r.is_empty())).count()
}

/// A terminal item of `worker`'s in the dump: its session.
fn shell_on(d: &Dump, worker: &str) -> Option<String> {
    d.items
        .iter()
        .find(|i| i.worker == worker && i.kind == "terminal")
        .and_then(|i| i.session.clone())
}

/// The relaunch budget and the idle budget, measured.
///
/// Ten shells on each of two workers, one of them behind a relay as another Mac is; the app is
/// killed (no goodbye, as a crash or an update leaves it) and started again [`RELAUNCHES`]
/// times, each timed from its start to its 20th tile drawing its shell, as the person sees
/// the morning's layout come back. Then the app is left alone and its CPU energy
/// (`ri_energy_nj`) and wakeups are read over [`rest`]. The binaries are the build's in
/// `SLOPTY_E2E_BIN_DIR`: the idle figures hold only for a release build, so only that one is
/// held to their budgets.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_relaunch_onto_twenty_tiles_and_the_app_at_rest_are_within_budget() {
    let mut stack = Stack::launch("e2e-start").await.unwrap();
    let second =
        SecondWorker::launch("second", slopty_shape::Link::CLEAR, &stack.server).await.unwrap();
    let drv = &mut stack.driver;
    let d = drv
        .wait_for("both workers connected, a shell on each", LIVE, |d| {
            d.workers.iter().filter(|w| w.status == "connected").count() == 2
                && shell_on(d, "second").is_some()
                && d.items.iter().any(|i| i.worker != "second" && i.kind == "terminal")
        })
        .await
        .unwrap();
    let first = d.workers.iter().find(|w| w.name != "second").map(|w| w.name.clone()).unwrap();
    let per_worker = per_worker();
    let more = u32::try_from(per_worker - 1).unwrap();
    for worker in [first.as_str(), "second"] {
        let session = shell_on(&drv.dump().await.unwrap(), worker).unwrap();
        drv.reveal(&session).await.unwrap();
        drv.open(&[], more).await.unwrap();
    }
    let tiles = 2 * per_worker;
    drv.wait_for("every shell live", LIVE, |d| d.terminals.len() == tiles && live(d) == tiles)
        .await
        .unwrap();

    let mut times = Vec::new();
    for _ in 0..RELAUNCHES {
        stack.kill_app().await.unwrap();
        let started = Instant::now();
        stack.relaunch_app_unlinked().await.unwrap();
        // Read every 10 ms, not at the driver's 100 ms poll, which would round every run up.
        loop {
            let d = stack.driver.dump().await.unwrap();
            if d.terminals.len() == tiles && live(&d) == tiles {
                break;
            }
            assert!(started.elapsed() < LIVE, "every tile back and live: {:#?}", d.terminals);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        times.push(started.elapsed());
    }
    let relaunch = median(times.clone());
    let slowest = times.iter().max().copied().unwrap_or_default();
    println!(
        "MEASURE relaunch: {tiles} tiles on two workers live in {relaunch:.0} ms \
         (median of {RELAUNCHES}, slowest {} ms)",
        slowest.as_millis()
    );

    let pid =
        stack.app_ix.and_then(|ix| stack.children.get(ix)).and_then(tokio::process::Child::id);
    let pid = i32::try_from(pid.unwrap()).unwrap();
    tokio::time::sleep(SETTLE).await;
    let read = || slopty_testkit::process::usage(pid).unwrap();
    let (before, started) = (read(), Instant::now());
    tokio::time::sleep(rest()).await;
    let (after, over) = (read(), started.elapsed().as_secs_f64());
    // Nanojoules and cycles read as nanoseconds: joules and gigacycles as seconds.
    let joules = Duration::from_nanos(after.energy_nj.saturating_sub(before.energy_nj));
    let milliwatts = joules.as_secs_f64() * 1e3 / over;
    let woke = u32::try_from(after.wakeups.saturating_sub(before.wakeups)).unwrap_or(u32::MAX);
    let wakeups = f64::from(woke) / over;
    let gigacycles = Duration::from_nanos(after.cycles.saturating_sub(before.cycles));
    let megacycles = gigacycles.as_secs_f64() * 1e3 / over;
    let release = std::env::var_os("SLOPTY_E2E_BIN_DIR")
        .is_some_and(|dir| std::path::Path::new(&dir).ends_with("release"));
    println!(
        "MEASURE idle ({} build, {tiles} tiles, {over:.0} s): {milliwatts:.2} mW of CPU, \
         {wakeups:.1} wakeups/s, {megacycles:.2} M cycles/s, footprint {} MB (peak {} MB)",
        if release { "release" } else { "debug" },
        after.footprint / 1_000_000,
        after.peak_footprint / 1_000_000
    );
    // A wall-clock budget holds on the Mac it was set on. A hosted runner is a shared
    // three-core virtual Mac, which relaunched in 1990 ms (CI e2e run 37359580819): there the
    // time is printed above and only the counts are held.
    if std::env::var_os("GITHUB_ACTIONS").is_none() {
        assert!(relaunch <= RELAUNCH_BUDGET.as_secs_f64() * 1e3, "relaunch {relaunch:.0} ms");
    }
    if release {
        assert!(wakeups <= WAKEUPS_BUDGET, "{wakeups:.1} wakeups/s at rest");
        assert!(milliwatts <= IDLE_MW_BUDGET, "{milliwatts:.2} mW at rest");
        let footprint = after.footprint / 1_000_000;
        assert!(footprint <= FOOTPRINT_BUDGET_MB, "{footprint} MB of footprint at rest");
    }
    drop(second);
    stack.shutdown().await;
}

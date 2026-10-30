//! Live again at once after a resume: the real app, a real worker behind a path the test cuts
//! (`slopty_e2e::cut`), and the system's resume handed over the test socket, as the wake, the
//! unlock or the path monitor hands it. Nothing sleeps and no network changes.
//!
//! Timed from the event to the first live row: the worker's shell drawn again from a link that
//! came up after the cut. Without a resume, only the dead link's silence says it is dead: no
//! pong for 4 s shows it silent and 6 s relinks it (`slopty_net::liveness`);
//! `docs/MEASUREMENTS.md` has both. Each step waits on the event it means and prints how long it
//! took; none is held to a time, since an app, a worker and a guest on one loaded machine move
//! every number (`docs/decisions/testing.md`, "A test waits on the event it means"). "At once"
//! is proved where only its own code runs, in `slopty_app::workers`: a probe waits at most a
//! second (`a_probe_is_sized_from_the_round_trip`), gives a dead link up at the first look at
//! its deadline (`a_probe_gives_a_dead_link_up_at_its_deadline`), and a worker whose link is
//! gone is dialled now (`a_resume_probes_a_live_link_and_dials_a_dead_one`).

use std::time::{Duration, Instant};

use slopty_e2e::{Command, Driver, Dump, Stack};

/// How long anything may take before the test fails.
const STEP: Duration = Duration::from_secs(20);
/// How often the timing loop reads the app: fine enough for a quarter-second answer.
const QUICK: Duration = Duration::from_millis(5);
/// Deaths timed with a resume after each.
const RESUMED: usize = 20;
/// Deaths timed with nothing said: each takes the silence's six seconds.
const SILENT: usize = 5;

fn links(d: &Dump) -> u64 {
    d.workers.first().map_or(0, |w| w.links)
}

fn doubts(d: &Dump) -> u64 {
    d.workers.first().map_or(0, |w| w.doubts)
}

fn status(d: &Dump) -> &str {
    d.workers.first().map_or("", |w| w.status.as_str())
}

/// The worker's shell drawn from a link newer than `seen`.
fn live_after(d: &Dump, seen: u64) -> bool {
    links(d) > seen
        && status(d) == "connected"
        && d.terminals.iter().any(|t| t.rows.iter().any(|r| !r.is_empty()))
}

/// Time from `since` to the first live row after `seen` links, and whether the tiles were set
/// back on the way: the app's own count of its doubts moved past `before`, so a doubt shorter
/// than a look (a starved machine's dumps come seconds apart) is not missed.
async fn until_live(drv: &mut Driver, seen: u64, before: u64, since: Instant) -> (Duration, bool) {
    let deadline = since.checked_add(STEP).unwrap();
    loop {
        let d = drv.dump().await.unwrap();
        if live_after(&d, seen) {
            return (since.elapsed(), doubts(&d) > before);
        }
        assert!(Instant::now() < deadline, "never live again; last state:\n{d:#?}");
        tokio::time::sleep(QUICK).await;
    }
}

fn percentile(sorted: &[Duration], p: usize) -> f64 {
    let at = sorted.len().saturating_sub(1).saturating_mul(p).div_ceil(100);
    sorted.get(at).map_or(0.0, |d| d.as_secs_f64() * 1e3)
}

fn report(what: &str, mut took: Vec<Duration>) {
    took.sort_unstable();
    let (p50, p95) = (percentile(&took, 50), percentile(&took, 95));
    println!("MEASURE {what}, {} deaths: p50 {p50:.0} ms p95 {p95:.0} ms", took.len());
}

/// A live link answers a resume's probe and stays; a dead one is given up and dialled again
/// once the probe goes unanswered, its tiles set back meanwhile, and its shell is live again.
#[tokio::test]
#[ignore = "live: cargo xtask e2e app"]
async fn a_resume_brings_a_dead_link_back_at_once() {
    let (mut stack, cut) = Stack::launch_behind_cut("e2e-worker").await.unwrap();
    let drv = &mut stack.driver;
    drv.wait_for("the worker", STEP, |d| status(d) == "connected").await.unwrap();
    drv.keys("cmd-d").await.unwrap();
    let d =
        drv.wait_for("the split's shell with a prompt", STEP, |d| live_after(d, 0)).await.unwrap();

    // A live link: probed (its tiles set back at once after a wake), it answers, and nothing is
    // dialled again.
    let (seen, before) = (links(&d), doubts(&d));
    drv.ok(&Command::Resume { what: "woke".into() }).await.unwrap();
    let d = drv
        .wait_for("the probe answered", STEP, |d| doubts(d) > before && status(d) == "connected")
        .await
        .unwrap();
    assert_eq!(links(&d), seen, "a live link is kept");

    // The link dies with nothing said: only its silence tells.
    let mut silent = Vec::with_capacity(SILENT);
    for _ in 0..SILENT {
        let d = drv.dump().await.unwrap();
        cut.cut();
        let (took, _doubted) = until_live(drv, links(&d), doubts(&d), Instant::now()).await;
        silent.push(took);
    }

    // The same death, then the resume the system would hand the app on waking, or on the path
    // moving.
    let mut resumed = Vec::with_capacity(RESUMED);
    for (n, what) in ["woke", "path-changed"].iter().cycle().take(RESUMED).enumerate() {
        let d = drv.dump().await.unwrap();
        cut.cut();
        let since = Instant::now();
        drv.ok(&Command::Resume { what: (*what).into() }).await.unwrap();
        let (took, doubted) = until_live(drv, links(&d), doubts(&d), since).await;
        assert!(doubted, "death {n} ({what}): the tiles were set back while it relinked");
        resumed.push(took);
    }

    report("a dead link found by its silence", silent);
    report("a dead link found by a resume's probe", resumed);
    assert!(cut.dropped() > 0, "the cuts dropped what the dead links sent");
    stack.shutdown().await;
}

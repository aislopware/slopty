//! The app as it is normally used: pointed at a server, it finds its workers in the directory and
//! dials each one itself.
//!
//! Runs only with `SLOPTY_THROUGH_SERVER_E2E=1` (`cargo xtask e2e through-server`). A
//! `slopty-server`, worker `near` on loopback and worker `far` behind a relay shaped like the
//! tailnet (`harness::TAILNET`), both registered with it, then the app at its first run
//! (`harness::ServerFleet`). In one serial test:
//!
//! 1. the first-run panel takes the server's address, typed and entered as a person would, and the
//!    app writes it to its settings;
//! 2. both workers come from the directory, nobody adds them, and each gets a shell that
//!    round-trips a command (far's over the shaped link, at the address the server listed);
//! 3. a permission hook played on far through `slopty hook` badges the pill from near's shell;
//! 4. far is killed and restarted twice: once as soon as the app shows it down, which its own link
//!    finds again; once after the server has called it unreachable and the app holds it, where the
//!    server listing it online again is what wakes the dial. Each is measured and bounded.
//!
//! No agent runs and nothing is typed into a shell to fake one: the hook JSON goes on the stdin
//! of a `slopty hook` this test spawns.

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use slopty_e2e::Dump;
    use slopty_e2e::harness::{ServerFleet, TAILNET, artifacts_dir};
    use slopty_e2e::snapshot::assert_matches;

    /// A round trip over the shaped link, a shell starting, the directory arriving.
    const STEP: Duration = Duration::from_secs(30);
    /// A killed worker must show down within this: the app's silence bar is three missed
    /// keep-alives, 3 s, and it read 3.3 to 3.7 s on a worker added by address (MEASUREMENTS,
    /// 2026-09-26).
    const DOWN_BOUND: Duration = Duration::from_secs(5);
    /// A restarted worker must be connected again within this, whichever path finds it: the
    /// app's own ping drawing a reset, or the server listing it online.
    const BACK_BOUND: Duration = Duration::from_secs(2);
    /// The server's lease ends 5 s after a worker goes silent; this leaves the app's dial the
    /// time to have failed and the server's word the time to arrive.
    const HELD_BOUND: Duration = Duration::from_secs(15);
    /// The renders' window: room for the top bar's pill and two workers' tiles.
    const WINDOW: (f32, f32) = (1280.0, 800.0);
    /// Fraction of pixels allowed to differ from the golden.
    const TOLERANCE: f64 = 0.01;

    const NEAR: &str = "studio";
    const FAR: &str = "remote";

    fn gated() -> bool {
        if std::env::var_os("SLOPTY_THROUGH_SERVER_E2E").is_none() {
            eprintln!(
                "skipped: set SLOPTY_THROUGH_SERVER_E2E=1 (or run `cargo xtask e2e through-server`)"
            );
            return false;
        }
        true
    }

    fn worker<'d>(d: &'d Dump, name: &str) -> Option<&'d slopty_e2e::WorkerInfo> {
        d.workers.iter().find(|w| w.name == name)
    }

    fn connected(d: &Dump, name: &str) -> bool {
        worker(d, name).is_some_and(|w| w.status == "connected")
    }

    fn shell_on<'d>(d: &'d Dump, name: &str) -> Option<&'d str> {
        d.items
            .iter()
            .find(|i| i.worker == name && i.kind == "terminal")
            .and_then(|i| i.session.as_deref())
    }

    fn focused_worker(d: &Dump) -> Option<&str> {
        d.items.iter().find(|i| i.active).map(|i| i.worker.as_str())
    }

    /// The "N need(s) you" pill's label, if the top bar shows one.
    fn pill(d: &Dump) -> Option<String> {
        d.a11y.iter().filter(|n| n.role == "Button").find_map(|n| {
            n.label.clone().filter(|l| l.ends_with("need you") || l.ends_with("needs you"))
        })
    }

    /// Give worker `name`'s shell the keyboard; its session.
    async fn focus_shell_on(fleet: &mut ServerFleet, name: &str) -> String {
        let d = fleet.driver.dump().await.unwrap();
        let session = shell_on(&d, name).unwrap_or_else(|| panic!("no shell on {name}: {d:#?}"));
        let session = session.to_owned();
        fleet.driver.reveal(&session).await.unwrap();
        fleet
            .driver
            .wait_for(&format!("{name}'s shell focused"), STEP, |d| {
                d.focused == format!("terminal:{session}")
            })
            .await
            .unwrap();
        session
    }

    /// Type a command that only the shell's arithmetic answers with `expect`, and wait for it.
    async fn round_trip(fleet: &mut ServerFleet, name: &str, command: &str, expect: &str) {
        focus_shell_on(fleet, name).await;
        fleet.driver.type_text(&format!("{command}\n")).await.unwrap();
        fleet
            .driver
            .wait_for(&format!("{expect} from {name}'s shell"), STEP, |d| {
                d.rows_containing(expect).iter().any(|r| r.trim() == expect)
            })
            .await
            .unwrap();
    }

    /// The server's word on worker `name`.
    async fn liveness(fleet: &ServerFleet, name: &str) -> String {
        let directory = fleet.directory().await.unwrap();
        let all = directory.as_array().cloned().unwrap_or_default();
        let entry = all.iter().find(|w| w["name"] == name);
        entry.and_then(|w| w["liveness"].as_str()).unwrap_or_default().to_owned()
    }

    /// Wait until nothing moves, so a spring still running cannot end up in the golden.
    async fn settled(fleet: &mut ServerFleet) {
        let mut last = fleet.driver.dump().await.unwrap();
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(120)).await;
            let next = fleet.driver.dump().await.unwrap();
            let still =
                |a: &[f32; 4], b: &[f32; 4]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 0.5);
            if next.items.len() == last.items.len()
                && next.items.iter().zip(&last.items).all(|(a, b)| still(&a.bounds, &b.bounds))
            {
                return;
            }
            last = next;
        }
    }

    // Multi-threaded: the relay in this process holds packets for their delay, and a dump being
    // parsed on the one thread would add to it.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_app_finds_its_workers_through_the_server() {
        if !gated() {
            return;
        }
        let started = Instant::now();
        let mut fleet = ServerFleet::launch(NEAR, FAR, TAILNET).await.unwrap();
        println!(
            "MEASURE through-server: server and two workers online in {:.1} s",
            started.elapsed().as_secs_f64()
        );
        let far_listed = fleet.far.address().to_owned();
        let directory = fleet.directory().await.unwrap();
        assert!(
            directory.as_array().is_some_and(|all| all
                .iter()
                .any(|w| w["name"] == FAR && w["address"] == far_listed.as_str())),
            "the server lists {FAR} at its relay, {far_listed}: {directory}"
        );
        let drv = &mut fleet.driver;
        drv.ok(&slopty_e2e::Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();

        // (1) The first run offers the server; its address is typed and entered.
        let d = drv.wait_for("the first-run panel", STEP, |d| d.adding).await.unwrap();
        assert!(d.workers.is_empty(), "nothing known at the first run: {d:#?}");
        assert!(d.a11y_node("Heading", Some("Connect to a server")).is_some(), "{:#?}", d.a11y);
        let server = fleet.server.address().to_owned();
        let drv = &mut fleet.driver;
        drv.type_text(&server).await.unwrap();
        drv.keys("enter").await.unwrap();
        let entered = Instant::now();

        // (2) Both workers from the directory, connected, each with its shell.
        let d = drv
            .wait_for("both workers from the directory, connected", STEP, |d| {
                !d.adding && d.workers.len() == 2 && connected(d, NEAR) && connected(d, FAR)
            })
            .await
            .unwrap();
        println!(
            "MEASURE through-server: both workers connected {:.2} s after the server's address was entered",
            entered.elapsed().as_secs_f64()
        );
        assert!(d.notice.as_deref().is_some_and(|n| n.starts_with("Connected to")), "{d:#?}");
        let settings = std::fs::read_to_string(fleet.dir.path().join("app/settings.toml")).unwrap();
        assert!(settings.contains(&server), "the server is kept in the settings: {settings}");
        let drv = &mut fleet.driver;
        drv.wait_for("a shell on each worker", STEP, |d| {
            shell_on(d, NEAR).is_some() && shell_on(d, FAR).is_some()
        })
        .await
        .unwrap();
        round_trip(&mut fleet, NEAR, "echo NEAR-$((6*7))", "NEAR-42").await;
        round_trip(&mut fleet, FAR, "echo FAR-$((6*7))", "FAR-42").await;

        // (3) A permission hook on far, played through `slopty hook`, badges the pill while
        // near's shell has the focus.
        let session_far = shell_on(&fleet.driver.dump().await.unwrap(), FAR).unwrap().to_owned();
        focus_shell_on(&mut fleet, NEAR).await;
        fleet
            .far
            .play_hook(&session_far, "PermissionRequest", r#","tool_name":"Bash""#)
            .await
            .unwrap();
        let hooked = Instant::now();
        let d = fleet
            .driver
            .wait_for("the pill to show far's waiting agent", STEP, |d| {
                focused_worker(d) == Some(NEAR)
                    && worker(d, FAR).is_some_and(|w| w.needs_you == 1)
                    && pill(d).as_deref() == Some("1 needs you")
            })
            .await
            .unwrap();
        println!(
            "MEASURE through-server: far's hook badged the pill within {:.0} ms",
            hooked.elapsed().as_secs_f64() * 1e3
        );
        assert_eq!(worker(&d, NEAR).map(|w| w.needs_you), Some(0), "{d:#?}");

        // Two workers the server listed, one waiting on the human: no other golden holds more
        // than one worker's tiles, or the pill's sum over them.
        // The "Connected to" notice leaves on a timer, so a frame taken near its end would
        // hold it or not by chance.
        fleet.driver.wait_for("the notice gone", STEP, |d| d.notice.is_none()).await.unwrap();
        settled(&mut fleet).await;
        let path = fleet.dir.path().join("through-server.png");
        let frame = fleet.driver.render(&path).await.unwrap();
        assert_matches("through-server", &frame, TOLERANCE, &artifacts_dir()).unwrap();

        // (4a) Killed, then restarted the moment the app shows it down: the app's own link
        // finds it (its pings draw the new process's reset), before the server has even noticed.
        fleet.far.kill_worker().await;
        let killed = Instant::now();
        fleet
            .driver
            .wait_for("far shown down", STEP, |d| !connected(d, FAR) && connected(d, NEAR))
            .await
            .unwrap();
        let down = killed.elapsed();
        println!(
            "MEASURE through-server: far shown down {:.1} s after the kill",
            down.as_secs_f64()
        );
        fleet.far.restart_worker().await.unwrap();
        let restarted = Instant::now();
        fleet.driver.wait_for("far back", STEP, |d| connected(d, FAR)).await.unwrap();
        let back = restarted.elapsed();
        println!(
            "MEASURE through-server: far connected again {:.2} s after a restart while shown down",
            back.as_secs_f64()
        );
        assert!(down <= DOWN_BOUND, "shown down after {down:?}, over {DOWN_BOUND:?}");
        assert!(back <= BACK_BOUND, "back after {back:?}, over {BACK_BOUND:?}");
        round_trip(&mut fleet, FAR, "echo AGAIN-$((6*7))", "AGAIN-42").await;

        // (4b) Killed and left down until the server calls it unreachable and the app holds it
        // there instead of redialling; restarted, the server lists it online and that wakes the
        // dial.
        fleet.far.kill_worker().await;
        let killed = Instant::now();
        fleet
            .driver
            .wait_for("far held as unreachable", HELD_BOUND, |d| {
                worker(d, FAR).is_some_and(|w| w.status == "unreachable")
            })
            .await
            .unwrap();
        println!(
            "MEASURE through-server: far held as unreachable {:.1} s after the kill",
            killed.elapsed().as_secs_f64()
        );
        assert_eq!(liveness(&fleet, FAR).await, "unreachable");
        fleet.far.restart_worker().await.unwrap();
        let restarted = Instant::now();
        fleet.driver.wait_for("far back from the hold", STEP, |d| connected(d, FAR)).await.unwrap();
        let back = restarted.elapsed();
        println!(
            "MEASURE through-server: far connected again {:.2} s after a restart while held unreachable",
            back.as_secs_f64()
        );
        assert!(back <= BACK_BOUND, "back from the hold after {back:?}, over {BACK_BOUND:?}");
        assert_eq!(liveness(&fleet, FAR).await, "online");
        round_trip(&mut fleet, FAR, "echo HELD-$((6*7))", "HELD-42").await;

        let carried = fleet.far.carried().await;
        println!(
            "MEASURE through-server: shaper up {} sent / {} lost, down {} sent / {} lost; whole run {:.1} s",
            carried.up.sent,
            carried.up.lost,
            carried.down.sent,
            carried.down.lost,
            started.elapsed().as_secs_f64()
        );
        assert!(
            carried.up.sent > 0 && carried.down.sent > 0,
            "far's traffic went through the relay"
        );
        fleet.shutdown().await;
    }
}

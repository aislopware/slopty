//! One client, two workers in one workspace: cross-worker attention against a real second worker
//! on this Mac, reached over a link shaped like the tailnet path to another Mac.
//!
//! Runs only with `SLOPTY_WORKERS_E2E=1` (`cargo xtask e2e workers`). Stack A is ptyd + worker +
//! the app, as `e2e app` builds it. Worker B is a second ptyd + worker under a root of its own
//! with a private HOME (`harness::SecondWorker`), so the real `~/.claude` is never touched and
//! `slopty hook install` is never run; the app reaches it only through a relay that adds the
//! mesh's round trip, jitter and loss (`harness::TAILNET`). The app adds both and drives, in one
//! serial test:
//!
//! 1. the dump shows two connected workers, and both workers' tiles in the one layout;
//! 2. a shell on worker B round-trips a command over the shaped link;
//! 3. attention: with A's shell focused, a permission hook played to worker B through the real
//!    `slopty hook` relay badges the pill with the cross-worker sum; a tap on the pill (and ⌘⇧A)
//!    focuses B's waiting session; the banner's tag (the session UUID) drives
//!    `notification_response` to it;
//! 4. killing worker B mid-stream shows it as down while its tiles stay and worker A keeps
//!    streaming, and a restart brings it back with the shell reattached.
//!
//! The agent is never a real Claude session and nothing is typed into a shell to fake one: the
//! hook JSON is piped into `slopty hook`'s stdin, exactly what Claude Code does.

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use slopty_e2e::harness::{SecondWorker, Stack, TAILNET};
    use slopty_e2e::{Command, Dump};

    /// A worker round trip (open a shell, run a command, badge a hook) may take this long over the
    /// shaped link. Noticing a killed worker takes the app's silence bound (8 s) and redialling a
    /// restarted one its drop bound (15 s), both inside this.
    const STEP: Duration = Duration::from_secs(30);

    /// Worker A, the stack's own.
    const A: &str = "studio";
    /// Worker B, the one behind the shaped link.
    const B: &str = "remote";

    /// Whether the suite is enabled; it skips otherwise.
    fn gated() -> bool {
        if std::env::var_os("SLOPTY_WORKERS_E2E").is_none() {
            eprintln!("skipped: set SLOPTY_WORKERS_E2E=1 (or run `cargo xtask e2e workers`)");
            return false;
        }
        true
    }

    /// The name of the worker whose tile has the focus.
    fn focused_worker(d: &Dump) -> Option<&str> {
        d.items.iter().find(|i| i.active).map(|i| i.worker.as_str())
    }

    /// A worker's row in the dump.
    fn worker<'d>(d: &'d Dump, name: &str) -> Option<&'d slopty_e2e::WorkerInfo> {
        d.workers.iter().find(|w| w.name == name)
    }

    /// The session of a terminal tile on worker `name`.
    fn shell_on<'d>(d: &'d Dump, name: &str) -> Option<&'d str> {
        d.items
            .iter()
            .find(|i| i.worker == name && i.kind == "terminal")
            .and_then(|i| i.session.as_deref())
    }

    /// The "N need(s) you" pill's label and centre, if the top bar shows one.
    fn pill(d: &Dump) -> Option<(String, (f32, f32))> {
        d.a11y
            .iter()
            .filter(|n| n.role == "Button")
            .find(|n| {
                n.label
                    .as_deref()
                    .is_some_and(|l| l.ends_with("need you") || l.ends_with("needs you"))
            })
            .map(|n| {
                let [left, top, width, height] = n.bounds;
                (n.label.clone().unwrap_or_default(), (left + width / 2.0, top + height / 2.0))
            })
    }

    /// Focus worker `name`'s terminal (and its keyboard), wherever the layout has it.
    async fn focus_shell_on(stack: &mut Stack, name: &str) -> String {
        let d = stack.driver.dump().await.unwrap();
        let session = shell_on(&d, name).unwrap_or_else(|| panic!("no shell on {name}: {d:#?}"));
        let session = session.to_owned();
        stack.driver.reveal(&session).await.unwrap();
        stack
            .driver
            .wait_for(&format!("{name}'s shell focused"), STEP, |d| {
                d.focused == format!("terminal:{session}")
            })
            .await
            .unwrap();
        session
    }

    // Multi-threaded: the relay in this process holds packets for their delay, and a dump being
    // parsed on the one thread would add to it.
    #[tokio::test(flavor = "multi_thread")]
    async fn cross_worker_attention_holds_with_a_real_second_worker() {
        if !gated() {
            return;
        }
        let started = Instant::now();

        // Stack A; a window with room for the top bar's pill and buttons.
        let mut stack = Stack::launch_with(A, &[]).await.unwrap();
        stack.driver.ok(&Command::Resize { width: 1280.0, height: 800.0 }).await.unwrap();
        stack
            .driver
            .wait_for("A's first shell", STEP, |d| {
                d.terminals.iter().any(|t| t.rows.iter().any(|r| !r.is_empty()))
            })
            .await
            .unwrap();

        // Worker B behind the shaped link. The app adds it by the relay's address.
        let launch_b = Instant::now();
        let mut worker_b = SecondWorker::launch(B, TAILNET).await.unwrap();
        println!(
            "MEASURE workers: brought worker B up in {:.1} s",
            launch_b.elapsed().as_secs_f64()
        );
        let address = worker_b.address().to_owned();
        stack.driver.ok(&Command::AddWorker { address }).await.unwrap();

        // (1) Two connected workers, one workspace: no switcher, both are simply there.
        stack
            .driver
            .wait_for("two connected workers", STEP, |d| {
                d.workers.len() == 2
                    && d.workers.iter().filter(|w| w.status == "connected").count() == 2
            })
            .await
            .unwrap();
        // Read both links' RTT once they are sampled (once a second).
        let d = stack
            .driver
            .wait_for("both workers' RTT sampled", STEP, |d| {
                [A, B].iter().all(|name| worker(d, name).is_some_and(|w| w.rtt_us.is_some()))
            })
            .await
            .unwrap();
        let rtt_ms = |name: &str| {
            let rtt = worker(&d, name).and_then(|w| w.rtt_us).unwrap_or_default();
            Duration::from_micros(rtt).as_secs_f64() * 1e3
        };
        println!(
            "MEASURE workers: RTT to worker B through the shaped link {:.1} ms (worker A on loopback {:.1} ms)",
            rtt_ms(B),
            rtt_ms(A),
        );
        assert_eq!(focused_worker(&d), Some(A), "adding does not take the focus: {d:#?}");

        // (2) Worker B's first shell is a tile beside A's, in the same layout. Focusing it makes
        // B the worker ⌘N and the palette talk to; a command round-trips over the shaped link.
        let d = stack
            .driver
            .wait_for("a shell on worker B in the workspace", STEP, |d| {
                shell_on(d, B).is_some() && shell_on(d, A).is_some()
            })
            .await
            .unwrap();
        assert!(
            d.items.iter().any(|i| i.worker == A) && d.items.iter().any(|i| i.worker == B),
            "both workers' tiles: {d:#?}"
        );
        let session_b = focus_shell_on(&mut stack, B).await;
        stack.driver.type_text("echo MESH-$((6*7))\n").await.unwrap();
        stack
            .driver
            .wait_for("the command's output back over the shaped link", STEP, |d| {
                !d.rows_containing("MESH-42").is_empty()
            })
            .await
            .unwrap();

        // (3) Cross-worker attention. Focus worker A's shell, then badge a session on worker B.
        let session_a = focus_shell_on(&mut stack, A).await;
        worker_b
            .play_hook(&session_b, "PermissionRequest", r#","tool_name":"Bash""#)
            .await
            .unwrap();
        let badged = Instant::now();
        let d = stack
            .driver
            .wait_for("the pill to show B's one waiting agent", STEP, |d| {
                focused_worker(d) == Some(A)
                    && worker(d, B).is_some_and(|w| w.needs_you == 1)
                    && pill(d).is_some_and(|(l, _)| l == "1 needs you")
            })
            .await
            .unwrap();
        println!(
            "MEASURE workers: cross-worker attention badged the pill within {:.0} ms of the hook",
            badged.elapsed().as_secs_f64() * 1e3
        );
        // The count is the sum across workers: worker A contributes nothing.
        assert_eq!(worker(&d, A).unwrap().needs_you, 0, "{d:#?}");

        // A tap on the pill focuses B's waiting session, in the same layout.
        let (_, (px, py)) = pill(&d).unwrap();
        stack.driver.click(px, py).await.unwrap();
        stack
            .driver
            .wait_for("the pill tap to reveal B's session", STEP, |d| {
                focused_worker(d) == Some(B) && d.focused == format!("terminal:{session_b}")
            })
            .await
            .unwrap();

        // ⌘⇧A does the same from A's shell.
        focus_shell_on(&mut stack, A).await;
        stack.driver.keys("cmd-shift-a").await.unwrap();
        stack
            .driver
            .wait_for("⌘⇧A to reach B's session", STEP, |d| {
                d.focused == format!("terminal:{session_b}")
            })
            .await
            .unwrap();

        // The banner's tag is the session UUID: a notification response for it finds worker B's
        // session and reveals it, from worker A's shell.
        focus_shell_on(&mut stack, A).await;
        stack.driver.notification_response(&session_b).await.unwrap();
        stack
            .driver
            .wait_for("the notification response to route to worker B", STEP, |d| {
                focused_worker(d) == Some(B) && d.focused == format!("terminal:{session_b}")
            })
            .await
            .unwrap();

        // (4) Kill worker B mid-stream: worker A keeps streaming, B shows as down while its tiles
        // stay; a restart brings it back and the shell reattaches.
        focus_shell_on(&mut stack, A).await;
        let tiles_b =
            stack.driver.dump().await.unwrap().items.iter().filter(|i| i.worker == B).count();
        worker_b.kill_worker().await;
        let killed = Instant::now();
        let d = stack
            .driver
            .wait_for("worker B down", STEP, |d| {
                worker(d, B).is_some_and(|w| w.status != "connected")
                    && worker(d, A).is_some_and(|w| w.status == "connected")
            })
            .await
            .unwrap();
        println!(
            "MEASURE workers: worker B shown down {:.1} s after the kill",
            killed.elapsed().as_secs_f64()
        );
        assert_eq!(
            d.items.iter().filter(|i| i.worker == B).count(),
            tiles_b,
            "a worker that is down keeps its tiles: {d:#?}"
        );
        assert_ne!(d.status, "connected", "the dump's status names the worker that is down");
        // Worker A is untouched: its own shell still answers.
        assert_eq!(d.focused, format!("terminal:{session_a}"), "{d:#?}");
        stack.driver.type_text("echo STILL-ALIVE\n").await.unwrap();
        stack
            .driver
            .wait_for("worker A still streaming", STEP, |d| {
                !d.rows_containing("STILL-ALIVE").is_empty()
            })
            .await
            .unwrap();

        worker_b.restart_worker().await.unwrap();
        let restarted = Instant::now();
        stack
            .driver
            .wait_for("worker B back", STEP, |d| {
                worker(d, B).is_some_and(|w| w.status == "connected")
            })
            .await
            .unwrap();
        println!(
            "MEASURE workers: worker B connected again {:.1} s after the restart",
            restarted.elapsed().as_secs_f64()
        );
        // The session survives the restart (ptyd kept the PTY; the worker re-exposes it). The grid
        // comes back blank — the worker's vt state does not outlive the restart — so "reattached"
        // is proven by live I/O: reveal the session and round-trip a command over the
        // shaped link again.
        stack
            .driver
            .wait_for("B's session present after reattach", STEP, |d| {
                d.terminal(&session_b).is_some()
            })
            .await
            .unwrap();
        stack.driver.reveal(&session_b).await.unwrap();
        stack
            .driver
            .wait_for("B's session focused", STEP, |d| d.focused == format!("terminal:{session_b}"))
            .await
            .unwrap();
        stack.driver.type_text("echo RE-$((6*7))\n").await.unwrap();
        stack
            .driver
            .wait_for("the reattached shell answers over the shaped link", STEP, |d| {
                !d.rows_containing("RE-42").is_empty()
            })
            .await
            .unwrap();

        println!(
            "MEASURE workers: full two-worker run in {:.1} s",
            started.elapsed().as_secs_f64()
        );

        // The whole run went through the shaper, and the shaper did lose packets on the way.
        let carried = worker_b.carried().await;
        println!(
            "MEASURE workers: shaper up {} sent / {} lost, down {} sent / {} lost",
            carried.up.sent, carried.up.lost, carried.down.sent, carried.down.lost
        );
        assert!(
            carried.up.lost > 0 && carried.down.lost > 0,
            "the link to worker B lost nothing both ways: {carried:?}"
        );

        worker_b.shutdown().await;
        stack.shutdown().await;
    }
}

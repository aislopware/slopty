//! One client, two workers on two machines, in one workspace: cross-worker attention against
//! a real second worker.
//!
//! Runs only with `SLOPTY_HOST2_E2E=1` and `SLOPTY_HOST2=<ssh name>` (`cargo xtask e2e hosts`);
//! `SLOPTY_HOST2_ADDR` names the address the app adds host B by when the ssh destination is an
//! alias no resolver knows (its host part is used otherwise).
//! Stack A is ptyd + hostd + the app on this Mac, as `e2e app` builds it; host B is ptyd +
//! hostd started over ssh on the second machine under one temp root, with a private HOME so its
//! real `~/.claude` is never touched and `slopty hook install` is never run. The app adds
//! both and drives, in one serial test so host B is set up once:
//!
//! 1. the dump shows two connected workers, and both workers' tiles in the one layout;
//! 2. a shell on host B round-trips a command over the mesh;
//! 3. attention: with A's shell focused, a permission hook played to host B through the real
//!    `slopty hook` relay badges the pill with the cross-worker sum; a tap on the pill (and ⌘⇧A)
//!    focuses B's waiting session; the banner's tag (the session UUID) drives
//!    `notification_response` to it;
//! 4. killing host B mid-stream shows it as down while its tiles stay and host A keeps streaming,
//!    and a restart brings it back with the shell reattached.
//!
//! The agent is never a real Claude session and nothing is typed into a shell to fake one: the
//! hook JSON is piped into `slopty hook`'s stdin over ssh, exactly what Claude Code does.

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use slopty_e2e::harness::{RemoteHost, Stack};
    use slopty_e2e::{Command, Dump};

    /// A host round trip (open a shell, run a command, badge a hook) may take this long over the
    /// mesh.
    const STEP: Duration = Duration::from_secs(30);

    /// The ssh name of the second machine, or `None` when the gate is off (the test skips). An
    /// enabled gate without a machine is a configuration error, not a skip: it panics.
    fn gated() -> Option<String> {
        let gate = std::env::var("SLOPTY_HOST2_E2E").ok();
        let host2 = std::env::var("SLOPTY_HOST2").ok();
        let ssh = slopty_e2e::harness::host2_gate(gate.as_deref(), host2.as_deref())
            .expect("the two-host suite is enabled but misconfigured");
        if ssh.is_none() {
            eprintln!(
                "skipped: set SLOPTY_HOST2_E2E=1 and SLOPTY_HOST2=<ssh name> (or run `cargo xtask e2e hosts`)"
            );
        }
        ssh
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

    #[tokio::test]
    async fn cross_host_attention_holds_with_a_real_second_host() {
        let Some(ssh) = gated() else { return };
        let started = Instant::now();

        // Stack A on this Mac; a window with room for the top bar's pill and buttons.
        let mut stack = Stack::launch_with("studio", &[]).await.unwrap();
        stack.driver.ok(&Command::Resize { width: 1280.0, height: 800.0 }).await.unwrap();
        stack
            .driver
            .wait_for("A's first shell", STEP, |d| {
                d.terminals.iter().any(|t| t.rows.iter().any(|r| !r.is_empty()))
            })
            .await
            .unwrap();

        // Host B on the second machine, over ssh. The app adds it by address too.
        let launch_b = Instant::now();
        let (mut host_b, address_b) = RemoteHost::launch(&ssh).await.unwrap();
        println!(
            "MEASURE hosts: brought host B up on {ssh} in {:.1} s",
            launch_b.elapsed().as_secs_f64()
        );
        stack.driver.ok(&Command::AddHost { address: address_b }).await.unwrap();

        // (1) Two connected workers, one workspace: no switcher, both are simply there.
        stack
            .driver
            .wait_for("two connected workers", STEP, |d| {
                d.workers.len() == 2
                    && d.workers.iter().filter(|w| w.status == "connected").count() == 2
            })
            .await
            .unwrap();
        // Read B's link RTT once it is sampled (once a second).
        let d = stack
            .driver
            .wait_for("host B's RTT sampled", STEP, |d| {
                worker(d, "macbook").is_some_and(|w| w.rtt_us.is_some())
            })
            .await
            .unwrap();
        let b = worker(&d, "macbook").unwrap();
        println!(
            "MEASURE hosts: mesh RTT to host B {:.1} ms",
            Duration::from_micros(b.rtt_us.unwrap()).as_secs_f64() * 1e3,
        );
        assert_eq!(focused_worker(&d), Some("studio"), "adding does not take the focus: {d:#?}");

        // (2) Host B's first shell is a tile beside A's, in the same layout. Focusing it makes
        // B the worker ⌘N and the palette talk to; a command round-trips over the mesh.
        let d = stack
            .driver
            .wait_for("a shell on host B in the workspace", STEP, |d| {
                shell_on(d, "macbook").is_some() && shell_on(d, "studio").is_some()
            })
            .await
            .unwrap();
        assert!(
            d.items.iter().any(|i| i.worker == "studio")
                && d.items.iter().any(|i| i.worker == "macbook"),
            "both workers' tiles: {d:#?}"
        );
        let session_b = focus_shell_on(&mut stack, "macbook").await;
        stack.driver.type_text("echo MESH-$((6*7))\n").await.unwrap();
        stack
            .driver
            .wait_for("the command's output back over the mesh", STEP, |d| {
                !d.rows_containing("MESH-42").is_empty()
            })
            .await
            .unwrap();

        // (3) Cross-worker attention. Focus host A's shell, then badge a session on host B.
        let session_a = focus_shell_on(&mut stack, "studio").await;
        host_b.play_hook(&session_b, "PermissionRequest", r#","tool_name":"Bash""#).await.unwrap();
        let badged = Instant::now();
        let d = stack
            .driver
            .wait_for("the pill to show B's one waiting agent", STEP, |d| {
                focused_worker(d) == Some("studio")
                    && worker(d, "macbook").is_some_and(|w| w.needs_you == 1)
                    && pill(d).is_some_and(|(l, _)| l == "1 needs you")
            })
            .await
            .unwrap();
        println!(
            "MEASURE hosts: cross-host attention badged the pill within {:.0} ms of the hook",
            badged.elapsed().as_secs_f64() * 1e3
        );
        // The count is the sum across workers: host A contributes nothing.
        assert_eq!(worker(&d, "studio").unwrap().needs_you, 0, "{d:#?}");

        // A tap on the pill focuses B's waiting session, in the same layout.
        let (_, (px, py)) = pill(&d).unwrap();
        stack.driver.click(px, py).await.unwrap();
        stack
            .driver
            .wait_for("the pill tap to reveal B's session", STEP, |d| {
                focused_worker(d) == Some("macbook") && d.focused == format!("terminal:{session_b}")
            })
            .await
            .unwrap();

        // ⌘⇧A does the same from A's shell.
        focus_shell_on(&mut stack, "studio").await;
        stack.driver.keys("cmd-shift-a").await.unwrap();
        stack
            .driver
            .wait_for("⌘⇧A to reach B's session", STEP, |d| {
                d.focused == format!("terminal:{session_b}")
            })
            .await
            .unwrap();

        // The banner's tag is the session UUID: a notification response for it finds host B's
        // session and reveals it, from host A's shell.
        focus_shell_on(&mut stack, "studio").await;
        stack.driver.notification_response(&session_b).await.unwrap();
        stack
            .driver
            .wait_for("the notification response to route to host B", STEP, |d| {
                focused_worker(d) == Some("macbook") && d.focused == format!("terminal:{session_b}")
            })
            .await
            .unwrap();

        // (4) Kill host B mid-stream: host A keeps streaming, B shows as down while its tiles
        // stay; a restart brings it back and the shell reattaches.
        focus_shell_on(&mut stack, "studio").await;
        let tiles_b = stack
            .driver
            .dump()
            .await
            .unwrap()
            .items
            .iter()
            .filter(|i| i.worker == "macbook")
            .count();
        host_b.kill_hostd().await.unwrap();
        let d = stack
            .driver
            .wait_for("host B down", STEP, |d| {
                worker(d, "macbook").is_some_and(|w| w.status != "connected")
                    && worker(d, "studio").is_some_and(|w| w.status == "connected")
            })
            .await
            .unwrap();
        assert_eq!(
            d.items.iter().filter(|i| i.worker == "macbook").count(),
            tiles_b,
            "a worker that is down keeps its tiles: {d:#?}"
        );
        assert_ne!(d.status, "connected", "the dump's status names the worker that is down");
        // Host A is untouched: its own shell still answers.
        assert_eq!(d.focused, format!("terminal:{session_a}"), "{d:#?}");
        stack.driver.type_text("echo STILL-ALIVE\n").await.unwrap();
        stack
            .driver
            .wait_for("host A still streaming", STEP, |d| {
                !d.rows_containing("STILL-ALIVE").is_empty()
            })
            .await
            .unwrap();

        host_b.restart_hostd().await.unwrap();
        stack
            .driver
            .wait_for("host B back", STEP, |d| {
                worker(d, "macbook").is_some_and(|w| w.status == "connected")
            })
            .await
            .unwrap();
        // The session survives the restart (ptyd kept the PTY; hostd re-exposes it). The grid
        // comes back blank — hostd's vt state does not outlive the restart — so "reattached" is
        // proven by live I/O: reveal the session and round-trip a command over the mesh again.
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
            .wait_for("the reattached shell answers over the mesh", STEP, |d| {
                !d.rows_containing("RE-42").is_empty()
            })
            .await
            .unwrap();

        println!("MEASURE hosts: full two-host run in {:.1} s", started.elapsed().as_secs_f64());

        // Teardown on both machines; nothing of ours may survive on the second.
        let stray = host_b.stray_processes().await.unwrap_or_default();
        println!("MEASURE hosts: remote strays before teardown: {:?}", stray.lines().count());
        host_b.shutdown().await.unwrap();
        stack.shutdown().await;
    }
}

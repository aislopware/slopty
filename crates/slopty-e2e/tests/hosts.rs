//! One client, two hosts on two machines: cross-host attention against a real second host.
//!
//! Runs only with `SLOPTY_HOST2_E2E=1` and `SLOPTY_HOST2=<ssh name>` (`cargo xtask e2e hosts`);
//! `SLOPTY_HOST2_ADDR` names the address the app adds host B by when the ssh destination is an
//! alias no resolver knows (its host part is used otherwise).
//! Stack A is ptyd + hostd + the app on this Mac, as `e2e app` builds it; host B is ptyd +
//! hostd started over ssh on the second machine under one temp root, with a private HOME so its
//! real `~/.claude` is never touched and `slopty hook install` is never run. The app adds
//! both and drives, in one serial test so host B is set up once:
//!
//! 1. the dump shows two connected hosts;
//! 2. a shell opened on host B (⌘N) round-trips a command over the mesh;
//! 3. attention: with host A on show, a permission hook played to host B through the real `slopty
//!    hook` relay badges the pill with the cross-host sum; a tap on the pill switches to B and
//!    focuses the waiting session; the banner's tag (the session UUID) drives
//!    `notification_response` back to B;
//! 4. killing host B mid-stream turns its row amber while host A keeps streaming, and a restart
//!    goes green with the shell reattached.
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

    /// The name of the host whose canvas is on show.
    fn active_name(d: &Dump) -> Option<&str> {
        d.hosts.iter().find(|h| h.active).map(|h| h.name.as_str())
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

    /// Step the host switcher until the host named `name` is on show.
    async fn show_host(stack: &mut Stack, name: &str) {
        for _ in 0..stack_hosts(stack).await {
            if active_name(&stack.driver.dump().await.unwrap()) == Some(name) {
                return;
            }
            stack.driver.keys("cmd-alt-right").await.unwrap();
        }
        stack
            .driver
            .wait_for(&format!("host {name} on show"), STEP, |d| active_name(d) == Some(name))
            .await
            .unwrap();
    }

    async fn stack_hosts(stack: &mut Stack) -> usize {
        stack.driver.dump().await.unwrap().hosts.len()
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

        // (1) Two connected hosts.
        stack
            .driver
            .wait_for("two connected hosts", STEP, |d| {
                d.hosts.len() == 2
                    && d.hosts.iter().filter(|h| h.status == "connected").count() == 2
            })
            .await
            .unwrap();
        // Adding left host B on show. Read its link RTT once it is sampled (once a second).
        let d = stack
            .driver
            .wait_for("host B's RTT sampled", STEP, |d| {
                d.hosts.iter().any(|h| h.name == "macbook" && h.rtt_us.is_some())
            })
            .await
            .unwrap();
        let b = d.hosts.iter().find(|h| h.name == "macbook").unwrap();
        println!(
            "MEASURE hosts: mesh RTT to host B {:.1} ms",
            Duration::from_micros(b.rtt_us.unwrap()).as_secs_f64() * 1e3,
        );
        assert!(active_name(&d) == Some("macbook"), "adding shows the new host: {d:#?}");

        // (2) Open a shell on host B (⌘N) and round-trip a command over the mesh.
        stack.driver.keys("cmd-n").await.unwrap();
        let session_b = stack
            .driver
            .wait_for("a focused shell on host B", STEP, |d| {
                active_name(d) == Some("macbook") && d.focused.starts_with("terminal:")
            })
            .await
            .unwrap()
            .focused
            .strip_prefix("terminal:")
            .unwrap()
            .to_owned();
        stack.driver.type_text("echo MESH-$((6*7))\n").await.unwrap();
        stack
            .driver
            .wait_for("the command's output back over the mesh", STEP, |d| {
                !d.rows_containing("MESH-42").is_empty()
            })
            .await
            .unwrap();

        // (3) Cross-host attention. Bring host A on show, then badge a session on host B.
        show_host(&mut stack, "studio").await;
        host_b.play_hook(&session_b, "PermissionRequest", r#","tool_name":"Bash""#).await.unwrap();
        let badged = Instant::now();
        let d = stack
            .driver
            .wait_for("the pill to show B's one waiting agent", STEP, |d| {
                active_name(d) == Some("studio")
                    && d.hosts
                        .iter()
                        .find(|h| h.name == "macbook")
                        .is_some_and(|h| h.needs_you == 1)
                    && pill(d).is_some_and(|(l, _)| l == "1 needs you")
            })
            .await
            .unwrap();
        println!(
            "MEASURE hosts: cross-host attention badged the pill within {:.0} ms of the hook",
            badged.elapsed().as_secs_f64() * 1e3
        );
        // The count is the sum across hosts: host A contributes nothing.
        assert_eq!(d.hosts.iter().find(|h| h.name == "studio").unwrap().needs_you, 0, "{d:#?}");

        // A tap on the pill switches to host B and focuses the waiting session.
        let (_, (px, py)) = pill(&d).unwrap();
        stack.driver.click(px, py).await.unwrap();
        stack
            .driver
            .wait_for("the pill tap to reveal B's session", STEP, |d| {
                active_name(d) == Some("macbook") && d.focused == format!("terminal:{session_b}")
            })
            .await
            .unwrap();

        // The banner's tag is the session UUID: a notification response for it resolves to host
        // B and reveals the session, from host A on show.
        show_host(&mut stack, "studio").await;
        stack.driver.notification_response(&session_b).await.unwrap();
        stack
            .driver
            .wait_for("the notification response to route to host B", STEP, |d| {
                active_name(d) == Some("macbook") && d.focused == format!("terminal:{session_b}")
            })
            .await
            .unwrap();

        // (4) Kill host B mid-stream: host A keeps streaming, B's row goes amber; a restart goes
        // green and the shell reattaches.
        show_host(&mut stack, "studio").await;
        host_b.kill_hostd().await.unwrap();
        stack
            .driver
            .wait_for("host B's row to go amber", STEP, |d| {
                d.hosts.iter().any(|h| h.name == "macbook" && h.status != "connected")
                    && d.hosts.iter().any(|h| h.name == "studio" && h.status == "connected")
            })
            .await
            .unwrap();
        // Host A is untouched: its own shell still answers.
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
            .wait_for("host B green again", STEP, |d| {
                d.hosts.iter().any(|h| h.name == "macbook" && h.status == "connected")
            })
            .await
            .unwrap();
        // The session survives the restart (ptyd kept the PTY; hostd re-exposes it). The grid
        // comes back blank — hostd's vt state does not outlive the restart — so "reattached" is
        // proven by live I/O: reveal the session and round-trip a command over the mesh again.
        show_host(&mut stack, "macbook").await;
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

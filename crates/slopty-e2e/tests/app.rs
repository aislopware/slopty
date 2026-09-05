//! The real app, driven from inside: pair with a host, open a shell, type, read the rows back,
//! render frames with the app's own renderer and compare them with the goldens.
//!
//! Runs only with `SLOPTY_APP_E2E=1` (`cargo xtask e2e app`), since it launches the app;
//! it needs no permission from the machine.

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use slopty_e2e::harness::{TRANSCRIPT_LINES, artifacts_dir};
    use slopty_e2e::snapshot::{assert_matches, foreground_fraction};
    use slopty_e2e::{Command, Stack};

    /// How long a host round trip (open a shell, run a command) may take.
    const STEP: Duration = Duration::from_secs(20);
    /// Window size for the renders: small, so the goldens stay small.
    const WINDOW: (f32, f32) = (900.0, 600.0);
    /// Fraction of pixels allowed to differ from a golden (hinting, RTT readout, cursor).
    const TOLERANCE: f64 = 0.01;
    /// Refresh requests the receiver may send for a target that produces no frame, checked
    /// against the host's own count of what it was asked for.
    fn refresh_cap() -> u64 {
        u64::from(slopty_media::Config::default().refresh_max_repeats)
    }

    fn gated() -> bool {
        if std::env::var_os("SLOPTY_APP_E2E").is_none() {
            eprintln!("skipped: set SLOPTY_APP_E2E=1 (or run `cargo xtask e2e app`)");
            return false;
        }
        true
    }

    #[tokio::test]
    async fn shell_typed_from_the_app_echoes_back_into_its_rows() {
        if !gated() {
            return;
        }
        let mut stack = Stack::launch("e2e-host").await.unwrap();
        let render_path = stack.path("terminal.png");
        let terminfo_db = stack.path("terminfo");
        let drv = &mut stack.driver;
        drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();

        // First connect: the host is up and the app opened one shell on the empty canvas, with
        // the keyboard, and the shell has printed its prompt.
        let dump = drv
            .wait_for("the first shell with a prompt", STEP, |d| {
                d.status == "connected"
                    && d.item("terminal").is_some()
                    && d.focus.as_deref() == Some("terminal")
                    && d.terminals.iter().any(|t| t.rows.iter().any(|r| !r.is_empty()))
            })
            .await
            .unwrap();
        assert_eq!(dump.hosts.len(), 1, "{dump:#?}");
        assert_eq!(dump.hosts[0].name, "e2e-host");
        // ptyd compiled ghostty's terminfo into the stack's own database while it came up, so
        // the shells it spawns can be told `TERM=xterm-ghostty` (the dump does not carry a
        // session's environment, and asking the shell would mean typing at it).
        let db = &terminfo_db;
        let compiled =
            || db.join("78/xterm-ghostty").exists() || db.join("x/xterm-ghostty").exists();
        let deadline = std::time::Instant::now() + STEP;
        while !compiled() && std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(compiled(), "ptyd installs ghostty's terminfo into {}", db.display());
        assert_eq!(dump.items.len(), 1, "{dump:#?}");
        let term = dump.item("terminal").unwrap().clone();
        assert!(term.active, "{dump:#?}");
        assert!(term.bounds[2] > 100.0 && term.bounds[3] > 100.0, "{term:?}");
        // What a screen reader gets: the item's heading, the grid as a terminal whose value is
        // the cursor row, the top bar's buttons, all inside the window.
        assert!(
            dump.a11y_node("Heading", None).is_some_and(|n| {
                n.label.as_deref().is_some_and(|l| l.starts_with("terminal "))
            }),
            "{:#?}",
            dump.a11y
        );
        let grid = dump.a11y_node("Terminal", None).unwrap_or_else(|| panic!("{:#?}", dump.a11y));
        assert!(grid.value.as_deref().is_some_and(|v| !v.is_empty()), "cursor row: {grid:?}");
        assert!(grid.bounds[2] > 100.0 && grid.bounds[3] > 100.0, "{grid:?}");
        for label in ["shell", "agent", "note", "window", "fit"] {
            assert!(dump.a11y_node("Button", Some(label)).is_some(), "{label}: {:#?}", dump.a11y);
        }

        // Typed text goes client → host → PTY → shell → host → client rows.
        drv.type_text("echo e2e-$((6*7))").await.unwrap();
        drv.keys("enter").await.unwrap();
        let dump = drv
            .wait_for("the echo", STEP, |d| {
                d.rows_containing("e2e-42").iter().any(|r| r.trim() == "e2e-42")
            })
            .await
            .unwrap();
        // The command line itself is echoed by the shell above the output.
        assert!(!dump.rows_containing("echo e2e-").is_empty(), "{dump:#?}");
        let cursor = dump.terminals[0].cursor;
        assert!(cursor[1] >= 2, "cursor moved below the output: {cursor:?}");

        // The frame the app draws, from its own renderer.
        let frame = drv.render(&render_path).await.unwrap();
        let fg = foreground_fraction(&frame);
        assert!(fg > 0.01, "frame is blank ({fg:.4} foreground)");
        assert_matches("terminal", &frame, TOLERANCE, &artifacts_dir()).unwrap();

        // ⌘N opens a second shell beside the first and takes the keyboard.
        drv.keys("cmd-n").await.unwrap();
        let dump = drv
            .wait_for("a second shell", STEP, |d| {
                d.items.len() == 2 && d.items.iter().any(|i| i.id != term.id && i.active)
            })
            .await
            .unwrap();
        assert!(!dump.items.iter().any(|i| i.id == term.id && i.active), "{dump:#?}");

        // The camera panned to reveal the new shell; ⌘1 fits both into the viewport. In this
        // window that is below the card zoom: the shells draw as cards and the keyboard goes
        // to the canvas (a focus left on an undrawn terminal would swallow every shortcut).
        drv.keys("cmd-1").await.unwrap();
        let dump = drv
            .wait_for("both shells in view as cards", STEP, |d| {
                d.zoom < 0.6
                    && d.focused == "canvas"
                    && d.items.iter().all(|i| {
                        let [x, y, w, h] = i.bounds;
                        x >= 0.0 && y >= 0.0 && x + w <= d.window.width && y + h <= d.window.height
                    })
            })
            .await
            .unwrap();
        // Clicking the first card makes it the active item again.
        let first = dump.items.iter().find(|i| i.id == term.id).unwrap();
        let (mid_x, mid_y) = first.center();
        drv.click(mid_x, mid_y).await.unwrap();
        let dump = drv
            .wait_for("the click to activate the first shell", STEP, |d| {
                d.items.iter().any(|i| i.id == term.id && i.active)
            })
            .await
            .unwrap();
        assert_eq!(dump.items.len(), 2);
        assert_eq!(dump.focused, "canvas", "{dump:#?}");

        // ⌘0 brings the grids back and the active shell takes the keyboard again.
        drv.keys("cmd-0").await.unwrap();
        let session = term.session.clone().unwrap();
        let _dump = drv
            .wait_for("the first shell to take focus at zoom 1", STEP, |d| {
                (d.zoom - 1.0).abs() < 1e-3 && d.focused == format!("terminal:{session}")
            })
            .await
            .unwrap();

        // ⌘W closes the active one; the host tears its session down and one shell remains.
        drv.keys("cmd-w").await.unwrap();
        let dump =
            drv.wait_for("the first shell to close", STEP, |d| d.items.len() == 1).await.unwrap();
        assert!(dump.items.iter().all(|i| i.id != term.id), "{dump:#?}");
        stack.shutdown().await;
    }

    /// A played Claude Code session (its hooks handed to hostd over the control socket from
    /// this test, its transcript a fixture): ⌘⇧L shows the conversation with every entry, the
    /// composer's text reaches the shell only on Enter, a permission puts the Allow / Deny row
    /// up, and ⌘⇧L brings the grid and the keyboard back.
    #[tokio::test]
    async fn the_conversation_view_reads_and_answers_the_agent() {
        if !gated() {
            return;
        }
        let mut stack = Stack::launch("e2e-host").await.unwrap();
        let render_path = stack.path("conversation.png");
        stack.driver.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
        let dump = stack
            .driver
            .wait_for("the first shell with a prompt", STEP, |d| {
                d.status == "connected"
                    && d.focus.as_deref() == Some("terminal")
                    && d.terminals.iter().any(|t| t.rows.iter().any(|r| !r.is_empty()))
            })
            .await
            .unwrap();
        let session = dump.terminals[0].session.clone();

        // The agent's `Stop` hook names the transcript; the host now knows the session runs
        // Claude Code.
        stack.play_hook(&session, "Stop", r#","last_assistant_message":"Fixed.""#).await.unwrap();
        let drv = &mut stack.driver;
        drv.wait_for("the agent to be seen", STEP, |d| {
            d.terminals.iter().any(|t| t.agent.as_deref() == Some("done"))
        })
        .await
        .unwrap();

        // ⌘⇧L: the conversation replaces the grid, the host streams the transcript, and the
        // composer has the keyboard.
        drv.keys("cmd-shift-l").await.unwrap();
        let dump = drv
            .wait_for("the conversation with every entry", STEP, |d| {
                d.terminals.iter().any(|t| {
                    t.conversation
                        .as_ref()
                        .is_some_and(|c| c.entries.len() == TRANSCRIPT_LINES.len())
                })
            })
            .await
            .unwrap();
        let conversation = dump.terminals[0].conversation.clone().unwrap();
        assert_eq!(conversation.entries, TRANSCRIPT_LINES, "{dump:#?}");
        assert!(conversation.composer_focused && conversation.pinned, "{conversation:?}");
        assert_eq!(conversation.attention, None);
        // The conversation as a screen reader gets it: every entry a list item, the composer
        // a labelled text field with the keyboard, its send button after it.
        let items = dump.a11y.iter().filter(|n| n.role == "ListItem").count();
        assert_eq!(items, TRANSCRIPT_LINES.len(), "{:#?}", dump.a11y);
        // (`composer_focused` above is the keyboard check: GPUI pins the focus to a node
        // inside gpui-kit's input, not to the labelled field.)
        assert!(
            dump.a11y_node("MultilineTextInput", Some("Message to Claude")).is_some(),
            "{:#?}",
            dump.a11y
        );
        assert!(dump.a11y_node("Button", Some("Send")).is_some(), "{:#?}", dump.a11y);

        // Typed text lands in the composer, not in the shell; Enter sends it into the shell
        // (a comment: the shell echoes it at its prompt and runs nothing) and clears it.
        drv.type_text("# from the composer").await.unwrap();
        let dump = drv
            .wait_for("the composer text", STEP, |d| {
                d.terminals[0]
                    .conversation
                    .as_ref()
                    .is_some_and(|c| c.composer == "# from the composer")
            })
            .await
            .unwrap();
        assert!(
            dump.rows_containing("from the composer").is_empty(),
            "nothing reached the shell yet"
        );
        drv.keys("enter").await.unwrap();
        let dump = drv
            .wait_for("the shell to echo the composer's line", STEP, |d| {
                !d.rows_containing("from the composer").is_empty()
            })
            .await
            .unwrap();
        assert_eq!(dump.terminals[0].conversation.as_ref().unwrap().composer, "");

        let frame = drv.render(&render_path).await.unwrap();
        assert!(foreground_fraction(&frame) > 0.01, "frame is blank");
        assert_matches("conversation", &frame, TOLERANCE, &artifacts_dir()).unwrap();

        // A permission request puts the Allow / Deny row above the composer (the headless test
        // presses the buttons; here the frame is the check).
        stack.play_hook(&session, "PermissionRequest", r#","tool_name":"Bash""#).await.unwrap();
        let drv = &mut stack.driver;
        let dump = drv
            .wait_for("the permission row", STEP, |d| {
                d.terminals[0]
                    .conversation
                    .as_ref()
                    .is_some_and(|c| c.attention.as_deref() == Some("permission:Bash"))
            })
            .await
            .unwrap();
        assert_eq!(dump.terminals[0].agent.as_deref(), Some("blocked:permission:Bash"));
        let frame = drv.render(&stack.dir.path().join("permission.png")).await.unwrap();
        assert_matches("conversation-permission", &frame, TOLERANCE, &artifacts_dir()).unwrap();

        // ⌘⇧L again: the grid is back with the keyboard.
        drv.keys("cmd-shift-l").await.unwrap();
        drv.wait_for("the grid back with the keyboard", STEP, |d| {
            d.terminals[0].conversation.is_none() && d.focused == format!("terminal:{session}")
        })
        .await
        .unwrap();
        stack.shutdown().await;
    }

    /// A `claude` nobody registered hooks for: "+ agent" starts the fake one the harness put
    /// on ptyd's `PATH`, and the host attributes it from its foreground process, then its
    /// title, then the transcript it writes — each signal taking over from the weaker one.
    /// ⌘⇧L then shows the conversation, which only works because the transcript was found
    /// rather than named by a hook.
    #[tokio::test]
    async fn an_agent_started_without_hooks_is_attributed_from_what_the_host_can_see() {
        if !gated() {
            return;
        }
        let mut stack = Stack::launch_with_fake_claude("e2e-host").await.unwrap();
        stack.driver.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
        stack
            .driver
            .wait_for("the first shell", STEP, |d| {
                d.status == "connected" && d.item("terminal").is_some()
            })
            .await
            .unwrap();

        // ⌘⇧T runs `claude`, which here is the fake: it prints where it runs and waits.
        stack.driver.keys("cmd-shift-t").await.unwrap();
        let dump = stack
            .driver
            .wait_for("the agent's terminal", STEP, |d| {
                d.items.len() == 2 && !d.rows_containing("fake claude in ").is_empty()
            })
            .await
            .unwrap();
        assert!(!dump.hooks_offered, "nothing has been offered yet");

        // No hook has fired and no title has been painted: the process is the whole signal.
        let dump = stack
            .driver
            .wait_for("the agent seen from its process alone", STEP, |d| {
                d.terminals.iter().any(|t| {
                    t.agent.as_deref() == Some("idle")
                        && t.agent_source.as_deref() == Some("process")
                })
            })
            .await
            .unwrap();
        let session = dump
            .terminals
            .iter()
            .find(|t| t.agent.is_some())
            .map(|t| t.session.clone())
            .expect("the agent's session");

        // It paints the spinning title: the cheapest heartbeat there is says a turn started.
        stack.fake_claude_stage("working").unwrap();
        stack
            .driver
            .wait_for("the title to say a turn is running", STEP, |d| {
                d.terminals.iter().any(|t| {
                    t.agent.as_deref() == Some("working")
                        && t.agent_source.as_deref() == Some("title")
                })
            })
            .await
            .unwrap();

        // It writes its conversation where Claude Code writes one; the host finds the file
        // from the session's own working directory and reads the turn out of it.
        stack.fake_claude_stage("transcript").unwrap();
        stack
            .driver
            .wait_for("the transcript to take over", STEP, |d| {
                d.terminals.iter().any(|t| t.agent_source.as_deref() == Some("transcript"))
            })
            .await
            .unwrap();
        stack.fake_claude_stage("done").unwrap();
        stack
            .driver
            .wait_for("the turn to finish", STEP, |d| {
                d.terminals.iter().any(|t| {
                    t.agent.as_deref() == Some("done")
                        && t.agent_source.as_deref() == Some("transcript")
                })
            })
            .await
            .unwrap();

        // ⌘⇧L reads the same discovered file: the conversation view needs no hooks either.
        stack.driver.keys("cmd-shift-l").await.unwrap();
        let dump = stack
            .driver
            .wait_for("the conversation", STEP, |d| {
                d.terminals.iter().any(|t| {
                    t.conversation
                        .as_ref()
                        .is_some_and(|c| c.entries.len() > TRANSCRIPT_LINES.len())
                })
            })
            .await
            .unwrap();
        let conversation = dump
            .terminals
            .iter()
            .find(|t| t.session == session)
            .and_then(|t| t.conversation.clone())
            .expect("the agent's conversation");
        assert_eq!(conversation.entries.first().map(String::as_str), Some(TRANSCRIPT_LINES[0]));
        assert_eq!(
            conversation.entries.last().map(String::as_str),
            Some("assistant: Both tests pass now.")
        );

        // The fake exits; with it the agent goes, whatever the last signal said.
        stack.fake_claude_stage("quit").unwrap();
        stack
            .driver
            .wait_for("the agent to go with its process", STEP, |d| {
                d.terminals.iter().all(|t| t.agent.is_none())
            })
            .await
            .unwrap();
        stack.shutdown().await;
    }

    #[tokio::test]
    async fn notes_and_zoom_change_the_canvas_as_dumped() {
        if !gated() {
            return;
        }
        let mut stack = Stack::launch("e2e-host").await.unwrap();
        let render_path = stack.path("note.png");
        let drv = &mut stack.driver;
        drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();

        drv.keys("cmd-shift-n").await.unwrap();
        let dump = drv.wait_for("a note", STEP, |d| d.item("note").is_some()).await.unwrap();
        let before = dump.item("note").unwrap().bounds;
        assert!((dump.zoom - 1.0).abs() < 1e-3, "{}", dump.zoom);

        // Zoom in twice: the camera scale and the item's window size both grow.
        drv.keys("cmd-= cmd-=").await.unwrap();
        let dump = drv.wait_for("zoom", STEP, |d| d.zoom > 1.05).await.unwrap();
        let after = dump.item("note").unwrap().bounds;
        assert!(after[2] > before[2] && after[3] > before[3], "{before:?} → {after:?}");

        // ⌘0 puts it back.
        drv.keys("cmd-0").await.unwrap();
        let dump = drv.wait_for("zoom reset", STEP, |d| (d.zoom - 1.0).abs() < 1e-3).await.unwrap();
        let reset = dump.item("note").unwrap().bounds;
        assert!((reset[2] - before[2]).abs() < 1.0, "{before:?} → {reset:?}");

        let frame = drv.render(&render_path).await.unwrap();
        assert_matches("note", &frame, TOLERANCE, &artifacts_dir()).unwrap();
        stack.shutdown().await;
    }

    /// A remote window whose target never draws: ⌘O picks it while it is still on screen, the
    /// helper takes it away before the stream opens, and the item says so instead of asking the
    /// host for refreshes no refresh can answer. Then the window draws again and the picture
    /// arrives without the client doing anything.
    #[tokio::test]
    async fn a_remote_window_that_never_draws_waits_instead_of_asking_forever() {
        if !gated() {
            return;
        }
        let mut stack = Stack::launch("e2e-host").await.unwrap();
        stack.driver.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
        let idle = stack.start_idle_window().await.unwrap();
        stack
            .driver
            .wait_for("the first shell", STEP, |d| {
                d.status == "connected" && d.item("terminal").is_some()
            })
            .await
            .unwrap();

        // ⌘O lists the host's windows. The picker shows on-screen windows only, which is why the
        // helper is still on screen here.
        stack.driver.keys("cmd-o").await.unwrap();
        let row = format!(", {}", idle.title());
        let dump = stack
            .driver
            .wait_for("the idle window in the picker", STEP, |d| {
                d.a11y.iter().any(|n| {
                    n.role == "Button" && n.label.as_deref().is_some_and(|l| l.ends_with(&row))
                })
            })
            .await
            .unwrap();
        let button = dump
            .a11y
            .iter()
            .find(|n| n.role == "Button" && n.label.as_deref().is_some_and(|l| l.ends_with(&row)))
            .unwrap_or_else(|| panic!("{:#?}", dump.a11y))
            .clone();

        // Off screen before the stream opens: the host can still capture it, and captures
        // nothing.
        idle.hide().unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        let [x, y, w, h] = button.bounds;
        stack.driver.click(x + w / 2.0, y + h / 2.0).await.unwrap();

        // The host says the target is idle; the item says which end everyone is waiting on, and
        // a screen reader is told the same thing.
        let dump = stack
            .driver
            .wait_for("the item to say the window is not drawing", STEP, |d| {
                d.screens.iter().any(|s| s.source == "idle")
            })
            .await
            .unwrap();
        assert_eq!(dump.screens.len(), 1, "{dump:#?}");
        assert_eq!(dump.screens[0].frames, 0, "an off-screen window produced pictures");
        assert!(
            dump.a11y_node("Status", Some("waiting for the window to draw…")).is_some(),
            "{:#?}",
            dump.a11y
        );

        // What the host was asked for over the whole time: a handful of refreshes, then silence.
        // The cap is the receiver's, and this is the only place it can be seen from outside.
        tokio::time::sleep(Duration::from_secs(3)).await;
        let screens = stack.host_screens().await.unwrap();
        let refreshes: u64 =
            screens.iter().filter_map(|s| s.get("stats")?.get("refreshes")?.as_u64()).sum();
        let cap = refresh_cap();
        assert!(
            refreshes <= cap,
            "{refreshes} refresh requests for a window that cannot answer one (cap {cap})"
        );

        // Drawing again is enough: no refresh, no reopen, the picture just starts.
        idle.show().unwrap();
        let dump = stack
            .driver
            .wait_for("the picture once the window draws", STEP, |d| {
                d.screens.iter().any(|s| s.source == "live" && s.frames > 0)
            })
            .await
            .unwrap();
        assert!(
            dump.a11y_node("Status", Some("waiting for the window to draw…")).is_none(),
            "{:#?}",
            dump.a11y
        );
        stack.shutdown().await;
    }
}

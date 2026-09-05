//! The real app, driven from inside: pair with a host, open a shell, type, read the rows back,
//! render frames with the app's own renderer and compare them with the goldens.
//!
//! Runs only with `SLOPTY_APP_E2E=1` (`cargo xtask e2e app`), since it launches the app;
//! it needs no permission from the machine.

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use slopty_e2e::harness::{TRANSCRIPT_LINES, artifacts_dir};
    use slopty_e2e::snapshot::assert_matches;
    use slopty_e2e::{Command, Stack};

    /// How long a host round trip (open a shell, run a command) may take.
    const STEP: Duration = Duration::from_secs(20);
    /// Window size for the renders: small, so the goldens stay small.
    const WINDOW: (f32, f32) = (900.0, 600.0);
    /// Fraction of pixels allowed to differ from a golden (hinting, RTT readout, cursor).
    const TOLERANCE: f64 = 0.01;

    fn gated() -> bool {
        if std::env::var_os("SLOPTY_APP_E2E").is_none() {
            eprintln!("skipped: set SLOPTY_APP_E2E=1 (or run `cargo xtask e2e app`)");
            return false;
        }
        true
    }

    /// Count the pixels that are not close to `bg`; a blank frame is a renderer failure, not
    /// a layout to compare.
    fn foreground_fraction(img: &image::RgbaImage) -> f64 {
        let bg = *img.get_pixel(0, 0);
        let differing = img
            .pixels()
            .filter(|p| p.0.iter().zip(bg.0.iter()).any(|(a, b)| a.abs_diff(*b) > 24))
            .count();
        let total = u64::from(img.width()).saturating_mul(u64::from(img.height()));
        #[expect(clippy::cast_precision_loss, reason = "pixel counts fit f64 exactly")]
        let f = differing as f64 / total as f64;
        f
    }

    #[tokio::test]
    async fn shell_typed_from_the_app_echoes_back_into_its_rows() {
        if !gated() {
            return;
        }
        let mut stack = Stack::launch("e2e-host").await.unwrap();
        let render_path = stack.path("terminal.png");
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
        assert_eq!(dump.items.len(), 1, "{dump:#?}");
        let term = dump.item("terminal").unwrap().clone();
        assert!(term.active, "{dump:#?}");
        assert!(term.bounds[2] > 100.0 && term.bounds[3] > 100.0, "{term:?}");

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
}

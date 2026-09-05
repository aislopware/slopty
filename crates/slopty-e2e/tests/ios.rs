//! The iOS app on the simulator, driven through its own test socket.
//!
//! Runs only with `SLOPTY_IOS_E2E=1` (`cargo xtask e2e ios [--sim iphone|ipad]`): the daemons
//! run on the Mac as for the app self-test, the app runs in the booted simulator named by
//! `SLOPTY_SIM_UDID` and binds its socket on the shared file system. Two tests, phone or
//! tablet: a shell opens, its rows come back, typed text echoes, and the terminal is sized for
//! the screen it is on (a phone shrinks it to the viewport, an iPad keeps the desktop size);
//! and a played Claude Code session (its hook handed to hostd from the test) shows its
//! conversation, inside the screen above the key bar, with a composer that types into the
//! shell. Each scenario renders the app's own frame (the fork's iOS `render_to_image`) and
//! compares it with a golden per device, `ios-phone-*.png` or `ios-pad-*.png`.

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use slopty_e2e::harness::{Simulator, Stack, TRANSCRIPT_LINES, artifacts_dir};
    use slopty_e2e::snapshot::{assert_matches, foreground_fraction};

    /// Per-step wait.
    const STEP: Duration = Duration::from_secs(30);
    /// The desktop terminal size a viewport this wide keeps (`slopty_client::canvas`).
    const DESKTOP_TERMINAL_WIDTH: f32 = 720.0;
    /// Fraction of pixels allowed to differ from a golden (hinting, RTT readout, cursor).
    const TOLERANCE: f64 = 0.01;
    /// Foreground below this is a blank frame: a fitted terminal's few lines are under 1 % of
    /// an iPad's 2064×2752 pixels.
    const BLANK: f64 = 0.002;

    /// Which device family the app is on, from its viewport: the golden's name prefix.
    fn device(window_width: f32) -> &'static str {
        if window_width >= DESKTOP_TERMINAL_WIDTH + 100.0 { "pad" } else { "phone" }
    }

    fn simulator() -> Option<Simulator> {
        if std::env::var_os("SLOPTY_IOS_E2E").is_none() {
            eprintln!("skipped: set SLOPTY_IOS_E2E=1 (or run `cargo xtask e2e ios`)");
            return None;
        }
        let udid = std::env::var("SLOPTY_SIM_UDID").expect("SLOPTY_SIM_UDID (a booted simulator)");
        let bundle_id = std::env::var("SLOPTY_SIM_BUNDLE_ID").expect("SLOPTY_SIM_BUNDLE_ID");
        Some(Simulator { udid, bundle_id })
    }

    #[tokio::test]
    async fn a_shell_on_the_simulator_echoes_and_is_sized_for_its_screen() {
        let Some(simulator) = simulator() else { return };
        let mut stack = Stack::launch_on_simulator("e2e-ios-host", simulator).await.unwrap();
        let render_path = stack.path("terminal.png");
        let drv = &mut stack.driver;

        let dump = drv
            .wait_for("the first shell with a prompt", STEP, |d| {
                d.status == "connected"
                    && d.item("terminal").is_some()
                    && d.terminals.iter().any(|t| t.rows.iter().any(|r| !r.is_empty()))
            })
            .await
            .unwrap();
        assert_eq!(dump.hosts.len(), 1, "{dump:#?}");
        assert_eq!(dump.items.len(), 1, "{dump:#?}");
        let term = dump.item("terminal").unwrap().clone();
        let [x, y, w, h] = term.bounds;
        let (vw, vh) = (dump.window.width, dump.window.height);
        assert!(vw > 300.0 && vh > 300.0, "window: {vw}x{vh}");
        // The host placed a desktop-sized terminal; the client fitted it to this screen.
        assert!(
            x >= 0.0 && y >= 0.0 && x + w <= vw + 1.0 && y + h <= vh + 1.0,
            "{term:?} in {vw}x{vh}"
        );
        if vw >= DESKTOP_TERMINAL_WIDTH + 100.0 {
            assert!(w >= DESKTOP_TERMINAL_WIDTH - 1.0, "tablet kept the desktop size: {term:?}");
        } else {
            assert!(w < DESKTOP_TERMINAL_WIDTH, "phone shrank the terminal: {term:?}");
        }

        drv.type_text("echo ios-$((6*7))").await.unwrap();
        drv.keys("enter").await.unwrap();
        let dump = drv
            .wait_for("the echo", STEP, |d| {
                d.rows_containing("ios-42").iter().any(|r| r.trim() == "ios-42")
            })
            .await
            .unwrap();
        assert!(!dump.rows_containing("echo ios-").is_empty(), "{dump:#?}");

        // The frame the app draws, from its own renderer, against this device's golden.
        let frame = drv.render(&render_path).await.unwrap();
        let fg = foreground_fraction(&frame);
        assert!(fg > BLANK, "frame is blank ({fg:.4} foreground)");
        let name = format!("ios-{}-terminal", device(vw));
        assert_matches(&name, &frame, TOLERANCE, &artifacts_dir()).unwrap();

        // ⌘N from a hardware keyboard (the same keystroke the socket dispatches) opens a second
        // shell; ⌘W closes it again.
        drv.keys("cmd-n").await.unwrap();
        let dump = drv.wait_for("a second shell", STEP, |d| d.items.len() == 2).await.unwrap();
        assert!(dump.items.iter().any(|i| i.id != term.id && i.active), "{dump:#?}");
        drv.keys("cmd-w").await.unwrap();
        drv.wait_for("the second shell to close", STEP, |d| d.items.len() == 1).await.unwrap();
        stack.shutdown().await;
    }

    /// The conversation view on the phone or the tablet: a hook handed to hostd (on the Mac)
    /// over its control socket names the fixture transcript, ⌘⇧L shows every entry with the
    /// composer above the key bar and focused, and the composer's text reaches the shell on
    /// Enter.
    #[tokio::test]
    async fn the_conversation_view_on_the_simulator() {
        let Some(simulator) = simulator() else { return };
        let mut stack = Stack::launch_on_simulator("e2e-ios-host", simulator).await.unwrap();
        let render_path = stack.path("conversation.png");
        let dump = stack
            .driver
            .wait_for("the first shell with a prompt", STEP, |d| {
                d.status == "connected"
                    && d.terminals.iter().any(|t| t.rows.iter().any(|r| !r.is_empty()))
            })
            .await
            .unwrap();
        let session = dump.terminals[0].session.clone();
        stack.play_hook(&session, "Stop", r#","last_assistant_message":"Fixed.""#).await.unwrap();
        let drv = &mut stack.driver;
        drv.wait_for("the agent to be seen", STEP, |d| {
            d.terminals.iter().any(|t| t.agent.as_deref() == Some("done"))
        })
        .await
        .unwrap();

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
        // The item (and the composer at its bottom) stays inside the screen, above the key
        // bar and the home indicator.
        let term = dump.item("terminal").unwrap();
        let [x, y, w, h] = term.bounds;
        assert!(
            x >= 0.0 && y >= 0.0 && x + w <= dump.window.width + 1.0 && y + h < dump.window.height,
            "{term:?} in {}x{}",
            dump.window.width,
            dump.window.height
        );

        drv.type_text("# from the composer").await.unwrap();
        drv.keys("enter").await.unwrap();
        let dump = drv
            .wait_for("the shell to echo the composer's line", STEP, |d| {
                !d.rows_containing("from the composer").is_empty()
            })
            .await
            .unwrap();
        assert_eq!(dump.terminals[0].conversation.as_ref().unwrap().composer, "");

        // The conversation as drawn on this device: every entry, the composer above the key bar.
        let frame = drv.render(&render_path).await.unwrap();
        assert!(foreground_fraction(&frame) > BLANK, "frame is blank");
        let name = format!("ios-{}-conversation", device(dump.window.width));
        assert_matches(&name, &frame, TOLERANCE, &artifacts_dir()).unwrap();

        drv.keys("cmd-shift-l").await.unwrap();
        drv.wait_for("the grid back", STEP, |d| d.terminals[0].conversation.is_none())
            .await
            .unwrap();
        stack.shutdown().await;
    }
}

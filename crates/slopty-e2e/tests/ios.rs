//! The iOS app on the simulator, driven through its own test socket.
//!
//! Runs only with `SLOPTY_IOS_E2E=1` (`cargo xtask e2e ios [--sim iphone|ipad]`): the daemons
//! run on the Mac as for the app self-test, the app runs in the booted simulator named by
//! `SLOPTY_SIM_UDID` and binds its socket on the shared file system. One test, phone or tablet:
//! a shell opens, its rows come back, typed text echoes, and the terminal is sized for the
//! screen it is on (a phone shrinks it to the viewport, an iPad keeps the desktop size).

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use slopty_e2e::harness::{Simulator, Stack};

    /// Per-step wait.
    const STEP: Duration = Duration::from_secs(30);
    /// The desktop terminal size a viewport this wide keeps (`slopty_client::canvas`).
    const DESKTOP_TERMINAL_WIDTH: f32 = 720.0;

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

        // ⌘N from a hardware keyboard (the same keystroke the socket dispatches) opens a second
        // shell; ⌘W closes it again.
        drv.keys("cmd-n").await.unwrap();
        let dump = drv.wait_for("a second shell", STEP, |d| d.items.len() == 2).await.unwrap();
        assert!(dump.items.iter().any(|i| i.id != term.id && i.active), "{dump:#?}");
        drv.keys("cmd-w").await.unwrap();
        drv.wait_for("the second shell to close", STEP, |d| d.items.len() == 1).await.unwrap();
        stack.shutdown().await;
    }
}

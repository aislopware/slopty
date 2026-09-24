//! The iOS app on the simulator, driven through its own test socket.
//!
//! Runs only with `SLOPTY_IOS_E2E=1` (`cargo xtask e2e ios [--sim iphone|ipad]`): the daemons
//! run on the Mac as for the app self-test, the app runs in the booted simulator named by
//! `SLOPTY_SIM_UDID` and binds its socket on the shared file system. Phone or tablet: a
//! shell opens, its rows come back, typed text echoes, and its column is sized for the screen
//! it is on (a phone's fills the screen but for the neighbours' peeks, an iPad's is half). The
//! scenario renders the app's own frame (the fork's iOS `render_to_image`) and compares it
//! with a golden per device, `ios-phone-*.png` or `ios-pad-*.png`.

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use slopty_e2e::harness::{Simulator, Stack, artifacts_dir};
    use slopty_e2e::snapshot::{assert_matches, foreground_fraction};
    use slopty_e2e::{Command, Driver};

    /// Per-step wait.
    const STEP: Duration = Duration::from_secs(30);
    /// A viewport narrower than this is a phone (`slopty_client::layout`'s `phone_below`).
    const PHONE_BELOW: f32 = 700.0;
    /// Fraction of pixels allowed to differ from a golden (hinting, RTT readout, cursor).
    const TOLERANCE: f64 = 0.01;
    /// Long enough for a spring or the soft keyboard to come to rest before a golden.
    const SETTLE: Duration = Duration::from_millis(600);
    /// Foreground below this is a blank frame: a fitted terminal's few lines are under 1 % of
    /// an iPad's 2064×2752 pixels.
    const BLANK: f64 = 0.002;

    /// Which device family the app is on, from its viewport: the golden's name prefix.
    fn device(window_width: f32) -> &'static str {
        if window_width >= PHONE_BELOW { "pad" } else { "phone" }
    }

    /// Tap the middle of the `role` node labelled `label`.
    async fn tap(drv: &mut Driver, role: &str, label: &str) {
        let d = drv.dump().await.unwrap();
        let node =
            d.a11y_node(role, Some(label)).unwrap_or_else(|| panic!("{label}: {:#?}", d.a11y));
        let [left, top, width, height] = node.bounds;
        drv.ui_tap(left + width / 2.0, top + height / 2.0).await.unwrap();
    }

    /// Without a hardware keyboard the titlebar's "…" is the way to every action: a tap opens
    /// its menu, "Command palette" opens the palette with the soft keyboard up.
    async fn open_palette(drv: &mut Driver) {
        tap(drv, "Button", "More").await;
        drv.wait_for("the … menu", STEP, |d| {
            d.a11y_node("MenuItem", Some("Command palette")).is_some()
        })
        .await
        .unwrap();
        tap(drv, "MenuItem", "Command palette").await;
        drv.wait_for("the palette", STEP, |d| d.a11y_node("Dialog", Some("Commands")).is_some())
            .await
            .unwrap();
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

    /// Render the frame and hold it against `golden/ios-<device>-<state>.png`; `crop` keeps
    /// only the top-left `(width, height)` points, the app's own area in a split view.
    async fn golden(
        drv: &mut Driver,
        dir: &std::path::Path,
        device: &str,
        state: &str,
        crop: Option<(f32, f32)>,
    ) {
        tokio::time::sleep(SETTLE).await;
        let name = format!("ios-{device}-{state}");
        let mut frame = drv.render(&dir.join(format!("{name}.png"))).await.unwrap();
        if let Some((w, h)) = crop {
            let scale = drv.dump().await.unwrap().window.scale;
            #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "pixels")]
            let (w, h) = ((w * scale).round() as u32, (h * scale).round() as u32);
            frame = image::imageops::crop_imm(&frame, 0, 0, w, h).to_image();
        }
        assert_matches(&name, &frame, TOLERANCE, &artifacts_dir()).unwrap();
    }

    #[tokio::test]
    async fn a_shell_on_the_simulator_echoes_and_is_sized_for_its_screen() {
        let Some(simulator) = simulator() else { return };
        let mut stack =
            Stack::launch_first_run_on_simulator("e2e-ios-host", simulator).await.unwrap();
        let dir = stack.dir.path().to_path_buf();
        let render_path = stack.path("terminal.png");

        // The first run: the way in and nothing else, on this device's screen.
        let dump = stack.driver.wait_for("the connect panel", STEP, |d| d.adding).await.unwrap();
        let dev = device(dump.window.width);
        assert!(dump.a11y_node("Heading", Some("Connect to a server")).is_some(), "{dump:#?}");
        assert!(dump.a11y_node("Button", Some("More")).is_none(), "{:#?}", dump.a11y);
        golden(&mut stack.driver, &dir, dev, "first-run", None).await;
        stack.add_worker().await.unwrap();
        let drv = &mut stack.driver;

        let dump = drv
            .wait_for("the first shell with a prompt", STEP, |d| {
                d.status == "connected"
                    && d.item("terminal").is_some()
                    && d.terminals.iter().any(|t| t.rows.iter().any(|r| !r.is_empty()))
            })
            .await
            .unwrap();
        assert_eq!(dump.workers.len(), 1, "{dump:#?}");
        assert_eq!(dump.items.len(), 1, "{dump:#?}");
        let term = dump.item("terminal").unwrap().clone();
        // The grid as VoiceOver gets it: a terminal whose value is the cursor row, on screen.
        let grid = dump.a11y_node("Terminal", None).unwrap_or_else(|| panic!("{:#?}", dump.a11y));
        assert!(grid.value.as_deref().is_some_and(|v| !v.is_empty()), "cursor row: {grid:?}");
        assert!(grid.bounds[0] >= 0.0 && grid.bounds[1] >= 0.0, "{grid:?}");
        let [x, y, w, h] = term.bounds;
        let (vw, vh) = (dump.window.width, dump.window.height);
        assert!(vw > 300.0 && vh > 300.0, "window: {vw}x{vh}");
        // The column is on screen and sized for it.
        assert!(
            x >= 0.0 && y >= 0.0 && x + w <= vw + 1.0 && y + h <= vh + 1.0,
            "{term:?} in {vw}x{vh}"
        );
        if vw < PHONE_BELOW {
            assert!(w > vw * 0.85, "a phone column fills the screen: {term:?} in {vw}");
        } else {
            assert!(w > vw * 0.4 && w < vw * 0.6, "a tablet column is half the screen: {term:?}");
        }

        drv.type_text("echo ios-$((6*7))").await.unwrap();
        drv.keys("enter").await.unwrap();
        // The echo, and the prompt back under it (the cursor's row is the grid's a11y value):
        // a frame rendered between the two is a different picture.
        let dump = drv
            .wait_for("the echo and the next prompt", STEP, |d| {
                d.rows_containing("ios-42").iter().any(|r| r.trim() == "ios-42")
                    && d.a11y_node("Terminal", None).is_some_and(|g| {
                        g.value
                            .as_deref()
                            .is_some_and(|v| !v.trim().is_empty() && !v.contains("ios-42"))
                    })
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

        // Without a hardware keyboard: the palette from the titlebar's "…", the soft keyboard
        // types into its field, ↩ runs the line.
        open_palette(drv).await;
        drv.ui_insert_text("new note").await.unwrap();
        drv.wait_for("one line", STEP, |d| {
            d.a11y.iter().filter(|n| n.role == "ListBoxOption").count() == 1
        })
        .await
        .unwrap();
        drv.keys("enter").await.unwrap();
        drv.wait_for("a note from the palette", STEP, |d| {
            d.item("note").is_some() && d.a11y_node("Dialog", Some("Commands")).is_none()
        })
        .await
        .unwrap();

        // The phone has no editor for a file in its sandbox: "Open settings" from the palette
        // puts `settings.toml` (the commented defaults here) in the in-app editor, ⌘A and the
        // soft keyboard replace it with one section, ⌘↩ writes the file.
        open_palette(drv).await;
        drv.ui_insert_text("open settings").await.unwrap();
        drv.wait_for("one line", STEP, |d| {
            d.a11y.iter().filter(|n| n.role == "ListBoxOption").count() == 1
        })
        .await
        .unwrap();
        drv.keys("enter").await.unwrap();
        drv.wait_for("the settings editor", STEP, |d| {
            d.a11y_node("Dialog", Some("Settings")).is_some()
        })
        .await
        .unwrap();
        drv.keys("cmd-a").await.unwrap();
        drv.ui_insert_text("[terminal]\nbell_alert = false\n").await.unwrap();
        drv.keys("cmd-enter").await.unwrap();
        drv.wait_for("the settings editor to close", STEP, |d| {
            d.a11y_node("Dialog", Some("Settings")).is_none()
        })
        .await
        .unwrap();
        let saved =
            std::fs::read_to_string(stack.dir.path().join("app").join("settings.toml")).unwrap();
        assert!(saved.contains("bell_alert = false"), "{saved}");

        // The phone names a tile the same way: "Name this tile" from the palette puts the
        // field in the focused tile's header, the soft keyboard types into it, ↩ keeps it.
        open_palette(drv).await;
        drv.ui_insert_text("name this").await.unwrap();
        drv.wait_for("one line", STEP, |d| {
            d.a11y.iter().filter(|n| n.role == "ListBoxOption").count() == 1
        })
        .await
        .unwrap();
        drv.keys("enter").await.unwrap();
        drv.wait_for("the name field", STEP, |d| {
            d.a11y_node("TextInput", Some("Tile name")).is_some()
        })
        .await
        .unwrap();
        drv.ui_insert_text("scratch").await.unwrap();
        drv.keys("enter").await.unwrap();
        drv.wait_for("the note named", STEP, |d| {
            d.a11y_node("Heading", Some("note scratch")).is_some()
                && d.a11y_node("TextInput", Some("Tile name")).is_none()
        })
        .await
        .unwrap();

        // The shell and the note side by side, the note focused: columns and their marks,
        // once the take-back offer for the closed shell has gone (it lasts five seconds).
        drv.wait_for("the undo offer to lapse", STEP, |d| d.notice.is_none()).await.unwrap();
        golden(drv, &dir, dev, "columns", None).await;
        // The palette as the phone reaches it, from "…".
        open_palette(drv).await;
        golden(drv, &dir, dev, "palette", None).await;
        drv.keys("escape").await.unwrap();
        drv.wait_for("the palette closed", STEP, |d| {
            d.a11y_node("Dialog", Some("Commands")).is_none()
        })
        .await
        .unwrap();
        if dev == "pad" {
            // Split View, the app on half the screen.
            let (w, h) = (dump.window.width / 2.0 - 5.0, dump.window.height);
            drv.ok(&Command::Resize { width: w, height: h }).await.unwrap();
            // The columns keep their proportions of the narrower window; the active one rests
            // inside it.
            drv.wait_for("the half-width layout", STEP, |d| {
                d.items.iter().any(|i| {
                    i.active
                        && i.bounds[2] < w / 2.0
                        && i.bounds[0] >= 0.0
                        && i.bounds[0] + i.bounds[2] <= w + 1.0
                })
            })
            .await
            .unwrap();
            golden(drv, &dir, dev, "split", Some((w, h))).await;
        }
        stack.shutdown().await;
    }
}

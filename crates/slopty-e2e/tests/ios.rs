//! The iOS app on the simulator, driven through its own test socket.
//!
//! Live (`#[ignore]`), run by `cargo xtask e2e ios [--sim iphone|ipad]`: the daemons
//! run on the Mac as for the app self-test, the app runs in the booted simulator named by
//! `SLOPTY_SIM_UDID` and binds its socket on the shared file system. Phone or tablet: a
//! shell opens, its rows come back, typed text echoes, and its column is sized for the screen
//! it is on (a phone's fills the screen but for the neighbours' peeks, an iPad's is half). The
//! scenario renders the app's own frame (the fork's iOS `render_to_image`) and compares it
//! with a golden per device, `ios-phone-*.png` or `ios-pad-*.png`.

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use slopty_e2e::harness::{APPEARANCE, Simulator, Stack, artifacts_dir, pinned_settings};
    use slopty_e2e::snapshot::{assert_matches, foreground_fraction};
    use slopty_e2e::{Command, Driver, Dump};

    /// Per-step wait.
    const STEP: Duration = Duration::from_secs(30);
    /// A viewport narrower than this is a phone (`slopty_client::layout`'s `phone_below`).
    const PHONE_BELOW: f32 = 700.0;
    /// Fraction of pixels allowed to differ from a golden: with the cursor steady, two runs
    /// differ by 0.009 % at most (iPhone; the iPad 0.002 %), so 0.05 % is five times the noise.
    /// At 1 %, an iPad frame, mostly bare workspace, passed with its whole chrome redrawn (0.84 %).
    const TOLERANCE: f64 = 0.0005;
    /// Long enough for a spring or the soft keyboard to come to rest before a golden.
    const SETTLE: Duration = Duration::from_millis(600);
    /// Foreground below this is a blank frame: a fitted terminal's few lines are under 1 % of
    /// an iPad's 2064×2752 pixels, and the light chrome's hairlines are too faint to count.
    const BLANK: f64 = 0.001;

    /// Which device family the app is on, from its viewport: the golden's name prefix.
    fn device(window_width: f32) -> &'static str {
        if window_width >= PHONE_BELOW { "pad" } else { "phone" }
    }

    /// The first shell still shows its echo: its rows survive the resizes the soft keyboard
    /// and the columns put it through (fewer rows trim blank rows at the bottom first).
    fn echo_on_screen(d: &Dump) -> bool {
        !d.rows_containing("echo ios-").is_empty()
            && d.rows_containing("ios-42").iter().any(|r| r.trim() == "ios-42")
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

    /// The booted simulator `cargo xtask e2e ios` installed the app on.
    fn simulator() -> Simulator {
        let udid = std::env::var("SLOPTY_SIM_UDID").expect("SLOPTY_SIM_UDID (a booted simulator)");
        let bundle_id = std::env::var("SLOPTY_SIM_BUNDLE_ID").expect("SLOPTY_SIM_BUNDLE_ID");
        Simulator { udid, bundle_id }
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
    #[ignore = "live: cargo xtask e2e ios"]
    async fn a_shell_on_the_simulator_echoes_and_is_sized_for_its_screen() {
        let simulator = simulator();
        let mut stack =
            Stack::launch_first_run_on_simulator("e2e-ios-worker", simulator).await.unwrap();
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
        // A lone column fills the width on a phone and a tablet alike
        // (docs/decisions/workspace.md).
        assert!(w > vw * 0.85, "a lone column fills the screen: {term:?} in {vw}");

        drv.type_text("echo ios-$((6*7))").await.unwrap();
        drv.keys("enter").await.unwrap();
        // The echo, and the prompt back under it (the cursor's row is the grid's a11y value):
        // a frame rendered between the two is a different picture. So is one between the
        // prompt and zle taking the line: the command left the default block cursor, and only
        // zle's line-init sets the insert bar again.
        let dump = drv
            .wait_for("the echo and the next prompt", STEP, |d| {
                d.terminals.iter().any(|t| t.cursor_shape == "Bar")
                    && d.rows_containing("ios-42").iter().any(|r| r.trim() == "ios-42")
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
        // shell, focused; ⌘W closes the focused tile, that second shell, and gives the focus
        // back to the first with its echo.
        drv.keys("cmd-n").await.unwrap();
        let dump = drv.wait_for("a second shell", STEP, |d| d.items.len() == 2).await.unwrap();
        assert!(dump.items.iter().any(|i| i.id != term.id && i.active), "{dump:#?}");
        drv.keys("cmd-w").await.unwrap();
        let dump =
            drv.wait_for("the second shell to close", STEP, |d| d.items.len() == 1).await.unwrap();
        assert!(dump.items[0].id == term.id && dump.items[0].active, "{dump:#?}");
        assert!(echo_on_screen(&dump), "{:#?}", dump.terminals);

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
        // opens the settings on their form, "Edit as TOML" puts `settings.toml` in the in-app
        // editor, ⌘A and the soft keyboard replace it, ⌘↩ writes the file. The new text keeps
        // the harness's pins: without them the cursor goes back to blinking as the shell asks,
        // and a golden holds it or not by the phase.
        open_palette(drv).await;
        drv.ui_insert_text("open settings").await.unwrap();
        drv.wait_for("one line", STEP, |d| {
            d.a11y.iter().filter(|n| n.role == "ListBoxOption").count() == 1
        })
        .await
        .unwrap();
        drv.keys("enter").await.unwrap();
        let dump = drv
            .wait_for("the settings editor", STEP, |d| {
                d.a11y_node("Dialog", Some("Settings")).is_some()
            })
            .await
            .unwrap();
        let [left, top, width, height] =
            dump.a11y_node("Button", Some("Edit as TOML")).expect("the file's link").bounds;
        drv.ui_tap(left + width / 2.0, top + height / 2.0).await.unwrap();
        drv.wait_for("the file's text", STEP, |d| {
            d.a11y_node("Button", Some("Edit with controls")).is_some()
        })
        .await
        .unwrap();
        drv.keys("cmd-a").await.unwrap();
        let settings = format!("{}bell_alert = false\n", pinned_settings(APPEARANCE));
        drv.ui_insert_text(&settings).await.unwrap();
        drv.keys("cmd-enter").await.unwrap();
        drv.wait_for("the settings editor to close", STEP, |d| {
            d.a11y_node("Dialog", Some("Settings")).is_none()
        })
        .await
        .unwrap();
        let saved =
            std::fs::read_to_string(stack.dir.path().join("app").join("settings.toml")).unwrap();
        assert_eq!(saved, settings);

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
        let dump =
            drv.wait_for("the undo offer to lapse", STEP, |d| d.notice.is_none()).await.unwrap();
        // The soft keyboard came and went with each field, and the note halved the shell's
        // width: a shell shrunk by rows keeps what it showed.
        assert!(echo_on_screen(&dump), "{:#?}", dump.terminals);
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
        // The navigator from the title bar's toggle: on a phone a drawer over a scrim that
        // leaves an edge of the strip, on an iPad a panel over the strip. The shell's row
        // closes it with the shell focused.
        tap(drv, "Button", "Navigator").await;
        let shown = drv
            .wait_for("the navigator", STEP, |d| {
                d.a11y_node("Navigation", Some("Navigator")).is_some()
            })
            .await
            .unwrap();
        let [nav_x, _, nav_w, _] = shown.a11y_node("Navigation", Some("Navigator")).unwrap().bounds;
        assert!(nav_x.abs() < 1.0 && nav_w < shown.window.width * 0.9, "{nav_x} {nav_w}");
        golden(drv, &dir, dev, "navigator", None).await;
        // A shell is titled by what it runs or where it stands, so its row is found by the
        // title its header shows.
        let title = shown
            .a11y
            .iter()
            .find_map(|n| {
                let label = n.label.as_deref().filter(|_| n.role == "Heading")?;
                let rest = label.strip_prefix("terminal")?;
                Some(rest.strip_prefix(' ').unwrap_or(label).to_owned())
            })
            .unwrap_or_else(|| panic!("the shell's heading: {:#?}", shown.a11y));
        let row = shown
            .a11y
            .iter()
            .find(|n| {
                n.role == "Button"
                    && n.label.as_deref().is_some_and(|l| l.starts_with(title.as_str()))
                    && n.bounds[0] + n.bounds[2] <= nav_x + nav_w + 1.0
            })
            .unwrap_or_else(|| panic!("the shell's row: {:#?}", shown.a11y));
        let [left, top, width, height] = row.bounds;
        drv.ui_tap(left + width / 2.0, top + height / 2.0).await.unwrap();
        drv.wait_for("the navigator closed on the shell", STEP, |d| {
            d.a11y_node("Navigation", Some("Navigator")).is_none()
                && d.item("terminal").is_some_and(|t| t.active)
        })
        .await
        .unwrap();
        if dev == "pad" {
            // Split View, the app on half the screen.
            let (w, h) = (dump.window.width / 2.0 - 5.0, dump.window.height);
            drv.ok(&Command::Resize { width: w, height: h }).await.unwrap();
            // Compact: the active column takes the whole width, edge to edge (a phone has no
            // struts), and the other column waits off screen beside it.
            drv.wait_for("one full-width column", STEP, |d| {
                let Some(active) = d.items.iter().find(|i| i.active) else { return false };
                let (left, right) = (active.bounds[0], active.bounds[0] + active.bounds[2]);
                active.bounds[2] >= w - 1.0
                    && left >= 0.0
                    && right <= w + 1.0
                    && d.items.len() >= 2
                    && d.items.iter().filter(|i| !i.active).all(|i| {
                        i.bounds[0] + i.bounds[2] <= left + 1.0 || i.bounds[0] >= right - 1.0
                    })
            })
            .await
            .unwrap();
            let split = drv.dump().await.unwrap();
            assert!(echo_on_screen(&split), "{:#?}", split.terminals);
            golden(drv, &dir, dev, "split", Some((w, h))).await;
        }
        stack.shutdown().await;
    }
}

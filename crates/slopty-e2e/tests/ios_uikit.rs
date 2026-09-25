//! The iOS app's own input path, driven at the UIKit boundary through its test socket.
//!
//! Runs only with `SLOPTY_IOS_E2E=1` (`cargo xtask e2e ios [--sim iphone|ipad]`), like `ios.rs`.
//! Where that file drives GPUI's dispatch (`Keys`, `Click`), this one delivers *described* UIKit
//! events (`UiKeyPress`, `UiTouch`, `UiPinch`, `UiInsertText`, `UiDeleteBackward`): the fork's
//! metal view runs the same code its `pressesBegan:` / `touchesBegan:` / pinch target /
//! `insertText:` / `deleteBackward` run once they have read their UIKit objects, so a lost
//! modifier, a wrongly mapped touch phase or a key the text system should have typed shows up
//! here and nowhere else. Every assertion reads the dump (rows, focus, overview, bounds, a11y);
//! no golden is involved.

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use slopty_e2e::harness::{Simulator, Stack};
    use slopty_e2e::{Driver, Dump, UiTouchPhase, UiTouchPoint};

    /// Per-step wait.
    const STEP: Duration = Duration::from_secs(30);
    /// How long a finger rests before lifting, so the release carries no velocity.
    const STILL: Duration = Duration::from_millis(100);
    /// Longer than the view's key repeat delay (400 ms) plus a few repeat ticks: a held key
    /// would have repeated by then.
    const REPEAT_WINDOW: Duration = Duration::from_millis(800);

    fn simulator() -> Option<Simulator> {
        if std::env::var_os("SLOPTY_IOS_E2E").is_none() {
            eprintln!("skipped: set SLOPTY_IOS_E2E=1 (or run `cargo xtask e2e ios`)");
            return None;
        }
        let udid = std::env::var("SLOPTY_SIM_UDID").expect("SLOPTY_SIM_UDID (a booted simulator)");
        let bundle_id = std::env::var("SLOPTY_SIM_BUNDLE_ID").expect("SLOPTY_SIM_BUNDLE_ID");
        Some(Simulator { udid, bundle_id })
    }

    /// The app connected to its worker and its first shell at a prompt.
    async fn shell(stack: &mut Stack) -> Dump {
        stack
            .driver
            .wait_for("the first shell with a prompt", STEP, |d| {
                d.status == "connected"
                    && d.item("terminal").is_some()
                    && d.terminals.iter().any(|t| t.rows.iter().any(|r| !r.is_empty()))
            })
            .await
            .unwrap()
    }

    /// `cat -v` typed through the text system: every control byte the shell gets from then
    /// on is echoed in caret notation, so a key's bytes can be read off the rows.
    async fn start_cat_v(stack: &mut Stack) {
        if let Err(e) = stack.driver.ui_insert_text("cat -v").await {
            panic!("{e:#}\napp log:\n{}", app_log_tail(stack, 40));
        }
        let drv = &mut stack.driver;
        drv.ui_key("enter").await.unwrap();
        drv.wait_for("cat -v to start", STEP, |d| !d.rows_containing("cat -v").is_empty())
            .await
            .unwrap();
    }

    /// The last `lines` of the app's own log mirror (`apps/slopty-ios`, under the self-test):
    /// the reason when the app stops answering.
    fn app_log_tail(stack: &Stack, lines: usize) -> String {
        let text = std::fs::read_to_string(stack.path("app").join("app.log")).unwrap_or_default();
        let all: Vec<&str> = text.lines().collect();
        all.get(all.len().saturating_sub(lines)..).unwrap_or_default().join("\n")
    }

    /// A horizontal two-finger swipe of `dx` points across the window's middle, the fingers
    /// resting before they lift so the release carries no velocity (no fling).
    async fn swipe(drv: &mut Driver, dump: &Dump, dx: f32) {
        let (vw, vh) = (dump.window.width, dump.window.height);
        let (x0, y) = (vw / 2.0 - dx / 2.0 - 20.0, vh / 2.0);
        let fingers = |x: f32| {
            [UiTouchPoint { id: 7, x, y }, UiTouchPoint { id: 8, x: x + 40.0, y: y + 30.0 }]
        };
        let steps = 6_u32;
        drv.ui_touch(&fingers(x0), UiTouchPhase::Began).await.unwrap();
        for i in 1..=steps {
            #[expect(clippy::cast_precision_loss, reason = "six steps")]
            let x = dx.mul_add(i as f32 / steps as f32, x0);
            drv.ui_touch(&fingers(x), UiTouchPhase::Moved).await.unwrap();
        }
        // A finger that stops reports nothing until it lifts (UIKit sends no `touchesMoved:`
        // for a still touch); the pause is what tells the recognizer not to fling.
        tokio::time::sleep(STILL).await;
        drv.ui_touch(&fingers(x0 + dx), UiTouchPhase::Ended).await.unwrap();
    }

    /// Tap the middle of the `role` node labelled `label`.
    async fn tap(drv: &mut Driver, role: &str, label: &str) {
        let d = drv.dump().await.unwrap();
        let node =
            d.a11y_node(role, Some(label)).unwrap_or_else(|| panic!("{label}: {:#?}", d.a11y));
        let [left, top, width, height] = node.bounds;
        drv.ui_tap(left + width / 2.0, top + height / 2.0).await.unwrap();
    }

    /// A hardware keyboard through `pressesBegan:` / `pressesEnded:` on the metal view: a
    /// ⌘⇧ chord reaches the keymaps (a note tile opens), arrows and Escape reach
    /// the shell as their escape sequences, and a plain key while the terminal is editing is
    /// left to the text system, which types it.
    #[tokio::test]
    async fn a_hardware_keyboard_on_the_simulator_arrives_through_presses() {
        let Some(simulator) = simulator() else { return };
        let mut stack = Stack::launch_on_simulator("e2e-ios-worker", simulator).await.unwrap();
        let dump = shell(&mut stack).await;
        let session = dump.terminals[0].session.clone();

        // Plain keys, one press each: the view hands them back to UIKit's text system.
        for key in ["c", "a", "t", "space", "-", "v", "enter"] {
            if let Err(e) = stack.driver.ui_key(key).await {
                panic!("{key}: {e:#}\napp log:\n{}", app_log_tail(&stack, 40));
            }
        }
        let drv = &mut stack.driver;
        drv.wait_for("cat -v from the keys", STEP, |d| !d.rows_containing("cat -v").is_empty())
            .await
            .unwrap();
        // Named keys are consumed by the view and encoded by the terminal: the tty echoes
        // them in caret notation under cat -v.
        drv.ui_key("escape").await.unwrap();
        drv.ui_key("up").await.unwrap();
        drv.ui_key("right").await.unwrap();
        let dump = drv
            .wait_for("the escape and arrow sequences", STEP, |d| {
                !d.rows_containing("^[^[[A^[[C").is_empty()
            })
            .await
            .unwrap();
        // A cancelled press releases the key: the view repeats a held key itself after 400 ms,
        // so a `left` that begins and is cancelled prints its sequence once and never again.
        // (Read before any other key: a later press would release it too.)
        let left_arrows = |d: &Dump| -> usize {
            d.terminals.iter().flat_map(|t| t.rows.iter()).map(|r| r.matches("^[[D").count()).sum()
        };
        drv.ok(&slopty_e2e::Command::UiKeyPress {
            usage: slopty_e2e::hid::usage("left").unwrap(),
            modifiers: String::new(),
            phase: slopty_e2e::UiPressPhase::Began,
        })
        .await
        .unwrap();
        drv.wait_for("the left arrow once", STEP, |d| left_arrows(d) == 1).await.unwrap();
        drv.ok(&slopty_e2e::Command::UiKeyPress {
            usage: slopty_e2e::hid::usage("left").unwrap(),
            modifiers: String::new(),
            phase: slopty_e2e::UiPressPhase::Cancelled,
        })
        .await
        .unwrap();
        tokio::time::sleep(REPEAT_WINDOW).await;
        let later = drv.dump().await.unwrap();
        assert_eq!(left_arrows(&later), 1, "a cancelled key does not repeat: {later:#?}");
        // ⌃C is a chord (no key_char): consumed, encoded, and cat exits.
        drv.ui_key("ctrl-c").await.unwrap();
        drv.wait_for("cat to exit", STEP, |d| !d.rows_containing("^C").is_empty()).await.unwrap();
        assert_eq!(dump.focused, format!("terminal:{session}"), "{dump:#?}");

        // ⌘⇧N: command and shift from the press's flags, `n` from its usage; ⌘W takes the
        // note away again and the shell has the keyboard back.
        drv.ui_key("cmd-shift-n").await.unwrap();
        drv.wait_for("a note tile", STEP, |d| d.item("note").is_some()).await.unwrap();
        drv.ui_key("cmd-w").await.unwrap();
        drv.wait_for("the note gone and the shell focused", STEP, |d| {
            d.item("note").is_none() && d.focused == format!("terminal:{session}")
        })
        .await
        .unwrap();
        // A cancelled chord leaves no stuck modifier: the next plain key types plainly.
        drv.ok(&slopty_e2e::Command::UiKeyPress {
            usage: slopty_e2e::hid::usage("a").unwrap(),
            modifiers: "cmd".into(),
            phase: slopty_e2e::UiPressPhase::Began,
        })
        .await
        .unwrap();
        drv.ok(&slopty_e2e::Command::UiKeyPress {
            usage: slopty_e2e::hid::usage("a").unwrap(),
            modifiers: "cmd".into(),
            phase: slopty_e2e::UiPressPhase::Cancelled,
        })
        .await
        .unwrap();
        drv.ui_key("x").await.unwrap();
        drv.wait_for("x typed plainly after the cancelled chord", STEP, |d| {
            !d.rows_containing("x").is_empty()
        })
        .await
        .unwrap();
        stack.shutdown().await;
    }

    /// Fingers through `touchesBegan:` / `touchesMoved:` / `touchesEnded:` and the pinch
    /// recognizer's target. With two shells in two columns (⌘N): a horizontal two-finger swipe
    /// drags the strip with the fingers and snaps to the column it lands on, which takes the
    /// focus; taps reach the titlebar's "…" menu and a tile in the overview; a pinch in opens
    /// the overview and a pinch out closes it.
    #[tokio::test]
    async fn fingers_on_the_simulator_swipe_tap_and_pinch() {
        let Some(simulator) = simulator() else { return };
        let mut stack = Stack::launch_on_simulator("e2e-ios-worker", simulator).await.unwrap();
        let dump = shell(&mut stack).await;
        let first = dump.item("terminal").unwrap().clone();
        let drv = &mut stack.driver;
        drv.keys("cmd-n").await.unwrap();
        let two = drv
            // At rest: the columns abut. An iPad's lone column sits centred and slides left as
            // the second opens beside it, and a dump taken in that slide is no starting point.
            .wait_for("a second shell, focused, beside the first", STEP, |d| {
                let first_right = d
                    .items
                    .iter()
                    .find(|i| i.id == first.id)
                    .map(|i| i.bounds[0] + i.bounds[2]);
                d.items.len() == 2
                    && d.items.iter().any(|i| {
                        i.id != first.id
                            && i.active
                            && first_right.is_some_and(|right| (right - i.bounds[0]).abs() < 1.0)
                    })
            })
            .await
            .unwrap();
        let item_by = |d: &Dump, id: &str| d.items.iter().find(|i| i.id == id).unwrap().clone();
        let second = two.items.iter().find(|i| i.id != first.id).unwrap().clone();
        assert_eq!(second.pos[1], item_by(&two, &first.id).pos[1] + 1, "{two:#?}");
        let [x0, y0, ..] = item_by(&two, &first.id).bounds;

        // A swipe to the right drags the first column back into view: the content follows
        // the fingers, and the snap focuses the column it lands on.
        let vw = two.window.width;
        swipe(drv, &two, vw * 0.6).await;
        let back = drv
            .wait_for("the swipe to focus the first column", STEP, |d| {
                item_by(d, &first.id).active
                    && d.focused == format!("terminal:{}", first.session.as_deref().unwrap_or(""))
            })
            .await
            .unwrap();
        let [x1, y1, w1, _] = item_by(&back, &first.id).bounds;
        assert!(
            x1 >= x0 && x1 >= 0.0 && x1 + w1 <= vw + 1.0,
            "in view: {x0} → {x1}\nbefore: {:#?}\nafter: {:#?}",
            two.items,
            back.items
        );
        assert!((y1 - y0).abs() < 2.0, "locked to the swipe's axis: {y0} → {y1}");
        assert!(!back.overview, "a swipe is not a pinch");

        // And to the left, the second column again.
        swipe(drv, &back, -vw * 0.6).await;
        drv.wait_for("the swipe to focus the second column", STEP, |d| {
            item_by(d, &second.id).active
        })
        .await
        .unwrap();

        // A tap on "…", a tap on its "Overview": every workspace at a glance. A tap on the
        // first shell there focuses it and closes the overview.
        tap(drv, "Button", "More").await;
        drv.wait_for("the … menu", STEP, |d| d.a11y_node("MenuItem", Some("Overview")).is_some())
            .await
            .unwrap();
        tap(drv, "MenuItem", "Overview").await;
        let over = drv.wait_for("the overview", STEP, |d| d.overview).await.unwrap();
        let (tx, ty) = item_by(&over, &first.id).center();
        drv.ui_tap(tx, ty).await.unwrap();
        drv.wait_for("the tap to focus the first shell", STEP, |d| {
            !d.overview && item_by(d, &first.id).active
        })
        .await
        .unwrap();

        // A pinch in opens the overview, a pinch out closes it.
        let (cx, cy) = (vw / 2.0, two.window.height / 2.0);
        drv.ui_pinch(cx, cy, 0.6, 4).await.unwrap();
        drv.wait_for("a pinch in to open the overview", STEP, |d| d.overview).await.unwrap();
        drv.ui_pinch(cx, cy, 1.6, 4).await.unwrap();
        let closed = drv.wait_for("a pinch out to close it", STEP, |d| !d.overview).await.unwrap();
        assert!(item_by(&closed, &first.id).active, "the focus kept: {closed:#?}");
        stack.shutdown().await;
    }

    /// The soft keyboard through the text input view's `insertText:` / `deleteBackward`: the
    /// terminal's input handler gets the text as typed, and a Telex-style composition, which
    /// reaches a `UIKeyInput` responder as the base letter, a delete and the composed letter,
    /// leaves exactly the composed letter in the shell (BSD `cat -v` prints a printable
    /// UTF-8 letter as itself; only control bytes get the caret form).
    #[tokio::test]
    async fn the_soft_keyboard_on_the_simulator_types_through_insert_text() {
        let Some(simulator) = simulator() else { return };
        let mut stack = Stack::launch_on_simulator("e2e-ios-worker", simulator).await.unwrap();
        shell(&mut stack).await;
        start_cat_v(&mut stack).await;
        let drv = &mut stack.driver;

        drv.ui_insert_text("a").await.unwrap();
        drv.ui_delete_backward().await.unwrap();
        drv.ui_insert_text("\u{e2}").await.unwrap();
        drv.ui_key("enter").await.unwrap();
        // The shell echoes the line and cat prints it back: two rows of exactly `â`.
        let dump = drv
            .wait_for("cat to print the composed letter", STEP, |d| {
                d.rows_containing("\u{e2}").iter().filter(|r| r.trim() == "\u{e2}").count() >= 2
            })
            .await
            .unwrap();
        assert!(
            dump.rows_containing("a\u{e2}").is_empty(),
            "the base letter was erased: {dump:#?}"
        );
        drv.ui_key("ctrl-c").await.unwrap();
        drv.wait_for("cat to exit", STEP, |d| !d.rows_containing("^C").is_empty()).await.unwrap();
        stack.shutdown().await;
    }

    /// The key bar's buttons, tapped where the accessibility tree puts them: Escape and an
    /// arrow reach the shell as their sequences, and ⌃ arms Control for the next typed
    /// character, so `c` ends the program.
    #[tokio::test]
    async fn the_key_bar_on_the_simulator_is_tapped_at_its_a11y_bounds() {
        let Some(simulator) = simulator() else { return };
        let mut stack = Stack::launch_on_simulator("e2e-ios-worker", simulator).await.unwrap();
        shell(&mut stack).await;
        start_cat_v(&mut stack).await;
        let drv = &mut stack.driver;

        let dump = drv.dump().await.unwrap();
        let centre = |label: &str| {
            let node = dump
                .a11y_node("Button", Some(label))
                .unwrap_or_else(|| panic!("{label} on the key bar: {:#?}", dump.a11y));
            let [x, y, w, h] = node.bounds;
            (x + w / 2.0, y + h / 2.0)
        };
        let (ex, ey) = centre("Escape");
        drv.ui_tap(ex, ey).await.unwrap();
        drv.wait_for("escape from the bar", STEP, |d| !d.rows_containing("^[").is_empty())
            .await
            .unwrap();
        let (ux, uy) = centre("Up arrow");
        drv.ui_tap(ux, uy).await.unwrap();
        drv.wait_for("up from the bar", STEP, |d| !d.rows_containing("^[^[[A").is_empty())
            .await
            .unwrap();
        let (kx, ky) = centre("Control");
        drv.ui_tap(kx, ky).await.unwrap();
        drv.ui_insert_text("c").await.unwrap();
        drv.wait_for("cat to exit on the armed ⌃C", STEP, |d| {
            !d.rows_containing("^C").is_empty()
        })
        .await
        .unwrap();
        stack.shutdown().await;
    }
}

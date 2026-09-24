//! The iOS app's own input path, driven at the UIKit boundary through its test socket.
//!
//! Runs only with `SLOPTY_IOS_E2E=1` (`cargo xtask e2e ios [--sim iphone|ipad]`), like `ios.rs`.
//! Where that file drives GPUI's dispatch (`Keys`, `Click`), this one delivers *described* UIKit
//! events (`UiKeyPress`, `UiTouch`, `UiPinch`, `UiInsertText`, `UiDeleteBackward`): the fork's
//! metal view runs the same code its `pressesBegan:` / `touchesBegan:` / pinch target /
//! `insertText:` / `deleteBackward` run once they have read their UIKit objects, so a lost
//! modifier, a wrongly mapped touch phase or a key the text system should have typed shows up
//! here and nowhere else. Every assertion reads the dump (rows, focus, zoom, bounds, a11y);
//! no golden is involved.

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use slopty_e2e::harness::{Simulator, Stack};
    use slopty_e2e::{Driver, Dump, ItemInfo, UiTouchPhase, UiTouchPoint};

    /// Per-step wait.
    const STEP: Duration = Duration::from_secs(30);
    /// Room the top bar and the key bar (with the home indicator) take, in points, when
    /// looking for a bare patch of canvas.
    const TOP_BAR: f32 = 80.0;
    const BOTTOM_BARS: f32 = 160.0;
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

    /// The app connected to its host and its first shell at a prompt.
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

    /// A point of bare canvas next to `item`: below it, above it, or to its right, whichever
    /// is inside the window and on no item.
    fn bare_canvas(dump: &Dump, item: &ItemInfo) -> (f32, f32) {
        let [x, y, w, h] = item.bounds;
        let (vw, vh) = (dump.window.width, dump.window.height);
        let candidates =
            [(x + w / 2.0, y + h + 24.0), (x + w / 2.0, y - 24.0), (x + w + 24.0, y + h / 2.0)];
        let inside = |(px, py): (f32, f32)| {
            px > 8.0 && px < vw - 8.0 && py > TOP_BAR + 8.0 && py < vh - BOTTOM_BARS
        };
        let on_item = |(px, py): (f32, f32)| {
            dump.items.iter().any(|i| {
                let [left, top, width, height] = i.bounds;
                px >= left && px <= left + width && py >= top && py <= top + height
            })
        };
        candidates
            .into_iter()
            .find(|p| inside(*p) && !on_item(*p))
            .unwrap_or_else(|| panic!("no bare canvas around {item:?} in {vw}x{vh}"))
    }

    /// The dump once the camera has stopped moving: two polls apart, the zoom and every
    /// item's bounds agree.
    async fn settled(drv: &mut Driver) -> Dump {
        let mut last: Option<(f32, Vec<[f32; 4]>)> = None;
        drv.wait_for("the camera to settle", STEP, |d| {
            let now = (d.zoom, d.items.iter().map(|i| i.bounds).collect::<Vec<_>>());
            let same = last.as_ref() == Some(&now);
            last = Some(now);
            same
        })
        .await
        .unwrap()
    }

    /// A hardware keyboard through `pressesBegan:` / `pressesEnded:` on the metal view: a
    /// ⌘⇧ chord reaches the keymaps (a note card opens), arrows and Escape reach
    /// the shell as their escape sequences, and a plain key while the terminal is editing is
    /// left to the text system, which types it.
    #[tokio::test]
    async fn a_hardware_keyboard_on_the_simulator_arrives_through_presses() {
        let Some(simulator) = simulator() else { return };
        let mut stack = Stack::launch_on_simulator("e2e-ios-host", simulator).await.unwrap();
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
        drv.wait_for("a note card", STEP, |d| d.item("note").is_some()).await.unwrap();
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
    /// recognizer's target. With two shells fitted side by side (⌘N, ⌘1): a tap on the first
    /// activates it; a one-finger drag on bare canvas, a second finger resting beside it, pans
    /// the camera so the content follows the finger along the locked axis and the zoom stays;
    /// a pinch zooms about the point under the fingers.
    #[tokio::test]
    async fn fingers_on_the_simulator_tap_pan_and_pinch() {
        let Some(simulator) = simulator() else { return };
        let mut stack = Stack::launch_on_simulator("e2e-ios-host", simulator).await.unwrap();
        let dump = shell(&mut stack).await;
        let first = dump.item("terminal").unwrap().clone();
        let drv = &mut stack.driver;
        drv.keys("cmd-n").await.unwrap();
        drv.wait_for("a second shell", STEP, |d| {
            d.items.len() == 2 && d.items.iter().any(|i| i.id != first.id && i.active)
        })
        .await
        .unwrap();
        drv.keys("cmd-1").await.unwrap();
        let fitted = settled(drv).await;
        let (vw, vh) = (fitted.window.width, fitted.window.height);
        for item in &fitted.items {
            let [x, y, w, h] = item.bounds;
            assert!(x >= 0.0 && y >= 0.0 && x + w <= vw && y + h <= vh, "fitted: {item:?}");
        }
        let item_by = |d: &Dump, id: &str| d.items.iter().find(|i| i.id == id).unwrap().clone();

        // A tap on the first shell activates it.
        let (tx, ty) = item_by(&fitted, &first.id).center();
        if let Err(e) = drv.ui_tap(tx, ty).await {
            panic!("{e:#}\napp log:\n{}", app_log_tail(&stack, 40));
        }
        let drv = &mut stack.driver;
        let before = drv
            .wait_for("the first shell active", STEP, |d| item_by(d, &first.id).active)
            .await
            .unwrap();

        // A pan of 120 points to the left, the second finger resting beside the first.
        let (bx, by) = bare_canvas(&before, &item_by(&before, &first.id));
        let [x0, y0, ..] = item_by(&before, &first.id).bounds;
        let (steps, travel) = (6_u32, -120.0_f32);
        let fingers = |x: f32| {
            [UiTouchPoint { id: 7, x, y: by }, UiTouchPoint { id: 8, x: x + 40.0, y: by + 30.0 }]
        };
        drv.ui_touch(&fingers(bx), UiTouchPhase::Began).await.unwrap();
        for i in 1..=steps {
            #[expect(clippy::cast_precision_loss, reason = "six steps")]
            let x = travel.mul_add(i as f32 / steps as f32, bx);
            drv.ui_touch(&fingers(x), UiTouchPhase::Moved).await.unwrap();
        }
        // A finger that stops reports nothing until it lifts (UIKit sends no `touchesMoved:`
        // for a still touch); the pause is what tells the recognizer not to fling.
        tokio::time::sleep(STILL).await;
        drv.ui_touch(&fingers(bx + travel), UiTouchPhase::Ended).await.unwrap();
        drv.wait_for("the camera to follow the finger", STEP, |d| {
            let [x, ..] = item_by(d, &first.id).bounds;
            (x - x0).abs() > 1.0
        })
        .await
        .unwrap();
        let after = settled(drv).await;
        let [x1, y1, ..] = item_by(&after, &first.id).bounds;
        assert!(
            (x1 - x0 - travel).abs() < 2.0,
            "the content follows the finger by {travel}, without a fling: {x0} → {x1} at zoom {}",
            after.zoom
        );
        assert!((y1 - y0).abs() < 2.0, "locked to the finger's axis: {y0} → {y1}");
        assert!((after.zoom - before.zoom).abs() < 1e-3, "a pan does not zoom");

        // A pinch about the first shell's centre: zoom × 1.5, the point under the fingers fixed.
        let (cx, cy) = item_by(&after, &first.id).center();
        drv.ui_pinch(cx, cy, 1.5, 4).await.unwrap();
        let zoomed = drv
            .wait_for("the zoom", STEP, |d| (d.zoom / after.zoom - 1.5).abs() < 0.01)
            .await
            .unwrap();
        let [zx, zy, ..] = item_by(&zoomed, &first.id).bounds;
        let expected = ((zx - cx) / 1.5 + cx, (zy - cy) / 1.5 + cy);
        assert!(
            (expected.0 - x1).abs() < 2.0 && (expected.1 - y1).abs() < 2.0,
            "zoomed about ({cx}, {cy}): {:?} from ({x1}, {y1}) at {}",
            (zx, zy),
            zoomed.zoom
        );
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
        let mut stack = Stack::launch_on_simulator("e2e-ios-host", simulator).await.unwrap();
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
        let mut stack = Stack::launch_on_simulator("e2e-ios-host", simulator).await.unwrap();
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

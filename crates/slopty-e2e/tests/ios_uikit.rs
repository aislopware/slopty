//! The iOS app's own input path, driven at the UIKit boundary through its test socket.
//!
//! Live (`#[ignore]`), run by `cargo xtask e2e ios [--sim iphone|ipad]`, like `ios.rs`.
//! Where that file drives GPUI's dispatch (`Keys`, `Click`), this one delivers *described* UIKit
//! events (`UiKeyPress`, `UiTouch`, `UiPinch`, `UiInsertText`, `UiDeleteBackward`): the fork's
//! metal view runs the same code its `pressesBegan:` / `touchesBegan:` / pinch target /
//! `insertText:` / `deleteBackward` run once they have read their UIKit objects, so a lost
//! modifier, a wrongly mapped touch phase or a key the text system should have typed shows up
//! here and nowhere else. Every assertion reads the dump (rows, focus, bounds, a11y);
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

    /// The booted simulator `cargo xtask e2e ios` installed the app on.
    fn simulator() -> Simulator {
        let udid = std::env::var("SLOPTY_SIM_UDID").expect("SLOPTY_SIM_UDID (a booted simulator)");
        let bundle_id = std::env::var("SLOPTY_SIM_BUNDLE_ID").expect("SLOPTY_SIM_BUNDLE_ID");
        Simulator { udid, bundle_id }
    }

    /// A viewport narrower than this is a phone (`slopty_client::layout`'s `phone_below`),
    /// which shows one pane at a time.
    const PHONE_BELOW: f32 = 700.0;

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
        drv.wait_for("cat -v to start", STEP, |d| !d.lines_containing("cat -v").is_empty())
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
    #[ignore = "live: cargo xtask e2e ios"]
    async fn a_hardware_keyboard_on_the_simulator_arrives_through_presses() {
        let simulator = simulator();
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
        drv.wait_for("cat -v from the keys", STEP, |d| !d.lines_containing("cat -v").is_empty())
            .await
            .unwrap();
        // Named keys are consumed by the view and encoded by the terminal: the tty echoes
        // them in caret notation under cat -v.
        drv.ui_key("escape").await.unwrap();
        drv.ui_key("up").await.unwrap();
        drv.ui_key("right").await.unwrap();
        let dump = drv
            .wait_for("the escape and arrow sequences", STEP, |d| {
                !d.lines_containing("^[^[[A^[[C").is_empty()
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
        drv.wait_for("cat to exit", STEP, |d| !d.lines_containing("^C").is_empty()).await.unwrap();
        assert_eq!(dump.focused, format!("terminal:{session}"), "{dump:#?}");

        // ⌘⇧N: command and shift from the press's flags, `n` from its usage; ⌘W takes the
        // note away again and the shell has the keyboard back.
        drv.ui_key("cmd-shift-n").await.unwrap();
        drv.wait_for("a note tile", STEP, |d| d.item("file").is_some()).await.unwrap();
        drv.ui_key("cmd-w").await.unwrap();
        drv.wait_for("the note gone and the shell focused", STEP, |d| {
            d.item("file").is_none() && d.focused == format!("terminal:{session}")
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
            !d.lines_containing("x").is_empty()
        })
        .await
        .unwrap();
        stack.shutdown().await;
    }

    /// Fingers through `touchesBegan:` / `touchesMoved:` / `touchesEnded:` and the pinch
    /// recognizer's target. With two shells (⌘D): on an iPad they stand in two panes and a tap
    /// on the first gives it the keyboard; a horizontal two-finger swipe and a pinch over a
    /// shell move no pane; taps reach the titlebar's "…" menu and its "Command palette" row.
    #[tokio::test]
    #[ignore = "live: cargo xtask e2e ios"]
    async fn fingers_on_the_simulator_tap_swipe_and_pinch() {
        let simulator = simulator();
        let mut stack = Stack::launch_on_simulator("e2e-ios-worker", simulator).await.unwrap();
        let dump = shell(&mut stack).await;
        let first = dump.item("terminal").unwrap().clone();
        let drv = &mut stack.driver;
        drv.keys("cmd-d").await.unwrap();
        let item_by = |d: &Dump, id: &str| d.items.iter().find(|i| i.id == id).unwrap().clone();
        let two = drv
            .wait_for("a second shell, focused", STEP, |d| {
                d.items.len() == 2 && d.items.iter().any(|i| i.id != first.id && i.active)
            })
            .await
            .unwrap();
        let second = two.items.iter().find(|i| i.id != first.id).unwrap().clone();
        let (vw, vh) = (two.window.width, two.window.height);
        let tablet = vw >= PHONE_BELOW;
        if tablet {
            // Two panes side by side, the second right of the first.
            let first_now = item_by(&two, &first.id);
            assert!(
                second.bounds[0] >= first_now.bounds[0] + first_now.bounds[2] - 1.0,
                "{two:#?}"
            );
            let (tx, ty) = first_now.center();
            drv.ui_tap(tx, ty).await.unwrap();
            drv.wait_for("the tap to focus the first shell", STEP, |d| {
                item_by(d, &first.id).active
                    && d.focused == format!("terminal:{}", first.session.as_deref().unwrap_or(""))
            })
            .await
            .unwrap();
        }
        let before = drv.dump().await.unwrap();
        let active = before.items.iter().find(|i| i.active).unwrap().id.clone();
        let places =
            |d: &Dump| d.items.iter().map(|i| (i.id.clone(), i.bounds)).collect::<Vec<_>>();

        // A swipe either way and a pinch each way over a shell: no pane moves and the focus
        // stays where it was.
        swipe(drv, &before, vw * 0.4).await;
        swipe(drv, &before, -vw * 0.4).await;
        let (cx, cy) = (vw / 2.0, vh / 2.0);
        drv.ui_pinch(cx, cy, 0.6, 4).await.unwrap();
        drv.ui_pinch(cx, cy, 1.6, 4).await.unwrap();
        let after = drv.dump().await.unwrap();
        assert_eq!(places(&after), places(&before), "a pane moved");
        assert!(item_by(&after, &active).active, "the focus kept: {after:#?}");

        // A tap on "…", a tap on its "Command palette": the palette is up.
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
        stack.shutdown().await;
    }

    /// The soft keyboard through the text input view's `insertText:` / `deleteBackward`: the
    /// terminal's input handler gets the text as typed, and a Telex-style composition, which
    /// reaches a `UIKeyInput` responder as the base letter, a delete and the composed letter,
    /// leaves exactly the composed letter in the shell (BSD `cat -v` prints a printable
    /// UTF-8 letter as itself; only control bytes get the caret form).
    #[tokio::test]
    #[ignore = "live: cargo xtask e2e ios"]
    async fn the_soft_keyboard_on_the_simulator_types_through_insert_text() {
        let simulator = simulator();
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
                d.lines_containing("\u{e2}").iter().filter(|r| r.trim() == "\u{e2}").count() >= 2
            })
            .await
            .unwrap();
        assert!(
            dump.lines_containing("a\u{e2}").is_empty(),
            "the base letter was erased: {dump:#?}"
        );
        drv.ui_key("ctrl-c").await.unwrap();
        drv.wait_for("cat to exit", STEP, |d| !d.lines_containing("^C").is_empty()).await.unwrap();
        stack.shutdown().await;
    }

    /// The key bar's buttons, tapped where the accessibility tree puts them: Escape and an
    /// arrow reach the shell as their sequences, and ⌃ arms Control for the next typed
    /// character, so `c` ends the program.
    #[tokio::test]
    #[ignore = "live: cargo xtask e2e ios"]
    async fn the_key_bar_on_the_simulator_is_tapped_at_its_a11y_bounds() {
        let simulator = simulator();
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
        drv.wait_for("escape from the bar", STEP, |d| !d.lines_containing("^[").is_empty())
            .await
            .unwrap();
        let (ux, uy) = centre("Up arrow");
        drv.ui_tap(ux, uy).await.unwrap();
        drv.wait_for("up from the bar", STEP, |d| !d.lines_containing("^[^[[A").is_empty())
            .await
            .unwrap();
        let (kx, ky) = centre("Control");
        drv.ui_tap(kx, ky).await.unwrap();
        drv.ui_insert_text("c").await.unwrap();
        drv.wait_for("cat to exit on the armed ⌃C", STEP, |d| {
            !d.lines_containing("^C").is_empty()
        })
        .await
        .unwrap();
        stack.shutdown().await;
    }

    /// A push the server sealed to the phone shows as APNs' fixed words until it is opened, and
    /// opens, with the key and the token the app kept in the Keychain it shares with its
    /// notification extension, into the note the app would have posted. The test plays the
    /// server: it hands the app a device token as the app delegate would (notes allowed
    /// quietly, since no one answers a prompt here) and seals a notice to the phone's key under
    /// that token.
    ///
    /// `simctl push` hands the payload straight to the system, which never starts the extension
    /// for it, so the note it shows is the payload's own words, and the opening is the
    /// extension's own function run in the app ([`Command::OpenPush`]). Only a real APNs sandbox
    /// push starts the extension itself.
    #[tokio::test]
    #[ignore = "live: cargo xtask e2e ios"]
    async fn a_sealed_push_shows_its_fallback_and_opens_with_the_kept_key() {
        use slopty_proto::push::PushBody;
        use slopty_proto::thread::ThreadId;
        use slopty_proto::thread::attention::{Notice, NoticeKind, Subject, ThreadAt};
        use slopty_push::apns;

        let simulator = simulator();
        let (udid, bundle) = (simulator.udid.clone(), simulator.bundle_id.clone());
        let mut stack = Stack::launch_on_simulator("e2e-ios-worker", simulator).await.unwrap();
        let token: Vec<u8> = (0_u8..32).map(|b| b.wrapping_mul(7).wrapping_add(3)).collect();
        let hex = data_encoding::HEXLOWER.encode(&token);
        let key = stack.driver.push_register(&token).await.unwrap();

        let thread = ThreadId::new();
        let notice = Notice {
            kind: NoticeKind::NeedsYou,
            about: Subject::Thread(ThreadAt { worker: slopty_core::WorkerId::new(), thread }),
            tile: None,
            title: "Ship the login fix".to_owned(),
            text: "Wants to run cargo test".to_owned(),
            worked_ms: None,
            via: None,
        };
        let body = slopty_proto::codec::encode_body(&PushBody { notice, ask: None, quiet: false })
            .unwrap();
        let sealed = slopty_push::seal::seal(&key, &hex, &body).unwrap();
        let note = apns::Note {
            urgent: true,
            thread: "e2e-thread".to_owned(),
            collapse: "e2e-note".to_owned(),
            sealed,
        };
        let push = apns::Push { token: hex, sandbox: true, what: apns::What::Note(note) };
        let request = apns::request(&push, &bundle, "e2e").unwrap();
        let payload = stack.path("push.apns");
        std::fs::write(&payload, &request.body).unwrap();
        let pushed = tokio::process::Command::new("xcrun")
            .args(["simctl", "push", &udid, &bundle])
            .arg(&payload)
            .output()
            .await
            .unwrap();
        assert!(
            pushed.status.success(),
            "simctl push: {}",
            String::from_utf8_lossy(&pushed.stderr)
        );

        let deadline = tokio::time::Instant::now() + STEP;
        let shown = loop {
            let notes = stack.driver.delivered().await.unwrap();
            if let Some(note) = notes.into_iter().next() {
                break note;
            }
            assert!(tokio::time::Instant::now() < deadline, "no note shown");
            tokio::time::sleep(Duration::from_millis(250)).await;
        };
        assert_eq!((shown.title.as_str(), shown.body.as_str()), (apns::TITLE, apns::URGENT));

        let payload = std::str::from_utf8(&request.body).unwrap();
        let opened = stack.driver.open_push(payload).await;
        let opened =
            opened.unwrap_or_else(|e| panic!("{e:#}\napp log:\n{}", app_log_tail(&stack, 40)));
        assert_eq!(opened.title, "Ship the login fix", "the notice's words, not APNs'");
        assert_eq!(opened.body, "Wants to run cargo test");
        assert!(opened.id.ends_with(&thread.to_string()), "the thread's note: {}", opened.id);
        stack.shutdown().await;
    }
}

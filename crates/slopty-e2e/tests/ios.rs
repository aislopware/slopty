//! The iOS app on the simulator, driven through its own test socket.
//!
//! Live (`#[ignore]`), run by `cargo xtask e2e ios [--sim iphone|ipad]`: the daemons
//! run on the Mac as for the app self-test, the app runs in the booted simulator named by
//! `SLOPTY_SIM_UDID` and binds its socket on the shared file system. Phone or tablet: a
//! shell opens, its rows come back, typed text echoes, and its pane is sized for the screen it
//! is on (a lone pane fills it, a phone's always does). The
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
    /// and the panes put it through (fewer rows trim blank rows at the bottom first).
    fn echo_on_screen(d: &Dump) -> bool {
        !d.lines_containing("echo ios-").is_empty()
            && d.lines_containing("ios-42").iter().any(|r| r.trim() == "ios-42")
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
    /// The palette's first line, the best match for what was typed.
    fn first_line(d: &Dump) -> Option<&str> {
        d.a11y.iter().find(|n| n.role == "ListBoxOption").and_then(|n| n.label.as_deref())
    }

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
            frame.image = image::imageops::crop_imm(&frame.image, 0, 0, w, h).to_image();
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
        stack.connect_server().await.unwrap();
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
        // The pane is on screen and sized for it.
        assert!(
            x >= 0.0 && y >= 0.0 && x + w <= vw + 1.0 && y + h <= vh + 1.0,
            "{term:?} in {vw}x{vh}"
        );
        // A lone pane fills the width on a phone and a tablet alike
        // (docs/decisions/workspace.md).
        assert!(w > vw * 0.85, "a lone pane fills the screen: {term:?} in {vw}");

        drv.type_text("echo ios-$((6*7))").await.unwrap();
        drv.keys("enter").await.unwrap();
        // The echo, and the prompt back under it (the cursor's row is the grid's a11y value):
        // a frame rendered between the two is a different picture. So is one between the
        // prompt and zle taking the line: the command left the default block cursor, and only
        // zle's line-init sets the insert bar again.
        let dump = drv
            .wait_for("the echo and the next prompt, the connect notice gone", STEP, |d| {
                d.notice.is_none()
                    && d.terminals.iter().any(|t| t.cursor_shape == "Bar")
                    && d.lines_containing("ios-42").iter().any(|r| r.trim() == "ios-42")
                    && d.a11y_node("Terminal", None).is_some_and(|g| {
                        g.value
                            .as_deref()
                            .is_some_and(|v| !v.trim().is_empty() && !v.contains("ios-42"))
                    })
            })
            .await
            .unwrap();
        assert!(!dump.lines_containing("echo ios-").is_empty(), "{dump:#?}");

        // The frame the app draws, from its own renderer, against this device's golden.
        let frame = drv.render(&render_path).await.unwrap();
        let fg = foreground_fraction(&frame);
        assert!(fg > BLANK, "frame is blank ({fg:.4} foreground)");
        let name = format!("ios-{}-terminal", device(vw));
        assert_matches(&name, &frame, TOLERANCE, &artifacts_dir()).unwrap();

        // ⌘⇧T from a hardware keyboard (the same keystroke the socket dispatches) opens a second
        // shell in a tab of its own, focused; ⌘W closes the focused tile, that second shell, and
        // gives the focus back to the first with its echo.
        drv.keys("cmd-shift-t").await.unwrap();
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
        // The best match leads; a looser one (a machine's clipboard line) may follow it.
        drv.wait_for("the note's line first", STEP, |d| {
            first_line(d).is_some_and(|l| l.starts_with("New note"))
        })
        .await
        .unwrap();
        drv.keys("enter").await.unwrap();
        drv.wait_for("a note from the palette", STEP, |d| {
            d.item("file").is_some() && d.a11y_node("Dialog", Some("Commands")).is_none()
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
        // The best match leads; a looser one (a machine's clipboard line) may follow it.
        drv.wait_for("the settings' line first", STEP, |d| {
            first_line(d).is_some_and(|l| l.starts_with("Open settings"))
        })
        .await
        .unwrap();
        drv.keys("enter").await.unwrap();
        // The page takes the panes' place, under the notices: the closed shell's take-back
        // offer hangs over its head until it lapses, and a tap there would take the shell back.
        let dump = drv
            .wait_for("the settings editor, the offer gone", STEP, |d| {
                d.a11y_node("Group", Some("Settings")).is_some() && d.notice.is_none()
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
        // The server the app follows stays in the file.
        let settings = format!(
            "{}alert = \"never\"\n\n[client]\nserver = \"{}\"\n",
            pinned_settings(APPEARANCE),
            stack.server.address()
        );
        drv.ui_insert_text(&settings).await.unwrap();
        drv.keys("cmd-enter").await.unwrap();
        drv.wait_for("the settings editor to close", STEP, |d| {
            d.a11y_node("Group", Some("Settings")).is_none()
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
        // The best match leads; a looser one (a machine's clipboard line) may follow it.
        drv.wait_for("the naming line first", STEP, |d| {
            first_line(d).is_some_and(|l| l.starts_with("Name this tile"))
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
            d.a11y_node("Heading", Some("file scratch")).is_some()
                && d.a11y_node("TextInput", Some("Tile name")).is_none()
        })
        .await
        .unwrap();

        // The shell and the note, the note focused: panes and their tabs,
        // once the take-back offer for the closed shell has gone (it lasts five seconds).
        let dump =
            drv.wait_for("the undo offer to lapse", STEP, |d| d.notice.is_none()).await.unwrap();
        // The soft keyboard came and went with each field, and the note took its place beside
        // the shell, or over it on a phone: a shell resized keeps what it showed.
        assert!(echo_on_screen(&dump), "{:#?}", dump.terminals);
        golden(drv, &dir, dev, "panes", None).await;
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
        // leaves an edge of the panes, on an iPad a panel over them. The shell's row
        // closes it with the shell focused.
        tap(drv, "Button", "Navigator").await;
        let shown = drv
            .wait_for("the navigator", STEP, |d| {
                d.a11y_node("Navigation", Some("Navigator")).is_some()
            })
            .await
            .unwrap();
        let [nav_x, _, nav_w, _] = shown.a11y_node("Navigation", Some("Navigator")).unwrap().bounds;
        // A phone's drawer floats a small step in from the edge; an iPad's panel meets it.
        assert!(
            (-1.0..=9.0).contains(&nav_x) && nav_w < shown.window.width * 0.9,
            "{nav_x} {nav_w}"
        );
        golden(drv, &dir, dev, "navigator", None).await;
        // A shell is titled by what it runs or where it stands, so its row is found by the
        // title its tile goes by. A phone draws the focused pane alone, here the note's, so the
        // shell is found by its terminal's title rather than its drawn tile.
        let title = shown
            .terminals
            .first()
            .and_then(|t| t.title.clone())
            .unwrap_or_else(|| panic!("the shell: {:#?}", shown.terminals));
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
            // Compact: the active pane takes the whole width, edge to edge, and the other tile
            // is not drawn beside it.
            drv.wait_for("one full-width pane", STEP, |d| {
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
    /// A `slopty://connect` link, what the Camera opens from a Mac's code, opens Slopty at
    /// "Connect to a server" with the address in the field and connects nothing until the
    /// person presses Connect. A link that is not exactly that changes nothing. The link comes
    /// in where the scene's delegate hands it over, since `simctl openurl` stops at a
    /// confirmation only a person may answer.
    #[tokio::test]
    #[ignore = "live: cargo xtask e2e ios"]
    async fn a_link_from_a_mac_s_code_fills_the_server_s_address() {
        let simulator = simulator();
        let mut stack =
            Stack::launch_first_run_on_simulator("e2e-ios-link", simulator).await.unwrap();
        stack.driver.wait_for("the connect panel", STEP, |d| d.adding).await.unwrap();
        let field = |d: &Dump| {
            d.a11y_node("TextInput", Some("Server address")).and_then(|n| n.value.clone())
        };

        let link = |url: String| Command::OpenLink { url };
        stack.driver.ok(&link("slopty://connect?server=home-server".to_owned())).await.unwrap();
        tokio::time::sleep(SETTLE).await;
        let d = stack.driver.dump().await.unwrap();
        assert_eq!(field(&d).unwrap_or_default(), "", "a link with no port: {:#?}", d.a11y);

        let server = stack.server.address().to_owned();
        stack.driver.ok(&link(format!("slopty://connect?server={server}"))).await.unwrap();
        stack
            .driver
            .wait_for("the address in the field", STEP, |d| {
                field(d).as_deref() == Some(server.as_str())
            })
            .await
            .unwrap();
        // A moment for a connect the link should not have started.
        tokio::time::sleep(SETTLE).await;
        let d = stack.driver.dump().await.unwrap();
        assert!(d.adding && d.workers.is_empty(), "a link alone connects nothing: {d:#?}");

        stack.driver.keys("enter").await.unwrap();
        stack
            .driver
            .wait_for("connected on the person's press", STEP, |d| {
                !d.adding && !d.workers.is_empty()
            })
            .await
            .unwrap();
        stack.shutdown().await;
    }

    /// A made-up Claude Code session of `turns` turns, each the person's prompt and a long
    /// answer, as its transcript keeps them: long enough that the first prompt is far above
    /// the tail on a phone.
    fn long_session(turns: usize) -> String {
        use serde_json::json;
        let mut out = String::new();
        let mut parent: Option<String> = None;
        let mut n = 0_u64;
        let mut push = |mut record: serde_json::Value| {
            n = n.saturating_add(1);
            let uuid = format!("00000000-0000-4000-9000-{n:012}");
            record["uuid"] = json!(uuid);
            record["parentUuid"] = parent.clone().map_or(serde_json::Value::Null, |p| json!(p));
            record["timestamp"] = json!(format!("2026-10-05T09:{:02}:{:02}.000Z", n / 60, n % 60));
            record["sessionId"] = json!("s1");
            record["isSidechain"] = json!(false);
            out.push_str(&record.to_string());
            out.push('\n');
            parent = Some(uuid);
        };
        for turn in 0..turns {
            push(json!({ "type": "user", "message": { "role": "user",
                "content": format!("Step {turn}: tighten the parser's error path") } }));
            let answer = (0..6)
                .map(|p| {
                    format!("Paragraph {p} of turn {turn}: the span is kept and the cursor reset.")
                })
                .collect::<Vec<_>>()
                .join("\n\n");
            push(json!({ "type": "assistant", "message": {
                "id": format!("msg_{turn}"), "role": "assistant", "model": "claude-opus-5-5",
                "content": [{ "type": "text", "text": answer }], "stop_reason": "end_turn",
                "usage": { "input_tokens": 12, "output_tokens": 420 } } }));
        }
        out
    }

    /// Whether the thread shows the person's first prompt.
    fn first_prompt_shows(d: &Dump) -> bool {
        d.a11y.iter().any(|n| {
            n.role == "Article" && n.label.as_deref().is_some_and(|l| l.starts_with("You: Step 0:"))
        })
    }

    /// One finger dragged from `from` to `to` in `steps` moves (iOS), as a reader's swipe.
    async fn swipe(drv: &mut Driver, from: (f32, f32), to: (f32, f32), steps: u16) {
        use slopty_e2e::{UiTouchPhase, UiTouchPoint};
        let at = |x: f32, y: f32| [UiTouchPoint { id: 1, x, y }];
        drv.ui_touch(&at(from.0, from.1), UiTouchPhase::Began).await.unwrap();
        for step in 1..=steps {
            let t = f32::from(step) / f32::from(steps);
            let (x, y) = ((to.0 - from.0).mul_add(t, from.0), (to.1 - from.1).mul_add(t, from.1));
            drv.ui_touch(&at(x, y), UiTouchPhase::Moved).await.unwrap();
        }
        drv.ui_touch(&at(to.0, to.1), UiTouchPhase::Ended).await.unwrap();
    }

    /// An agent's day on a phone, played by a hook stand-in as the Mac's tests are: no agent
    /// runs and nothing is typed into a shell.
    ///
    /// The agent's session starts in a shell through the real relay (`slopty hook`), and its
    /// tile opens on its thread at the tail. Reading by touch: swipes down the thread bring its
    /// first prompt into view. The agent asks a question (`AskUserQuestion`, held by the
    /// relay): it stacks over the composer, a tap on "Other" gives it the keyboard, the soft
    /// keyboard's text goes in (`insertText:`), and "Submit" hands Claude Code the answer in its
    /// own form. With the person elsewhere, on a shell of their own, the agent's banner tapped
    /// brings its tile back on its thread.
    #[tokio::test]
    #[ignore = "live: cargo xtask e2e ios"]
    async fn an_agent_is_answered_from_the_phone_on_the_simulator() {
        use serde_json::{Value, json};

        let mut stack = Stack::launch_on_simulator("e2e-ios-agent", simulator()).await.unwrap();
        let dump = stack
            .driver
            .wait_for("the first shell with a prompt", STEP, |d| {
                d.status == "connected"
                    && d.terminals.iter().any(|t| t.rows.iter().any(|r| !r.is_empty()))
            })
            .await
            .unwrap();
        let session = dump.terminals[0].session.clone();
        let transcript = stack.path("projects").join("s1.jsonl");
        std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        std::fs::write(&transcript, long_session(12)).unwrap();
        let transcript = transcript.to_string_lossy().into_owned();
        let start = json!({
            "hook_event_name": "SessionStart", "source": "startup", "session_id": "s1",
            "transcript_path": transcript, "cwd": stack.path("home"),
        });
        let started = stack.relay_hook(&session, &[], &start).unwrap().wait().await.unwrap();
        assert!(started.success(), "the relay ran");
        let drv = &mut stack.driver;
        let thread = |d: &Dump| d.a11y_node("Group", Some("Thread")).is_some();
        let dump = drv
            .wait_for("the thread at its tail", STEP, |d| {
                thread(d)
                    && d.a11y.iter().any(|n| {
                        n.role == "Article"
                            && n.label
                                .as_deref()
                                .is_some_and(|l| l.starts_with("Paragraph 0 of turn 11"))
                    })
            })
            .await
            .unwrap();
        assert!(!first_prompt_shows(&dump), "the first prompt starts out of view");

        // Reading: a finger drags the thread down, a few times, as a reader looks back.
        let mut read = false;
        for _ in 0..40 {
            let now = drv.dump().await.unwrap();
            if first_prompt_shows(&now) {
                read = true;
                break;
            }
            let [left, top, width, height] = now.a11y_node("Group", Some("Thread")).unwrap().bounds;
            let mid = width.mul_add(0.5, left);
            let (from, to) = (height.mul_add(0.25, top), height.mul_add(0.85, top));
            swipe(drv, (mid, from), (mid, to), 8).await;
        }
        assert!(read, "touch brought the first prompt into view");

        // A question, answered with the soft keyboard.
        let question = "Which layout should the review use?";
        let ask = json!({
            "hook_event_name": "PermissionRequest", "session_id": "s1",
            "transcript_path": transcript, "cwd": stack.path("home"),
            "tool_name": "AskUserQuestion", "tool_input": { "questions": [{
                "question": question, "header": "Layout", "multiSelect": false,
                "options": [
                    { "label": "Split", "description": "Old and new side by side" },
                    { "label": "Unified", "description": "One column, changes inline" }
                ]
            }] },
        });
        let held = stack.relay_hook(&session, &[], &ask).unwrap();
        let drv = &mut stack.driver;
        drv.wait_for("the question over the composer", STEP, |d| {
            d.a11y_node("RadioButton", Some("Unified")).is_some()
                && d.a11y_node("MultilineTextInput", Some("Other")).is_some()
        })
        .await
        .unwrap();
        // The tap gives the field the keyboard: what the soft keyboard sends lands in it.
        tap(drv, "MultilineTextInput", "Other").await;
        let other =
            |d: &Dump| d.a11y_node("MultilineTextInput", Some("Other")).is_some_and(|n| n.focused);
        let after_tap = drv.wait_for("the field with the keyboard", STEP, other).await;
        assert!(after_tap.is_ok(), "a tap on the field gave it no keyboard: {after_tap:?}");
        drv.ui_insert_text("Unified, split for renames").await.unwrap();
        drv.wait_for("the words in the field", STEP, |d| {
            d.a11y_node("MultilineTextInput", Some("Other"))
                .is_some_and(|n| n.value.as_deref() == Some("Unified, split for renames"))
        })
        .await
        .unwrap();
        tap(drv, "Button", "Submit").await;
        let answered = tokio::time::timeout(STEP, held.wait_with_output()).await.unwrap().unwrap();
        let decision: Value = serde_json::from_slice(&answered.stdout).unwrap();
        assert_eq!(
            decision["hookSpecificOutput"]["decision"]["updatedInput"]["answers"],
            json!({ question: "Unified, split for renames" }),
            "{decision:#}"
        );

        // Elsewhere, then the agent's banner tapped: its tile, on its thread.
        drv.open(&[], 1).await.unwrap();
        drv.wait_for("a shell of the person's own in front", STEP, |d| {
            d.items.iter().any(|i| i.active && i.session.as_deref() != Some(session.as_str()))
        })
        .await
        .unwrap();
        drv.notification_response(&session).await.unwrap();
        drv.wait_for("the agent's tile back, on its thread", STEP, |d| {
            d.items.iter().any(|i| i.active && i.session.as_deref() == Some(session.as_str()))
                && thread(d)
        })
        .await
        .unwrap();
        stack.shutdown().await;
    }

    /// A note's "Deny" pressed while the app is not running: the system launches it in the
    /// background and hands it the press before any window, so the press is answered with no
    /// window, through a link of its own to the server its settings name. The held relay gets
    /// its answer and the thread's request is gone. Pressed again on the prompt now gone, the
    /// press says so in a note in place of the one pressed.
    ///
    /// simctl delivers pushes but cannot press a note's button, so the press is handed in at
    /// launch through the e2e build's seam ([`slopty_e2e::LAUNCH_TAP_ENV`]), along the path the
    /// system's launching response takes.
    #[tokio::test]
    #[ignore = "live: cargo xtask e2e ios"]
    async fn a_killed_app_answers_a_note_s_deny_with_no_window() {
        use serde_json::json;
        use slopty_e2e::harness::slopty_json;

        let mut stack = Stack::launch_on_simulator("e2e-ios-cold", simulator()).await.unwrap();
        let dump = stack
            .driver
            .wait_for("the first shell with a prompt", STEP, |d| {
                d.status == "connected"
                    && d.terminals.iter().any(|t| t.rows.iter().any(|r| !r.is_empty()))
            })
            .await
            .unwrap();
        let session = dump.terminals[0].session.clone();
        // Notes allowed with no prompt, so the press's own note can be shown.
        stack.driver.push_register(&[0; 32]).await.unwrap();

        let transcript = stack.path("projects").join("s1.jsonl");
        std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        std::fs::write(&transcript, long_session(1)).unwrap();
        let transcript = transcript.to_string_lossy().into_owned();
        let start = json!({
            "hook_event_name": "SessionStart", "source": "startup", "session_id": "s1",
            "transcript_path": transcript, "cwd": stack.path("home"),
        });
        let started = stack.relay_hook(&session, &[], &start).unwrap().wait().await.unwrap();
        assert!(started.success(), "the relay ran");
        let ask = json!({
            "hook_event_name": "PermissionRequest", "session_id": "s1",
            "transcript_path": transcript, "cwd": stack.path("home"),
            "tool_name": "Bash", "tool_input": { "command": "rm -rf build" },
        });
        let mut held = stack.relay_hook(&session, &[], &ask).unwrap();

        let term = format!("e2e-ios-cold/{session}");
        let cli = stack.path("cli");
        let address = stack.server.address().to_owned();
        let read_thread = async || {
            let args = ["agent", "read", "--term", term.as_str()];
            slopty_json(&address, &cli, &args, b"").await.unwrap()
        };
        let mut read = read_thread().await;
        let waited = std::time::Instant::now();
        while read["requests"][0]["ask"].as_str().is_none() {
            assert!(waited.elapsed() < STEP, "the thread holds no request: {read}");
            tokio::time::sleep(Duration::from_millis(100)).await;
            read = read_thread().await;
        }
        let ask = read["requests"][0]["ask"].as_str().unwrap().to_owned();
        let held_on: slopty_core::WorkerId = read["worker"].as_str().unwrap().parse().unwrap();
        // Spelled as `slopty_platform::notify::info` keys a note, which builds on Apple only.
        let pressed = json!({
            "id": "e2e-cold",
            "action": "deny",
            "info": {
                "worker": held_on.as_uuid().as_u128().to_string(),
                "session": session,
                "ask": ask,
            },
        })
        .to_string();

        // Killed, then launched by the press.
        let launched = std::time::Instant::now();
        stack.relaunch_on_simulator(&[(slopty_e2e::LAUNCH_TAP_ENV, &pressed)]).await.unwrap();
        let status = tokio::time::timeout(STEP, held.wait()).await;
        let status = status.expect("the held relay got its answer").unwrap();
        println!(
            "MEASURE ios-cold: a Deny pressed on a killed app reached the held relay within {:.0} ms",
            launched.elapsed().as_secs_f64() * 1e3
        );
        assert!(status.success(), "the relay ends well on an answer: {status}");
        let read = read_thread().await;
        assert_eq!(read["requests"].as_array().map(Vec::len), Some(0), "{read}");

        // The same press on the prompt now gone: said in a note in its place.
        stack.relaunch_on_simulator(&[(slopty_e2e::LAUNCH_TAP_ENV, &pressed)]).await.unwrap();
        let waited = std::time::Instant::now();
        loop {
            let notes = stack.driver.delivered().await.unwrap();
            if notes
                .iter()
                .any(|n| n.id == "e2e-cold" && n.title == "That prompt is no longer waiting")
            {
                break;
            }
            assert!(waited.elapsed() < STEP, "no note says the prompt is gone: {notes:?}");
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        stack.shutdown().await;
    }
}

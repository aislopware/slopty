//! The iOS app on the simulator, driven through its own test socket.
//!
//! Runs only with `SLOPTY_IOS_E2E=1` (`cargo xtask e2e ios [--sim iphone|ipad]`): the daemons
//! run on the Mac as for the app self-test, the app runs in the booted simulator named by
//! `SLOPTY_SIM_UDID` and binds its socket on the shared file system. Two tests, phone or
//! tablet: a shell opens, its rows come back, typed text echoes, and the terminal is sized for
//! the screen it is on (a phone shrinks it to the viewport, an iPad keeps the desktop size);
//! and a played Claude Code session (its hook handed to hostd from the test) shows its
//! conversation, inside the screen above the key bar, with a composer that types into the
//! shell; and a driven agent (the host runs the fake `claude` over stream-json) shows as a
//! card with its header, its answer and the composer above the key bar. Each scenario renders
//! the app's own frame (the fork's iOS `render_to_image`) and compares it with a golden per
//! device, `ios-phone-*.png` or `ios-pad-*.png`.

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
        // The grid as VoiceOver gets it: a terminal whose value is the cursor row, on screen.
        let grid = dump.a11y_node("Terminal", None).unwrap_or_else(|| panic!("{:#?}", dump.a11y));
        assert!(grid.value.as_deref().is_some_and(|v| !v.is_empty()), "cursor row: {grid:?}");
        assert!(grid.bounds[0] >= 0.0 && grid.bounds[1] >= 0.0, "{grid:?}");
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
        let dump =
            drv.wait_for("the second shell to close", STEP, |d| d.items.len() == 1).await.unwrap();

        // Without a hardware keyboard the top bar's "⋯" is the way to every action: a tap
        // opens the command palette, the soft keyboard types into its field, ↩ runs the line.
        let commands = dump
            .a11y_node("Button", Some("Commands"))
            .unwrap_or_else(|| panic!("{:#?}", dump.a11y));
        let [x, y, w, h] = commands.bounds;
        drv.ui_tap(x + w / 2.0, y + h / 2.0).await.unwrap();
        drv.wait_for("the palette", STEP, |d| d.a11y_node("Dialog", Some("Commands")).is_some())
            .await
            .unwrap();
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

        // The phone names a card the same way: "Name this card" from the palette puts the
        // field in the active card's title bar, the soft keyboard types into it, ↩ keeps it.
        let dump = drv.dump().await.unwrap();
        let commands = dump
            .a11y_node("Button", Some("Commands"))
            .unwrap_or_else(|| panic!("{:#?}", dump.a11y));
        let [x, y, w, h] = commands.bounds;
        drv.ui_tap(x + w / 2.0, y + h / 2.0).await.unwrap();
        drv.wait_for("the palette", STEP, |d| d.a11y_node("Dialog", Some("Commands")).is_some())
            .await
            .unwrap();
        drv.ui_insert_text("name this").await.unwrap();
        drv.wait_for("one line", STEP, |d| {
            d.a11y.iter().filter(|n| n.role == "ListBoxOption").count() == 1
        })
        .await
        .unwrap();
        drv.keys("enter").await.unwrap();
        drv.wait_for("the name field", STEP, |d| {
            d.a11y_node("TextInput", Some("Card name")).is_some()
        })
        .await
        .unwrap();
        drv.ui_insert_text("scratch").await.unwrap();
        drv.keys("enter").await.unwrap();
        drv.wait_for("the note named", STEP, |d| {
            d.a11y_node("Heading", Some("note scratch")).is_some()
                && d.a11y_node("TextInput", Some("Card name")).is_none()
        })
        .await
        .unwrap();
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
        slopty_e2e::harness::check_jetbrains_mono_face(dump.terminals[0].face.as_ref()).unwrap();
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
        // VoiceOver's view of it, from the same tree the bridge mirrors: the entries as list
        // items, the composer field with the keyboard, the send button, the key bar's keys.
        // The list paints only what fits above the keyboard, pinned to the newest entry,
        // so the items are a tail of the transcript ending in its last line.
        let items: Vec<&str> = dump
            .a11y
            .iter()
            .filter(|n| n.role == "ListItem")
            .filter_map(|n| n.label.as_deref())
            .collect();
        assert!(!items.is_empty() && items.len() <= TRANSCRIPT_LINES.len(), "{:#?}", dump.a11y);
        assert_eq!(
            items.last().copied(),
            Some("Claude: Fixed: `b` compared **id** with *name*."),
            "{:#?}",
            dump.a11y
        );
        // (`composer_focused` above is the keyboard check: GPUI pins the focus to a node
        // inside gpui-kit's input, not to the labelled field.)
        assert!(
            dump.a11y_node("MultilineTextInput", Some("Message to Claude")).is_some(),
            "{:#?}",
            dump.a11y
        );
        assert!(dump.a11y_node("Button", Some("Send")).is_some(), "{:#?}", dump.a11y);
        for key in ["Escape", "Control", "Command", "Paste"] {
            assert!(dump.a11y_node("Button", Some(key)).is_some(), "{key}: {:#?}", dump.a11y);
        }
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

    /// The driven agent's card on the phone or the tablet: opened over the socket as ⌘⌥T
    /// would, the composer takes the keyboard, a prompt typed there is answered, and the
    /// header names the model and the mode. The frame is compared with the device's golden.
    #[tokio::test]
    async fn the_driven_agent_card_on_the_simulator() {
        let Some(simulator) = simulator() else { return };
        let mut stack =
            Stack::launch_on_simulator_with_driven_claude("e2e-ios-host", simulator).await.unwrap();
        let render_path = stack.path("agent.png");
        let question_path = stack.path("question.png");
        stack
            .driver
            .wait_for("the first shell", STEP, |d| {
                d.status == "connected" && d.item("terminal").is_some()
            })
            .await
            .unwrap();
        let drv = &mut stack.driver;
        // A finger opens it: "+ agent" on the bar, then "Conversation" in its menu — the
        // phone has no ⌘⌥T.
        let dump = drv.dump().await.unwrap();
        let centre = |dump: &slopty_e2e::Dump, role: &str, label: &str| {
            let node = dump
                .a11y_node(role, Some(label))
                .unwrap_or_else(|| panic!("{label}: {:#?}", dump.a11y));
            let [x, y, w, h] = node.bounds;
            (x + w / 2.0, y + h / 2.0)
        };
        let (ax, ay) = centre(&dump, "Button", "agent");
        drv.ui_tap(ax, ay).await.unwrap();
        let dump = drv
            .wait_for("the agent menu", STEP, |d| d.a11y_node("Menu", Some("Agent")).is_some())
            .await
            .unwrap();
        let (mx, my) = centre(&dump, "MenuItem", "Conversation");
        drv.ui_tap(mx, my).await.unwrap();
        drv.wait_for("the card with the caret in its composer", STEP, |d| {
            d.terminals.iter().any(|t| {
                t.kind == "agent" && t.conversation.as_ref().is_some_and(|c| c.composer_focused)
            })
        })
        .await
        .unwrap();
        drv.type_text("hello").await.unwrap();
        drv.keys("enter").await.unwrap();
        let dump = drv
            .wait_for("the answered turn", STEP, |d| {
                d.terminals.iter().any(|t| {
                    t.kind == "agent"
                        && t.agent.as_deref() == Some("done")
                        && t.conversation.as_ref().is_some_and(|c| {
                            c.entries == ["user: hello", "assistant: Hello from the fake"]
                                && c.model.as_deref() == Some("fake-model")
                        })
                })
            })
            .await
            .unwrap();
        assert!(dump.a11y_node("Button", Some("Model: fake-model")).is_some(), "{:#?}", dump.a11y);
        assert!(
            dump.a11y_node("Button", Some("Permission mode: Ask")).is_some(),
            "{:#?}",
            dump.a11y
        );
        assert!(dump.a11y_node("Button", Some("Send")).is_some(), "{:#?}", dump.a11y);
        let card = dump.terminals.iter().find(|t| t.kind == "agent").unwrap();
        let item = dump.items.iter().find(|i| i.session.as_deref() == Some(&card.session)).unwrap();
        let [x, y, w, h] = item.bounds;
        assert!(
            x >= 0.0 && y >= 0.0 && x + w <= dump.window.width + 1.0 && y + h < dump.window.height,
            "{item:?} in {}x{}",
            dump.window.width,
            dump.window.height
        );
        // A card fitted to the phone flies there: render once the camera and the card rest
        // (two dumps in a row agree), and say where they rested if the golden disagrees.
        let session = card.session.clone();
        let place = move |d: &slopty_e2e::Dump| {
            let bounds = d
                .items
                .iter()
                .find(|i| i.session.as_deref() == Some(&session))
                .map(|i| i.bounds)
                .unwrap_or_default();
            (d.zoom.to_bits(), bounds.map(f32::to_bits))
        };
        let mut last = place(&dump);
        let dump = drv
            .wait_for("the camera at rest", STEP, |d| {
                let now = place(d);
                let rest = now == last;
                last = now;
                rest
            })
            .await
            .unwrap();
        eprintln!("agent card at rest: zoom {} items {:?}", dump.zoom, dump.items);
        let frame = drv.render(&render_path).await.unwrap();
        assert!(foreground_fraction(&frame) > BLANK, "frame is blank");
        let name = format!("ios-{}-agent", device(dump.window.width));
        assert_matches(&name, &frame, TOLERANCE, &artifacts_dir()).unwrap();

        // A question: the options are buttons over the composer, and a finger on "Blue"
        // is the whole answer — no Allow / Deny, nothing typed.
        drv.type_text("ask me").await.unwrap();
        drv.keys("enter").await.unwrap();
        let dump = drv
            .wait_for("the question", STEP, |d| {
                d.terminals.iter().any(|t| {
                    t.kind == "agent"
                        && t.conversation.as_ref().is_some_and(|c| {
                            c.attention.as_deref() == Some("question")
                                && c.question_options == ["Red", "Blue"]
                        })
                }) && d.a11y_node("Button", Some("Blue")).is_some()
            })
            .await
            .unwrap();
        assert!(dump.a11y_node("Button", Some("Allow")).is_none(), "{:#?}", dump.a11y);
        let frame = drv.render(&question_path).await.unwrap();
        let name = format!("ios-{}-question", device(dump.window.width));
        assert_matches(&name, &frame, TOLERANCE, &artifacts_dir()).unwrap();
        let (bx, by) = centre(&dump, "Button", "Blue");
        drv.ui_tap(bx, by).await.unwrap();
        drv.wait_for("the answer taken", STEP, |d| {
            d.terminals.iter().any(|t| {
                t.kind == "agent"
                    && t.agent.as_deref() == Some("done")
                    && t.conversation.as_ref().is_some_and(|c| {
                        c.entries.last().map(String::as_str) == Some("assistant: Blue")
                            && c.attention.is_none()
                    })
            })
        })
        .await
        .unwrap();

        // A picture: a screenshot on the simulator's own pasteboard (put there through GPUI,
        // read back through UIPasteboard by the fork), the key bar's "paste" attaches it as a
        // chip, and ↩ sends it as an image block the fake reads back.
        let png = slopty_e2e::snapshot::tiny_png();
        let chip = format!("PNG · {} B", png.len());
        let read_back = format!("assistant: Saw 1 picture(s): image/png {} B", png.len());
        drv.clipboard_image("image/png", &png).await.unwrap();
        let dump = drv.dump().await.unwrap();
        let (px, py) = centre(&dump, "Button", "Paste");
        drv.ui_tap(px, py).await.unwrap();
        drv.wait_for("the attachment chip", STEP, |d| {
            d.terminals
                .iter()
                .any(|t| t.conversation.as_ref().is_some_and(|c| c.attachments == [chip.clone()]))
        })
        .await
        .unwrap();
        drv.ui_insert_text("what colour?").await.unwrap();
        drv.keys("enter").await.unwrap();
        drv.wait_for("the picture read back", STEP, |d| {
            d.terminals.iter().any(|t| {
                t.conversation.as_ref().is_some_and(|c| {
                    c.attachments.is_empty()
                        && c.entries
                            .ends_with(&["user: what colour? [+1]".to_owned(), read_back.clone()])
                })
            })
        })
        .await
        .unwrap();

        // An edit, then a finger on its "view": the file card is the phone's way to read the
        // file, so it appears, active, saying the file is not there yet.
        drv.ui_insert_text("edit the note").await.unwrap();
        drv.keys("enter").await.unwrap();
        // The turn must be over before the button is located: the entries that follow the
        // edit (the todo result, the answer, its caption) shift the list under a finger.
        let dump = drv
            .wait_for("the edit's view button after the turn", STEP, |d| {
                d.terminals.iter().any(|t| {
                    t.agent.as_deref() == Some("done")
                        && t.conversation.as_ref().is_some_and(|c| {
                            c.entries.last().map(String::as_str)
                                == Some("assistant: Edited: hi is now hello.")
                        })
                }) && d.a11y_node("Button", Some("View note.txt on the canvas")).is_some()
            })
            .await
            .unwrap();
        let (vx, vy) = centre(&dump, "Button", "View note.txt on the canvas");
        drv.ui_tap(vx, vy).await.unwrap();
        let dump = drv
            .wait_for("the file card", STEP, |d| {
                d.item("file").is_some_and(|i| {
                    i.active
                        && i.file.as_ref().is_some_and(|f| {
                            f.path.ends_with("/note.txt") && f.summary.starts_with("missing:")
                        })
                })
            })
            .await
            .unwrap();

        // The file lands on the host (the simulator's host is this Mac): "reload" reads it.
        let file = dump.item("file").unwrap().file.clone().unwrap();
        std::fs::write(&file.path, "hello\nthere\n").unwrap();
        let (rx, ry) = centre(&dump, "Button", "Read the file again");
        drv.ui_tap(rx, ry).await.unwrap();
        let dump = drv
            .wait_for("the file's lines", STEP, |d| {
                d.item("file").is_some_and(|i| i.file.as_ref().is_some_and(|f| f.lines == 2))
            })
            .await
            .unwrap();

        // The "find" pill is the phone's ⌘F: the bar opens with its field focused, the soft
        // keyboard's text finds line 2, and that hit is the card's reading line.
        let (fx, fy) = centre(&dump, "Button", "Find in the file");
        drv.ui_tap(fx, fy).await.unwrap();
        drv.wait_for("the find bar", STEP, |d| {
            d.a11y_node("Group", Some("Find in file")).is_some()
        })
        .await
        .unwrap();
        drv.ui_insert_text("there").await.unwrap();
        drv.wait_for("the hit as the reading line", STEP, |d| {
            d.item("file").is_some_and(|i| i.file.as_ref().is_some_and(|f| f.line == Some(2)))
        })
        .await
        .unwrap();
        stack.shutdown().await;
    }
}

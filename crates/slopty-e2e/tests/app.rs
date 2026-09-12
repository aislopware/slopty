//! The real app, driven from inside: pair with a host, open a shell, type, read the rows back,
//! render frames with the app's own renderer and compare them with the goldens.
//!
//! Runs only with `SLOPTY_APP_E2E=1` (`cargo xtask e2e app`), since it launches the app. Every
//! case but one needs no permission from the machine; the one that captures a window says so and
//! skips without `SLOPTY_SCREEN_E2E`.

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use slopty_e2e::harness::{TRANSCRIPT_LINES, artifacts_dir};
    use slopty_e2e::snapshot::{assert_matches, foreground_fraction};
    use slopty_e2e::{Button, Command, Stack};

    /// How long a host round trip (open a shell, run a command) may take.
    const STEP: Duration = Duration::from_secs(20);
    /// Window size for the renders: small, so the goldens stay small.
    const WINDOW: (f32, f32) = (900.0, 600.0);
    /// Fraction of pixels allowed to differ from a golden (hinting, RTT readout, cursor).
    const TOLERANCE: f64 = 0.01;
    /// Refresh requests the receiver may send for a target that produces no frame, checked
    /// against the host's own count of what it was asked for.
    fn refresh_cap() -> u64 {
        u64::from(slopty_media::Config::default().refresh_max_repeats)
    }

    fn gated() -> bool {
        if std::env::var_os("SLOPTY_APP_E2E").is_none() {
            eprintln!("skipped: set SLOPTY_APP_E2E=1 (or run `cargo xtask e2e app`)");
            return false;
        }
        true
    }

    /// [`gated`], plus the screen-recording grant hostd needs to capture anything. The rest of
    /// this suite asks the machine for nothing, so a case that captures gates on both.
    fn gated_on_capture() -> bool {
        if !gated() {
            return false;
        }
        if std::env::var_os("SLOPTY_SCREEN_E2E").is_none() {
            eprintln!("skipped: capturing a window needs SLOPTY_SCREEN_E2E=1 and the grant");
            return false;
        }
        true
    }

    #[tokio::test]
    async fn shell_typed_from_the_app_echoes_back_into_its_rows() {
        if !gated() {
            return;
        }
        let mut stack = Stack::launch("e2e-host").await.unwrap();
        let render_path = stack.path("terminal.png");
        let terminfo_db = stack.path("terminfo");
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
        // ptyd compiled ghostty's terminfo into the stack's own database while it came up, so
        // the shells it spawns can be told `TERM=xterm-ghostty` (the dump does not carry a
        // session's environment, and asking the shell would mean typing at it).
        let db = &terminfo_db;
        let compiled =
            || db.join("78/xterm-ghostty").exists() || db.join("x/xterm-ghostty").exists();
        let deadline = std::time::Instant::now() + STEP;
        while !compiled() && std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(compiled(), "ptyd installs ghostty's terminfo into {}", db.display());
        assert_eq!(dump.items.len(), 1, "{dump:#?}");
        let term = dump.item("terminal").unwrap().clone();
        assert!(term.active, "{dump:#?}");
        assert!(term.bounds[2] > 100.0 && term.bounds[3] > 100.0, "{term:?}");
        // What a screen reader gets: the item's heading, the grid as a terminal whose value is
        // the cursor row, the top bar's buttons, all inside the window.
        assert!(
            dump.a11y_node("Heading", None).is_some_and(|n| {
                n.label.as_deref().is_some_and(|l| l.starts_with("terminal "))
            }),
            "{:#?}",
            dump.a11y
        );
        let grid = dump.a11y_node("Terminal", None).unwrap_or_else(|| panic!("{:#?}", dump.a11y));
        assert!(grid.value.as_deref().is_some_and(|v| !v.is_empty()), "cursor row: {grid:?}");
        assert!(grid.bounds[2] > 100.0 && grid.bounds[3] > 100.0, "{grid:?}");
        for label in ["shell", "agent", "note", "window", "fit"] {
            assert!(dump.a11y_node("Button", Some(label)).is_some(), "{label}: {:#?}", dump.a11y);
        }

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
        slopty_e2e::harness::check_jetbrains_mono_face(dump.terminals[0].face.as_ref()).unwrap();

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
        // The conversation as a screen reader gets it: every entry a list item, the composer
        // a labelled text field with the keyboard, its send button after it.
        let items = dump.a11y.iter().filter(|n| n.role == "ListItem").count();
        assert_eq!(items, TRANSCRIPT_LINES.len(), "{:#?}", dump.a11y);
        // (`composer_focused` above is the keyboard check: GPUI pins the focus to a node
        // inside gpui-kit's input, not to the labelled field.)
        assert!(
            dump.a11y_node("MultilineTextInput", Some("Message to Claude")).is_some(),
            "{:#?}",
            dump.a11y
        );
        assert!(dump.a11y_node("Button", Some("Send")).is_some(), "{:#?}", dump.a11y);

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

    /// A structured agent: the host runs Claude Code (here the fake, `SLOPTY_CLAUDE_BIN`)
    /// over its stream-json protocol and the card is the conversation with no grid. The
    /// composer speaks to the agent and the prompt comes back as the user entry; the answer
    /// streams in under the list ahead of its entry; Esc interrupts a turn; a tool call puts
    /// Allow / Deny above the composer with what the tool would do, and the answer goes back
    /// by request id; ⌘W ends the agent and takes the card away.
    #[tokio::test]
    async fn a_driven_agent_talks_over_stream_json() {
        if !gated() {
            return;
        }
        let mut stack = Stack::launch_with_driven_claude("e2e-host").await.unwrap();
        let home = stack.home().unwrap();
        stack.driver.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
        stack
            .driver
            .wait_for("the first shell", STEP, |d| {
                d.status == "connected" && d.item("terminal").is_some()
            })
            .await
            .unwrap();
        let drv = &mut stack.driver;
        // The card is opened the way a mouse does it: "+ agent" opens a menu of the three
        // ways to an agent, "Conversation" is the driven one.
        let dump = drv.dump().await.unwrap();
        let pill =
            dump.a11y_node("Button", Some("agent")).unwrap_or_else(|| panic!("{:#?}", dump.a11y));
        let (px_, py_) = a11y_center(pill);
        drv.click(px_, py_).await.unwrap();
        let dump = drv
            .wait_for("the agent menu", STEP, |d| d.a11y_node("Menu", Some("Agent")).is_some())
            .await
            .unwrap();
        assert_eq!(
            dump.a11y
                .iter()
                .filter(|n| n.role == "MenuItem")
                .map(|n| n.label.clone().unwrap_or_default())
                .collect::<Vec<_>>(),
            ["Terminal agent", "Conversation", "Resume conversation…"],
            "{:#?}",
            dump.a11y
        );
        let (mx, my) = a11y_center(dump.a11y_node("MenuItem", Some("Conversation")).unwrap());
        drv.click(mx, my).await.unwrap();
        let dump = drv
            .wait_for("the agent card with the caret in its composer", STEP, |d| {
                d.terminals.iter().any(|t| {
                    t.kind == "agent" && t.conversation.as_ref().is_some_and(|c| c.composer_focused)
                })
            })
            .await
            .unwrap();
        let card = dump.terminals.iter().find(|t| t.kind == "agent").unwrap();
        let session = card.session.clone();
        assert_eq!(card.agent_source.as_deref(), Some("driven"), "{card:?}");
        assert_eq!(dump.items.len(), 2, "{:?}", dump.items);
        let chat = move |d: &slopty_e2e::Dump| {
            d.terminals
                .iter()
                .find(|t| t.session == session)
                .and_then(|t| t.conversation.clone().map(|c| (t.agent.clone(), c)))
        };

        // A prompt: sent on ↩, shown at once as the user entry (the agent's replay of it is
        // not shown twice), answered by a streamed text that becomes the assistant entry.
        drv.type_text("hello").await.unwrap();
        drv.keys("enter").await.unwrap();
        let dump = drv
            .wait_for("the turn to end", STEP, |d| {
                chat(d).is_some_and(|(agent, conv)| {
                    agent.as_deref() == Some("done")
                        && conv.entries == ["user: hello", "assistant: Hello from the fake"]
                })
            })
            .await
            .unwrap();
        let (_, conv) = chat(&dump).unwrap();
        assert!(
            conv.partial.is_empty() && conv.composer.is_empty() && conv.attention.is_none(),
            "{conv:?}"
        );
        // What the agent said about itself, in the header: the model, the mode, the turn.
        assert_eq!(conv.model.as_deref(), Some("fake-model"), "{conv:?}");
        assert_eq!(conv.permission_mode.as_deref(), Some("default"), "{conv:?}");
        assert_eq!(conv.agent_session.as_deref(), Some("fake-session"), "{conv:?}");
        assert_eq!(conv.turns, 1, "{conv:?}");
        assert!(conv.slash_commands.iter().any(|c| c == "/cost"), "{conv:?}");
        assert_eq!(conv.usage.as_deref(), Some("5h 23% · 7d 74%"), "{conv:?}");
        assert!(dump.a11y_node("Button", Some("Model: fake-model")).is_some(), "{:#?}", dump.a11y);

        // The model chip opens the menu; picking Opus retunes the agent in place (no
        // restart: the same session id) and the chip follows the agent's word.
        let chip = dump.a11y_node("Button", Some("Model: fake-model")).unwrap();
        let [bx, by, bw, bh] = chip.bounds;
        drv.click(bx + bw / 2.0, by + bh / 2.0).await.unwrap();
        let dump = drv
            .wait_for("the model menu", STEP, |d| {
                chat(d).is_some_and(|(_, conv)| conv.model_menu)
                    && d.a11y_node("MenuItem", Some("Opus 5")).is_some()
            })
            .await
            .unwrap();
        let opus = dump.a11y_node("MenuItem", Some("Opus 5")).unwrap();
        let [bx, by, bw, bh] = opus.bounds;
        drv.click(bx + bw / 2.0, by + bh / 2.0).await.unwrap();
        let dump = drv
            .wait_for("the agent on opus", STEP, |d| {
                chat(d).is_some_and(|(_, conv)| {
                    conv.model.as_deref() == Some("opus") && !conv.model_menu
                })
            })
            .await
            .unwrap();
        assert_eq!(chat(&dump).unwrap().1.agent_session.as_deref(), Some("fake-session"));
        // The mode chip cycles to the next mode; the agent's status record confirms it.
        let mode = dump.a11y_node("Button", Some("Permission mode: Ask")).unwrap();
        let [bx, by, bw, bh] = mode.bounds;
        drv.click(bx + bw / 2.0, by + bh / 2.0).await.unwrap();
        drv.wait_for("accept edits", STEP, |d| {
            chat(d).is_some_and(|(_, conv)| conv.permission_mode.as_deref() == Some("acceptEdits"))
        })
        .await
        .unwrap();

        // Slash completion: `/c` lists the three, Esc hides them without interrupting,
        // `/cos` + Tab completes `/cost`, ↩ sends it and the agent answers it.
        drv.type_text("/c").await.unwrap();
        drv.wait_for("the completions", STEP, |d| {
            chat(d).is_some_and(|(_, conv)| conv.completions == ["/compact", "/clear", "/cost"])
        })
        .await
        .unwrap();
        drv.keys("escape").await.unwrap();
        drv.wait_for("the list hidden", STEP, |d| {
            chat(d).is_some_and(|(agent, conv)| {
                conv.completions.is_empty() && agent.as_deref() == Some("done")
            })
        })
        .await
        .unwrap();
        drv.type_text("os").await.unwrap();
        drv.wait_for("one completion", STEP, |d| {
            chat(d).is_some_and(|(_, conv)| conv.completions == ["/cost"])
        })
        .await
        .unwrap();
        drv.keys("tab").await.unwrap();
        drv.wait_for("the completed command", STEP, |d| {
            chat(d)
                .is_some_and(|(_, conv)| conv.composer == "/cost " && conv.completions.is_empty())
        })
        .await
        .unwrap();
        drv.keys("enter").await.unwrap();
        drv.wait_for("the command's answer", STEP, |d| {
            chat(d).is_some_and(|(agent, conv)| {
                agent.as_deref() == Some("done")
                    && conv.entries.last().map(String::as_str)
                        == Some("assistant: Total cost: $0.02")
                    && conv.turns == 2
            })
        })
        .await
        .unwrap();

        // A turn that streams and stops: the partial shows under the list while the agent is
        // working, Send has become Stop; a click on it interrupts, and the turn ends with the
        // interrupted record.
        drv.type_text("linger").await.unwrap();
        drv.keys("enter").await.unwrap();
        let dump = drv
            .wait_for("the streamed text", STEP, |d| {
                chat(d).is_some_and(|(agent, conv)| {
                    agent.as_deref() == Some("working") && conv.partial == "Hello from the fake"
                })
            })
            .await
            .unwrap();
        assert!(dump.a11y_node("Button", Some("Send")).is_none(), "{:#?}", dump.a11y);
        let stop =
            dump.a11y_node("Button", Some("Stop")).unwrap_or_else(|| panic!("{:#?}", dump.a11y));
        let (sx, sy) = a11y_center(stop);
        drv.click(sx, sy).await.unwrap();
        drv.wait_for("the interrupted turn", STEP, |d| {
            chat(d).is_some_and(|(_, conv)| {
                conv.partial.is_empty()
                    && conv.entries.last().map(String::as_str)
                        == Some("user: [Request interrupted by user]")
            })
        })
        .await
        .unwrap();

        // A tool call: Allow / Deny above the composer name the tool and what it would do;
        // Allow answers the request and the agent goes on to its result and its closing line.
        drv.type_text("write the note").await.unwrap();
        drv.keys("enter").await.unwrap();
        let dump = drv
            .wait_for("the permission row", STEP, |d| {
                chat(d).is_some_and(|(agent, conv)| {
                    agent.as_deref() == Some("blocked:permission:Write")
                        && conv.attention.as_deref() == Some("permission:Write")
                })
            })
            .await
            .unwrap();
        let (_, conv) = chat(&dump).unwrap();
        assert_eq!(conv.permission.as_deref(), Some("Write:note.txt"), "{conv:?}");
        assert_eq!(
            conv.entries.last().map(String::as_str),
            Some("tool Write: note.txt"),
            "{conv:?}"
        );
        let allow = dump.a11y_node("Button", Some("Allow")).unwrap();
        let [bx, by, bw, bh] = allow.bounds;
        drv.click(bx + bw / 2.0, by + bh / 2.0).await.unwrap();
        let dump = drv
            .wait_for("the allowed call's result and the closing line", STEP, |d| {
                chat(d).is_some_and(|(agent, conv)| {
                    agent.as_deref() == Some("done")
                        && conv.entries.last().map(String::as_str)
                            == Some("assistant: Done: the note is written.")
                })
            })
            .await
            .unwrap();
        let (_, conv) = chat(&dump).unwrap();
        assert!(conv.entries.contains(&"result Write: wrote note.txt".to_owned()), "{conv:?}");
        assert!(conv.attention.is_none() && conv.permission.is_none(), "{conv:?}");

        // An edit the settings allow: no permission, and the card shows the call as a diff
        // with its counts and the todo list as a checklist (the golden is that card).
        drv.type_text("edit the note").await.unwrap();
        drv.keys("enter").await.unwrap();
        let dump = drv
            .wait_for("the edit's diff and the todo list", STEP, |d| {
                chat(d).is_some_and(|(agent, conv)| {
                    agent.as_deref() == Some("done")
                        && conv.entries.last().map(String::as_str)
                            == Some("assistant: Edited: hi is now hello.")
                })
            })
            .await
            .unwrap();
        let (_, conv) = chat(&dump).unwrap();
        assert!(conv.entries.contains(&"tool Edit: note.txt (+1 -1)".to_owned()), "{conv:?}");
        assert!(conv.entries.contains(&"tool TodoWrite: 1/2 done".to_owned()), "{conv:?}");
        assert!(
            dump.a11y_node("ListItem", Some("Tool Edit: note.txt, 1 added, 1 removed")).is_some(),
            "{:#?}",
            dump.a11y
        );
        let frame = drv.render(&stack.dir.path().join("tools.png")).await.unwrap();
        assert_matches("conversation-tools", &frame, TOLERANCE, &artifacts_dir()).unwrap();

        // A question: the options are buttons above the composer (no Allow / Deny), one
        // tap on "Blue" answers by request id with the answer filed under the question, and
        // the agent's result and closing line carry the choice.
        drv.type_text("ask me").await.unwrap();
        drv.keys("enter").await.unwrap();
        let dump = drv
            .wait_for("the question", STEP, |d| {
                chat(d).is_some_and(|(agent, conv)| {
                    agent.as_deref() == Some("blocked:question")
                        && conv.attention.as_deref() == Some("question")
                })
            })
            .await
            .unwrap();
        let (_, conv) = chat(&dump).unwrap();
        assert_eq!(conv.question_options, ["Red", "Blue"], "{conv:?}");
        assert_eq!(
            conv.entries.last().map(String::as_str),
            Some("tool AskUserQuestion: Which colour do you prefer?"),
            "{conv:?}"
        );
        assert!(dump.a11y_node("Button", Some("Allow")).is_none(), "{:#?}", dump.a11y);
        let frame = drv.render(&stack.dir.path().join("question.png")).await.unwrap();
        assert_matches("conversation-question", &frame, TOLERANCE, &artifacts_dir()).unwrap();
        let blue = dump.a11y_node("Button", Some("Blue")).unwrap();
        let (bx, by) = a11y_center(blue);
        drv.click(bx, by).await.unwrap();
        let dump = drv
            .wait_for("the answer's result and the closing line", STEP, |d| {
                chat(d).is_some_and(|(agent, conv)| {
                    agent.as_deref() == Some("done")
                        && conv.entries.last().map(String::as_str) == Some("assistant: Blue")
                })
            })
            .await
            .unwrap();
        let (_, conv) = chat(&dump).unwrap();
        assert!(
            conv.entries.contains(
                &"result AskUserQuestion: Your questions have been answered: \"Which colour do you prefer?\"=\"Blue\". You can now continue with these answers in mind.".to_owned()
            ),
            "{conv:?}"
        );
        assert!(conv.attention.is_none() && conv.question_options.is_empty(), "{conv:?}");

        // Deny: the call fails with the host's message and the agent says so.
        drv.type_text("write it again").await.unwrap();
        drv.keys("enter").await.unwrap();
        let dump = drv
            .wait_for("the second permission row", STEP, |d| {
                chat(d)
                    .is_some_and(|(_, conv)| conv.attention.as_deref() == Some("permission:Write"))
            })
            .await
            .unwrap();
        let deny = dump.a11y_node("Button", Some("Deny")).unwrap();
        let [bx, by, bw, bh] = deny.bounds;
        drv.click(bx + bw / 2.0, by + bh / 2.0).await.unwrap();
        let dump = drv
            .wait_for("the denied call", STEP, |d| {
                chat(d).is_some_and(|(agent, conv)| {
                    agent.as_deref() == Some("done")
                        && conv.entries.last().map(String::as_str)
                            == Some("assistant: Understood, I did not write it.")
                })
            })
            .await
            .unwrap();
        let (_, conv) = chat(&dump).unwrap();
        assert!(conv.entries.iter().any(|e| e.starts_with("result Write failed: ")), "{conv:?}");

        // A subagent: its own records stay off the card, and its progress shows under the
        // call that spawned it, running then done.
        drv.type_text("delegate the listing").await.unwrap();
        drv.keys("enter").await.unwrap();
        let dump = drv
            .wait_for("the subagent's turn", STEP, |d| {
                chat(d).is_some_and(|(agent, conv)| {
                    agent.as_deref() == Some("done")
                        && conv.entries.last().map(String::as_str)
                            == Some("assistant: The subagent found note.txt.")
                })
            })
            .await
            .unwrap();
        let (_, conv) = chat(&dump).unwrap();
        assert!(conv.entries.contains(&"tool Agent: List files".to_owned()), "{conv:?}");
        assert!(!conv.entries.iter().any(|e| e == "tool Bash: ls"), "{conv:?}");
        assert!(conv.tasks.iter().any(|t| t.ends_with(":Running List files:1:done")), "{conv:?}");
        assert!(
            dump.a11y_node(
                "ListItem",
                Some("Tool Agent: List files, Explore done, 1 tool use, 3 s, Bash")
            )
            .is_some(),
            "{:#?}",
            dump.a11y
        );

        // Always: the row names what the agent suggested; taking it allows this call, the
        // mode chip follows the agent's status record, and the next write does not ask.
        drv.type_text("write once more").await.unwrap();
        drv.keys("enter").await.unwrap();
        let dump = drv
            .wait_for("the third permission row", STEP, |d| {
                chat(d)
                    .is_some_and(|(_, conv)| conv.attention.as_deref() == Some("permission:Write"))
            })
            .await
            .unwrap();
        let (_, conv) = chat(&dump).unwrap();
        assert_eq!(conv.always.as_deref(), Some("accept edits for this session"), "{conv:?}");
        let always = dump
            .a11y_node("Button", Some("Always: accept edits for this session"))
            .unwrap_or_else(|| panic!("{:#?}", dump.a11y));
        let (ax, ay) = a11y_center(always);
        drv.click(ax, ay).await.unwrap();
        drv.wait_for("the mode switched and the call done", STEP, |d| {
            chat(d).is_some_and(|(agent, conv)| {
                agent.as_deref() == Some("done")
                    && conv.permission_mode.as_deref() == Some("acceptEdits")
                    && conv.entries.last().map(String::as_str)
                        == Some("assistant: Done: the note is written.")
            })
        })
        .await
        .unwrap();
        drv.type_text("write yet again").await.unwrap();
        drv.keys("enter").await.unwrap();
        let dump = drv
            .wait_for("a write with no question asked", STEP, |d| {
                chat(d).is_some_and(|(agent, conv)| {
                    agent.as_deref() == Some("done")
                        && conv
                            .entries
                            .iter()
                            .filter(|e| e == &"result Write: wrote note.txt")
                            .count()
                            == 3
                })
            })
            .await
            .unwrap();
        let (_, conv) = chat(&dump).unwrap();
        assert!(conv.attention.is_none() && conv.permission.is_none(), "{conv:?}");

        // ⌘W: the agent ends, its session closes, the card goes.
        drv.keys("cmd-w").await.unwrap();
        drv.wait_for("the card gone", STEP, |d| {
            d.items.len() == 1 && d.terminals.iter().all(|t| t.kind != "agent")
        })
        .await
        .unwrap();

        // ⌘⌥R: the host lists the conversations in the active shell's directory when the
        // shell has reported one (OSC 7), offering every directory too; without one it lists
        // every directory on the host outright — the private HOME holds one project either
        // way, the fake's. The whole host's list holds the same conversation and offers
        // nothing wider. The row named by the first prompt opens the same Claude Code session
        // again, as a new card titled by that prompt.
        drv.keys("cmd-alt-r").await.unwrap();
        let hello_row = |d: &slopty_e2e::Dump| {
            d.a11y_node("Dialog", Some("Resume a conversation")).is_some()
                && d.a11y.iter().any(|n| {
                    n.role == "Button"
                        && n.label.as_deref().is_some_and(|l| l.starts_with("hello, "))
                })
        };
        let everywhere = |d: &slopty_e2e::Dump| {
            d.a11y.iter().any(|n| {
                n.role == "Button"
                    && n.label.as_deref().is_some_and(|l| l.starts_with("Every directory, "))
            })
        };
        let dump = drv.wait_for("the resume picker", STEP, hello_row).await.unwrap();
        if let Some(wider) = dump
            .a11y
            .iter()
            .find(|n| n.label.as_deref().is_some_and(|l| l.starts_with("Every directory, ")))
        {
            let [bx, by, bw, bh] = wider.bounds;
            drv.click(bx + bw / 2.0, by + bh / 2.0).await.unwrap();
        }
        let dump = drv
            .wait_for("the whole host's list", STEP, |d| hello_row(d) && !everywhere(d))
            .await
            .unwrap();
        let row = dump
            .a11y
            .iter()
            .find(|n| {
                n.role == "Button" && n.label.as_deref().is_some_and(|l| l.starts_with("hello, "))
            })
            .unwrap();
        // The directory as the agent's process saw it (macOS resolves `/var` to `/private/var`).
        let seen = std::fs::canonicalize(&home).unwrap();
        assert!(
            row.label.as_deref().is_some_and(|l| l.contains(&*seen.to_string_lossy())),
            "the row names the directory: {row:?}"
        );
        let [bx, by, bw, bh] = row.bounds;
        drv.click(bx + bw / 2.0, by + bh / 2.0).await.unwrap();
        let dump = drv
            .wait_for("the resumed card", STEP, |d| {
                d.terminals.iter().any(|t| {
                    t.kind == "agent"
                        && t.conversation.as_ref().is_some_and(|c| {
                            c.agent_session.as_deref() == Some("fake-session") && c.composer_focused
                        })
                })
            })
            .await
            .unwrap();
        assert_eq!(dump.items.len(), 2, "{:?}", dump.items);
        assert!(dump.a11y_node("Dialog", None).is_none(), "the picker closed");
        let card = dump.terminals.iter().find(|t| t.kind == "agent").unwrap();
        assert_eq!(card.title.as_deref(), Some("hello"), "titled by the first prompt: {card:?}");
        // Its past, read from the transcript, is shown before the agent says anything new.
        let dump = drv
            .wait_for("the resumed past", STEP, |d| {
                d.terminals
                    .iter()
                    .any(|t| t.conversation.as_ref().is_some_and(|c| c.entries.len() >= 15))
            })
            .await
            .unwrap();
        let card = dump.terminals.iter().find(|t| t.kind == "agent").unwrap();
        let entries = &card.conversation.as_ref().unwrap().entries;
        assert_eq!(
            entries,
            &[
                "user: hello".to_owned(),
                "assistant: Hello from the fake".to_owned(),
                "user: /cost".to_owned(),
                "assistant: Total cost: $0.02".to_owned(),
                "user: linger".to_owned(),
                "user: write the note".to_owned(),
                "user: edit the note".to_owned(),
                "assistant: Edited: hi is now hello.".to_owned(),
                "user: ask me".to_owned(),
                "user: write it again".to_owned(),
                "user: delegate the listing".to_owned(),
                "assistant: The subagent found note.txt.".to_owned(),
                "user: write once more".to_owned(),
                "user: write yet again".to_owned(),
                "assistant: Done: the note is written.".to_owned(),
            ],
            "the whole past, oldest first (the fake logs text replies only)"
        );

        // A picture attached to the next prompt (the socket's stand-in for ⌘V with one on the
        // clipboard): a chip names it, ↩ sends it as an image block the fake reads back, the
        // bubble says a picture went with the prompt, and the chip is gone.
        let png = slopty_e2e::snapshot::tiny_png();
        let chip = format!("PNG · {} B", png.len());
        let read_back = format!("assistant: Saw 1 picture(s): image/png {} B", png.len());
        drv.attach("image/png", &png).await.unwrap();
        drv.wait_for("the attachment chip", STEP, |d| {
            d.terminals
                .iter()
                .any(|t| t.conversation.as_ref().is_some_and(|c| c.attachments == [chip.clone()]))
        })
        .await
        .unwrap();
        drv.type_text("what colour?").await.unwrap();
        drv.keys("enter").await.unwrap();
        let dump = drv
            .wait_for("the picture read back", STEP, |d| {
                d.terminals.iter().any(|t| {
                    t.conversation.as_ref().is_some_and(|c| {
                        c.attachments.is_empty()
                            && c.entries.ends_with(&[
                                "user: what colour? [+1]".to_owned(),
                                read_back.clone(),
                            ])
                    })
                })
            })
            .await
            .unwrap();
        let card = dump.terminals.iter().find(|t| t.kind == "agent").unwrap();
        assert!(
            dump.a11y.iter().any(|n| n.label.as_deref().is_some_and(|l| l.contains("1 picture"))),
            "the bubble says a picture went with it: {:?}",
            card.conversation
        );
        stack.shutdown().await;
    }

    /// The window point at the middle of an accessibility node.
    fn a11y_center(node: &slopty_e2e::A11yNode) -> (f32, f32) {
        let [x, y, w, h] = node.bounds;
        (x + w / 2.0, y + h / 2.0)
    }

    /// A `claude` nobody registered hooks for: "+ agent" starts the fake one the harness put
    /// on ptyd's `PATH`, and the host attributes it from its foreground process, then its
    /// title, then the transcript it writes — each signal taking over from the weaker one.
    /// ⌘⇧L then shows the conversation, which only works because the transcript was found
    /// rather than named by a hook.
    #[tokio::test]
    async fn an_agent_started_without_hooks_is_attributed_from_what_the_host_can_see() {
        if !gated() {
            return;
        }
        let mut stack = Stack::launch_with_fake_claude("e2e-host").await.unwrap();
        stack.driver.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
        stack
            .driver
            .wait_for("the first shell", STEP, |d| {
                d.status == "connected" && d.item("terminal").is_some()
            })
            .await
            .unwrap();

        // ⌘⇧T runs `claude`, which here is the fake: it prints where it runs and waits.
        stack.driver.keys("cmd-shift-t").await.unwrap();
        let dump = stack
            .driver
            .wait_for("the agent's terminal", STEP, |d| {
                d.items.len() == 2 && !d.rows_containing("fake claude in ").is_empty()
            })
            .await
            .unwrap();
        assert!(!dump.hooks_offered, "nothing has been offered yet");

        // No hook has fired and no title has been painted: the process is the whole signal.
        let dump = stack
            .driver
            .wait_for("the agent seen from its process alone", STEP, |d| {
                d.terminals.iter().any(|t| {
                    t.agent.as_deref() == Some("idle")
                        && t.agent_source.as_deref() == Some("process")
                })
            })
            .await
            .unwrap();
        let session = dump
            .terminals
            .iter()
            .find(|t| t.agent.is_some())
            .map(|t| t.session.clone())
            .expect("the agent's session");

        // It paints the spinning title: the cheapest heartbeat there is says a turn started.
        stack.fake_claude_stage("working").unwrap();
        stack
            .driver
            .wait_for("the title to say a turn is running", STEP, |d| {
                d.terminals.iter().any(|t| {
                    t.agent.as_deref() == Some("working")
                        && t.agent_source.as_deref() == Some("title")
                })
            })
            .await
            .unwrap();

        // It writes its conversation where Claude Code writes one; the host finds the file
        // from the session's own working directory and reads the turn out of it.
        stack.fake_claude_stage("transcript").unwrap();
        stack
            .driver
            .wait_for("the transcript to take over", STEP, |d| {
                d.terminals.iter().any(|t| t.agent_source.as_deref() == Some("transcript"))
            })
            .await
            .unwrap();
        stack.fake_claude_stage("done").unwrap();
        stack
            .driver
            .wait_for("the turn to finish", STEP, |d| {
                d.terminals.iter().any(|t| {
                    t.agent.as_deref() == Some("done")
                        && t.agent_source.as_deref() == Some("transcript")
                })
            })
            .await
            .unwrap();

        // ⌘⇧L reads the same discovered file: the conversation view needs no hooks either.
        stack.driver.keys("cmd-shift-l").await.unwrap();
        let dump = stack
            .driver
            .wait_for("the conversation", STEP, |d| {
                d.terminals.iter().any(|t| {
                    t.conversation
                        .as_ref()
                        .is_some_and(|c| c.entries.len() > TRANSCRIPT_LINES.len())
                })
            })
            .await
            .unwrap();
        let conversation = dump
            .terminals
            .iter()
            .find(|t| t.session == session)
            .and_then(|t| t.conversation.clone())
            .expect("the agent's conversation");
        assert_eq!(conversation.entries.first().map(String::as_str), Some(TRANSCRIPT_LINES[0]));
        assert_eq!(
            conversation.entries.last().map(String::as_str),
            Some("assistant: Both tests pass now.")
        );

        // The fake exits; with it the agent goes, whatever the last signal said.
        stack.fake_claude_stage("quit").unwrap();
        stack
            .driver
            .wait_for("the agent to go with its process", STEP, |d| {
                d.terminals.iter().all(|t| t.agent.is_none())
            })
            .await
            .unwrap();
        stack.shutdown().await;
    }

    /// The relay command a settings document registers, from the first entry that is ours.
    fn relay_command(settings: &str) -> String {
        let doc: serde_json::Value = serde_json::from_str(settings).expect("settings are JSON");
        doc["hooks"]
            .as_object()
            .expect("a hooks map")
            .values()
            .filter_map(serde_json::Value::as_array)
            .flatten()
            .filter_map(|group| group.get("hooks")?.as_array())
            .flatten()
            .find(|entry| slopty_agent::hooks::is_relay(entry))
            .and_then(|entry| entry.get("command")?.as_str())
            .expect("a relay entry")
            .to_owned()
    }

    /// The "hooks" pill, clicked the way a human clicks it, ends in hostd writing Claude
    /// Code's settings — in the *harness's* home, which is the only home these daemons have.
    ///
    /// The click goes through the accessibility tree: the pill publishes its bounds there
    /// because it is a button with a label, which is also how a screen reader reaches it.
    #[tokio::test]
    async fn the_hooks_pill_installs_the_relay_in_the_harness_home() {
        if !gated() {
            return;
        }
        let mut stack = Stack::launch_with_fake_claude("e2e-host").await.unwrap();
        // Before anything else: the daemons' home is the run's own directory, so this test
        // cannot touch the developer's `~/.claude` even if the wiring were wrong.
        let home = stack.path("home");
        let settings = home.join(".claude").join("settings.json");
        assert!(
            settings.starts_with(stack.dir.path()),
            "the harness owns HOME: {}",
            settings.display()
        );
        assert!(!settings.exists(), "and starts without settings: {}", settings.display());

        stack.driver.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
        stack
            .driver
            .wait_for("the first shell", STEP, |d| {
                d.status == "connected" && d.item("terminal").is_some()
            })
            .await
            .unwrap();

        // ⌘⇧T starts the fake `claude`; with no hook firing the host has to guess, which is
        // exactly when the pill is offered.
        stack.driver.keys("cmd-shift-t").await.unwrap();
        let dump = stack
            .driver
            .wait_for("the hooks pill on the guessed agent", STEP, |d| {
                d.terminals.iter().any(|t| t.agent_source.as_deref() == Some("process"))
                    && d.a11y_node("Button", Some("install hooks")).is_some()
            })
            .await
            .unwrap();
        assert!(!dump.hooks_offered, "not offered until it is clicked");

        let [x, y, w, h] =
            dump.a11y_node("Button", Some("install hooks")).expect("the pill").bounds;
        stack
            .driver
            .ok(&Command::Click { x: x + w / 2.0, y: y + h / 2.0, button: Button::Left, count: 1 })
            .await
            .unwrap();
        stack
            .driver
            .wait_for("the offer to retire", STEP, |d| {
                d.hooks_offered && d.a11y_node("Button", Some("install hooks")).is_none()
            })
            .await
            .unwrap();

        // hostd writes the file; give it the same grace the driver gives the app.
        let deadline = std::time::Instant::now() + STEP;
        while !settings.exists() && std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(settings.exists(), "hostd wrote {}", settings.display());

        let registered = slopty_agent::hooks::registered(&settings).unwrap();
        assert_eq!(
            registered.len(),
            slopty_agent::HOOK_EVENTS.len(),
            "every event relays: {registered:?}"
        );
        let written = std::fs::read_to_string(&settings).unwrap();
        assert!(written.contains("slopty"), "the relay is the slopty beside the host: {written}");
        assert!(written.contains("hook"), "and it is the hook subcommand: {written}");

        // Installing the same relay again over what the daemon wrote changes nothing. (The
        // pill itself retires after one click, so a second *click* is not reachable through
        // the UI.) The command has to be the daemon's own — `install` repoints a relay that
        // moved — so read it back out of the file the daemon wrote.
        let relay = relay_command(&written);
        assert!(relay.ends_with("slopty"), "the relay beside the host: {relay}");
        let outcome = slopty_agent::hooks::install_at(&settings, &relay).unwrap();
        assert_eq!(outcome, slopty_agent::hooks::Outcome::Unchanged);
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), written, "byte for byte");

        stack.fake_claude_stage("quit").unwrap();
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

        // ⌘⇧R tidies the canvas into a block per repository. The shell has a working
        // directory and the note does not, so there are two blocks, each under a heading a
        // screen reader can read.
        let scattered = dump.item("note").unwrap().bounds;
        drv.keys("cmd-shift-r").await.unwrap();
        let dump = drv
            .wait_for("the arrangement", STEP, |d| {
                d.a11y_node("Heading", Some("no repository")).is_some()
            })
            .await
            .unwrap();
        let headings =
            dump.a11y.iter().filter(|n| n.role == "Heading" && n.label.is_some()).count();
        assert!(headings >= 2, "one heading per repository, and one for the note: {headings}");
        let tidied = dump.item("note").unwrap().bounds;
        let moved = tidied.iter().zip(scattered).any(|(a, b)| (a - b).abs() > 0.5);
        assert!(moved, "the note was tidied: {scattered:?} -> {tidied:?}");

        // The shell's heading is its checkout's name, which differs from machine to machine;
        // close it so the golden is only what every run has.
        let terminal = dump.item("terminal").expect("the first shell");
        drv.ok(&Command::Click {
            x: terminal.center().0,
            y: terminal.center().1,
            button: Button::Left,
            count: 1,
        })
        .await
        .unwrap();
        drv.keys("cmd-w").await.unwrap();
        drv.wait_for("the shell to go", STEP, |d| d.item("terminal").is_none()).await.unwrap();
        drv.keys("cmd-shift-r").await.unwrap();
        drv.wait_for("the note alone under its heading", STEP, |d| {
            d.items.len() == 1 && d.a11y_node("Heading", Some("no repository")).is_some()
        })
        .await
        .unwrap();

        let arranged = stack.path("arrange-by-repo.png");
        let frame = stack.driver.render(&arranged).await.unwrap();
        assert_matches("arrange-by-repo", &frame, TOLERANCE, &artifacts_dir()).unwrap();
        stack.shutdown().await;
    }

    /// A remote window whose target never draws: ⌘O picks it while it is still on screen, the
    /// helper takes it away before the stream opens, and the item says so instead of asking the
    /// host for refreshes no refresh can answer. Then the window draws again and the picture
    /// arrives without the client doing anything.
    #[tokio::test]
    async fn a_remote_window_that_never_draws_waits_instead_of_asking_forever() {
        if !gated_on_capture() {
            return;
        }
        let mut stack = Stack::launch("e2e-host").await.unwrap();
        stack.driver.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
        let idle = stack.start_idle_window().await.unwrap();
        stack
            .driver
            .wait_for("the first shell", STEP, |d| {
                d.status == "connected" && d.item("terminal").is_some()
            })
            .await
            .unwrap();

        // ⌘O lists the host's windows. The picker shows on-screen windows only, which is why the
        // helper is still on screen here.
        stack.driver.keys("cmd-o").await.unwrap();
        let row = format!(", {}", idle.title());
        let dump = stack
            .driver
            .wait_for("the idle window in the picker", STEP, |d| {
                d.a11y.iter().any(|n| {
                    n.role == "Button" && n.label.as_deref().is_some_and(|l| l.ends_with(&row))
                })
            })
            .await
            .unwrap();
        let button = dump
            .a11y
            .iter()
            .find(|n| n.role == "Button" && n.label.as_deref().is_some_and(|l| l.ends_with(&row)))
            .unwrap_or_else(|| panic!("{:#?}", dump.a11y))
            .clone();

        // Off screen before the stream opens: the host can still capture it, and captures
        // nothing.
        idle.hide().unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        let [x, y, w, h] = button.bounds;
        stack.driver.click(x + w / 2.0, y + h / 2.0).await.unwrap();

        // The host says the target is idle; the item says which end everyone is waiting on, and
        // a screen reader is told the same thing.
        let dump = stack
            .driver
            .wait_for("the item to say the window is not drawing", STEP, |d| {
                d.screens.iter().any(|s| s.source == "idle")
            })
            .await
            .unwrap();
        assert_eq!(dump.screens.len(), 1, "{dump:#?}");
        assert_eq!(dump.screens[0].frames, 0, "an off-screen window produced pictures");
        assert!(
            dump.a11y_node("Status", Some("waiting for the window to draw…")).is_some(),
            "{:#?}",
            dump.a11y
        );

        // What the host was asked for over the whole time: a handful of refreshes, then silence.
        // The cap is the receiver's, and this is the only place it can be seen from outside.
        tokio::time::sleep(Duration::from_secs(3)).await;
        let screens = stack.host_screens().await.unwrap();
        let refreshes: u64 =
            screens.iter().filter_map(|s| s.get("stats")?.get("refreshes")?.as_u64()).sum();
        let cap = refresh_cap();
        assert!(
            refreshes <= cap,
            "{refreshes} refresh requests for a window that cannot answer one (cap {cap})"
        );

        // Drawing again is enough: no refresh, no reopen, the picture just starts.
        idle.show().unwrap();
        let dump = stack
            .driver
            .wait_for("the picture once the window draws", STEP, |d| {
                d.screens.iter().any(|s| s.source == "live" && s.frames > 0)
            })
            .await
            .unwrap();
        assert!(
            dump.a11y_node("Status", Some("waiting for the window to draw…")).is_none(),
            "{:#?}",
            dump.a11y
        );
        stack.shutdown().await;
    }

    /// A scrolling terminal as the capture target under injected loss: the app floods a shell so
    /// its window is busy, captures the display that window fills, and the stream comes back over
    /// loopback with a fixed fraction of its datagrams dropped on the app's receive path
    /// (`SLOPTY_E2E_DROP_PERMILLE`, one app process per rate). This is the run the hostd loss
    /// test could not drive (it captures the still desktop it cannot change); here the app self
    /// -test drives the content. Recovery is read from the client's own `ScreenStats`, surfaced
    /// in `dump.screens[].recovery`. Needs the screen-recording grant.
    #[tokio::test]
    async fn a_scrolling_window_recovers_from_injected_loss() {
        if !gated_on_capture() {
            return;
        }
        // A shell that prints as fast as it can, so the captured window changes every frame.
        let flood = "i=0; while :; do printf '%06d the quick brown fox jumps over the lazy dog\\n' \"$i\"; i=$((i+1)); done";
        let mut rows = Vec::new();
        for permille in [0_u32, 20, 50, 100] {
            let value = permille.to_string();
            let mut stack = Stack::launch_with("e2e-loss", &[("SLOPTY_E2E_DROP_PERMILLE", &value)])
                .await
                .unwrap();
            stack.driver.ok(&Command::Resize { width: 1200.0, height: 800.0 }).await.unwrap();
            stack
                .driver
                .wait_for("the first shell", STEP, |d| {
                    d.status == "connected" && d.item("terminal").is_some()
                })
                .await
                .unwrap();
            // Flood a shell so the app's window is a scrolling terminal, then capture the
            // display it fills. The picker does not offer the app its own window, so the target
            // is the display the flooding window sits on — the app self-test still drives the
            // content (a shell printing as fast as it can) the hostd loss test could not.
            stack.driver.open(&["/bin/sh", "-c", flood], 1).await.unwrap();
            stack.driver.add_display().await.unwrap();

            // Let it stream: at least 5 s of frames, as the hostd loss table samples.
            stack
                .driver
                .wait_for("the window streaming", STEP, |d| d.screens.iter().any(|s| s.frames > 5))
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_secs(5)).await;
            let dump = stack.driver.dump().await.unwrap();
            assert_eq!(dump.screens.len(), 1, "one capture: {dump:#?}");
            rows.push((permille, dump.screens[0].clone()));
            stack.shutdown().await;
        }

        // The loss table (client-side recovery counters, one app process per rate).
        println!(
            "\nMEASURE (loss) app self-test: a scrolling window under injected loss (loopback, debug)"
        );
        println!(
            "| drop | frames | by parity | by NACK | lost | datagrams (lost) | kB | parity ‰ | NACK / refresh | stalls | gap p50 ms | audio played / lost / concealed |"
        );
        println!("| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |");
        for (permille, s) in &rows {
            let r = &s.recovery;
            println!(
                "| {permille} ‰ | {} | {} | {} | {} | {} ({}) | {} | {} | {} / {} | {} | {:.1} | {} / {} / {} |",
                r.frames,
                r.frames_fec,
                r.frames_retransmit,
                r.frames_lost,
                r.datagrams,
                r.datagrams_lost,
                r.bytes / 1000,
                r.parity_permille,
                r.nacks,
                r.refreshes,
                r.stalls,
                slopty_e2e::FrameInfo::ms(s.interval_p50_us),
                r.audio_packets,
                r.audio_lost,
                r.audio_concealed,
            );
        }

        // Verdicts: every rate reassembled frames; nothing was lost with no loss injected; loss
        // is recovered (parity or a retransmission repairs frames) once it is injected.
        for (permille, s) in &rows {
            let r = &s.recovery;
            assert!(r.frames >= 1, "{permille} ‰ reassembled nothing: {s:#?}");
            if *permille == 0 {
                assert_eq!(r.frames_lost, 0, "0 ‰ still lost a frame: {s:#?}");
            } else {
                assert!(
                    r.frames_fec + r.frames_retransmit > 0,
                    "{permille} ‰ injected loss repaired nothing: {s:#?}"
                );
            }
        }
    }
}

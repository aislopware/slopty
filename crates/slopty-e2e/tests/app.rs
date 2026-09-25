//! The real app, driven from inside: add a worker, open a shell, type, read the rows back,
//! render frames with the app's own renderer and compare them with the goldens.
//!
//! Runs only with `SLOPTY_APP_E2E=1` (`cargo xtask e2e app`), since it launches the app. Every
//! case but one needs no permission from the machine; the one that captures a window says so and
//! skips without `SLOPTY_SCREEN_E2E`.

#[cfg(test)]
#[path = "app/gallery.rs"]
mod gallery;

#[cfg(test)]
#[path = "app/tiles.rs"]
mod tiles;

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use slopty_e2e::harness::artifacts_dir;
    use slopty_e2e::snapshot::{assert_matches, foreground_fraction, pixels_near};
    use slopty_e2e::{Button, Command, Stack};

    /// How long a worker round trip (open a shell, run a command) may take.
    const STEP: Duration = Duration::from_secs(20);
    /// Window size for the renders: small, so the goldens stay small.
    const WINDOW: (f32, f32) = (900.0, 600.0);
    /// Fraction of pixels allowed to differ from a golden (hinting, RTT readout, cursor).
    const TOLERANCE: f64 = 0.01;
    /// Refresh requests the receiver may send for a target that produces no frame, checked
    /// against the worker's own count of what it was asked for.
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

    /// [`gated`], plus the screen-recording grant the worker needs to capture anything. The rest of
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
        let mut stack = Stack::launch("e2e-worker").await.unwrap();
        let render_path = stack.path("terminal.png");
        let image_path = stack.path("terminal-image.png");
        let terminfo_db = stack.path("terminfo");
        let drv = &mut stack.driver;
        drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();

        // First connect: the worker is up and the app opened one shell in the empty workspace,
        // with the keyboard, and the shell has printed its prompt.
        let dump = drv
            .wait_for("the first shell with a prompt", STEP, |d| {
                d.status == "connected"
                    && d.item("terminal").is_some()
                    && d.focus.as_deref() == Some("terminal")
                    && d.terminals.iter().any(|t| t.rows.iter().any(|r| !r.is_empty()))
            })
            .await
            .unwrap();
        assert_eq!(dump.workers.len(), 1, "{dump:#?}");
        assert_eq!(dump.workers[0].name, "e2e-worker");
        assert_eq!(dump.workspace, "Workspace 1", "{dump:#?}");
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
        assert_eq!(term.worker, "e2e-worker", "{term:?}");
        assert!(term.bounds[2] > 100.0 && term.bounds[3] > 100.0, "{term:?}");
        // What a screen reader gets: the tile's heading, the grid as a terminal whose value is
        // the cursor row, the titlebar's buttons, all inside the window.
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
        for label in ["Open", "More", "Workspace 1, show every workspace"] {
            assert!(dump.a11y_node("Button", Some(label)).is_some(), "{label}: {:#?}", dump.a11y);
        }

        // The prompt's own height, measured before anything is typed: a fresh screen puts the
        // cursor back on this row.
        let prompt_row = dump.terminals[0].cursor[1];

        // Typed text goes client → worker → PTY → shell → worker → client rows.
        drv.type_text("echo e2e-$((6*7))").await.unwrap();
        drv.keys("enter").await.unwrap();
        // The echo, and the prompt back under it (the cursor's row is the grid's a11y value):
        // a frame rendered between the two is a different picture.
        let dump = drv
            .wait_for("the echo and the next prompt", STEP, |d| {
                d.rows_containing("e2e-42").iter().any(|r| r.trim() == "e2e-42")
                    && d.a11y_node("Terminal", None).is_some_and(|g| {
                        g.value
                            .as_deref()
                            .is_some_and(|v| !v.trim().is_empty() && !v.contains("e2e-42"))
                    })
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
        // Panes sit flush on a surface a shade from the title bar's, so what differs from the
        // corner pixel is the text and the chrome: about 1% of this window. Blank is none.
        let fg = foreground_fraction(&frame);
        assert!(fg > 0.004, "frame is blank ({fg:.4} foreground)");
        assert_matches("terminal", &frame, TOLERANCE, &artifacts_dir()).unwrap();

        // A kitty graphics image typed through the shell: 16 × 16 red pixels transmitted and
        // placed at the cursor. The worker lays it out, the pixels cross the wire once, and the
        // app paints a block of red the text never has.
        let red_before = pixels_near(&frame, [255, 0, 0]);
        let apc = concat!(
            r#"printf '\e_Ga=T,f=32,s=16,v=16;%s\e\\' "#,
            r#""$(printf '\377\0\0\377%.0s' {1..256} | base64 | tr -d '\n')""#,
        );
        drv.type_text(apc).await.unwrap();
        drv.keys("enter").await.unwrap();
        drv.wait_for("the image to be placed", STEP, |d| d.terminals[0].images == 1).await.unwrap();
        let with_image = drv.render(&image_path).await.unwrap();
        let red = pixels_near(&with_image, [255, 0, 0]);
        assert!(red >= red_before + 200, "red block of 256 pixels expected: {red_before} → {red}");

        // ⌘K: the worker erases the history and the shell repaints its prompt at the top, so
        // the echo is gone and the cursor is back where a fresh prompt puts it.
        drv.keys("cmd-k").await.unwrap();
        drv.wait_for("the screen to clear", STEP, |d| {
            d.rows_containing("e2e-42").is_empty()
                && d.terminals[0].cursor[1] == prompt_row
                && d.terminals[0].rows.iter().any(|r| !r.trim().is_empty())
        })
        .await
        .unwrap();

        // A long command left running in this shell while the human moves on: the badge on its
        // title bar says it finished, read from the shell integration marks ptyd injected.
        drv.type_text("sleep 6").await.unwrap();
        drv.keys("enter").await.unwrap();

        // ⌘N opens a second shell beside the first and takes the keyboard.
        drv.keys("cmd-n").await.unwrap();
        let dump = drv
            .wait_for("a second shell", STEP, |d| {
                d.items.len() == 2 && d.items.iter().any(|i| i.id != term.id && i.active)
            })
            .await
            .unwrap();
        assert!(!dump.items.iter().any(|i| i.id == term.id && i.active), "{dump:#?}");
        let finished = |d: &slopty_e2e::Dump| {
            d.a11y.iter().any(|n| {
                n.role == "Button" && n.label.as_deref().is_some_and(|l| l.starts_with("done "))
            })
        };
        drv.wait_for("the sleep to badge its shell", STEP + Duration::from_secs(8), finished)
            .await
            .unwrap();

        // The new shell opened in a column right of the first. ⌘⌥← goes back to the first
        // column, whose terminal takes the keyboard; both half-width columns are in view.
        drv.keys("cmd-alt-left").await.unwrap();
        let session = term.session.clone().unwrap();
        let dump = drv
            .wait_for("the first shell focused again", STEP, |d| {
                d.items.iter().any(|i| i.id == term.id && i.active)
                    && d.focused == format!("terminal:{session}")
            })
            .await
            .unwrap();
        assert_eq!(dump.items.len(), 2);
        assert!(
            dump.items.iter().all(|i| {
                let [x, y, w, h] = i.bounds;
                w > 0.0
                    && x >= 0.0
                    && y >= 0.0
                    && x + w <= dump.window.width
                    && y + h <= dump.window.height
            }),
            "both columns in view: {dump:#?}"
        );
        assert!(!finished(&dump), "looking at the shell clears its badge: {:#?}", dump.a11y);
        let second = dump.items.iter().find(|i| i.id != term.id).unwrap().clone();
        assert_eq!(second.pos[1], term.pos[1] + 1, "opened right of the first: {dump:#?}");

        // A click on the second tile focuses it; ⌘1 goes to the first column again.
        let (x, y) = second.center();
        drv.click(x, y).await.unwrap();
        drv.wait_for("the click to focus the second shell", STEP, |d| {
            d.items.iter().any(|i| i.id == second.id && i.active)
        })
        .await
        .unwrap();
        drv.keys("cmd-1").await.unwrap();
        let dump = drv
            .wait_for("⌘1 on the first column", STEP, |d| {
                d.focused == format!("terminal:{session}")
            })
            .await
            .unwrap();

        // A saved settings file reaches the live grid: the app polls `settings.toml` in its
        // data directory, folds it into the theme and the face is measured at the new size
        // (`face.size` is points × display scale, so the ratio is what to check).
        let before = dump.terminals[0].face.as_ref().map_or(0.0, |f| f.size);
        assert!(before > 0.0, "{dump:#?}");
        let settings = stack.dir.path().join("app").join("settings.toml");
        std::fs::write(&settings, "[font]\nmono_size = 20\n").unwrap();
        let dump = drv
            .wait_for("the grid to take the new font size", STEP + Duration::from_secs(2), |d| {
                d.terminals.iter().any(|t| t.face.as_ref().is_some_and(|f| f.size > before * 1.4))
            })
            .await
            .unwrap();
        let after = dump.terminals[0].face.as_ref().map_or(0.0, |f| f.size);
        assert!(((after / before) - 20.0 / 13.0).abs() < 0.02, "{before} → {after}");
        assert_eq!(dump.status, "connected", "{dump:#?}");

        // ⌘, opens the in-app editor on the file; a line typed into it and ⌘↩ write the
        // file (the phone's only way to change a setting), and the dialog goes.
        drv.keys("cmd-,").await.unwrap();
        drv.wait_for("the settings editor", STEP, |d| {
            d.a11y_node("Dialog", Some("Settings")).is_some()
        })
        .await
        .unwrap();
        drv.keys("enter").await.unwrap();
        drv.type_text("[terminal]").await.unwrap();
        drv.keys("enter").await.unwrap();
        drv.type_text("bell_alert = false").await.unwrap();
        drv.keys("cmd-enter").await.unwrap();
        drv.wait_for("the settings editor to close", STEP, |d| {
            d.a11y_node("Dialog", Some("Settings")).is_none()
        })
        .await
        .unwrap();
        let saved = std::fs::read_to_string(&settings).unwrap();
        assert!(
            saved.contains("mono_size = 20") && saved.contains("bell_alert = false"),
            "{saved}"
        );

        // ⌘W closes the active one; the worker tears its session down and one shell remains.
        drv.keys("cmd-w").await.unwrap();
        let dump =
            drv.wait_for("the first shell to close", STEP, |d| d.items.len() == 1).await.unwrap();
        assert!(dump.items.iter().all(|i| i.id != term.id), "{dump:#?}");
        stack.shutdown().await;
    }

    /// A `claude` nobody registered hooks for: "+ agent" starts the fake one the harness put
    /// on ptyd's `PATH`, and the worker attributes it from its foreground process, then its
    /// title, then the transcript it writes — each signal taking over from the weaker one.
    #[tokio::test]
    async fn an_agent_started_without_hooks_is_attributed_from_what_the_worker_can_see() {
        if !gated() {
            return;
        }
        let mut stack = Stack::launch_with_fake_claude("e2e-worker").await.unwrap();
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
        stack
            .driver
            .wait_for("the agent seen from its process alone", STEP, |d| {
                d.terminals.iter().any(|t| {
                    t.agent.as_deref() == Some("idle")
                        && t.agent_source.as_deref() == Some("process")
                })
            })
            .await
            .unwrap();

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

        // It writes its transcript where Claude Code writes one; the worker finds the file
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

    /// The "hooks" pill, clicked the way a human clicks it, ends in the worker writing Claude
    /// Code's settings — in the *harness's* home, which is the only home these daemons have.
    ///
    /// The click goes through the accessibility tree: the pill publishes its bounds there
    /// because it is a button with a label, which is also how a screen reader reaches it.
    #[tokio::test]
    async fn the_hooks_pill_installs_the_relay_in_the_harness_home() {
        if !gated() {
            return;
        }
        let mut stack = Stack::launch_with_fake_claude("e2e-worker").await.unwrap();
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

        // ⌘⇧T starts the fake `claude`; with no hook firing the worker has to guess, which is
        // exactly when the pill is offered.
        stack.driver.keys("cmd-shift-t").await.unwrap();
        let dump = stack
            .driver
            .wait_for("the hooks pill on the guessed agent", STEP, |d| {
                d.terminals.iter().any(|t| t.agent_source.as_deref() == Some("process"))
                    && d.a11y_node("Button", Some("Install hooks")).is_some()
            })
            .await
            .unwrap();
        assert!(!dump.hooks_offered, "not offered until it is clicked");

        let [x, y, w, h] =
            dump.a11y_node("Button", Some("Install hooks")).expect("the pill").bounds;
        stack
            .driver
            .ok(&Command::Click { x: x + w / 2.0, y: y + h / 2.0, button: Button::Left, count: 1 })
            .await
            .unwrap();
        stack
            .driver
            .wait_for("the offer to retire", STEP, |d| {
                d.hooks_offered && d.a11y_node("Button", Some("Install hooks")).is_none()
            })
            .await
            .unwrap();

        // The worker writes the file; give it the same grace the driver gives the app.
        let deadline = std::time::Instant::now() + STEP;
        while !settings.exists() && std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(settings.exists(), "the worker wrote {}", settings.display());

        let registered = slopty_agent::hooks::registered(&settings).unwrap();
        assert_eq!(
            registered.len(),
            slopty_agent::HOOK_EVENTS.len(),
            "every event relays: {registered:?}"
        );
        let written = std::fs::read_to_string(&settings).unwrap();
        assert!(written.contains("slopty"), "the relay is the slopty beside the worker: {written}");
        assert!(written.contains("hook"), "and it is the hook subcommand: {written}");

        // Installing the same relay again over what the daemon wrote changes nothing. (The
        // pill itself retires after one click, so a second *click* is not reachable through
        // the UI.) The command has to be the daemon's own — `install` repoints a relay that
        // moved — so read it back out of the file the daemon wrote.
        let relay = relay_command(&written);
        assert!(relay.ends_with("slopty"), "the relay beside the worker: {relay}");
        let outcome = slopty_agent::hooks::install_at(&settings, &relay).unwrap();
        assert_eq!(outcome, slopty_agent::hooks::Outcome::Unchanged);
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), written, "byte for byte");

        stack.fake_claude_stage("quit").unwrap();
        stack.shutdown().await;
    }

    #[tokio::test]
    async fn notes_and_the_width_keys_change_the_workspace_as_dumped() {
        if !gated() {
            return;
        }
        let mut stack = Stack::launch("e2e-worker").await.unwrap();
        let render_path = stack.path("note.png");
        let drv = &mut stack.driver;
        drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();

        // The note comes from the command palette: ⌘⇧P, "new note", ↩ — the same action the
        // shortcut runs, once the palette is gone and the keyboard is back.
        drv.keys("cmd-shift-p").await.unwrap();
        let dump = drv
            .wait_for("the palette", STEP, |d| d.a11y_node("Dialog", Some("Commands")).is_some())
            .await
            .unwrap();
        assert!(
            dump.a11y_node("ListBoxOption", Some("New note ⇧⌘N")).is_some(),
            "the lines carry their keys: {:#?}",
            dump.a11y
        );
        drv.type_text("new note").await.unwrap();
        drv.wait_for("one line", STEP, |d| {
            d.a11y.iter().filter(|n| n.role == "ListBoxOption").count() == 1
        })
        .await
        .unwrap();
        drv.keys("enter").await.unwrap();
        let dump = drv
            .wait_for("a note, drawn", STEP, |d| {
                d.item("note").is_some_and(|n| n.bounds[2] > 0.0)
                    && d.a11y_node("Dialog", Some("Commands")).is_none()
            })
            .await
            .unwrap();
        // The note opened in a column right of the shell and has the focus.
        let note = dump.item("note").unwrap().clone();
        let shell = dump.item("terminal").unwrap().clone();
        assert!(note.active, "{dump:#?}");
        assert_eq!(note.pos[1], shell.pos[1] + 1, "{dump:#?}");
        let before = note.bounds;

        // ⌘R: the next preset width (two thirds); ⌘⇧R back to half.
        drv.keys("cmd-r").await.unwrap();
        let dump = drv
            .wait_for("a wider note", STEP, |d| {
                d.item("note").is_some_and(|n| n.bounds[2] > before[2] + 50.0)
            })
            .await
            .unwrap();
        let after = dump.item("note").unwrap().bounds;
        assert!((after[3] - before[3]).abs() < 1.0, "only the width moved: {before:?} → {after:?}");
        drv.keys("cmd-shift-r").await.unwrap();
        let dump = drv
            .wait_for("the note back at half", STEP, |d| {
                d.item("note").is_some_and(|n| (n.bounds[2] - before[2]).abs() < 1.0)
            })
            .await
            .unwrap();

        let frame = drv.render(&render_path).await.unwrap();
        assert_matches("note", &frame, TOLERANCE, &artifacts_dir()).unwrap();

        // The shell, a column left of the note, takes a name: ⌘E puts a field in its header,
        // ↩ keeps the name, the heading says it, and the shell has the keyboard back (⌘W
        // below needs it focused).
        let shell = dump.item("terminal").unwrap().id.clone();
        drv.keys("cmd-alt-left").await.unwrap();
        drv.wait_for("the shell focused", STEP, |d| {
            d.items.iter().any(|i| i.id == shell && i.active)
        })
        .await
        .unwrap();
        drv.keys("cmd-e").await.unwrap();
        // (The field's focus does not show in the tree — gpui-kit tracks it on an element
        // without a role — so the name landing is the proof the keys went to it.)
        drv.wait_for("the name field", STEP, |d| {
            d.a11y_node("TextInput", Some("Tile name")).is_some()
        })
        .await
        .unwrap();
        drv.type_text("build box").await.unwrap();
        drv.keys("enter").await.unwrap();
        drv.wait_for("the shell named", STEP, |d| {
            d.a11y_node("Heading", Some("terminal build box")).is_some()
                && d.a11y_node("TextInput", Some("Tile name")).is_none()
        })
        .await
        .unwrap();
        drv.keys("cmd-w").await.unwrap();
        drv.wait_for("the shell to go, the note alone", STEP, |d| {
            d.item("terminal").is_none() && d.items.len() == 1
        })
        .await
        .unwrap();
        stack.shutdown().await;
    }

    /// A remote window whose target never draws: ⌘O picks it while it is still on screen, the
    /// helper takes it away before the stream opens, and the item says so instead of asking the
    /// worker for refreshes no refresh can answer. Then the window draws again and the picture
    /// arrives without the client doing anything.
    #[tokio::test]
    async fn a_remote_window_that_never_draws_waits_instead_of_asking_forever() {
        if !gated_on_capture() {
            return;
        }
        let mut stack = Stack::launch("e2e-worker").await.unwrap();
        stack.driver.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
        let idle = stack.start_idle_window().await.unwrap();
        stack
            .driver
            .wait_for("the first shell", STEP, |d| {
                d.status == "connected" && d.item("terminal").is_some()
            })
            .await
            .unwrap();

        // ⌘O lists the worker's windows. The picker shows on-screen windows only, which is why the
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

        // Off screen before the stream opens: the worker can still capture it, and captures
        // nothing.
        idle.hide().unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        let [x, y, w, h] = button.bounds;
        stack.driver.click(x + w / 2.0, y + h / 2.0).await.unwrap();

        // The worker says the target is idle; the item says which end everyone is waiting on, and
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
            dump.a11y_node("Status", Some("Waiting for the window to draw…")).is_some(),
            "{:#?}",
            dump.a11y
        );

        // What the worker was asked for over the whole time: a handful of refreshes, then silence.
        // The cap is the receiver's, and this is the only place it can be seen from outside.
        tokio::time::sleep(Duration::from_secs(3)).await;
        let screens = stack.worker_screens().await.unwrap();
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
            dump.a11y_node("Status", Some("Waiting for the window to draw…")).is_none(),
            "{:#?}",
            dump.a11y
        );
        stack.shutdown().await;
    }

    /// A scrolling terminal as the capture target under injected loss: the app floods a shell so
    /// its window is busy, captures the display that window fills, and the stream comes back over
    /// loopback with a fixed fraction of its datagrams dropped on the app's receive path
    /// (`SLOPTY_E2E_DROP_PERMILLE`, one app process per rate). This is the run the worker loss
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
            // content (a shell printing as fast as it can) the worker loss test could not.
            stack.driver.open(&["/bin/sh", "-c", flood], 1).await.unwrap();
            stack.driver.add_display().await.unwrap();

            // Let it stream: at least 5 s of frames, as the worker loss table samples.
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

    /// The app launched with only `[client] server` (and the pinned appearance) in its settings:
    /// `slopty-app` from this build with its data in `dir`, its test socket beside it, and a
    /// driver on the socket.
    async fn launch_app_for(
        dir: &std::path::Path,
        server: &str,
    ) -> anyhow::Result<(tokio::process::Child, slopty_e2e::Driver)> {
        std::fs::create_dir_all(dir)?;
        let appearance = slopty_e2e::harness::APPEARANCE;
        std::fs::write(
            dir.join("settings.toml"),
            format!("[client]\nserver = \"{server}\"\n\n[theme]\nappearance = \"{appearance}\"\n"),
        )?;
        let sock = dir.with_extension("sock");
        let log = std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_owned());
        let mut app =
            tokio::process::Command::new(slopty_e2e::harness::bin_dir()?.join("slopty-app"))
                .env("RUST_LOG", log)
                .env("SLOPTY_DATA_DIR", dir)
                .env(slopty_e2e::SOCKET_ENV, &sock)
                .env("SLOPTY_PREDICT", "never")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::inherit())
                .kill_on_drop(true)
                .spawn()?;
        let deadline = tokio::time::Instant::now().checked_add(STEP);
        loop {
            if let Some(status) = app.try_wait()? {
                anyhow::bail!("slopty-app exited early: {status}");
            }
            if let Ok(driver) = slopty_e2e::Driver::connect(&sock).await {
                return Ok((app, driver));
            }
            anyhow::ensure!(
                deadline.is_some_and(|d| tokio::time::Instant::now() < d),
                "no test socket"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// `slopty-server` again on the ports and data directory of the one that was killed.
    fn restart_server(
        server: &slopty_e2e::harness::ServerDaemon,
    ) -> anyhow::Result<tokio::process::Child> {
        let port = server.address().rsplit_once(':').map_or("", |(_, p)| p).to_owned();
        Ok(tokio::process::Command::new(slopty_e2e::harness::bin_dir()?.join("slopty-server"))
            .args(["--port", &port, "--mcp-port", &server.mcp().port().to_string()])
            .arg("--data-dir")
            .arg(server.data_dir())
            .args(["--name", "e2e-server"])
            .env("RUST_LOG", std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_owned()))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true)
            .spawn()?)
    }

    /// Type `echo <tag>-$((6*7))` into the focused shell and wait for `<tag>-42` in its rows.
    async fn echo(drv: &mut slopty_e2e::Driver, tag: &str) {
        drv.type_text(&format!("echo {tag}-$((6*7))")).await.unwrap();
        drv.keys("enter").await.unwrap();
        let want = format!("{tag}-42");
        drv.wait_for(tag, STEP, |d| d.rows_containing(&want).iter().any(|r| r.trim() == want))
            .await
            .unwrap();
    }

    fn server_unreachable(d: &slopty_e2e::Dump) -> bool {
        d.a11y_node("Status", Some("server unreachable")).is_some()
    }

    /// The app finds its worker through the server, opens a terminal on it directly, keeps
    /// typing into it while the server is dead, and links to the server again once it is back.
    #[tokio::test]
    async fn the_app_reaches_its_worker_through_the_server_and_outlives_it() {
        if !gated() {
            return;
        }
        let mut stack = slopty_e2e::harness::ServerStack::launch("e2e-worker").await.unwrap();
        let app_dir = stack.path("app");
        let (mut app, mut drv) = launch_app_for(&app_dir, stack.server.address()).await.unwrap();
        drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();

        // Listed by the server, dialled directly: the first shell on the empty worker.
        let dump = drv
            .wait_for("the worker listed and a shell open on it", STEP, |d| {
                d.workers.iter().any(|w| w.name == "e2e-worker" && w.status == "connected")
                    && d.item("terminal").is_some()
                    && d.terminals.iter().any(|t| t.rows.iter().any(|r| !r.is_empty()))
            })
            .await
            .unwrap();
        assert!(!dump.adding, "no panel with a server set: {dump:#?}");
        assert!(!server_unreachable(&dump), "{:#?}", dump.a11y);
        echo(&mut drv, "direct").await;
        let cache = std::fs::read_to_string(app_dir.join("directory.json")).unwrap_or_default();
        assert!(cache.contains("e2e-worker"), "the directory is cached: {cache}");

        // The server dies: one quiet line says so, and the terminal goes on.
        stack.server.kill().await;
        let dump = drv.wait_for("the server line", STEP, server_unreachable).await.unwrap();
        assert!(!dump.adding, "no modal: {dump:#?}");
        echo(&mut drv, "degraded").await;
        let dump = drv.dump().await.unwrap();
        assert_eq!(dump.workers.len(), 1, "{dump:#?}");
        assert_eq!(dump.workers[0].status, "connected", "{dump:#?}");
        let frame = drv.render(&stack.path("server-unreachable.png")).await.unwrap();
        assert!(foreground_fraction(&frame) > 0.01, "the degraded frame is blank");
        assert_matches("server-unreachable", &frame, TOLERANCE, &artifacts_dir()).unwrap();

        // Back on the same port: the app links again, and the terminal never noticed.
        let mut server = restart_server(&stack.server).unwrap();
        drv.wait_for("the relink", STEP, |d| !server_unreachable(d)).await.unwrap();
        echo(&mut drv, "relinked").await;
        let dump = drv.dump().await.unwrap();
        assert_eq!(dump.workers[0].status, "connected", "{dump:#?}");
        assert_eq!(dump.items.len(), 1, "one tile throughout: {dump:#?}");

        let _killed = app.start_kill();
        let _reaped = app.wait().await;
        let _killed = server.start_kill();
        let _reaped = server.wait().await;
    }

    /// The first shell, prompted and focused, moved into a directory of the run's own (typed,
    /// as the human would): where a drop on it lands.
    pub async fn shell_in(drv: &mut slopty_e2e::Driver, dir: &std::path::Path) -> slopty_e2e::Dump {
        drv.wait_for("the first shell with a prompt", STEP, |d| {
            d.status == "connected"
                && d.focus.as_deref() == Some("terminal")
                && d.terminals.iter().any(|t| t.rows.iter().any(|r| !r.is_empty()))
        })
        .await
        .unwrap();
        drv.type_text(&format!("cd '{}' && pwd", dir.display())).await.unwrap();
        drv.keys("enter").await.unwrap();
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        drv.wait_for("the shell in the drop directory", STEP, |d| {
            d.rows_containing(&name).iter().any(|r| r.trim().ends_with(&*name))
        })
        .await
        .unwrap()
    }

    /// A file dropped on a terminal tile goes up to the shell's directory on the worker, and
    /// its quoted path is typed at the prompt. The drop is GPUI's own file-drop events at the
    /// tile, delivered through the self-test socket; no system drag.
    #[tokio::test]
    async fn a_file_dropped_on_a_shell_lands_on_the_worker_and_its_path_is_typed() {
        if !gated() {
            return;
        }
        let mut stack = Stack::launch("e2e-drop").await.unwrap();
        let work = stack.path("drop-here");
        let outbox = stack.path("outbox");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(&outbox).unwrap();
        let source = outbox.join("report 1.txt");
        let body: Vec<u8> = (0..300_000_u32).map(|i| b'a' + (i % 26) as u8).collect();
        std::fs::write(&source, &body).unwrap();
        let drv = &mut stack.driver;
        let dump = shell_in(drv, &work).await;
        let tile = dump.item("terminal").unwrap().bounds;
        let (x, y) = (tile[0] + tile[2] / 2.0, tile[1] + tile[3] / 2.0);
        // Straight after typing: a file drag must count as the pointer (keyboard modality
        // unhovers every hitbox, and the drop would fall through).
        drv.drop_files(&[source.as_path()], x, y).await.unwrap();

        let dump = drv
            .wait_for("the quoted path typed at the prompt", STEP, |d| {
                !d.rows_containing("drop-here/report 1.txt'").is_empty()
                    && d.terminals[0].upload.is_none()
            })
            .await
            .unwrap();
        let landed = work.join("report 1.txt");
        assert_eq!(std::fs::read(&landed).unwrap(), body, "the file arrives whole");
        assert!(!work.join("report 1.txt.partial").exists(), "renamed into place");
        let screen = dump.terminals[0].rows.concat();
        // The shell's own `$PWD` names the directory, so /var may or may not be /private/var.
        let canonical = std::fs::canonicalize(&landed).unwrap();
        let quoted = [&landed, &canonical].map(|p| format!("'{}'", p.display()));
        assert!(
            quoted.iter().any(|q| screen.contains(q)),
            "its absolute path, quoted for its space: {screen}"
        );
        stack.shutdown().await;
    }

    /// The worker's clipboard reaches the app's while a shell of the worker has the keyboard:
    /// short text at once, a picture as a promise the app keeps by fetching it when something
    /// reads it. Both ends are the run's own named pasteboards; the human's is never touched.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn the_worker_clipboard_arrives_as_text_and_a_picture_fetched_on_paste() {
        use slopty_e2e::harness::pasteboard_name;
        use slopty_platform::pasteboard::{MacPasteboard, Pasteboard as _};

        if !gated() {
            return;
        }
        let mut stack = Stack::launch("e2e-clip").await.unwrap();
        let worker_name = pasteboard_name(stack.dir.path(), "worker");
        let app_name = pasteboard_name(stack.dir.path(), "app");
        let drv = &mut stack.driver;
        drv.wait_for("the worker's clipboard watched", STEP, |d| {
            d.status == "connected"
                && d.focus.as_deref() == Some("terminal")
                && d.workers.iter().any(|w| w.clipboard_watched)
        })
        .await
        .unwrap();
        // A copy the worker makes before it hears the watch is, by design, never announced.
        // The watch went out on the control stream ahead of these keys, and the worker reads
        // that stream in order: once their echo is back, it watches.
        drv.type_text("clipboard-watched").await.unwrap();
        drv.wait_for("the keys behind the watch echoed", STEP, |d| {
            !d.rows_containing("clipboard-watched").is_empty()
        })
        .await
        .unwrap();
        let (worker, app) = (MacPasteboard::named(&worker_name), MacPasteboard::named(&app_name));

        let text = format!("copied on the worker {}", std::process::id());
        worker.copy(&[("public.utf8-plain-text", text.as_bytes())]);
        let deadline = std::time::Instant::now() + STEP;
        while app.text().as_deref() != Some(&*text) && std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(app.text().as_deref(), Some(&*text), "the text arrived inline");

        let picture: Vec<u8> = (0..400_000_u32).map(|i| (i % 251) as u8).collect();
        worker.copy(&[("public.png", &picture)]);
        let deadline = std::time::Instant::now() + STEP;
        while !app.types().iter().any(|t| t == "public.png") && std::time::Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(app.types().iter().any(|t| t == "public.png"), "promised: {:?}", app.types());
        // Reading it is a paste: the app's provider fetches it from the worker, bulk.
        drop(app);
        let reader = app_name.clone();
        let read =
            tokio::task::spawn_blocking(move || MacPasteboard::named(&reader).data("public.png"))
                .await
                .unwrap();
        assert!(read.as_deref() == Some(&*picture), "the picture arrived whole on paste");
        drop(worker);
        stack.shutdown().await;
    }

    /// Files copied here, as Finder copies them, and pasted into a shell with ⌘V go up to the
    /// shell's directory, and their quoted paths are typed: a drop by the keyboard. The copy is
    /// on the run's own named pasteboard; the human's is never touched.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn files_copied_here_and_pasted_into_a_shell_land_there() {
        use slopty_e2e::harness::pasteboard_name;
        use slopty_platform::pasteboard::MacPasteboard;

        if !gated() {
            return;
        }
        let mut stack = Stack::launch("e2e-paste-files").await.unwrap();
        let app_name = pasteboard_name(stack.dir.path(), "app");
        let work = stack.path("paste-here");
        let outbox = stack.path("copied");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(&outbox).unwrap();
        let source = outbox.join("notes 2.txt");
        let body: Vec<u8> = (0..300_000_u32).map(|i| b'a' + (i % 26) as u8).collect();
        std::fs::write(&source, &body).unwrap();
        let drv = &mut stack.driver;
        shell_in(drv, &work).await;
        let app = MacPasteboard::named(&app_name);
        app.copy_files(&[source.as_path()]);
        drv.keys("cmd-v").await.unwrap();

        let dump = drv
            .wait_for("the quoted path typed at the prompt", STEP, |d| {
                !d.rows_containing("paste-here/notes 2.txt'").is_empty()
                    && d.terminals[0].upload.is_none()
            })
            .await
            .unwrap();
        let landed = work.join("notes 2.txt");
        assert_eq!(std::fs::read(&landed).unwrap(), body, "the file arrives whole");
        let screen = dump.terminals[0].rows.concat();
        // The shell's own `$PWD` names the directory, so /var may or may not be /private/var.
        let canonical = std::fs::canonicalize(&landed).unwrap();
        let quoted = [&landed, &canonical].map(|p| format!("'{}'", p.display()));
        assert!(
            quoted.iter().any(|q| screen.contains(q)),
            "its absolute path, quoted for its space: {screen}"
        );
        stack.shutdown().await;
        app.release();
    }

    /// Files copied on the worker, pasted into a shell of that worker, are typed where they
    /// are: the worker announces their URLs, the app remembers whose they are, fetches the URLs
    /// on ⌘V and types the paths. Both pasteboards are the run's own.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn files_copied_on_the_worker_paste_into_its_shell_as_their_paths() {
        use slopty_e2e::harness::pasteboard_name;
        use slopty_platform::pasteboard::{MacPasteboard, Pasteboard as _};

        if !gated() {
            return;
        }
        let mut stack = Stack::launch("e2e-paste-worker-files").await.unwrap();
        let worker_name = pasteboard_name(stack.dir.path(), "worker");
        let app_name = pasteboard_name(stack.dir.path(), "app");
        let copied = stack.path("on-the-worker");
        std::fs::create_dir_all(&copied).unwrap();
        let (a, b) = (copied.join("a b.txt"), copied.join("c.txt"));
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"c").unwrap();
        let drv = &mut stack.driver;
        drv.wait_for("the worker's clipboard watched", STEP, |d| {
            d.status == "connected"
                && d.focus.as_deref() == Some("terminal")
                && d.workers.iter().any(|w| w.clipboard_watched)
        })
        .await
        .unwrap();
        drv.type_text("clipboard-watched").await.unwrap();
        drv.wait_for("the keys behind the watch echoed", STEP, |d| {
            !d.rows_containing("clipboard-watched").is_empty()
        })
        .await
        .unwrap();
        drv.keys("ctrl-u").await.unwrap();
        let (worker, app) = (MacPasteboard::named(&worker_name), MacPasteboard::named(&app_name));
        let before = app.change_count();
        worker.copy_files(&[a.as_path(), b.as_path()]);
        let deadline = std::time::Instant::now() + STEP;
        while app.change_count() == before && std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_ne!(app.change_count(), before, "the worker's copy reached the app");
        drv.keys("cmd-v").await.unwrap();

        let typed = format!("'{}' {}", a.display(), b.display());
        let dump = drv
            .wait_for("the worker's paths typed", STEP, |d| {
                d.terminals[0].rows.concat().contains(&typed)
            })
            .await
            .unwrap();
        assert!(dump.terminals[0].upload.is_none(), "nothing moved");
        stack.shutdown().await;
        worker.release();
        app.release();
    }

    /// ⌘-drag on a worker path in a shell drags the file out as a file promise. The self-test
    /// app parks the promise instead of starting a system drag; keeping it, as a drop into a
    /// directory would ask, brings the whole file down from the worker under its own name.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn a_path_dragged_out_of_a_shell_is_kept_by_a_download() {
        if !gated() {
            return;
        }
        let mut stack = Stack::launch("e2e-drag-out").await.unwrap();
        // Short, so the printed path fits one row.
        let short = tempfile::Builder::new().prefix("slopty-drag-").tempdir_in("/tmp").unwrap();
        let source = short.path().join("out.bin");
        let body: Vec<u8> =
            (0..3_000_000_u32).map(|i| i.wrapping_mul(2_654_435_761).to_le_bytes()[2]).collect();
        std::fs::write(&source, &body).unwrap();
        let into = stack.path("dropped-here");
        std::fs::create_dir_all(&into).unwrap();
        let drv = &mut stack.driver;
        drv.wait_for("the first shell with a prompt", STEP, |d| {
            d.status == "connected"
                && d.focus.as_deref() == Some("terminal")
                && d.terminals.iter().any(|t| t.rows.iter().any(|r| !r.is_empty()))
        })
        .await
        .unwrap();
        let printed = source.display().to_string();
        drv.type_text(&format!("printf '%s\\n' {printed}")).await.unwrap();
        drv.keys("enter").await.unwrap();
        let dump = drv
            .wait_for("the path printed on a row of its own", STEP, |d| {
                d.terminals[0].grid.is_some()
                    && d.terminals[0].rows.iter().any(|r| r.trim() == printed)
            })
            .await
            .unwrap();
        let term = &dump.terminals[0];
        let row = term.rows.iter().position(|r| r.trim() == printed).unwrap();
        let col = term.rows[row].find('/').unwrap() + 3;
        let (x, y) = term.cell_center(col, row).unwrap();
        drv.ok(&Command::Move { x, y }).await.unwrap();
        drv.cmd_drag(x, y, x + 60.0, y + 20.0).await.unwrap();
        drv.keep_dragged(&into).await.unwrap();

        let landed = into.join("out.bin");
        assert_eq!(std::fs::read(&landed).unwrap(), body, "the file arrives whole");
        let left: Vec<_> =
            std::fs::read_dir(&into).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(left, ["out.bin"], "nothing half-arrived is left beside it");
        stack.shutdown().await;
    }

    /// A port a shell listens on is served here: the chip's local port reaches the program on
    /// the worker through a tunnel. With the worker on this very Mac the port is taken here by
    /// the program itself, so the forward moves to the next free one.
    #[tokio::test]
    async fn a_port_listening_in_a_shell_is_reachable_here() {
        use tokio::io::AsyncWriteExt as _;

        if !gated() {
            return;
        }
        let mut stack = Stack::launch("e2e-ports").await.unwrap();
        let drv = &mut stack.driver;
        drv.wait_for("the first shell focused", STEP, |d| {
            d.status == "connected" && d.focus.as_deref() == Some("terminal")
        })
        .await
        .unwrap();
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        drv.type_text(&format!("nc -l {port}")).await.unwrap();
        drv.keys("enter").await.unwrap();
        let dump = drv
            .wait_for("the port forwarded", STEP, |d| {
                d.terminals.iter().any(|t| t.ports.iter().any(|p| p[0] == port && p[1] != 0))
            })
            .await
            .unwrap();
        let local = dump.terminals[0].ports.iter().find(|p| p[0] == port).unwrap()[1];
        assert_ne!(local, port, "nc holds the port on this Mac: the next free one serves");
        let mut client = tokio::net::TcpStream::connect(("127.0.0.1", local)).await.unwrap();
        client.write_all(b"through-the-tunnel\n").await.unwrap();
        drv.wait_for("nc to print what came through", STEP, |d| {
            d.rows_containing("through-the-tunnel").iter().any(|r| r.trim() == "through-the-tunnel")
        })
        .await
        .unwrap();
        drop(client);
        stack.shutdown().await;
    }
}

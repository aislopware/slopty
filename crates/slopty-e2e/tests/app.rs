//! The real app, driven from inside: pair with a host, open a shell, type, read the rows back,
//! render frames with the app's own renderer and compare them with the goldens.
//!
//! Runs only with `SLOPTY_APP_E2E=1` (`cargo xtask e2e app`), since it launches the app. Every
//! case but one needs no permission from the machine; the one that captures a window says so and
//! skips without `SLOPTY_SCREEN_E2E`.

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use slopty_e2e::harness::artifacts_dir;
    use slopty_e2e::snapshot::{assert_matches, foreground_fraction, pixels_near};
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
        let image_path = stack.path("terminal-image.png");
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
        for label in ["shell", "agent", "note", "window", "fit", "Commands"] {
            assert!(dump.a11y_node("Button", Some(label)).is_some(), "{label}: {:#?}", dump.a11y);
        }

        // The prompt's own height, measured before anything is typed: a fresh screen puts the
        // cursor back on this row.
        let prompt_row = dump.terminals[0].cursor[1];

        // Typed text goes client → host → PTY → shell → host → client rows.
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
        let fg = foreground_fraction(&frame);
        assert!(fg > 0.01, "frame is blank ({fg:.4} foreground)");
        assert_matches("terminal", &frame, TOLERANCE, &artifacts_dir()).unwrap();

        // A kitty graphics image typed through the shell: 16 × 16 red pixels transmitted and
        // placed at the cursor. The host lays it out, the pixels cross the wire once, and the
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

        // ⌘K: the host erases the history and the shell repaints its prompt at the top, so
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
        assert!(!finished(&dump), "looking at the shell clears its badge: {:#?}", dump.a11y);

        // ⌘0 brings the grids back and the active shell takes the keyboard again.
        drv.keys("cmd-0").await.unwrap();
        let session = term.session.clone().unwrap();
        let dump = drv
            .wait_for("the first shell to take focus at zoom 1", STEP, |d| {
                (d.zoom - 1.0).abs() < 1e-3 && d.focused == format!("terminal:{session}")
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

        // ⌘W closes the active one; the host tears its session down and one shell remains.
        drv.keys("cmd-w").await.unwrap();
        let dump =
            drv.wait_for("the first shell to close", STEP, |d| d.items.len() == 1).await.unwrap();
        assert!(dump.items.iter().all(|i| i.id != term.id), "{dump:#?}");
        stack.shutdown().await;
    }

    /// A `claude` nobody registered hooks for: "+ agent" starts the fake one the harness put
    /// on ptyd's `PATH`, and the host attributes it from its foreground process, then its
    /// title, then the transcript it writes — each signal taking over from the weaker one.
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

        // It writes its transcript where Claude Code writes one; the host finds the file
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
            .wait_for("a note", STEP, |d| {
                d.item("note").is_some() && d.a11y_node("Dialog", Some("Commands")).is_none()
            })
            .await
            .unwrap();
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
        // First it takes a name: ⌘E puts a field in its title bar, ↩ keeps the name, the
        // heading says it, and the shell has the keyboard back (⌘W below needs it active).
        drv.keys("cmd-e").await.unwrap();
        // (The field's focus does not show in the tree — gpui-kit tracks it on an element
        // without a role — so the name landing is the proof the keys went to it.)
        drv.wait_for("the name field", STEP, |d| {
            d.a11y_node("TextInput", Some("Card name")).is_some()
        })
        .await
        .unwrap();
        drv.type_text("build box").await.unwrap();
        drv.keys("enter").await.unwrap();
        drv.wait_for("the shell named", STEP, |d| {
            d.a11y_node("Heading", Some("terminal build box")).is_some()
                && d.a11y_node("TextInput", Some("Card name")).is_none()
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
            dump.a11y_node("Status", Some("Waiting for the window to draw…")).is_some(),
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
            dump.a11y_node("Status", Some("Waiting for the window to draw…")).is_none(),
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

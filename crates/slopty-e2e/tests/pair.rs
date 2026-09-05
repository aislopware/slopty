//! Two clients on one host, each driven through its own test socket.
//!
//! Runs only with `SLOPTY_PAIR_E2E=1` (`cargo xtask e2e pair`): ptyd, hostd and two app
//! processes on this Mac, the second paired with a ticket of its own. With
//! `SLOPTY_PAIR_IOS_E2E=1` (`cargo xtask e2e pair-ios [--sim iphone|ipad]`) the second client
//! is the app in the simulator, running the subset the phone supports.
//!
//! The behaviours, one test each, in the order of the brief: (a) a terminal opened on A is on B
//! within a round trip with the same title, size and rows; (b) typing on both into one session
//! is serialised, nothing lost or reordered, and the PTY size follows the "take" pill; (c) an
//! agent's attention badges both, an answer on A clears A at once and B when the agent moves on;
//! (d) a display streams to both (needs `SLOPTY_SCREEN_E2E`), and the host's `screens` listing
//! says how many times it encodes; (e) camera and zoom are per client while item geometry is
//! shared; (f) A dying leaves B streaming, and a relaunched A catches up; (g) closing on A closes
//! on B and a note edited on A reads the same on B. Each prints `MEASURE` lines for
//! `docs/MEASUREMENTS.md`.

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use serde_json::json;
    use slopty_e2e::harness::{Pair, Simulator, Stack};
    use slopty_e2e::{Command, Driver, Dump};

    /// How long a host round trip (open a shell, run a command) may take.
    const STEP: Duration = Duration::from_secs(20);
    /// Window size: wide enough for two host-placed terminals side by side at zoom 1, so both
    /// clients can type into the second one without zooming.
    const WINDOW: (f32, f32) = (1600.0, 720.0);
    /// (a) A terminal one client opens is on the other no later than this after it is on the
    /// opener, over loopback.
    const PROPAGATION_LIMIT: Duration = Duration::from_millis(250);
    /// (f) A ceiling on the surviving client's longest pause in the flood. This is well above
    /// the dump-poll granularity (each `dump` is a socket round trip gated on the app's next
    /// frame, ~100-150 ms under load), so it catches a real freeze (the app blocked for
    /// seconds) rather than one missed frame; the `MEASURE` line reports the baseline with the
    /// other client alive beside it.
    const STALL_LIMIT: Duration = Duration::from_millis(600);
    /// (f) How long B's output is watched after A is killed.
    const WATCH: Duration = Duration::from_secs(3);
    /// (d) The median arrival → present on each viewer stays under this (the pacing budget is a
    /// frame or two of the stream's own cadence; see MEASUREMENTS "arrival → present"). A p95
    /// outlier is desktop scheduling jitter and is recorded, not gated.
    const PRESENT_LIMIT: Duration = Duration::from_millis(120);

    /// A shell that prints as fast as it can (the smooth suite's load line).
    const LOAD: &[&str] = &[
        "/bin/sh",
        "-c",
        "i=0; while :; do i=$((i+1)); printf '%06d the quick brown fox jumps over the lazy dog %06x\\n' \"$i\" \"$i\"; done",
    ];
    const FOX: &str = "the quick brown fox";
    /// (b) What each side types, one key at a time, turn and turn about.
    const FROM_A: &str = "abcdefghij";
    const FROM_B: &str = "0123456789";

    fn gated() -> bool {
        if std::env::var_os("SLOPTY_PAIR_E2E").is_none() {
            eprintln!("skipped: set SLOPTY_PAIR_E2E=1 (or run `cargo xtask e2e pair`)");
            return false;
        }
        true
    }

    fn simulator() -> Option<Simulator> {
        if std::env::var_os("SLOPTY_PAIR_IOS_E2E").is_none() {
            eprintln!("skipped: set SLOPTY_PAIR_IOS_E2E=1 (or run `cargo xtask e2e pair-ios`)");
            return None;
        }
        let udid = std::env::var("SLOPTY_SIM_UDID").expect("SLOPTY_SIM_UDID (a booted simulator)");
        let bundle_id = std::env::var("SLOPTY_SIM_BUNDLE_ID").expect("SLOPTY_SIM_BUNDLE_ID");
        Some(Simulator { udid, bundle_id })
    }

    /// Both apps up, both windows the same size, both showing the first shell.
    async fn launch() -> Pair {
        let mut pair = Stack::launch_pair("e2e-pair").await.unwrap();
        let (a, b) = pair.drivers();
        for drv in [&mut *a, &mut *b] {
            drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
        }
        let first = a.dump().await.unwrap();
        let session = first.terminals[0].session.clone();
        b.wait_for("the first shell on B", STEP, |d| {
            d.terminal(&session).is_some_and(|t| t.rows.iter().any(|r| !r.is_empty()))
        })
        .await
        .unwrap();
        pair
    }

    /// Sessions in a dump.
    fn sessions(d: &Dump) -> Vec<String> {
        d.terminals.iter().map(|t| t.session.clone()).collect()
    }

    /// Whether `d` shows a terminal for a session not in `known`, with a prompt in its rows.
    fn new_shell<'d>(d: &'d Dump, known: &[String]) -> Option<&'d str> {
        d.terminals
            .iter()
            .find(|t| !known.contains(&t.session) && t.rows.iter().any(|r| !r.is_empty()))
            .map(|t| t.session.as_str())
    }

    /// Open a shell on `a` and time when each app first shows it with a prompt.
    ///
    /// Returns the new session and how much later than `a` `b` showed it (zero when `b` was
    /// first: the `SessionOpened` broadcast reaches both at once).
    async fn open_on_a(a: &mut Driver, b: &mut Driver) -> (String, Duration) {
        let known = sessions(&a.dump().await.unwrap());
        let start = Instant::now();
        a.keys("cmd-n").await.unwrap();
        let (mut on_a, mut on_b): (Shown, Shown) = (None, None);
        while on_a.is_none() || on_b.is_none() {
            assert!(
                start.elapsed() < STEP,
                "the new shell never showed on both: {on_a:?} {on_b:?}"
            );
            if on_a.is_none() {
                let d = a.dump().await.unwrap();
                if let Some(s) = new_shell(&d, &known) {
                    on_a = Some((Instant::now(), s.to_owned()));
                }
            }
            if on_b.is_none() {
                let d = b.dump().await.unwrap();
                if let Some(s) = new_shell(&d, &known) {
                    on_b = Some((Instant::now(), s.to_owned()));
                }
            }
        }
        let ((at_a, session), (at_b, session_b)) = (on_a.unwrap(), on_b.unwrap());
        assert_eq!(session, session_b, "the same session on both");
        (session, at_b.saturating_duration_since(at_a))
    }

    /// Wait until `session` reads the same on both: title, size, rows and item rect.
    async fn wait_same(a: &mut Driver, b: &mut Driver, session: &str) -> (Dump, Dump) {
        let start = Instant::now();
        loop {
            let (da, db) = (a.dump().await.unwrap(), b.dump().await.unwrap());
            let (ta, tb) = (da.terminal(session), db.terminal(session));
            let (ia, ib) = (da.item_for_session(session), db.item_for_session(session));
            let same = match (ta, tb, ia, ib) {
                (Some(ta), Some(tb), Some(ia), Some(ib)) => {
                    ta.title == tb.title
                        && ta.size == tb.size
                        && ta.rows == tb.rows
                        && same_rect(ia.rect, ib.rect)
                }
                _ => false,
            };
            if same {
                return (da, db);
            }
            assert!(
                start.elapsed() < STEP,
                "{session} never read the same on both:\nA {ta:#?} {ia:#?}\nB {tb:#?} {ib:#?}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// The centre of the first accessibility button labelled `label` inside `bounds`.
    fn button_in(d: &Dump, label: &str, bounds: [f32; 4]) -> Option<(f32, f32)> {
        let [left, top, width, height] = bounds;
        d.a11y
            .iter()
            .filter(|n| n.role == "Button" && n.label.as_deref() == Some(label))
            .find(|n| {
                n.bounds[0] >= left
                    && n.bounds[0] <= left + width
                    && n.bounds[1] >= top
                    && n.bounds[1] <= top + height
            })
            .map(|n| (n.bounds[0] + n.bounds[2] / 2.0, n.bounds[1] + n.bounds[3] / 2.0))
    }

    /// Whether two rects agree to the snap grid's tolerance.
    fn same_rect(x: [f32; 4], y: [f32; 4]) -> bool {
        x.iter().zip(y.iter()).all(|(p, q)| (p - q).abs() < 0.5)
    }

    /// A shown moment, by which app.
    type Shown = Option<(Instant, String)>;

    /// The "N need(s) you" pill's label, if the top bar shows one.
    fn needs_you(d: &Dump) -> Option<String> {
        d.a11y
            .iter()
            .filter(|n| n.role == "Button")
            .filter_map(|n| n.label.clone())
            .find(|l| l.ends_with("needs you") || l.ends_with("need you"))
    }

    /// Click the middle of `session`'s item on `drv` and wait for its grid to hold the keyboard.
    async fn focus_session(drv: &mut Driver, session: &str) {
        let d = drv.dump().await.unwrap();
        let item = d.item_for_session(session).unwrap_or_else(|| panic!("{session} on {d:#?}"));
        let (x, y) = item.center();
        drv.click(x, y).await.unwrap();
        drv.wait_for("the grid to take the keyboard", STEP, |d| {
            d.focused == format!("terminal:{session}")
        })
        .await
        .unwrap();
    }

    /// (a) A terminal opened on A appears on B with the same title, size and rows, within a
    /// round trip; (e) A's zoom and camera are A's alone, an item A moves lands on B where A
    /// put it; (g) a note edited on A reads the same on B, and ⌘W on A takes the item off B.
    #[tokio::test]
    async fn a_terminal_opened_on_one_client_is_on_the_other_and_geometry_is_shared_on_the_mac() {
        if !gated() {
            return;
        }
        let mut pair = launch().await;
        let (a, b) = pair.drivers();

        // (a)
        let (session, lag) = open_on_a(a, b).await;
        let (da, db) = wait_same(a, b, &session).await;
        let (ta, ia) = (da.terminal(&session).unwrap(), da.item_for_session(&session).unwrap());
        println!(
            "MEASURE (a) pair mac: shell opened on A shown on B {:.1} ms after A · title {:?} · {}×{} · rect {:?}",
            lag.as_secs_f64() * 1e3,
            ta.title,
            ta.size[0],
            ta.size[1],
            ia.rect
        );
        assert!(lag <= PROPAGATION_LIMIT, "B lagged A by {lag:?}: {db:#?}");
        assert!(ta.driving, "the opener drives: {ta:#?}");
        assert!(!db.terminal(&session).unwrap().driving, "the other client does not");

        // (e) Zoom is per client.
        a.keys("cmd-=").await.unwrap();
        a.wait_for("A zoomed in", STEP, |d| d.zoom > 1.04).await.unwrap();
        let db = b.dump().await.unwrap();
        assert!((db.zoom - 1.0).abs() < 1e-3, "B's zoom moved with A's: {}", db.zoom);
        a.keys("cmd-0").await.unwrap();
        a.wait_for("A back at 1", STEP, |d| (d.zoom - 1.0).abs() < 1e-3).await.unwrap();

        // (e) Geometry is shared: A drags the new shell's title bar 160 pt to the right. Read
        // the item's window bounds now, after the zoom cycle above (which moves A's camera), so
        // the grab lands on the title bar wherever it sits.
        let here = a.dump().await.unwrap();
        let ia = here.item_for_session(&session).unwrap();
        let before = ia.rect;
        let [left, top, width, _height] = ia.bounds;
        let grab = (left + width / 2.0, top + 10.0);
        a.drag(grab.0, grab.1, grab.0 + 160.0, grab.1).await.unwrap();
        let moved = |d: &Dump| {
            d.item_for_session(&session)
                .is_some_and(|i| (i.rect[0] - before[0] - 160.0).abs() < 17.0)
        };
        let da = a.wait_for("the item moved on A", STEP, moved).await.unwrap();
        let db = b.wait_for("the item moved on B", STEP, moved).await.unwrap();
        let (ra, rb) = (
            da.item_for_session(&session).unwrap().rect,
            db.item_for_session(&session).unwrap().rect,
        );
        assert!(same_rect(ra, rb), "host geometry: A {ra:?} B {rb:?}");

        // (g) A note typed on A is on B once the editor commits it.
        a.keys("cmd-shift-n").await.unwrap();
        a.wait_for("a note on A", STEP, |d| d.item("note").is_some()).await.unwrap();
        a.type_text("shared note").await.unwrap();
        let text = |d: &Dump| d.item("note").and_then(|n| n.note.clone());
        b.wait_for("the note's text on B", STEP, |d| text(d).as_deref() == Some("shared note"))
            .await
            .unwrap();
        let da = a.dump().await.unwrap();
        assert_eq!(text(&da).as_deref(), Some("shared note"));

        // (g) ⌘W on A closes the new shell for both.
        focus_session(a, &session).await;
        a.keys("cmd-w").await.unwrap();
        let gone =
            |d: &Dump| d.item_for_session(&session).is_none() && d.terminal(&session).is_none();
        a.wait_for("the shell closed on A", STEP, gone).await.unwrap();
        b.wait_for("the shell closed on B", STEP, gone).await.unwrap();
        pair.shutdown().await;
    }

    /// (b) Both clients type into one session: every key from each side lands, in its order,
    /// and the take pill moves the PTY size from the opener to the taker.
    #[tokio::test]
    async fn typing_from_both_clients_is_serialised_and_the_take_pill_hands_over_on_the_mac() {
        if !gated() {
            return;
        }
        let mut pair = launch().await;
        let (a, b) = pair.drivers();
        let (session, _lag) = open_on_a(a, b).await;
        wait_same(a, b, &session).await;

        // The opener (A) drives; the other client (B) wears the "take" pill (on the active item
        // only, so both clients make the new shell active first — as the typing step needs too).
        focus_session(b, &session).await;
        focus_session(a, &session).await;
        let (da, db) = (a.dump().await.unwrap(), b.dump().await.unwrap());
        let ib = db.item_for_session(&session).unwrap().bounds;
        println!(
            "MEASURE (b) pair mac: opener drives A {} B {}",
            da.terminal(&session).unwrap().driving,
            db.terminal(&session).unwrap().driving
        );
        assert!(da.terminal(&session).unwrap().driving, "the opener drives: {da:#?}");
        assert!(!db.terminal(&session).unwrap().driving, "the other client does not: {db:#?}");
        assert!(button_in(&db, "take over", ib).is_some(), "B offers take over: {:#?}", db.a11y);

        // A starts a comment so the shell runs nothing.
        a.type_text("# ").await.unwrap();
        for (ca, cb) in FROM_A.chars().zip(FROM_B.chars()) {
            a.type_text(&ca.to_string()).await.unwrap();
            b.type_text(&cb.to_string()).await.unwrap();
        }
        a.keys("enter").await.unwrap();
        let line_with_all = |d: &Dump| -> Option<String> {
            d.terminal(&session)?
                .rows
                .iter()
                .find(|r| {
                    r.contains('#') && FROM_A.chars().chain(FROM_B.chars()).all(|c| r.contains(c))
                })
                .cloned()
        };
        let da = a.wait_for("every key on A", STEP, |d| line_with_all(d).is_some()).await.unwrap();
        let db = b.wait_for("every key on B", STEP, |d| line_with_all(d).is_some()).await.unwrap();
        let line = line_with_all(&da).unwrap();
        assert_eq!(line, line_with_all(&db).unwrap(), "both show the same line");
        let after_hash = line.split_once('#').map_or(line.as_str(), |(_, rest)| rest);
        let from_a: String = after_hash.chars().filter(|c| FROM_A.contains(*c)).collect();
        let from_b: String = after_hash.chars().filter(|c| FROM_B.contains(*c)).collect();
        println!("MEASURE (b) pair mac: interleaved line {after_hash:?}");
        assert_eq!(from_a, FROM_A, "A's keys in order, none lost: {line:?}");
        assert_eq!(from_b, FROM_B, "B's keys in order, none lost: {line:?}");

        // B takes over: B drives, A wears the pill; the PTY keeps one size for both.
        let (tx, ty) = button_in(&db, "take over", ib).unwrap();
        b.click(tx, ty).await.unwrap();
        let db = b
            .wait_for("B to drive", STEP, |d| d.terminal(&session).is_some_and(|t| t.driving))
            .await
            .unwrap();
        let da = a
            .wait_for("A to stop driving", STEP, |d| {
                d.terminal(&session).is_some_and(|t| !t.driving)
                    && d.item_for_session(&session)
                        .is_some_and(|i| button_in(d, "take over", i.bounds).is_some())
            })
            .await
            .unwrap();
        assert_eq!(da.terminal(&session).unwrap().size, db.terminal(&session).unwrap().size);

        // The pill moved the driver, not just its label: prove B now sizes the PTY. Zoom A
        // down to summary cards (below CARD_ZOOM, 0.6) so A stops measuring a grid and can no
        // longer be the client that proposes a size — only B can. A card still tracks the
        // host's size through `Resized`, it just does not ask for one; so if the take-over were
        // cosmetic (B's flag flips but A keeps driving), nobody would size the PTY and it would
        // not change. B grows the terminal by its bottom-right grip and both clients follow.
        let before = db.terminal(&session).unwrap().size;
        for _ in 0..4 {
            if a.dump().await.unwrap().zoom < 0.6 {
                break;
            }
            a.keys("cmd--").await.unwrap();
        }
        a.wait_for("A collapsed to cards", STEP, |d| d.zoom < 0.6).await.unwrap();

        // Grab B's resize grip (14 pt at the bottom-right corner, B at zoom 1) and drag it
        // inward to shrink the terminal, keeping every point of the drag inside the window.
        let [gl, gt, gw, gh] = db.item_for_session(&session).unwrap().bounds;
        let (grip_x, grip_y) = (gl + gw - 7.0, gt + gh - 7.0);
        b.drag(grip_x, grip_y, grip_x - 240.0, grip_y - 120.0).await.unwrap();
        let resized = |d: &Dump| {
            d.terminal(&session).is_some_and(|t| t.size[0] < before[0] && t.size[1] < before[1])
        };
        let db = b.wait_for("B's resize taking hold", STEP, resized).await.unwrap();
        let da = a.wait_for("A (a card) following B's size", STEP, resized).await.unwrap();
        let (sa, sb) = (da.terminal(&session).unwrap().size, db.terminal(&session).unwrap().size);
        println!(
            "MEASURE (b) pair mac: B drove a resize {}×{} → {}×{}; A (a card) follows",
            before[0], before[1], sb[0], sb[1]
        );
        assert_eq!(sa, sb, "both clients show the size B, the new driver, drove");
        pair.shutdown().await;
    }

    /// (c) An agent's attention (a hook played to hostd) badges both clients and both count
    /// one waiting; "allow" on A clears A's count at once, and B's when the agent moves on.
    #[tokio::test]
    async fn an_agents_attention_badges_both_clients_and_an_answer_clears_both_on_the_mac() {
        if !gated() {
            return;
        }
        let mut pair = launch().await;
        let session = pair.stack.driver.dump().await.unwrap().terminals[0].session.clone();
        pair.stack
            .play_hook(&session, "PermissionRequest", r#","tool_name":"Bash""#)
            .await
            .unwrap();
        let (a, b) = pair.drivers();
        let blocked = |d: &Dump| {
            d.terminal(&session)
                .is_some_and(|t| t.agent.as_deref() == Some("blocked:permission:Bash"))
                && needs_you(d).as_deref() == Some("1 needs you")
        };
        let start = Instant::now();
        let da = a.wait_for("the badge on A", STEP, blocked).await.unwrap();
        let db = b.wait_for("the badge on B", STEP, blocked).await.unwrap();
        println!(
            "MEASURE (c) pair mac: permission badge on both within {:.0} ms of the hook",
            start.elapsed().as_secs_f64() * 1e3
        );
        let bounds = da.item_for_session(&session).unwrap().bounds;
        assert!(button_in(&db, "allow", db.item_for_session(&session).unwrap().bounds).is_some());

        // Allow on A: A's own count drops now (the answer is A's, typed into the prompt).
        let (x, y) = button_in(&da, "allow", bounds).unwrap();
        a.click(x, y).await.unwrap();
        a.wait_for("A's count cleared", STEP, |d| needs_you(d).is_none()).await.unwrap();
        // B still counts it: only the host can say the agent moved on.
        let db = b.dump().await.unwrap();
        assert_eq!(needs_you(&db).as_deref(), Some("1 needs you"), "{db:#?}");

        // The agent proceeds (what Claude Code does after Yes): the host says so to both.
        pair.stack.play_hook(&session, "PreToolUse", r#","tool_name":"Bash""#).await.unwrap();
        let (a, b) = pair.drivers();
        let running = |d: &Dump| {
            d.terminal(&session).is_some_and(|t| t.agent.as_deref() == Some("tool:Bash"))
                && needs_you(d).is_none()
        };
        a.wait_for("the agent running on A", STEP, running).await.unwrap();
        b.wait_for("the agent running on B", STEP, running).await.unwrap();
        pair.shutdown().await;
    }

    /// hostd's accumulated CPU time, from `ps` (`MM:SS.cc`).
    fn cpu_time(pid: u32) -> Duration {
        let out = std::process::Command::new("ps")
            .args(["-o", "time=", "-p", &pid.to_string()])
            .output()
            .expect("ps");
        let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        let mut parts = text.rsplit(':');
        let secs: f64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
        let mins: f64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
        let hours: f64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
        Duration::from_secs_f64(mins.mul_add(60.0, hours.mul_add(3600.0, secs)))
    }

    /// hostd's CPU share over `for_` (1.0 = one core).
    async fn cpu_share(pid: u32, for_: Duration) -> f64 {
        let (t0, c0) = (Instant::now(), cpu_time(pid));
        tokio::time::sleep(for_).await;
        cpu_time(pid).saturating_sub(c0).as_secs_f64() / t0.elapsed().as_secs_f64()
    }

    /// (d) A display added on A streams to both; the host's `screens` listing says whether it
    /// encodes once or once per viewer, and hostd's CPU with two viewers against one is the
    /// cost of the answer. Needs Screen Recording for hostd (`SLOPTY_SCREEN_E2E`).
    #[tokio::test]
    async fn a_display_streams_to_both_clients_on_the_mac() {
        if !gated() {
            return;
        }
        if std::env::var_os("SLOPTY_SCREEN_E2E").is_none() {
            eprintln!("skipped: the display scenario needs SLOPTY_SCREEN_E2E=1 (Screen Recording)");
            return;
        }
        let mut pair = launch().await;
        let pid = pair.stack.hostd_pid().unwrap();
        let (a, b) = pair.drivers();
        a.add_display().await.unwrap();
        let streaming = |d: &Dump| {
            d.item("display").is_some()
                && d.screens.iter().any(|s| s.presented > 30 && s.latency_p95_us > 0)
        };
        let da = a.wait_for("the display streaming on A", STEP, streaming).await.unwrap();
        let db = b.wait_for("the display streaming on B", STEP, streaming).await.unwrap();
        for (name, d) in [("A", &da), ("B", &db)] {
            let s = &d.screens[0];
            println!(
                "MEASURE (d) pair mac {name}: {}×{} · presented {} · arrival → present p50 {:.1} / p95 {:.1} ms · skipped {} late {}",
                s.size[0],
                s.size[1],
                s.presented,
                slopty_e2e::FrameInfo::ms(s.latency_p50_us),
                slopty_e2e::FrameInfo::ms(s.latency_p95_us),
                s.skipped,
                s.late
            );
            // The claim is present-on-arrival, a property of the typical frame: gate the median
            // and the pacer's own dropped/late counts. A single p95 outlier is desktop
            // scheduling jitter, not a regression, so it is recorded but not gated.
            assert!(
                Duration::from_micros(s.latency_p50_us) <= PRESENT_LIMIT,
                "{name} does not present on arrival: {s:#?}"
            );
            assert_eq!((s.skipped, s.late), (0, 0), "{name} dropped or held a frame: {s:#?}");
        }

        // What the host does for two viewers of one display.
        let listing = pair.stack.ctl(&json!({ "cmd": "screens" })).await.unwrap();
        let live = listing["live"].as_array().cloned().unwrap_or_default();
        let clients: std::collections::BTreeSet<&str> =
            live.iter().filter_map(|s| s["client"].as_str()).collect();
        let two = cpu_share(pid, Duration::from_secs(4)).await;
        pair.b.shutdown().await;
        tokio::time::sleep(Duration::from_secs(1)).await;
        let one = cpu_share(pid, Duration::from_secs(4)).await;
        println!(
            "MEASURE (d) pair mac: host streams {} for {} client(s) {:?} · hostd CPU {:.2} cores with two viewers, {:.2} with one",
            live.len(),
            clients.len(),
            clients,
            two,
            one
        );
        assert_eq!(clients.len(), 2, "both clients are viewers: {listing}");
        assert!(
            clients.contains(da.client.as_str()) && clients.contains(db.client.as_str()),
            "the listing names both apps: {listing}"
        );
        pair.stack.shutdown().await;
    }

    /// The rows of the terminal showing the flood, if any.
    fn flood_rows(d: &Dump) -> Option<&[String]> {
        d.terminals
            .iter()
            .find(|t| t.rows.iter().any(|r| r.contains(FOX)))
            .map(|t| t.rows.as_slice())
    }

    /// Watch `drv`'s flood for `span`: the longest pause between two changes of its rows.
    ///
    /// The clock starts before the first `dump`, and that first dump counts as a pause: a
    /// client frozen right after the other dies answers it slowly, and that stall must be
    /// measured, not discarded. The trailing gap is never capped at `span` either — a freeze
    /// longer than the whole window has to be reported, not clamped down to it.
    async fn longest_stall(drv: &mut Driver, span: Duration) -> Duration {
        let start = Instant::now();
        let mut last_change = start;
        let mut last_rows = flood_rows(&drv.dump().await.unwrap()).map(<[String]>::to_vec);
        let mut longest = Duration::ZERO;
        while start.elapsed() < span {
            let d = drv.dump().await.unwrap();
            let rows = flood_rows(&d).map(<[String]>::to_vec);
            if rows != last_rows {
                longest = longest.max(last_change.elapsed());
                last_change = Instant::now();
                last_rows = rows;
            }
        }
        longest.max(last_change.elapsed())
    }

    /// (f) A dies without a word; B keeps streaming and typing with no stall; A relaunched on
    /// the same identity reattaches and shows the current rows; when A's dead connection idles
    /// out at the host, neither client loses its viewer.
    #[tokio::test]
    async fn a_client_dying_leaves_the_other_streaming_and_comes_back_caught_up_on_the_mac() {
        if !gated() {
            return;
        }
        let mut pair = launch().await;
        let (a, b) = pair.drivers();
        let shell = a.dump().await.unwrap().terminals[0].session.clone();
        a.open(LOAD, 1).await.unwrap();
        let flooding = |d: &Dump| d.terminals.len() == 2 && flood_rows(d).is_some();
        a.wait_for("the flood on A", STEP, flooding).await.unwrap();
        b.wait_for("the flood on B", STEP, flooding).await.unwrap();
        let quiet = longest_stall(b, Duration::from_secs(1)).await;

        // A dies. B's flood must not pause and B's typing must still echo.
        pair.stack.kill_app().await.unwrap();
        let b = &mut pair.b.driver;
        let stall = longest_stall(b, WATCH).await;
        focus_session(b, &shell).await;
        b.type_text("echo after-a-$((6*7))").await.unwrap();
        b.keys("enter").await.unwrap();
        b.wait_for("B's echo with A dead", STEP, |d| {
            d.rows_containing("after-a-42").iter().any(|r| r.trim() == "after-a-42")
        })
        .await
        .unwrap();
        println!(
            "MEASURE (f) pair mac: B's longest pause in the flood {:.0} ms while A died ({:.0} ms with A alive)",
            stall.as_secs_f64() * 1e3,
            quiet.as_secs_f64() * 1e3
        );
        assert!(stall <= STALL_LIMIT, "B stalled for {stall:?} when A died");

        // A comes back on the same identity: no ticket, the same two items, current rows.
        pair.stack.relaunch_app().await.unwrap();
        let a = &mut pair.stack.driver;
        let da = a
            .wait_for("A caught up", STEP, |d| {
                flooding(d)
                    && d.rows_containing("after-a-42").iter().any(|r| r.trim() == "after-a-42")
            })
            .await
            .unwrap();
        let db = pair.b.driver.dump().await.unwrap();
        let rects = |d: &Dump| {
            let mut r: Vec<(Option<String>, [f32; 4])> =
                d.items.iter().map(|i| (i.session.clone(), i.rect)).collect();
            r.sort_by(|x, y| x.0.cmp(&y.0));
            r
        };
        assert_eq!(rects(&da), rects(&db), "the same canvas on both");
        let stall_a = longest_stall(&mut pair.stack.driver, Duration::from_secs(1)).await;
        assert!(stall_a <= STALL_LIMIT, "A is not streaming after its relaunch: {stall_a:?}");

        // The dead connection idles out at the host (QUIC idle timeout): the host drops it
        // and only it; both live clients keep their viewers.
        let start = Instant::now();
        let clients = loop {
            let doctor = pair.stack.ctl(&json!({ "cmd": "doctor" })).await.unwrap();
            let clients = doctor["clients"].as_u64().unwrap_or(0);
            if clients <= 2 {
                break clients;
            }
            assert!(
                start.elapsed() < Duration::from_secs(70),
                "the dead connection never idled out: {doctor}"
            );
            tokio::time::sleep(Duration::from_millis(500)).await;
        };
        println!(
            "MEASURE (f) pair mac: the dead connection idled out {:.1} s after the relaunch; {clients} clients connected",
            start.elapsed().as_secs_f64()
        );
        let (a, b) = pair.drivers();
        let (stall_a, stall_b) = (
            longest_stall(a, Duration::from_secs(1)).await,
            longest_stall(b, Duration::from_secs(1)).await,
        );
        assert!(
            stall_a <= STALL_LIMIT && stall_b <= STALL_LIMIT,
            "a viewer was lost: A {stall_a:?} B {stall_b:?}"
        );
        pair.shutdown().await;
    }

    /// The Mac and the phone on one host: a shell opened on the Mac is on the phone with the
    /// same rows, typing on the phone shows on the Mac, an agent's attention badges both, and
    /// ⌘W on the Mac takes the shell off the phone.
    #[tokio::test]
    async fn the_mac_and_the_phone_share_a_host_with_the_simulator() {
        let Some(simulator) = simulator() else { return };
        let mut pair = Stack::launch_pair_with_simulator("e2e-pair-ios", simulator).await.unwrap();
        pair.stack.driver.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
        let (a, b) = pair.drivers();
        let first = a.dump().await.unwrap().terminals[0].session.clone();
        b.wait_for("the first shell on the phone", STEP, |d| {
            d.terminal(&first).is_some_and(|t| t.rows.iter().any(|r| !r.is_empty()))
        })
        .await
        .unwrap();

        // (a) opened on the Mac, on the phone; the phone reads the same rows and title. The
        // rect may differ: the phone fits an item it drives to its screen, this one it does not.
        let (session, lag) = open_on_a(a, b).await;
        let same_rows = |da: &Dump, db: &Dump| match (da.terminal(&session), db.terminal(&session))
        {
            (Some(ta), Some(tb)) => ta.rows == tb.rows && ta.title == tb.title,
            _ => false,
        };
        let start = Instant::now();
        loop {
            let (da, db) = (a.dump().await.unwrap(), b.dump().await.unwrap());
            if same_rows(&da, &db) {
                break;
            }
            assert!(start.elapsed() < STEP, "rows never agreed:\n{da:#?}\n{db:#?}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        println!(
            "MEASURE (a) pair ios: shell opened on the Mac shown on the phone {:.1} ms after the Mac",
            lag.as_secs_f64() * 1e3
        );
        assert!(lag <= PROPAGATION_LIMIT, "the phone lagged by {lag:?}");

        // (b) typed on the phone into the first shell, read on the Mac. The second shell left
        // the phone fitted to two desktop-sized items at a card zoom, so reveal this one first:
        // that zooms it up to a live grid (a plain click cannot) and the soft keyboard routes
        // to it.
        b.reveal(&first).await.unwrap();
        b.wait_for("the phone's grid to take the keyboard", STEP, |d| {
            d.focused == format!("terminal:{first}")
        })
        .await
        .unwrap();
        b.type_text("echo phone-$((6*7))").await.unwrap();
        b.keys("enter").await.unwrap();
        a.wait_for("the phone's echo on the Mac", STEP, |d| {
            d.rows_containing("phone-42").iter().any(|r| r.trim() == "phone-42")
        })
        .await
        .unwrap();

        // (c) a hook badges both.
        pair.stack.play_hook(&first, "PermissionRequest", r#","tool_name":"Bash""#).await.unwrap();
        let (a, b) = pair.drivers();
        let blocked = |d: &Dump| {
            d.terminal(&first)
                .is_some_and(|t| t.agent.as_deref() == Some("blocked:permission:Bash"))
                && needs_you(d).as_deref() == Some("1 needs you")
        };
        a.wait_for("the badge on the Mac", STEP, blocked).await.unwrap();
        b.wait_for("the badge on the phone", STEP, blocked).await.unwrap();

        // (g) ⌘W on the Mac closes the shell it opened, on both.
        focus_session(a, &session).await;
        a.keys("cmd-w").await.unwrap();
        let gone = |d: &Dump| d.item_for_session(&session).is_none();
        a.wait_for("closed on the Mac", STEP, gone).await.unwrap();
        b.wait_for("closed on the phone", STEP, gone).await.unwrap();
        pair.shutdown().await;
    }

    /// The refresh guard with the phone as the viewer. The idle-window helper is an AppKit
    /// window on the host's Mac (the simulator shares that machine); the phone captures it, and
    /// because it never draws the phone asks the host for refreshes. The host counts what
    /// actually arrived and it stops at the receiver's cap — the same guard the Mac app self
    /// -test proves, now with the phone doing the asking. An idle source sends no frames, so this
    /// needs no video decode on the simulator, only the picker and the refresh loop. Needs
    /// `SLOPTY_SCREEN_E2E` for the capture.
    #[tokio::test]
    async fn the_refresh_guard_holds_with_the_phone_asking_with_the_simulator() {
        let Some(simulator) = simulator() else { return };
        if std::env::var_os("SLOPTY_SCREEN_E2E").is_none() {
            eprintln!("skipped: the guard needs SLOPTY_SCREEN_E2E=1 (Screen Recording)");
            return;
        }
        let mut pair = Stack::launch_pair_with_simulator("e2e-guard-ios", simulator).await.unwrap();
        let idle = pair.stack.start_idle_window().await.unwrap();

        // ⌘O on the phone lists the host's on-screen windows; find the idle window's row.
        let ends = format!(", {}", idle.title());
        let phone = &mut pair.b.driver;
        phone.keys("cmd-o").await.unwrap();
        let dump = phone
            .wait_for("the idle window in the phone's picker", STEP, |d| {
                d.a11y.iter().any(|n| {
                    n.role == "Button" && n.label.as_deref().is_some_and(|l| l.ends_with(&ends))
                })
            })
            .await
            .unwrap();
        let button = dump
            .a11y
            .iter()
            .find(|n| n.role == "Button" && n.label.as_deref().is_some_and(|l| l.ends_with(&ends)))
            .unwrap_or_else(|| panic!("{:#?}", dump.a11y))
            .clone();

        // Off screen before the stream opens: captured, drawing nothing.
        idle.hide().unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        let [x, y, w, h] = button.bounds;
        phone.click(x + w / 2.0, y + h / 2.0).await.unwrap();
        phone
            .wait_for("the phone sees the window is not drawing", STEP, |d| {
                d.screens.iter().any(|s| s.source == "idle")
            })
            .await
            .unwrap();

        // What the host was asked for over the next stretch: a handful, then silence, inside
        // the receiver's cap — read from the host's own count of what arrived.
        tokio::time::sleep(Duration::from_secs(3)).await;
        let screens = pair.stack.host_screens().await.unwrap();
        let refreshes: u64 =
            screens.iter().filter_map(|s| s.get("stats")?.get("refreshes")?.as_u64()).sum();
        let cap = u64::from(slopty_media::Config::default().refresh_max_repeats);
        println!(
            "MEASURE (guard) pair ios: {refreshes} refresh requests from the phone for an idle window, cap {cap}"
        );
        assert!(refreshes <= cap, "the phone kept asking: {refreshes} > cap {cap}");

        // Drawing again flips the host's source hint back to live for the phone's stream (the
        // picture itself needs a decoder this simulator may not have, so only the hint, which
        // the guard turns on, is asserted).
        idle.show().unwrap();
        pair.b
            .driver
            .wait_for("the host calls the window live again", STEP, |d| {
                d.screens.iter().any(|s| s.source == "live")
            })
            .await
            .unwrap();
        pair.shutdown().await;
    }
}

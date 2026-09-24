//! Frame-time budget: the workspace under load, measured from inside the app.
//!
//! Runs only with `SLOPTY_SMOOTH_E2E=1` (`cargo xtask e2e smooth`) or, in the simulator, with
//! `SLOPTY_SMOOTH_IOS_E2E=1` (`cargo xtask e2e smooth-ios --sim ipad`). The app's frame probe
//! (`slopty_ui::frames`, read back through `dump.frames`) times every frame the window draws
//! while the driver scrolls the strip, opens the overview and types over the test socket; each
//! scenario runs for [`RUN`] and prints one `MEASURE` line with the percentiles
//! (`docs/MEASUREMENTS.md` quotes them). Scrolling the strip across twenty streaming shells is
//! the guarded scenario: its p95 draw must stay under [`PAN_P95_LIMIT`] on the Mac.
//!
//! Load is twenty sessions running [`LOAD`] straight from `OpenSession` (nothing is typed into
//! a shell); the display scenario needs Screen Recording for the worker and runs only when
//! `SLOPTY_SCREEN_E2E` is set as well.

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use slopty_e2e::harness::Simulator;
    use slopty_e2e::{Command, Driver, Dump, FrameInfo, Stack};

    /// How long a worker round trip (open twenty shells, first output) may take.
    const STEP: Duration = Duration::from_secs(30);
    /// Each scenario's measured span.
    const RUN: Duration = Duration::from_secs(5);
    /// Window size for the Mac scenarios: a laptop-sized viewport shows two of the twenty
    /// half-width columns.
    const WINDOW: (f32, f32) = (1280.0, 800.0);
    /// Shells in the workspace in the strip and overview scenarios (`SLOPTY_SMOOTH_SHELLS`
    /// overrides it for a scaling run; the guard applies to the default).
    const SHELLS: u32 = 20;

    fn shells() -> u32 {
        std::env::var("SLOPTY_SMOOTH_SHELLS").ok().and_then(|s| s.parse().ok()).unwrap_or(SHELLS)
    }
    /// Shells beside the display stream.
    const SHELLS_WITH_DISPLAY: u32 = 5;
    /// Streaming shells all in view for the view-cache scenario.
    const BUSY: u32 = 6;
    /// Typing rate for the keystroke scenario, characters per second.
    const TYPING_CPS: u64 = 15;
    /// Characters typed.
    const TYPED: usize = 60;
    /// Keys that must have echoed for the run to count (the last one or two may still be in
    /// flight when the loop ends).
    const TYPED_MIN: u64 = TYPED as u64 - 2;
    /// The guard: scrolling the strip across twenty streaming shells keeps its p95 draw under this,
    /// one and a half 60 Hz periods (measured 3.9 ms on the Mac Studio with other sessions
    /// building; see MEASUREMENTS "canvas frame time").
    const PAN_P95_LIMIT: Duration = Duration::from_millis(25);

    /// A shell that prints as fast as it can, one varied line at a time: the worker frames it
    /// at its own rate, every visible row changes on every frame, and nothing is typed.
    const LOAD: &[&str] = &[
        "/bin/sh",
        "-c",
        "i=0; while :; do i=$((i+1)); printf '%06d the quick brown fox jumps over the lazy dog %06x\\n' \"$i\" \"$i\"; done",
    ];

    fn gated(var: &str, hint: &str) -> bool {
        if std::env::var_os(var).is_none() {
            eprintln!("skipped: set {var}=1 (or run `{hint}`)");
            return false;
        }
        true
    }

    fn simulator() -> Option<Simulator> {
        let udid = std::env::var("SLOPTY_SIM_UDID").ok()?;
        let bundle_id = std::env::var("SLOPTY_SIM_BUNDLE_ID").ok()?;
        Some(Simulator { udid, bundle_id })
    }

    fn measure(scenario: &str, frames: &FrameInfo) {
        println!("MEASURE {scenario}: {}", frames.row());
    }

    /// The first shell has printed its prompt.
    async fn ready(drv: &mut Driver) -> Dump {
        drv.wait_for("the first shell with a prompt", STEP, |d| {
            d.status == "connected"
                && d.item("terminal").is_some()
                && d.terminals.iter().any(|t| t.rows.iter().any(|r| !r.is_empty()))
        })
        .await
        .unwrap()
    }

    /// `total` terminals in the workspace, the load ones (all but the first, interactive shell)
    /// showing screens full of output.
    async fn load(drv: &mut Driver, total: u32) -> Dump {
        let have = u32::try_from(drv.dump().await.unwrap().terminals.len()).unwrap();
        drv.open(LOAD, total.saturating_sub(have)).await.unwrap();
        let total = usize::try_from(total).unwrap();
        drv.wait_for("every shell streaming", STEP, |d| {
            let streaming = d
                .terminals
                .iter()
                .filter(|t| t.rows.iter().filter(|r| !r.is_empty()).count() >= 10);
            d.terminals.len() == total && streaming.count() >= total.saturating_sub(1)
        })
        .await
        .unwrap()
    }

    /// The flooding terminals of a dump (the interactive shell shows a prompt, not the fox),
    /// by session, with their rows.
    fn floods(d: &Dump) -> Vec<(&str, &[String])> {
        d.terminals
            .iter()
            .filter(|t| t.rows.iter().any(|r| r.contains("the quick brown fox")))
            .map(|t| (t.session.as_str(), t.rows.as_slice()))
            .collect()
    }

    /// Every flooding terminal's rows moved between `before` and `after`: cheap draws of a
    /// frozen or detached session do not count as smooth.
    fn assert_floods_advanced(before: &Dump, after: &Dump) {
        let (was, now) = (floods(before), floods(after));
        assert!(!was.is_empty(), "no flooding terminal in the dump: {before:#?}");
        assert_eq!(was.len(), now.len(), "a flooding terminal vanished");
        let stalled: Vec<&str> = was
            .iter()
            .filter(|(session, rows)| now.iter().any(|(s, later)| s == session && later == rows))
            .map(|(session, _)| *session)
            .collect();
        assert!(
            stalled.is_empty(),
            "output stalled in {} of {}: {stalled:?}",
            stalled.len(),
            was.len()
        );
    }

    /// How often the strip is stepped: faster than a column's ease settles, so the view is
    /// always moving.
    const STRIP_STEP: Duration = Duration::from_millis(150);
    /// How often the overview is toggled: about as long as its zoom takes.
    const OVERVIEW_STEP: Duration = Duration::from_millis(400);

    fn clock(period: Duration) -> tokio::time::Interval {
        let mut clock = tokio::time::interval(period);
        clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        clock
    }

    /// Step the focus a column at a time (⌘⌥→, and ⌘⌥← back from the last column) every
    /// [`STRIP_STEP`] for `run`, from the first column: the strip scrolls the whole time.
    async fn pan(drv: &mut Driver, run: Duration) -> FrameInfo {
        let d = drv.dump().await.unwrap();
        let columns = d.items.iter().map(|i| i.pos[1]).max().unwrap_or(0);
        drv.frames_reset().await.unwrap();
        let mut clock = clock(STRIP_STEP);
        let start = Instant::now();
        let (mut column, mut right) = (0_usize, true);
        while start.elapsed() < run {
            clock.tick().await;
            if column == columns {
                right = false;
            } else if column == 0 {
                right = true;
            }
            drv.keys(if right { "cmd-alt-right" } else { "cmd-alt-left" }).await.unwrap();
            column = if right { column.saturating_add(1) } else { column.saturating_sub(1) };
        }
        let after = drv.dump().await.unwrap();
        assert_floods_advanced(&d, &after);
        after.frames
    }

    /// Open and close the overview (⌘⌥O) every [`OVERVIEW_STEP`] for `run`.
    async fn overview_cycle(drv: &mut Driver, run: Duration) -> FrameInfo {
        let d = drv.dump().await.unwrap();
        drv.frames_reset().await.unwrap();
        let mut clock = clock(OVERVIEW_STEP);
        let start = Instant::now();
        while start.elapsed() < run {
            clock.tick().await;
            drv.keys("cmd-alt-o").await.unwrap();
        }
        let after = drv.dump().await.unwrap();
        if after.overview {
            drv.keys("cmd-alt-o").await.unwrap();
        }
        assert_floods_advanced(&d, &after);
        after.frames
    }

    /// Type [`TYPED`] letters into the focused shell at [`TYPING_CPS`], then clear the line.
    async fn typing(drv: &mut Driver) -> slopty_e2e::LatencyInfo {
        let period = Duration::from_millis(1000 / TYPING_CPS);
        drv.frames_reset().await.unwrap();
        for i in 0..TYPED {
            let at = Instant::now();
            let ch = char::from(b'a'.saturating_add(u8::try_from(i % 26).unwrap()));
            drv.type_text(&ch.to_string()).await.unwrap();
            tokio::time::sleep(period.saturating_sub(at.elapsed())).await;
        }
        // The last echoes need a frame or two to land.
        let dump = drv
            .wait_for("every key echoed", STEP, |d| {
                d.terminals.iter().any(|t| t.latency.echoed >= TYPED_MIN)
            })
            .await
            .unwrap();
        drv.keys("ctrl-u").await.unwrap();
        let term = dump.terminals.iter().max_by_key(|t| t.latency.echoed).unwrap();
        println!("MEASURE frames while typing: {}", dump.frames.row());
        term.latency
    }

    /// Focus the first column's shell (⌘1).
    async fn focus_first_shell(drv: &mut Driver) {
        drv.keys("cmd-1").await.unwrap();
        drv.wait_for("the first column's shell focused", STEP, |d| {
            d.focused.starts_with("terminal:")
                && d.items.iter().any(|i| i.active && i.pos[1] == 0 && i.kind == "terminal")
        })
        .await
        .unwrap();
    }

    /// The scenarios on one stack: the strip and the overview over `shells` streaming shells
    /// (returns the strip numbers for the guard).
    async fn strip_and_overview(drv: &mut Driver, label: &str, shells: u32) -> FrameInfo {
        ready(drv).await;
        load(drv, shells).await;
        focus_first_shell(drv).await;
        let panned = pan(drv, RUN).await;
        measure(
            &format!("(a) {label}: {shells} streaming shells, strip column to column"),
            &panned,
        );
        let overview = overview_cycle(drv, RUN).await;
        measure(&format!("(b) {label}: {shells} streaming shells, overview in and out"), &overview);
        panned
    }

    #[tokio::test]
    async fn twenty_streaming_shells_scroll_the_strip_within_budget_on_the_mac() {
        if !gated("SLOPTY_SMOOTH_E2E", "cargo xtask e2e smooth") {
            return;
        }
        let mut stack = Stack::launch("e2e-smooth").await.unwrap();
        let drv = &mut stack.driver;
        drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
        let shells = shells();
        let panned = strip_and_overview(drv, "mac", shells).await;
        stack.shutdown().await;

        assert!(panned.frames >= 100, "too few frames to judge: {panned:?}");
        let p95 = Duration::from_micros(panned.draw_p95_us);
        assert!(
            shells != SHELLS || p95 <= PAN_P95_LIMIT,
            "strip p95 draw {p95:?} over the {PAN_P95_LIMIT:?} limit ({})",
            panned.row()
        );
    }

    /// A file tile holding the most a tile carries (2 000 lines) beside five streaming
    /// shells: the tile's rows are a `uniform_list`, so scrolling the strip should cost what
    /// the rows on screen cost, not the file.
    #[tokio::test]
    async fn a_full_file_tile_beside_five_shells_scrolls_on_the_mac() {
        if !gated("SLOPTY_SMOOTH_E2E", "cargo xtask e2e smooth") {
            return;
        }
        let mut stack = Stack::launch("e2e-smooth-file").await.unwrap();
        let file = stack.dir.path().join("big.rs");
        let body: String = (0..2_000)
            .map(|i| {
                format!(
                    "fn line_{i}() -> u32 {{ {i} * 2 + 1 }} // padding to a source-like width\n"
                )
            })
            .collect::<Vec<_>>()
            .concat();
        std::fs::write(&file, body).unwrap();
        let drv = &mut stack.driver;
        drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
        ready(drv).await;
        load(drv, SHELLS_WITH_DISPLAY).await;
        drv.open_file(file.to_str().unwrap(), Some(1_000)).await.unwrap();
        drv.wait_for("the file tile full", STEP, |d| {
            d.item("file").is_some_and(|i| i.file.as_ref().is_some_and(|f| f.lines == 2_000))
        })
        .await
        .unwrap();
        focus_first_shell(drv).await;
        let panned = pan(drv, RUN).await;
        measure(
            "(e) mac: 1 file tile of 2 000 lines + 5 streaming shells, strip column to column",
            &panned,
        );
        let overview = overview_cycle(drv, RUN).await;
        measure(
            "(f) mac: 1 file tile of 2 000 lines + 5 streaming shells, overview in and out",
            &overview,
        );
        stack.shutdown().await;
        assert!(panned.frames >= 100, "too few frames to judge: {panned:?}");
    }

    #[tokio::test]
    async fn a_display_stream_beside_five_shells_pans_on_the_mac() {
        if !gated("SLOPTY_SMOOTH_E2E", "cargo xtask e2e smooth") {
            return;
        }
        if std::env::var_os("SLOPTY_SCREEN_E2E").is_none() {
            eprintln!("skipped: the display scenario needs SLOPTY_SCREEN_E2E=1 (Screen Recording)");
            return;
        }
        let mut stack = Stack::launch("e2e-smooth-display").await.unwrap();
        let drv = &mut stack.driver;
        drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
        ready(drv).await;
        load(drv, SHELLS_WITH_DISPLAY).await;
        drv.add_display().await.unwrap();
        drv.wait_for("the display streaming", STEP, |d| d.screens.iter().any(|s| s.frames >= 10))
            .await
            .unwrap();
        focus_first_shell(drv).await;
        let panned = pan(drv, RUN).await;
        measure("(c) mac: 1 display stream + 5 streaming shells, strip column to column", &panned);
        stack.shutdown().await;
        assert!(panned.frames >= 100, "too few frames to judge: {panned:?}");
    }

    /// A shell that fills its screen once and then sits still.
    const STILL: &[&str] = &["/bin/sh", "-c", "seq 1 60; exec sleep 3600"];

    /// Open the overview and, once its zoom has settled, time the frames drawn over [`RUN`]
    /// while nothing moves. Returns the dumps either side.
    async fn overview_still(drv: &mut Driver) -> (Dump, Dump) {
        drv.keys("cmd-alt-o").await.unwrap();
        drv.wait_for("the overview open", STEP, |d| d.overview).await.unwrap();
        tokio::time::sleep(OVERVIEW_STEP).await;
        let before = drv.dump().await.unwrap();
        drv.frames_reset().await.unwrap();
        tokio::time::sleep(RUN).await;
        (before, drv.dump().await.unwrap())
    }

    /// Six shells streaming in view at once (the overview) while nothing moves, then typing
    /// into a seventh with the six streaming beside it; then one streaming shell beside five
    /// still ones. A frame should redraw the terminals whose output changed, not every tile,
    /// so these numbers are the view cache's.
    #[tokio::test]
    async fn six_streaming_shells_in_view_on_the_mac() {
        if !gated("SLOPTY_SMOOTH_E2E", "cargo xtask e2e smooth") {
            return;
        }
        let mut stack = Stack::launch("e2e-smooth-busy").await.unwrap();
        let drv = &mut stack.driver;
        drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
        ready(drv).await;
        load(drv, BUSY.saturating_add(1)).await;
        focus_first_shell(drv).await;
        let (before, after) = overview_still(drv).await;
        assert_floods_advanced(&before, &after);
        measure(&format!("(g) mac: {BUSY} streaming shells in the overview, still"), &after.frames);
        drv.keys("cmd-alt-o").await.unwrap();
        drv.wait_for("the overview closed", STEP, |d| !d.overview).await.unwrap();
        focus_first_shell(drv).await;
        let latency = typing(drv).await;
        println!("MEASURE (h) mac: typing beside {BUSY} streaming shells: {}", latency.row());
        stack.shutdown().await;
        assert!(after.frames.frames >= 100, "too few frames to judge: {:?}", after.frames);
        assert!(latency.echoed >= TYPED_MIN, "{latency:?}");

        let mut stack = Stack::launch("e2e-smooth-still").await.unwrap();
        let drv = &mut stack.driver;
        drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
        ready(drv).await;
        // The streaming shell goes next to the first column, where the overview shows it.
        load(drv, 2).await;
        let still = BUSY.saturating_sub(1);
        drv.open(STILL, still).await.unwrap();
        let shells = usize::try_from(still).unwrap().saturating_add(2);
        drv.wait_for("the still shells full", STEP, |d| {
            d.terminals.len() == shells
                && d.terminals.iter().filter(|t| t.rows.iter().any(|r| r == "60")).count()
                    == usize::try_from(still).unwrap()
        })
        .await
        .unwrap();
        focus_first_shell(drv).await;
        let (before, after) = overview_still(drv).await;
        assert_floods_advanced(&before, &after);
        measure(
            &format!("(i) mac: 1 streaming shell beside {still} still ones in the overview"),
            &after.frames,
        );
        stack.shutdown().await;
        assert!(after.frames.frames >= 100, "too few frames to judge: {:?}", after.frames);
    }

    #[tokio::test]
    async fn typing_is_timed_with_and_without_the_local_echo_on_the_mac() {
        if !gated("SLOPTY_SMOOTH_E2E", "cargo xtask e2e smooth") {
            return;
        }
        for policy in ["never", "always"] {
            let mut stack = Stack::launch_with("e2e-smooth-typing", &[("SLOPTY_PREDICT", policy)])
                .await
                .unwrap();
            let drv = &mut stack.driver;
            drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
            ready(drv).await;
            focus_first_shell(drv).await;
            let latency = typing(drv).await;
            println!("MEASURE (d) mac, SLOPTY_PREDICT={policy}: {}", latency.row());
            stack.shutdown().await;
            assert!(latency.echoed >= TYPED_MIN, "{latency:?}");
            if policy == "always" {
                assert!(latency.predicted >= TYPED_MIN, "no predictions drawn: {latency:?}");
            }
        }
    }

    #[tokio::test]
    async fn the_same_scenarios_on_the_simulator() {
        if !gated("SLOPTY_SMOOTH_IOS_E2E", "cargo xtask e2e smooth-ios --sim ipad") {
            return;
        }
        let simulator = simulator().expect("SLOPTY_SIM_UDID and SLOPTY_SIM_BUNDLE_ID");
        // The simulator draws at 60 Hz whatever device it imitates.
        let mut stack = Stack::launch_on_simulator_with(
            "e2e-smooth-ios",
            simulator.clone(),
            &[("SLOPTY_FRAME_HZ", "60")],
        )
        .await
        .unwrap();
        let drv = &mut stack.driver;
        let panned = strip_and_overview(drv, "simulator", shells()).await;
        stack.shutdown().await;
        assert!(panned.frames >= 100, "too few frames to judge: {panned:?}");

        for policy in ["never", "always"] {
            let mut stack = Stack::launch_on_simulator_with(
                "e2e-smooth-ios-typing",
                simulator.clone(),
                &[("SLOPTY_FRAME_HZ", "60"), ("SLOPTY_PREDICT", policy)],
            )
            .await
            .unwrap();
            let drv = &mut stack.driver;
            ready(drv).await;
            focus_first_shell(drv).await;
            let latency = typing(drv).await;
            println!("MEASURE (d) simulator, SLOPTY_PREDICT={policy}: {}", latency.row());
            stack.shutdown().await;
        }
    }
}

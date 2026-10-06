//! Frame-time budget: the workspace under load, measured from inside the app.
//!
//! Every test is live (`#[ignore]`): `cargo xtask e2e smooth` runs the Mac's, and
//! `cargo xtask e2e smooth-ios --sim ipad` the simulator's. The app's frame probe
//! (`slopty_ui::frames`, read back through `dump.frames`) times every frame the window draws
//! while the driver steps a pane from tab to tab, opens the palette and types over the test
//! socket; each scenario runs for [`RUN`] and prints one `MEASURE` line with the percentiles
//! (`docs/MEASUREMENTS.md` quotes them). Stepping a pane through twenty streaming shells is the
//! guarded scenario: its p95 draw must stay under [`PAN_P95_LIMIT`] on the Mac.
//!
//! Load is twenty sessions running [`LOAD`] straight from `OpenSession` (nothing is typed into
//! a shell); the display scenario needs Screen Recording for the worker and runs only with
//! `--screen-recording` as well.

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use slopty_e2e::harness::Simulator;
    use slopty_e2e::{Command, Driver, Dump, FrameInfo, Stack};

    /// How long a worker round trip (open twenty shells, first output) may take.
    const STEP: Duration = Duration::from_secs(30);
    /// The app keeps GPUI's motion, which the self-test otherwise holds still: these runs
    /// measure what moving costs.
    const MOVING: (&str, &str) = ("SLOPTY_E2E_MOTION", "1");
    /// Each scenario's measured span.
    const RUN: Duration = Duration::from_secs(5);
    /// Window size for the Mac scenarios: a laptop-sized viewport, two panes side by side.
    const WINDOW: (f32, f32) = (1280.0, 800.0);
    /// Shells in the project in the pane scenario (`SLOPTY_SMOOTH_SHELLS` overrides it for a
    /// scaling run; the guard applies to the default).
    const SHELLS: u32 = 20;

    fn shells() -> u32 {
        std::env::var("SLOPTY_SMOOTH_SHELLS").ok().and_then(|s| s.parse().ok()).unwrap_or(SHELLS)
    }
    /// Shells beside the display stream.
    const SHELLS_WITH_DISPLAY: u32 = 5;
    /// Streaming shells beside the typed one in the view-cache scenario.
    const BUSY: u32 = 6;
    /// Typing rate for the keystroke scenario, characters per second.
    const TYPING_CPS: u64 = 15;
    /// Characters typed.
    const TYPED: usize = 60;
    /// Keys that must have echoed for the run to count (the last one or two may still be in
    /// flight when the loop ends).
    const TYPED_MIN: u64 = TYPED as u64 - 2;
    /// The guard: stepping a pane through twenty streaming shells keeps its p95 draw under this,
    /// one and a half 60 Hz periods (measured 3.9 ms on the Mac Studio with other sessions
    /// building; see MEASUREMENTS 2026-09-05, frame time under streaming load).
    const PAN_P95_LIMIT: Duration = Duration::from_millis(25);
    /// Round trips the shaped typing scenario runs at: under the tailnet's echo p50 of 10–12 ms,
    /// at it, and a refresh or more past it.
    const SHAPED_RTTS: [Duration; 4] = [
        Duration::from_millis(5),
        Duration::from_millis(10),
        Duration::from_millis(15),
        Duration::from_millis(20),
    ];

    /// A measured round trip from which the adaptive policy must draw guesses on any display:
    /// past half a 60 Hz refresh.
    const ADAPTIVE_FROM_US: u64 = 8_400;

    /// A shell that prints as fast as it can, one varied line at a time: the worker frames it
    /// at its own rate, every visible row changes on every frame, and nothing is typed.
    const LOAD: &[&str] = &[
        "/bin/sh",
        "-c",
        "i=0; while :; do i=$((i+1)); printf '%06d the quick brown fox jumps over the lazy dog %06x\\n' \"$i\" \"$i\"; done",
    ];

    /// The booted simulator `cargo xtask e2e smooth-ios` installed the app on.
    fn simulator() -> Simulator {
        let udid = std::env::var("SLOPTY_SIM_UDID").expect("SLOPTY_SIM_UDID (a booted simulator)");
        let bundle_id = std::env::var("SLOPTY_SIM_BUNDLE_ID").expect("SLOPTY_SIM_BUNDLE_ID");
        Simulator { udid, bundle_id }
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

    /// `total` terminals in the project, the load ones (all but the first, interactive shell)
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

    /// How often a pane is stepped to its next tab: every few frames, so a fresh shell is
    /// drawn the whole time.
    const PANE_STEP: Duration = Duration::from_millis(150);
    /// How often the palette is opened or closed.
    const PALETTE_STEP: Duration = Duration::from_millis(400);

    fn clock(period: Duration) -> tokio::time::Interval {
        let mut clock = tokio::time::interval(period);
        clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        clock
    }

    /// Show the next tab of the pane holding the most shells (⌥⌘]) every [`PANE_STEP`] for
    /// `run`: each step draws another streaming shell in the pane's place.
    async fn pan(drv: &mut Driver, run: Duration) -> FrameInfo {
        let d = drv.dump().await.unwrap();
        let mut panes: std::collections::BTreeMap<slopty_e2e::Place, Vec<&str>> =
            std::collections::BTreeMap::new();
        for item in &d.items {
            if let Some(session) = item.session.as_deref() {
                panes.entry(item.place()).or_default().push(session);
            }
        }
        let fullest = panes.values().max_by_key(|s| s.len()).expect("a pane of shells");
        assert!(fullest.len() > 1, "no pane holds two shells: {d:#?}");
        let session = fullest.first().copied().unwrap_or_default().to_owned();
        drv.reveal(&session).await.unwrap();
        drv.frames_reset().await.unwrap();
        let mut clock = clock(PANE_STEP);
        let start = Instant::now();
        while start.elapsed() < run {
            clock.tick().await;
            drv.keys("cmd-alt-]").await.unwrap();
        }
        let after = drv.dump().await.unwrap();
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

    /// The frames drawn while [`TYPED`] letters are typed at [`TYPING_CPS`] into the focused
    /// shell, once every letter shows in its rows. Unlike [`typing`], nothing here waits on a
    /// presentation report, which a covered window never gets: this is what an echo costs to
    /// draw, not when it reaches the glass.
    async fn typing_frames(drv: &mut Driver) -> FrameInfo {
        let period = Duration::from_millis(1000 / TYPING_CPS);
        let typed: String = (0..TYPED)
            .map(|i| char::from(b'a'.saturating_add(u8::try_from(i % 26).unwrap())))
            .collect();
        drv.frames_reset().await.unwrap();
        for ch in typed.chars() {
            let at = Instant::now();
            drv.type_text(&ch.to_string()).await.unwrap();
            tokio::time::sleep(period.saturating_sub(at.elapsed())).await;
        }
        let dump = drv
            .wait_for("every letter echoed", STEP, |d| {
                d.terminals.iter().any(|t| t.rows.iter().any(|r| r.contains(&typed)))
            })
            .await
            .unwrap();
        drv.keys("ctrl-u").await.unwrap();
        dump.frames
    }

    /// Show the first tab (⌘1), a shell on it holding the keyboard.
    async fn focus_first_shell(drv: &mut Driver) {
        drv.keys("cmd-1").await.unwrap();
        drv.wait_for("a shell on the first tab focused", STEP, |d| {
            d.focused.starts_with("terminal:")
                && d.items.iter().any(|i| i.active && i.tab == 0 && i.kind == "terminal")
        })
        .await
        .unwrap();
    }

    /// The guarded scenario over `shells` streaming shells: a pane stepped from tab to tab.
    async fn through_a_pane(drv: &mut Driver, label: &str, shells: u32) -> FrameInfo {
        ready(drv).await;
        load(drv, shells).await;
        focus_first_shell(drv).await;
        let panned = pan(drv, RUN).await;
        measure(&format!("(a) {label}: {shells} streaming shells, pane tab to tab"), &panned);
        panned
    }

    #[tokio::test]
    #[ignore = "live: cargo xtask e2e smooth"]
    async fn twenty_streaming_shells_step_through_a_pane_within_budget_on_the_mac() {
        let mut stack = Stack::launch_with("e2e-smooth", &[MOVING]).await.unwrap();
        let drv = &mut stack.driver;
        drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
        let shells = shells();
        let panned = through_a_pane(drv, "mac", shells).await;
        stack.shutdown().await;

        assert!(panned.frames >= 100, "too few frames to judge: {panned:?}");
        let p95 = Duration::from_micros(panned.draw_p95_us);
        assert!(
            shells != SHELLS || p95 <= PAN_P95_LIMIT,
            "pane p95 draw {p95:?} over the {PAN_P95_LIMIT:?} limit ({})",
            panned.row()
        );
    }

    /// Scenario (j): one shell, typed into, with nothing else on the screen changing: what a
    /// keystroke's echo costs the UI to draw. Every frame here is the terminal's.
    #[tokio::test]
    #[ignore = "live: cargo xtask e2e smooth"]
    async fn typing_draws_only_the_echo_on_the_mac() {
        let mut stack = Stack::launch_with("e2e-smooth-echo", &[MOVING]).await.unwrap();
        let drv = &mut stack.driver;
        drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
        ready(drv).await;
        focus_first_shell(drv).await;
        let frames = typing_frames(drv).await;
        measure("(j) mac: typing into one shell, its echo drawn", &frames);
        stack.shutdown().await;
        assert!(frames.frames >= TYPED_MIN, "fewer frames than echoes: {frames:?}");
    }

    /// Lines in the file scenario's tile (`SLOPTY_SMOOTH_FILE_LINES` overrides it, for the
    /// 2 000-line baseline and the 200 000-line run).
    const FILE_LINES: usize = 20_000;

    fn file_lines() -> usize {
        std::env::var("SLOPTY_SMOOTH_FILE_LINES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(FILE_LINES)
    }

    /// How often the file scenario pages its editor: about as often as a held key repeats.
    const PAGE_STEP: Duration = Duration::from_millis(60);

    /// Page the focused editor down (`pagedown`) and back up for `run`, every [`PAGE_STEP`];
    /// the caret's line must have moved, so the frames are of a scrolling editor.
    async fn page_through(drv: &mut Driver, run: Duration) -> FrameInfo {
        let line = |d: &Dump| d.item("file").and_then(|i| i.file.as_ref()).and_then(|f| f.line);
        let before = line(&drv.dump().await.unwrap());
        drv.frames_reset().await.unwrap();
        let mut clock = clock(PAGE_STEP);
        let start = Instant::now();
        let (mut step, mut moved) = (0_u32, false);
        while start.elapsed() < run {
            clock.tick().await;
            // Twenty pages one way, then twenty back.
            drv.keys(if step % 40 < 20 { "pageup" } else { "pagedown" }).await.unwrap();
            step = step.saturating_add(1);
            if step == 20 {
                moved = line(&drv.dump().await.unwrap()) != before;
            }
        }
        assert!(moved, "the editor did not scroll");
        drv.dump().await.unwrap().frames
    }

    /// Type [`TYPED`] letters into the focused editor at [`TYPING_CPS`]; the frames drawn while
    /// the tile takes them.
    async fn type_into_file(drv: &mut Driver) -> FrameInfo {
        let period = Duration::from_millis(1000 / TYPING_CPS);
        drv.frames_reset().await.unwrap();
        for i in 0..TYPED {
            let at = Instant::now();
            let ch = char::from(b'a'.saturating_add(u8::try_from(i % 26).unwrap()));
            drv.type_text(&ch.to_string()).await.unwrap();
            tokio::time::sleep(period.saturating_sub(at.elapsed())).await;
        }
        let dump = drv
            .wait_for("the typing in the file", STEP, |d| {
                d.item("file").and_then(|i| i.file.as_ref()).is_some_and(|f| f.edited)
            })
            .await
            .unwrap();
        dump.frames
    }

    /// A file tile beside five streaming shells, [`FILE_LINES`] lines of source by default: the
    /// editor draws the rows on screen, so paging through it, typing into it and stepping a
    /// pane of shells beside it should cost what those rows cost, not the file.
    #[tokio::test]
    #[ignore = "live: cargo xtask e2e smooth"]
    async fn a_large_file_tile_beside_five_shells_scrolls_and_types_on_the_mac() {
        let lines = file_lines();
        let mut stack = Stack::launch_with("e2e-smooth-file", &[MOVING]).await.unwrap();
        // `SLOPTY_SMOOTH_FILE_NAME=big.txt` measures the same text uncoloured.
        let name = std::env::var("SLOPTY_SMOOTH_FILE_NAME").unwrap_or_else(|_| "big.rs".to_owned());
        let file = stack.dir.path().join(name);
        let body: String = (0..lines)
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
        let near_end = u32::try_from(lines.saturating_sub(10)).unwrap();
        drv.open_file(file.to_str().unwrap(), Some(near_end)).await.unwrap();
        drv.wait_for("the file tile full, with the keyboard", STEP, |d| {
            d.item("file").is_some_and(|i| i.file.as_ref().is_some_and(|f| f.lines == lines))
                && d.focused.starts_with("file:")
        })
        .await
        .unwrap();
        let label = format!("1 file tile of {lines} lines + 5 streaming shells");
        let typed = type_into_file(drv).await;
        measure(&format!("(k) mac: {label}, typing into the file"), &typed);
        let paged = page_through(drv, RUN).await;
        measure(&format!("(j) mac: {label}, paging through the file"), &paged);
        focus_first_shell(drv).await;
        let panned = pan(drv, RUN).await;
        measure(&format!("(e) mac: {label}, pane tab to tab"), &panned);
        stack.shutdown().await;
        assert!(panned.frames >= 100, "too few frames to judge: {panned:?}");
    }

    /// A page of text, served on localhost by the test to every request.
    const PAGE: &str = "<!doctype html><html><head><meta charset=utf-8><title>smooth page</title>\
</head><body style=\"margin:0;font:15px -apple-system,sans-serif;padding:24px\">\
<h1>A page beside the shells</h1><p>Lorem ipsum dolor sit amet, consectetur adipiscing elit, \
sed do eiusmod tempor incididunt ut labore et dolore magna aliqua.</p></body></html>";

    /// Serve [`PAGE`] on an ephemeral localhost port until the process ends; the port.
    fn serve_page() -> u16 {
        use std::io::{BufRead as _, BufReader, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(&stream);
                let mut line = String::new();
                while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
                    line.clear();
                }
                let answer = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{PAGE}",
                    PAGE.len()
                );
                let _sent = (&stream).write_all(answer.as_bytes());
            }
        });
        port
    }

    /// Open the palette and close it again, each every [`PALETTE_STEP`], for `run`.
    async fn palette_cycle(drv: &mut Driver, run: Duration) -> FrameInfo {
        drv.frames_reset().await.unwrap();
        let mut clock = clock(PALETTE_STEP);
        let start = Instant::now();
        let mut open = false;
        while start.elapsed() < run {
            clock.tick().await;
            drv.keys(if open { "escape" } else { "cmd-shift-p" }).await.unwrap();
            open = !open;
        }
        let after = drv.dump().await.unwrap();
        if open {
            drv.keys("escape").await.unwrap();
        }
        after.frames
    }

    /// A browser tile beside five streaming shells: the page is a native view the window
    /// composes, so the palette over it and a pane of shells stepped beside it should cost
    /// what the shells cost.
    #[tokio::test]
    #[ignore = "live: cargo xtask e2e smooth"]
    async fn a_page_tile_beside_five_shells_pans_on_the_mac() {
        let port = serve_page();
        let url = format!("http://127.0.0.1:{port}/");
        let mut stack = Stack::launch_with("e2e-smooth-page", &[MOVING]).await.unwrap();
        let drv = &mut stack.driver;
        drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
        ready(drv).await;
        load(drv, SHELLS_WITH_DISPLAY).await;
        drv.ok(&Command::OpenUrl { url: url.clone() }).await.unwrap();
        drv.wait_for("the page, loaded and shown", STEP, |d| {
            d.item("browser")
                .and_then(|i| i.browser.as_ref())
                .is_some_and(|b| b.title == "smooth page" && !b.loading && b.shown && b.snapshot)
        })
        .await
        .unwrap();
        let label = "1 page tile + 5 streaming shells";
        let palette = palette_cycle(drv, RUN).await;
        measure(&format!("(l) mac: {label}, palette in and out over the page"), &palette);
        focus_first_shell(drv).await;
        let panned = pan(drv, RUN).await;
        measure(&format!("(e) mac: {label}, pane tab to tab"), &panned);
        stack.shutdown().await;
        assert!(panned.frames >= 100, "too few frames to judge: {panned:?}");
    }

    /// The scenario that captures a display, which needs the Screen Recording grant
    /// (`cargo xtask e2e smooth --screen-recording`).
    mod screen_recording {
        use super::*;

        #[tokio::test]
        #[ignore = "live: cargo xtask e2e smooth --screen-recording"]
        async fn a_display_stream_beside_five_shells_pans_on_the_mac() {
            let mut stack = Stack::launch_with("e2e-smooth-display", &[MOVING]).await.unwrap();
            let drv = &mut stack.driver;
            drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
            ready(drv).await;
            load(drv, SHELLS_WITH_DISPLAY).await;
            drv.add_display().await.unwrap();
            drv.wait_for("the display streaming", STEP, |d| {
                d.screens.iter().any(|s| s.frames >= 10)
            })
            .await
            .unwrap();
            focus_first_shell(drv).await;
            let panned = pan(drv, RUN).await;
            measure("(c) mac: 1 display stream + 5 streaming shells, pane tab to tab", &panned);
            stack.shutdown().await;
            assert!(panned.frames >= 100, "too few frames to judge: {panned:?}");
        }
    }

    /// The window of the sash scenario, its navigator put away: two panes side by side with
    /// 210 pt each above a pointer's least width to give the sash, the second split below.
    const SASH_WINDOW: (f32, f32) = (1500.0, 1000.0);
    /// Flooding shells beside the sash drag.
    const SASH_FLOODS: u32 = 5;
    /// How often a held sash drag steps: a 120 Hz display's frame.
    const DRAG_STEP: Duration = Duration::from_millis(8);
    /// How far either side of where it was pressed the sash is carried.
    const DRAG_SWING: f32 = 160.0;
    /// Drag steps from one end of the swing to the other.
    const DRAG_LEG: u32 = 60;

    /// The sash between the first two panes side by side in the tab on show: the window point
    /// in the gap between them, halfway down where both stand.
    fn sash(d: &Dump) -> Option<(f32, f32)> {
        let drawn: Vec<_> = d.on_show().filter(|i| i.bounds[2] > 0.0).collect();
        drawn.iter().find_map(|a| {
            let [ax, ay, aw, ah] = a.bounds;
            drawn.iter().find_map(|b| {
                let [bx, by, _, bh] = b.bounds;
                let (top, bottom) = (ay.max(by), (ay + ah).min(by + bh));
                let beside = bx > ax + aw - 1.0 && bx - (ax + aw) < 8.0 && bottom - top > 100.0;
                beside.then(|| (f32::midpoint(ax + aw, bx), f32::midpoint(top, bottom)))
            })
        })
    }

    /// Every resize the terminals asked for so far.
    fn resizes(d: &Dump) -> u64 {
        d.terminals.iter().map(|t| t.resizes).sum()
    }

    /// The most resizes any one terminal asked for between `before` and `after`.
    fn most_resizes(before: &Dump, after: &Dump) -> u64 {
        after
            .terminals
            .iter()
            .map(|t| {
                let was = before.terminal(&t.session).map_or(0, |b| b.resizes);
                t.resizes.saturating_sub(was)
            })
            .max()
            .unwrap_or_default()
    }

    /// Carry the sash at `at` back and forth by [`DRAG_SWING`] for `run`, a step every
    /// [`DRAG_STEP`], then let it go where it was pressed: the frames drawn, and the steps.
    async fn swing(drv: &mut Driver, at: (f32, f32), run: Duration) -> (FrameInfo, u32) {
        let (x, y) = at;
        drv.press(x, y).await.unwrap();
        drv.frames_reset().await.unwrap();
        let mut clock = clock(DRAG_STEP);
        let start = Instant::now();
        let mut step = 0_u32;
        while start.elapsed() < run {
            clock.tick().await;
            // A triangle wave over the swing: out to one side, across, and back.
            let phase = step % (DRAG_LEG * 2);
            let leg = if phase < DRAG_LEG { phase } else { phase.abs_diff(DRAG_LEG * 2) };
            #[expect(clippy::cast_precision_loss, reason = "a step count well under 2^24")]
            let along = (leg as f32 / DRAG_LEG as f32).mul_add(2.0, -1.0);
            drv.drag_to(along.mul_add(DRAG_SWING, x), y).await.unwrap();
            step = step.saturating_add(1);
        }
        let frames = drv.dump().await.unwrap().frames;
        drv.release(x, y).await.unwrap();
        (frames, step)
    }

    /// The panes' own motion: a sash held and carried back and forth for [`RUN`] beside
    /// [`SASH_FLOODS`] flooding shells, the panes on both sides resized at every step. What it
    /// costs a frame, and how many PTY sizes it asks for (one per whole change of a shell's
    /// cell count, `TerminalView::fitted`), go to MEASUREMENTS (2026-10-07, the sash drag).
    #[tokio::test]
    #[ignore = "live: cargo xtask e2e smooth"]
    async fn a_sash_drag_beside_five_flooding_shells_on_the_mac() {
        let mut stack = Stack::launch_with("e2e-smooth-sash", &[MOVING]).await.unwrap();
        let drv = &mut stack.driver;
        drv.ok(&Command::Resize { width: SASH_WINDOW.0, height: SASH_WINDOW.1 }).await.unwrap();
        ready(drv).await;
        drv.keys("cmd-b").await.unwrap();
        let before = load(drv, SASH_FLOODS.saturating_add(1)).await;
        let d = drv
            .wait_for("two panes side by side", STEP, |d| {
                d.panes_on_show() >= 2 && sash(d).is_some()
            })
            .await
            .unwrap();
        let at = sash(&d).expect("a sash");
        let drawn = d.on_show().filter(|i| i.bounds[2] > 0.0).count();
        let asked = resizes(&d);
        let (frames, steps) = swing(drv, at, RUN).await;
        let after = drv.dump().await.unwrap();
        let sent = resizes(&after).saturating_sub(asked);
        let most = most_resizes(&d, &after);
        measure(
            &format!(
                "(m) mac: a sash dragged beside {SASH_FLOODS} flooding shells, {drawn} panes \
                 drawn, {steps} steps, {sent} resizes asked, {most} by the busiest shell"
            ),
            &frames,
        );
        stack.shutdown().await;
        assert_floods_advanced(&before, &after);
        assert!(frames.frames >= 100, "too few frames to judge: {frames:?}");
        assert!(sent > 0, "the drag resized nothing: {after:#?}");
        // A step carries the sash less than a cell, so a shell asks for a size only on the
        // steps that change its whole cell count, never on every one.
        assert!(
            most < u64::from(steps),
            "a resize for every step ({most} for {steps}): sizes must follow whole cells"
        );
    }

    /// Typing into a shell with [`BUSY`] streaming beside it: the view cache's numbers, a frame
    /// redrawing the terminals whose output changed, not every tile.
    #[tokio::test]
    #[ignore = "live: cargo xtask e2e smooth"]
    async fn typing_beside_six_streaming_shells_on_the_mac() {
        let mut stack = Stack::launch_with("e2e-smooth-busy", &[MOVING]).await.unwrap();
        let drv = &mut stack.driver;
        drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
        ready(drv).await;
        load(drv, BUSY.saturating_add(1)).await;
        focus_first_shell(drv).await;
        let latency = typing(drv).await;
        println!("MEASURE (h) mac: typing beside {BUSY} streaming shells: {}", latency.row());
        println!("MEASURE (h) mac hops: {}", latency.hops());
        stack.shutdown().await;
        assert!(latency.echoed >= TYPED_MIN, "{latency:?}");
    }

    #[tokio::test]
    #[ignore = "live: cargo xtask e2e smooth"]
    async fn typing_is_timed_with_and_without_the_local_echo_on_the_mac() {
        for policy in ["never", "always"] {
            let env = [("SLOPTY_PREDICT", policy), MOVING];
            let mut stack = Stack::launch_with("e2e-smooth-typing", &env).await.unwrap();
            let drv = &mut stack.driver;
            drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
            ready(drv).await;
            focus_first_shell(drv).await;
            let latency = typing(drv).await;
            println!("MEASURE (d) mac, SLOPTY_PREDICT={policy}: {}", latency.row());
            println!("MEASURE (d) mac hops, SLOPTY_PREDICT={policy}: {}", latency.hops());
            stack.shutdown().await;
            assert!(latency.echoed >= TYPED_MIN, "{latency:?}");
            if policy == "always" {
                // On loopback the echo can land before the frame the guess would have been on;
                // that frame then shows the echo, which is right. What must never happen is a
                // frame after a key that shows neither its guess nor its echo.
                let answered = latency.predicted.saturating_add(latency.echo_first);
                assert!(
                    answered >= TYPED_MIN,
                    "keys neither guessed nor echoed first: {latency:?}"
                );
                assert!(latency.predicted > 0, "no predictions drawn: {latency:?}");
                assert_eq!(latency.guess_late, 0, "a guess fell behind its frame: {latency:?}");
            }
        }
    }

    /// Scenario (d) over a mesh-like round trip: the relay adds half of each of
    /// [`SHAPED_RTTS`] each way, and the keys are typed with the guesses drawn never, always,
    /// and as the adaptive policy decides. Where a guess lands ahead of the echo, and by how much,
    /// decides the predictor's `SLOW_LINK` (MEASUREMENTS, "the prediction threshold over a
    /// shaped link"). Misses are the app's `prediction miss` lines
    /// (`RUST_LOG=slopty_ui::terminal::view=debug`).
    #[tokio::test]
    #[ignore = "live: cargo xtask e2e smooth"]
    async fn typing_over_a_shaped_round_trip_on_the_mac() {
        for rtt in SHAPED_RTTS {
            let link = slopty_shape::Link { delay: rtt / 2, ..slopty_shape::Link::CLEAR };
            for policy in ["never", "always", "adaptive"] {
                let env = [("SLOPTY_PREDICT", policy), MOVING];
                let (mut stack, _relay) =
                    Stack::launch_shaped("e2e-smooth-shaped", &env, link).await.unwrap();
                let drv = &mut stack.driver;
                drv.ok(&Command::Resize { width: WINDOW.0, height: WINDOW.1 }).await.unwrap();
                ready(drv).await;
                focus_first_shell(drv).await;
                let latency = typing(drv).await;
                // The round trip the predictor was told, as the connection measured it.
                let told = drv.dump().await.unwrap().workers.first().and_then(|w| w.rtt_us);
                let label = format!("(d) mac, rtt {} ms, SLOPTY_PREDICT={policy}", rtt.as_millis());
                println!("MEASURE {label}: {} · link rtt {told:?} µs", latency.row());
                println!("MEASURE {label} hops: {}", latency.hops());
                stack.shutdown().await;
                assert!(latency.echoed >= TYPED_MIN, "{latency:?}");
                if policy == "always" {
                    assert!(latency.predicted > 0, "no predictions drawn: {latency:?}");
                }
                // Past half the slowest refresh (8.3 ms at 60 Hz) the adaptive policy draws
                // every guess after its warm-up; half the keys leaves room for one miss's mute.
                if policy == "adaptive" && told.is_some_and(|us| us >= ADAPTIVE_FROM_US) {
                    assert!(
                        latency.predicted >= TYPED as u64 / 2,
                        "adaptive prediction did not engage at {told:?} µs: {latency:?}"
                    );
                    assert_eq!(latency.guess_late, 0, "a guess fell behind its frame: {latency:?}");
                }
            }
        }
    }

    #[tokio::test]
    #[ignore = "live: cargo xtask e2e smooth-ios"]
    async fn the_same_scenarios_on_the_simulator() {
        let simulator = simulator();
        // The simulator draws at 60 Hz whatever device it imitates.
        let mut stack = Stack::launch_on_simulator_with(
            "e2e-smooth-ios",
            simulator.clone(),
            &[("SLOPTY_FRAME_HZ", "60"), MOVING],
        )
        .await
        .unwrap();
        let drv = &mut stack.driver;
        let panned = through_a_pane(drv, "simulator", shells()).await;
        stack.shutdown().await;
        assert!(panned.frames >= 100, "too few frames to judge: {panned:?}");

        for policy in ["never", "always"] {
            let mut stack = Stack::launch_on_simulator_with(
                "e2e-smooth-ios-typing",
                simulator.clone(),
                &[("SLOPTY_FRAME_HZ", "60"), ("SLOPTY_PREDICT", policy), MOVING],
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

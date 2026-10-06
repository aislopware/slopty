//! The frame's chrome in the headless workspace: what an echo costs beside it.

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use slopty_core::WallMs;

use super::*;

/// One worker with `shells` shells (a directory, a branch and a start each) and `notes` notes,
/// in one snapshot; the sessions it opened, oldest first.
fn crowd(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    fake: &Fake,
    shells: usize,
    notes: usize,
) -> Vec<SessionId> {
    let mut items: Vec<Item> = Vec::new();
    let mut summaries = Vec::new();
    let started = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis();
    for n in 0..shells {
        let session = SessionId::new();
        summaries.push(SessionSummary {
            branch: Some("main".into()),
            changes: None,
            started_ms: WallMs::from_millis(u64::try_from(started).unwrap()),
            ..summary(session, Some(&format!("/Users/me/src/project_{n}")))
        });
        items.push(Item {
            id: ItemId::new(),
            kind: ItemKind::Terminal { session },
            name: None,
            facts: BTreeMap::new(),
        });
    }
    for _ in 0..notes {
        let kind = ItemKind::Folder { path: "/w/notes".into() };
        items.push(Item { id: ItemId::new(), kind, name: None, facts: BTreeMap::new() });
    }
    let sessions = summaries.iter().map(|s| s.id).collect();
    let key = fake.key;
    view.update_in(cx, |v, _window, cx| {
        for s in summaries {
            v.session_opened(key, s, cx);
        }
        v.apply_sync(key, ItemSync::Snapshot { version: 1, items }, cx);
    });
    cx.run_until_parked();
    sessions
}

/// What one frame costs when only a terminal changed (an echo) and when the workspace itself
/// did, over 60 shells and 60 notes with the navigator docked. The last shell is off screen in
/// this crowd, so its echo is a change nothing draws; then a neighbour's echo beside a focused
/// shell full of text. Run by hand (it prints, it does not judge); `docs/MEASUREMENTS.md` has
/// the numbers and the command.
#[gpui::test]
#[ignore = "a measurement, run by hand: see docs/MEASUREMENTS.md"]
fn measure_an_echo_frame_beside_the_chrome(cx: &mut TestAppContext) {
    const FRAMES: usize = 400;
    const WARM: usize = 20;
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let sessions = crowd(&view, cx, &studio, 60, 60);
    let last = *sessions.last().expect("a shell");
    let terminal = view.read_with(cx, |v, _| v.terminal(last).cloned()).expect("attached");
    assert!(navigator_docked(&view, cx));
    let time = |cx: &mut VisualTestContext, echo: bool| {
        let mut took = Vec::with_capacity(FRAMES);
        for n in 0..WARM + FRAMES {
            let start = Instant::now();
            if echo {
                terminal.update(cx, |_, cx| cx.notify());
            } else {
                view.update(cx, |_, cx| cx.notify());
            }
            cx.run_until_parked();
            if n >= WARM {
                took.push(start.elapsed());
            }
        }
        took.sort_unstable();
        let pct = |p: usize| slopty_client::pacing::percentile(&took, p).as_secs_f64() * 1e3;
        (pct(50), pct(95))
    };
    let echo = time(cx, true);
    let own = time(cx, false);
    println!(
        "MEASURE echo frame beside the chrome, 60 shells + 60 notes, {FRAMES} frames: echo p50 \
         {:.3} ms p95 {:.3} ms; workspace p50 {:.3} ms p95 {:.3} ms",
        echo.0, echo.1, own.0, own.1
    );

    // A neighbour's echo while the focused shell holds a dense screen: whether the focused
    // grid is replayed or drawn again with every frame another tile causes.
    let focused_tile = view.read_with(cx, |v, _| v.tile_of_session(last)).expect("its tile");
    let neighbour = *sessions.iter().rev().nth(1).expect("two shells");
    let dense = dense_screen(DENSE_ROWS);
    let dense: Vec<&str> = dense.iter().map(String::as_str).collect();
    view.update_in(cx, |v, _w, cx| {
        v.term_event(last, frame(&dense), cx);
        v.focus_tile(focused_tile, cx);
    });
    cx.run_until_parked();
    assert!(terminal_focused(&view, cx, last), "the dense shell has the keyboard");
    let beside = view.read_with(cx, |v, _| v.terminal(neighbour).cloned()).expect("attached");
    assert!(drawn_at(&view, cx, focused_tile).is_some(), "drawn");
    let mut took = Vec::with_capacity(FRAMES);
    let drawn = terminal.read_with(cx, |t, _| t.renders());
    for n in 0..WARM + FRAMES {
        let start = Instant::now();
        beside.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        if n >= WARM {
            took.push(start.elapsed());
        }
    }
    took.sort_unstable();
    let pct = |p: usize| slopty_client::pacing::percentile(&took, p).as_secs_f64() * 1e3;
    let redrawn = terminal.read_with(cx, |t, _| t.renders()).saturating_sub(drawn);
    println!(
        "MEASURE neighbour echo beside a focused dense grid (80 × {DENSE_ROWS}), {FRAMES} \
         frames: p50 {:.3} ms p95 {:.3} ms; the focused grid rendered {redrawn} times",
        pct(50),
        pct(95)
    );
}

/// Rows in [`dense_screen`].
const DENSE_ROWS: usize = 40;

/// `rows` rows of 80 columns, every cell a word's letter or the space between two.
fn dense_screen(rows: usize) -> Vec<String> {
    const WORDS: [&str; 8] =
        ["cargo", "build", "--release", "target", "slopty", "ok", "0.47", "µs"];
    (0..rows)
        .map(|row| {
            let mut line = String::new();
            for word in WORDS.iter().cycle().skip(row) {
                if line.chars().count().saturating_add(word.chars().count()) >= 80 {
                    break;
                }
                line.push_str(word);
                line.push(' ');
            }
            line
        })
        .collect()
}

/// How many times each region has drawn: the navigator, the title bar.
fn renders(view: &Entity<WorkspaceView>, cx: &VisualTestContext) -> [usize; 2] {
    view.read_with(cx, WorkspaceView::chrome_renders)
}

/// An echo draws its terminal and leaves the navigator and the title bar as they were drawn; a
/// change to the workspace draws them all again.
#[gpui::test]
fn an_echo_leaves_the_chrome_as_it_was_drawn(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let [_, _, (session, _)] = three_shells(&view, cx, &studio);
    assert!(cx.debug_bounds("navigator").is_some() && cx.debug_bounds("titlebar").is_some());
    let before = renders(&view, cx);
    for (seq, typed) in (1..).zip(["$ l", "$ ls", "$ ls -"]) {
        let rows = [(typed, SemanticMark::Prompt { exit: None, input: Some(2) })];
        view.update_in(cx, |v, _w, cx| v.term_event(session, marked_frame(seq, &rows, 0), cx));
        cx.run_until_parked();
    }
    let terminal = view.read_with(cx, |v, _| v.terminal(session).cloned()).expect("attached");
    assert!(terminal.read_with(cx, |t, _| t.state().screen().rows()) > 0, "the echo landed");
    assert_eq!(renders(&view, cx), before, "the chrome was not drawn for it");
    assert!(cx.debug_bounds("navigator").is_some(), "and is still there, cached");

    view.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();
    let after = renders(&view, cx);
    assert!(after.iter().zip(before).all(|(a, b)| *a > b), "{before:?} → {after:?}");
}

/// What one move of the overview, opening or closing, cost the chrome when its frames came
/// `period` apart on the workspace's held clock.
#[derive(Debug)]
struct Move {
    /// The frames of motion drawn after the change's own.
    frames: u32,
    /// How many of them changed which tiles are on screen.
    screens: u32,
    /// How many times the navigator and the title bar drew for the change.
    change: [usize; 2],
    /// How many times they drew in the frames of motion after it.
    motion: [usize; 2],
    /// How many changes the workspace took and how many times it worked out the titles.
    counts: (usize, usize),
}

/// Open the overview, or close it, with the held clock at `clock`, and draw its motion a frame
/// every `period` until it rests.
fn overview_move(
    view: &Entity<WorkspaceView>,
    cx: &mut VisualTestContext,
    clock: &mut Duration,
    period: Duration,
) -> Move {
    let screen = |cx: &mut VisualTestContext| {
        view.read_with(cx, |v, _| {
            let mut items: Vec<ItemId> = v.drawn.on_screen.borrow().iter().copied().collect();
            items.sort_unstable();
            items
        })
    };
    let (chrome, counts) = view.read_with(cx, |v, cx| (v.chrome_renders(cx), v.counts));
    view.update(cx, |v, cx| {
        v.hold_clock(Some(*clock));
        v.tick();
        let open = v.layout.overview_open();
        v.layout.set_overview(!open);
        cx.notify();
    });
    cx.run_until_parked();
    let changed = renders(view, cx);
    let (mut frames, mut screens, mut shown) = (0_u32, 0_u32, screen(cx));
    while view.read_with(cx, |v, _| v.layout.frame().animating) {
        assert!(frames < 10_000, "the move rests");
        *clock = clock.saturating_add(period);
        view.update(cx, |v, _| v.hold_clock(Some(*clock)));
        cx.update(Window::simulate_next_frame);
        cx.run_until_parked();
        frames = frames.saturating_add(1);
        let now = screen(cx);
        if now != shown {
            screens = screens.saturating_add(1);
            shown = now;
        }
    }
    let (after, took) = view.read_with(cx, |v, cx| (v.chrome_renders(cx), v.counts));
    Move {
        frames,
        screens,
        change: [0, 1].map(|i| changed[i].saturating_sub(chrome[i])),
        motion: [0, 1].map(|i| after[i].saturating_sub(changed[i])),
        counts: (took.0.saturating_sub(counts.0), took.1.saturating_sub(counts.1)),
    }
}

/// The overview opening is one change and many frames of motion: the strip draws each frame,
/// and the chrome, the titles and who needs the human are worked out for the change alone. No
/// frame of the motion draws the chrome, even one that changes which tiles are on screen, so
/// what the chrome costs does not depend on how many frames the motion took: opened at 60 Hz
/// and again at 240 Hz, it draws the same.
#[gpui::test]
fn a_frame_of_motion_is_no_news_for_the_chrome(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let _sessions = crowd(&view, cx, &studio, 8, 4);
    view.update(cx, |v, _| v.set_animation(true));
    // From where the layout's clock already stands: setting the workspace up ran on real time
    // (the first symbol drawn in a test binary can take seconds), and a clock held behind it
    // would spend frames catching up before anything moved.
    let mut clock = view.read_with(cx, |v, _| v.now());
    let slow = overview_move(&view, cx, &mut clock, Duration::from_micros(16_667));
    let _closed = overview_move(&view, cx, &mut clock, Duration::from_micros(16_667));
    let fast = overview_move(&view, cx, &mut clock, Duration::from_micros(4_167));
    for opening in [&slow, &fast] {
        assert!(opening.frames > 1 && opening.screens > 0, "it moved the tiles: {opening:?}");
        assert_eq!(opening.counts, (1, 1), "one change, one working out of the titles");
        assert!(opening.change.iter().all(|n| *n > 0), "the change drew it: {opening:?}");
        assert_eq!(opening.motion, [0; 2], "no frame of motion drew it: {opening:?}");
    }
    assert!(fast.frames > slow.frames, "{slow:?} {fast:?}");
    assert_eq!(fast.change, slow.change, "the frames' pace is no news: {slow:?} {fast:?}");
    println!("MEASURE overview opening: at 60 Hz {slow:?}, at 240 Hz {fast:?}");
}

/// What a frame of motion costs beside the chrome: the overview opening and closing over 60
/// shells and 60 notes with the navigator docked, each frame of the moves timed. The springs
/// run on the workspace's clock, held and moved on a 60 Hz frame at a time, so every run draws
/// the same frames however slowly it draws them. Run by hand (it prints, it does not judge);
/// `docs/MEASUREMENTS.md` has the numbers and the command.
#[gpui::test]
#[ignore = "a measurement, run by hand: see docs/MEASUREMENTS.md"]
fn measure_a_frame_of_motion_beside_the_chrome(cx: &mut TestAppContext) {
    const FRAMES: usize = 400;
    const PERIOD: Duration = Duration::from_micros(16_667);
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let _sessions = crowd(&view, cx, &studio, 60, 60);
    assert!(navigator_docked(&view, cx));
    // From where the layout's clock already stands: setting the workspace up ran on real time
    // (the first symbol drawn in a test binary can take seconds), and a clock held behind it
    // would spend frames catching up before anything moved.
    let mut clock = view.read_with(cx, |v, _| v.now());
    view.update(cx, |v, _| {
        v.set_animation(true);
        v.hold_clock(Some(clock));
    });
    let before = renders(&view, cx);
    let mut took = Vec::with_capacity(FRAMES);
    let mut moves = 0_u32;
    while took.len() < FRAMES {
        clock = clock.saturating_add(PERIOD);
        view.update(cx, |v, _| v.hold_clock(Some(clock)));
        if !view.read_with(cx, |v, _| v.layout.frame().animating) {
            view.update(cx, |v, cx| {
                v.tick();
                let open = v.layout.overview_open();
                v.layout.set_overview(!open);
                cx.notify();
            });
            cx.run_until_parked();
            moves = moves.saturating_add(1);
            continue;
        }
        let start = Instant::now();
        cx.update(Window::simulate_next_frame);
        cx.run_until_parked();
        took.push(start.elapsed());
    }
    let after = renders(&view, cx);
    took.sort_unstable();
    let pct = |p: usize| slopty_client::pacing::percentile(&took, p).as_secs_f64() * 1e3;
    println!(
        "MEASURE a frame of motion beside the chrome, 60 shells + 60 notes, {FRAMES} frames over \
         {moves} moves: p50 {:.3} ms p95 {:.3} ms; chrome renders {before:?} -> {after:?}",
        pct(50),
        pct(95)
    );
}

/// A program's title is news for the chrome only when the tile's title follows it: the shell's
/// own name leaves "Terminal 2" as it was, an editor's title renames the tile (and ends the
/// twin's number). A command starting names its shell and renumbers what read alike, though
/// only the navigator is told.
#[gpui::test]
fn a_programs_title_draws_the_chrome_only_when_the_tiles_title_follows(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let (one, two) = (SessionId::new(), SessionId::new());
    let first = opens(&view, cx, &studio, one, studio.me, 1);
    let second = opens(&view, cx, &studio, two, studio.me, 2);
    let titles = |cx: &mut VisualTestContext| {
        view.read_with(cx, |v, _| [first, second].map(|t| v.tile_title(v.item(t).unwrap())))
    };
    assert_eq!(titles(cx), ["Terminal", "Terminal 2"]);
    let before = renders(&view, cx);
    view.update_in(cx, |v, _w, cx| v.term_event(two, TermEvent::Title("zsh".into()), cx));
    cx.run_until_parked();
    assert_eq!(renders(&view, cx), before, "the shell's own name changes no title");
    assert_eq!(titles(cx), ["Terminal", "Terminal 2"]);

    view.update_in(cx, |v, _w, cx| v.term_event(two, TermEvent::Title("vim notes.md".into()), cx));
    cx.run_until_parked();
    let after = renders(&view, cx);
    assert!(after.iter().zip(before).all(|(a, b)| *a > b), "{before:?} → {after:?}");
    assert_eq!(titles(cx), ["Terminal", "vim notes.md"]);

    view.update_in(cx, |v, _w, cx| v.term_event(two, TermEvent::Title("zsh".into()), cx));
    cx.run_until_parked();
    assert_eq!(titles(cx), ["Terminal", "Terminal 2"], "back to reading alike");
    let prompt = SemanticMark::Prompt { exit: None, input: Some(2) };
    let rows = [("$ make", prompt), ("building", SemanticMark::Output)];
    view.update_in(cx, |v, _w, cx| v.term_event(one, marked_frame(1, &rows, 1), cx));
    cx.run_until_parked();
    assert_eq!(
        titles(cx),
        ["make", "Terminal"],
        "the command names the first; the second is alone"
    );
}

/// A command starting in a shell is news for its navigator row, and only for the navigator.
#[gpui::test]
fn a_command_that_starts_draws_the_navigator_alone(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let [_, _, (session, _)] = three_shells(&view, cx, &studio);
    let prompt = SemanticMark::Prompt { exit: None, input: Some(2) };
    let rows = [("$ make", prompt)];
    view.update_in(cx, |v, _w, cx| v.term_event(session, marked_frame(1, &rows, 0), cx));
    cx.run_until_parked();
    let before = renders(&view, cx);
    let rows = [("$ make", prompt), ("building", SemanticMark::Output)];
    view.update_in(cx, |v, _w, cx| v.term_event(session, marked_frame(2, &rows, 1), cx));
    cx.run_until_parked();
    let after = renders(&view, cx);
    assert!(after[0] > before[0], "the navigator draws the new command: {before:?} → {after:?}");
    assert_eq!(after[1..], before[1..], "the bars do not");
    let lines = view.read_with(cx, WorkspaceView::navigator_lines);
    assert!(lines.iter().any(|(title, ..)| title == "make"), "its title: {lines:#?}");
}

/// A key typed into a focused shell holds the working marks' steps for its echo, for the round
/// trip to its worker and a refresh at most; the terminal's next change (the echo) lets them go.
/// A chord for the app holds nothing.
#[gpui::test]
fn a_typed_key_holds_the_working_marks_for_its_echo(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let [_, _, (session, _)] = three_shells(&view, cx, &studio);
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| v.set_rtt(key, Some(Duration::from_millis(30)), cx));
    cx.run_until_parked();
    assert!(terminal_focused(&view, cx, session), "the newest shell has the keyboard");
    let held = |cx: &mut VisualTestContext| cx.update(|_w, cx| crate::icons::steps_held_until(cx));
    assert_eq!(held(cx), None);

    let at = cx.update(|_w, cx| cx.background_executor().now());
    cx.simulate_keystrokes("a");
    let until = held(cx).expect("held for the echo");
    let wait = until.saturating_duration_since(at);
    assert!(
        wait >= Duration::from_millis(30) && wait <= Duration::from_millis(47),
        "the round trip and a refresh: {wait:?}"
    );
    let rows = [("$ a", SemanticMark::Prompt { exit: None, input: Some(2) })];
    view.update_in(cx, |v, _w, cx| v.term_event(session, marked_frame(1, &rows, 0), cx));
    cx.run_until_parked();
    assert_eq!(held(cx), None, "the echo let them go");

    cx.simulate_keystrokes("cmd-b");
    cx.run_until_parked();
    assert_eq!(held(cx), None, "a chord for the app awaits no echo");
}

fn click_at(cx: &mut VisualTestContext, selector: &'static str) {
    let at = cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is not drawn"));
    cx.simulate_click(at.center(), Modifiers::default());
    cx.run_until_parked();
}

fn leak(selector: String) -> &'static str {
    Box::leak(selector.into_boxed_str())
}

/// Unix milliseconds `ago` before now, as a worker stamps an agent's change.
fn ms_ago(ago: Duration) -> u64 {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().saturating_sub(ago);
    u64::try_from(now.as_millis()).unwrap()
}

/// An agent at rest shows how long it has waited though nothing else counts: no command runs
/// and no agent works, so the readouts' clock is still.
#[gpui::test]
fn an_agent_at_rest_shows_its_age_with_no_clock_running(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    cx.update(|_w, cx| cx.set_reduce_motion(true));
    let studio = connect(&view, cx, 1, "studio");
    let session = SessionId::new();
    opens(&view, cx, &studio, session, studio.me, 1);
    let since_ms = ms_ago(Duration::from_mins(5));
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(
            AgentEvent { since_ms: WallMs::from_millis(since_ms), ..blocked(session) },
            cx,
        );
    });
    cx.run_until_parked();
    click_at(cx, leak(format!("nav-worker-{}", studio.key)));
    assert!(cx.debug_bounds(leak(format!("nav-waiting-{session}"))).is_some(), "its row");
    assert!(
        cx.debug_bounds(leak(format!("nav-waiting-time-{session}"))).is_some(),
        "how long it has waited"
    );
}

/// Agents at their turn are marked on their tiles' rows and nowhere else: no section lists them,
/// folded away or not, and no clock ticks for them, so a second passing draws nothing.
#[gpui::test]
fn agents_at_their_turn_draw_no_section_and_no_clock(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    cx.update(|_w, cx| cx.set_reduce_motion(true));
    let studio = connect(&view, cx, 1, "studio");
    let sessions: Vec<SessionId> = (1..=5_u64)
        .map(|version| {
            let session = SessionId::new();
            opens(&view, cx, &studio, session, studio.me, version);
            session
        })
        .collect();
    let since_ms = ms_ago(Duration::from_secs(65));
    view.update_in(cx, |v, _w, cx| {
        for session in &sessions {
            let working = AgentEvent {
                status: AgentStatus::Working,
                since_ms: WallMs::from_millis(since_ms),
                ..blocked(*session)
            };
            v.agent_event(working, cx);
        }
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("nav-working").is_none(), "their rows say it");
    click_at(cx, leak(format!("nav-worker-{}", studio.key)));
    assert!(cx.debug_bounds("nav-working").is_none(), "folded away too");

    // What the fold's click set going settles first; then the seconds tick nothing.
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    let before = renders(&view, cx);
    for _ in 0..3 {
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
    }
    assert_eq!(renders(&view, cx), before, "the seconds passing draw nothing");
}

/// With the navigator hidden where it docks, a worker is on the rail only while something of
/// its own (a tile with no project) wants the person, marked with what; it goes to that tile.
#[gpui::test]
fn the_rail_shows_a_worker_while_its_own_tiles_wait(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let laptop = connect(&view, cx, 2, "laptop");
    let away = SessionId::new();
    let theirs = opens(&view, cx, &laptop, away, laptop.me, 1);
    let _mine = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    assert!(cx.debug_bounds("nav-rail").is_none(), "the navigator shows");
    cx.simulate_keystrokes("cmd-b");
    cx.run_until_parked();
    let rail = cx.debug_bounds("nav-rail").expect("the rail in its place");
    assert!((f32::from(rail.size.width) - navigator::RAIL_W).abs() < 0.5, "{rail:?}");
    let glyph = leak(format!("nav-rail-{}", laptop.key));
    assert!(cx.debug_bounds(glyph).is_none(), "at rest and well, no worker");
    view.update_in(cx, |v, _w, cx| v.agent_event(blocked(away), cx));
    cx.run_until_parked();
    let badge = leak(format!("nav-rail-{}-rollup", laptop.key));
    assert!(cx.debug_bounds(badge).is_some(), "what waits shows on the rail");
    click_at(cx, glyph);
    assert_eq!(focused(&view, cx), Some(theirs), "the worker's tile");
}

/// The handle is 12 pt centred on the navigator's edge, and a double-click puts the width back.
#[gpui::test]
fn the_handle_straddles_the_edge_and_a_double_click_resets_it(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let _studio = connect(&view, cx, 1, "studio");
    view.update(cx, |v, cx| {
        let nav = v.layout.navigator().clone();
        v.layout.set_navigator(slopty_client::layout::Navigator { width: 320.0, ..nav });
        cx.notify();
    });
    cx.run_until_parked();
    let (panel, handle) = (
        cx.debug_bounds("navigator").expect("drawn"),
        cx.debug_bounds("navigator-handle").expect("drawn"),
    );
    assert!((f32::from(handle.size.width) - navigator::HANDLE_W).abs() < 0.5, "{handle:?}");
    assert!((f32::from(handle.center().x - panel.right())).abs() < 0.5, "on the edge");
    let at = handle.center();
    let left = gpui::MouseButton::Left;
    let modifiers = Modifiers::default();
    cx.simulate_event(gpui::MouseDownEvent {
        button: left,
        position: at,
        modifiers,
        click_count: 2,
        first_mouse: false,
    });
    cx.simulate_event(gpui::MouseUpEvent { button: left, position: at, modifiers, click_count: 2 });
    cx.run_until_parked();
    let width = view.read_with(cx, |v, _| v.navigator_width());
    assert!((width - slopty_client::layout::Navigator::DEFAULT_WIDTH).abs() < 0.5, "{width}");
}

/// What waits in the workspace in view is its own tiles' and the bell's to say: the
/// breadcrumb's workspace carries no second mark for it, nor a count of what it holds, which
/// the navigator and the overview give. Its name is whole, and "+" follows the breadcrumb.
#[gpui::test]
fn the_workspace_in_view_carries_no_second_mark(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let [(asking, _), ..] = three_shells(&view, cx, &studio);
    view.update_in(cx, |v, _w, cx| v.agent_event(blocked(asking), cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("bell-count").is_some(), "the bell counts the one waiting");
    assert!(cx.debug_bounds("crumb-elsewhere").is_none(), "no second mark");
    cx.update(|window, _cx| window.set_a11y_active(true));
    view.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();
    let tree = cx.update(|window, _cx| crate::a11y::tree(window));
    // Its first shell is at home, which names nothing, so the workspace takes its worker's.
    assert!(tree.iter().any(|n| n.is("Button", Some("studio"))), "{tree:#?}");
    let name = cx.debug_bounds("crumb-workspace").expect("the name");
    let whole = cx.update(|window, _cx| {
        let mut style = window.text_style();
        style.font_weight = gpui::FontWeight(slopty_theme::Typography::MEDIUM_WEIGHT);
        let text = "studio";
        let run = style.to_run(text.len());
        let size = px(Theme::default().typography.ui_size);
        window.text_system().shape_line(text.into(), size, &[run], None).width
    });
    assert!(name.size.width + px(0.5) >= whole, "{name:?}, whole {whole:?}");
    let crumbs = cx.debug_bounds("crumb-workspace").expect("drawn");
    let new = cx.debug_bounds("new-menu").expect("+");
    let gap = f32::from(new.left() - crumbs.right());
    assert!((0.0..=Theme::default().spacing.md).contains(&gap), "+ follows it: {gap}");
}

/// No bar runs along the bottom: the strip's panels stand a gutter above the window's bottom
/// edge, with nothing but the canvas under them, and the title
/// bar says nothing of itself while nothing needs saying. The server out of reach is said at
/// its trailing end until it answers; a word about no one tile sits in its lane; a failure in
/// a tile's own work sits beside that tile, under its header at its trailing edge, and moves to
/// the title bar once the tile is gone.
#[gpui::test]
fn no_bar_runs_along_the_bottom_and_a_notice_sits_by_its_work(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let bounds = |cx: &mut VisualTestContext, selector: &'static str| cx.debug_bounds(selector);
    let strip = bounds(cx, "strip").expect("the strip is drawn");
    let window = cx.update(|window, _| window.viewport_size());
    assert!(bounds(cx, "statusbar").is_none(), "no bar along the bottom");
    let tile = cx.debug_bounds(selector("item", shell.item)).expect("the shell");
    let gutter = Theme::default().spacing.gutter();
    assert!(
        (f32::from(window.height - tile.bottom()) - gutter).abs() < 0.5,
        "a gutter under the panel: {tile:?} in {window:?}"
    );
    assert!(
        (f32::from(window.height - strip.bottom()) - gutter / 2.0).abs() < 0.5,
        "the strip half a gutter above the edge: {strip:?} in {window:?}"
    );
    assert!(bounds(cx, "readouts").is_none(), "nothing to say, nothing said");
    assert!(bounds(cx, "status-worker").is_none(), "the worker is the navigator's to name");

    let titlebar = bounds(cx, "titlebar").expect("the title bar");
    view.update_in(cx, |v, _w, cx| v.set_server_status(Some("server unreachable".into()), cx));
    cx.run_until_parked();
    let server = bounds(cx, "readout-server").expect("the server's word");
    assert!(titlebar.contains(&server.center()), "at the title bar's end: {server:?}");
    let bell = bounds(cx, "bell").expect("the bell");
    assert!(server.right() <= bell.left(), "before the bell: {server:?} {bell:?}");
    view.update_in(cx, |v, _w, cx| v.set_server_status(None, cx));
    cx.run_until_parked();
    assert!(bounds(cx, "readouts").is_none(), "gone once it answers");

    view.update(cx, |v, cx| v.show_notice("Copied".to_owned(), cx));
    cx.run_until_parked();
    let lane = bounds(cx, "notices").expect("a word about no one tile");
    assert!(titlebar.contains(&lane.center()), "in the title bar's lane: {lane:?}");

    view.update(cx, |v, cx| v.show_failure_at(shell, "The drop did not land".to_owned(), cx));
    cx.run_until_parked();
    let tile = view.read_with(cx, |v, _| v.tile_bounds(shell)).expect("the shell is drawn");
    let beside = bounds(cx, leak(format!("tile-notices-{}", shell.item.as_uuid())))
        .expect("the failure beside its tile");
    assert!(tile.contains(&beside.center()), "within its tile: {beside:?} in {tile:?}");
    let header = Theme::default().density.header;
    assert!(
        f32::from(beside.top()) >= f32::from(tile.top()) + header - 0.5,
        "under its header: {beside:?} in {tile:?}"
    );
    assert!(
        (f32::from(tile.right() - beside.right())).abs() <= Theme::default().spacing.md,
        "at its trailing edge"
    );
    let texts = view.read_with(cx, |v, _| v.toast_texts());
    assert_eq!(texts, ["Copied", "The drop did not land"], "both up, each in its place");

    view.update_in(cx, |v, window, cx| v.close_tile(shell, window, cx));
    cx.run_until_parked();
    assert!(bounds(cx, leak(format!("tile-notices-{}", shell.item.as_uuid()))).is_none());
    let texts = view.read_with(cx, |v, _| v.toast_texts());
    assert!(
        texts.iter().any(|t| t == "The drop did not land"),
        "a failure outlives its tile: {texts:?}"
    );
    assert!(bounds(cx, "notices").is_some(), "and moves to the title bar");
}

/// Notices the bar has no room for are never cut off unseen: the newest stays whole, the
/// older go behind a count, and the count opens them under it. With room again they come back
/// beside it and the count goes.
#[gpui::test]
fn the_notices_the_bar_has_no_room_for_go_behind_a_count_that_opens_them(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let wide = size(px(1800.0), px(900.0));
    cx.simulate_resize(wide);
    let older = "The settings did not parse: an unknown key at line twelve".to_owned();
    let newer = "The save did not land: the worker closed the file first".to_owned();
    view.update(cx, |v, cx| v.show_failure(older.clone(), cx));
    view.update(cx, |v, cx| v.show_failure(newer.clone(), cx));
    cx.run_until_parked();
    let texts = view.read_with(cx, |v, _| v.toast_texts());
    assert_eq!(texts, [older.clone(), newer.clone()], "both up");
    assert!(cx.debug_bounds("notices-more").is_none(), "with room, both shown and no count");

    cx.simulate_resize(size(px(760.0), px(900.0)));
    cx.run_until_parked();
    let titlebar = cx.debug_bounds("titlebar").expect("the title bar");
    let lane = cx.debug_bounds("notices").expect("the notices");
    assert!(lane.right() <= titlebar.right(), "within the bar: {lane:?} {titlebar:?}");
    let more = cx.debug_bounds("notices-more").expect("the older behind a count");
    assert!(lane.contains(&more.center()), "the count is in the lane: {more:?} {lane:?}");
    assert!(cx.debug_bounds("notices-left").is_none(), "closed until asked");
    click_at(cx, "notices-more");
    let left = cx.debug_bounds("notices-left").expect("the count opens the older notice");
    assert!(left.top() >= more.bottom(), "under the count: {left:?} {more:?}");
    click_at(cx, "notices-more");
    assert!(cx.debug_bounds("notices-left").is_none(), "and closes it again");

    cx.simulate_resize(wide);
    cx.run_until_parked();
    assert!(cx.debug_bounds("notices-more").is_none(), "room again, the count goes");
}

/// On a phone under the touch density every button is a finger's 44 pt, and the title bar
/// still holds them all: taller to fit them, nothing past its right edge.
#[gpui::test]
fn the_phone_title_bar_fits_its_touch_targets(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let _shells = three_shells(&view, cx, &studio);
    view.update(cx, |v, cx| {
        let mut theme = v.theme().clone();
        theme.density = slopty_theme::Density::TOUCH;
        v.set_theme(theme, cx);
    });
    cx.simulate_resize(size(px(402.0), px(874.0)));
    cx.run_until_parked();
    let bounds = |cx: &mut VisualTestContext, selector: &'static str| {
        cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is drawn"))
    };
    let (bar, toggle, bell, more) = (
        bounds(cx, "titlebar"),
        bounds(cx, "navigator-toggle"),
        bounds(cx, "bell"),
        bounds(cx, "more"),
    );
    for button in [toggle, bell, more] {
        assert!(f32::from(button.size.height) >= 44.0, "a finger's target: {button:?}");
        assert!(button.top() >= bar.top() && button.bottom() <= bar.bottom(), "{button:?}");
    }
    assert!(f32::from(more.right()) <= 402.0, "nothing past the edge: {more:?}");
    assert!(toggle.right() <= bell.left(), "{toggle:?} {bell:?}");
}

/// The "…" button closes the menu it opened: the press outside the menu only dismisses, and
/// does not reach the button under it to open the menu again. The keyboard goes back to the
/// focused shell, not to the button that took it when pressed.
#[gpui::test]
fn a_second_press_on_the_menu_button_closes_its_menu(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let _shells = three_shells(&view, cx, &studio);
    let tile = focused(&view, cx).expect("a focused shell");
    let session = view.read_with(cx, |v, _| session_of(v, tile));
    let more = cx.debug_bounds("more").expect("… is drawn").center();
    cx.simulate_click(more, Modifiers::none());
    cx.run_until_parked();
    assert!(cx.debug_bounds("menu").is_some(), "the first press opens the menu");
    cx.simulate_click(more, Modifiers::none());
    cx.run_until_parked();
    assert!(cx.debug_bounds("menu").is_none(), "the second press closes it");
    assert!(terminal_focused(&view, cx, session), "the shell keeps the keyboard");
}

/// What held the keyboard a moment (the settings, a dialog) gives it back to the focused
/// shell, not to the workspace around it, where the shell's cursor went hollow.
#[gpui::test]
fn the_keyboard_goes_back_to_the_focused_shell(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let _shells = three_shells(&view, cx, &studio);
    let tile = focused(&view, cx).expect("a focused shell");
    let session = view.read_with(cx, |v, _| session_of(v, tile));
    view.update_in(cx, |v, window, cx| window.focus(&v.focus, cx));
    assert!(!terminal_focused(&view, cx, session), "the workspace took it");
    view.update_in(cx, |v, window, cx| v.return_keyboard(window, cx));
    assert!(terminal_focused(&view, cx, session), "the shell has it back");
}

/// "+" is a menu of what to open, hung from its own left edge: the empty workspace's three ways
/// to begin and a note, then a new workspace apart. A row runs what its keys run: New terminal
/// asks the worker for a shell in the focused one's directory, as ⌘T does.
#[gpui::test]
fn plus_lists_what_to_open_and_runs_it_as_its_keys_do(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let _shell = opens_in(&view, cx, &studio, SessionId::new(), studio.me, 1, Some("/tmp/work"));
    studio.drain();
    click_at(cx, "new-menu");
    cx.update(|window, _cx| window.set_a11y_active(true));
    view.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();
    let rows: Vec<String> = cx
        .update(|window, _cx| crate::a11y::tree(window))
        .into_iter()
        .filter(|n| n.role == "MenuItem")
        .filter_map(|n| n.label)
        .collect();
    assert_eq!(
        rows,
        [
            "New terminal",
            "New agent\u{2026}",
            "Add a window or display",
            "New note",
            "New workspace"
        ]
    );
    assert!(cx.debug_bounds("menu-separator-4").is_some(), "a new workspace stands apart");
    let (menu, plus) =
        (cx.debug_bounds("menu").expect("the menu"), cx.debug_bounds("new-menu").expect("+"));
    assert!((f32::from(menu.left() - plus.left())).abs() < 0.5, "{menu:?} under {plus:?}");

    click_at(cx, "menu-New terminal");
    assert!(cx.debug_bounds("menu").is_none(), "the menu goes");
    let sent = studio.drain();
    assert!(
        matches!(
            sent.as_slice(),
            [ClientMsg::OpenSession { spec: OpenSession { cwd: Some(cwd), .. }, .. }] if cwd == "/tmp/work"
        ),
        "{sent:?}"
    );
}

/// The "…" menu reads in sections, a hairline between each: where to go, the settings, then
/// the connections, whatever order the app handed its rows in. The machines are the
/// navigator's, so it has no row for them.
#[gpui::test]
fn the_more_menu_groups_its_rows_into_sections(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let _shells = three_shells(&view, cx, &studio);
    let entry = |group, label: &'static str| MenuEntry {
        group,
        label: label.into(),
        detail: SharedString::default(),
        run: Rc::new(|_window, _cx| {}),
    };
    view.update(cx, |v, cx| {
        let entries = vec![
            entry(MenuGroup::Connections, "Add a machine"),
            entry(MenuGroup::Settings, "Settings"),
        ];
        v.set_more_menu(entries, cx);
    });
    let more = cx.debug_bounds("more").expect("… is drawn").center();
    cx.simulate_click(more, Modifiers::none());
    cx.run_until_parked();
    let top = |cx: &mut VisualTestContext, label: &str| {
        let selector = Box::leak(format!("menu-{label}").into_boxed_str());
        f32::from(cx.debug_bounds(selector).unwrap_or_else(|| panic!("{label}")).top())
    };
    let order = ["Command palette", "Overview", "Stream stats", "Settings", "Add a machine"];
    let tops: Vec<f32> = order.iter().map(|label| top(cx, label)).collect();
    assert!(tops.windows(2).all(|w| w[0] < w[1]), "{order:?} at {tops:?}");
    let hairlines = (0..8).filter(|i| {
        let selector = Box::leak(format!("menu-separator-{i}").into_boxed_str());
        cx.debug_bounds(selector).is_some()
    });
    assert_eq!(hairlines.count(), 2, "one between each of the three sections");
    let settings = top(cx, "Settings");
    let separator = (0..8)
        .find_map(|i| {
            let selector = Box::leak(format!("menu-separator-{i}").into_boxed_str());
            cx.debug_bounds(selector)
        })
        .expect("a hairline");
    assert!(f32::from(separator.top()) < settings, "the settings open a section");

    assert!(cx.debug_bounds("menu-Machines").is_none(), "the machines are the navigator's");
}

/// What a frame costs while the pointer moves over a shell and the strip is built for
/// something else in the same frame (a spring, a hover on its chrome): 60 shells and 60 notes
/// with the navigator docked, the pointer crossing the focused shell's grid a cell at a time.
/// Run by hand (it prints, it does not judge); `docs/MEASUREMENTS.md` has the numbers and the
/// command.
#[gpui::test]
#[ignore = "a measurement, run by hand: see docs/MEASUREMENTS.md"]
fn measure_a_pointer_frame_beside_the_chrome(cx: &mut TestAppContext) {
    const FRAMES: usize = 400;
    const WARM: usize = 20;
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let sessions = crowd(&view, cx, &studio, 60, 60);
    let last = *sessions.last().expect("a shell");
    let tile = view.read_with(cx, |v, _| v.tile_of_session(last)).expect("its tile");
    let dense = dense_screen(DENSE_ROWS);
    let dense: Vec<&str> = dense.iter().map(String::as_str).collect();
    view.update_in(cx, |v, _w, cx| {
        v.term_event(last, frame(&dense), cx);
        v.focus_tile(tile, cx);
    });
    cx.run_until_parked();
    let body = drawn_at(&view, cx, tile).expect("drawn");
    let strip = view.read_with(cx, |v, _| v.strip_host.entity_id());
    let terminal = view.read_with(cx, |v, _| v.terminal(last).cloned()).expect("attached");
    let mut took = Vec::with_capacity(FRAMES);
    let drawn = terminal.read_with(cx, |t, _| t.renders());
    for n in 0..WARM + FRAMES {
        let step = f32::from(u16::try_from(n % 200).unwrap_or(0));
        let at = body.origin + point(px(f32::mul_add(step, 2.0, 20.0)), body.size.height / 2.0);
        let start = Instant::now();
        cx.simulate_mouse_move(at, None, Modifiers::default());
        cx.update(|_w, cx| cx.notify(strip));
        cx.run_until_parked();
        if n >= WARM {
            took.push(start.elapsed());
        }
    }
    took.sort_unstable();
    let pct = |p: usize| slopty_client::pacing::percentile(&took, p).as_secs_f64() * 1e3;
    let built = terminal.read_with(cx, |t, _| t.renders()).saturating_sub(drawn);
    println!(
        "MEASURE pointer frame beside the chrome, 60 shells + 60 notes, {FRAMES} frames: p50 \
         {:.3} ms p95 {:.3} ms; the shell under the pointer built {built} times",
        pct(50),
        pct(95)
    );
}

/// What the keyboard moving between two shells costs, the frame that moves it and the one
/// after, over 60 shells and 60 notes with the navigator docked. Run by hand (it prints, it
/// does not judge); `docs/MEASUREMENTS.md` has the numbers and the command.
#[gpui::test]
#[ignore = "a measurement, run by hand: see docs/MEASUREMENTS.md"]
fn measure_the_keyboard_moving_beside_the_chrome(cx: &mut TestAppContext) {
    const MOVES: usize = 200;
    const WARM: usize = 10;
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let sessions = crowd(&view, cx, &studio, 60, 60);
    let pair: Vec<SessionId> = sessions.iter().rev().take(2).copied().collect();
    let mut took = Vec::with_capacity(MOVES);
    for n in 0..WARM + MOVES {
        let start = Instant::now();
        view.update_in(cx, |v, _w, cx| {
            v.pending_focus = Some(pair[n % 2]);
            cx.notify();
        });
        cx.run_until_parked();
        if n >= WARM {
            took.push(start.elapsed());
        }
    }
    took.sort_unstable();
    let pct = |p: usize| slopty_client::pacing::percentile(&took, p).as_secs_f64() * 1e3;
    println!(
        "MEASURE the keyboard moving beside the chrome, 60 shells + 60 notes, {MOVES} moves: \
         p50 {:.3} ms p95 {:.3} ms",
        pct(50),
        pct(95)
    );
}

/// What the keyboard moving between two shells costs when a view of its own moves it (a click
/// in a body, a find bar giving it back), with nothing in the workspace asked: over 60 shells
/// and 60 notes with the navigator docked, the frame that draws the move, and how many times
/// the workspace and the strip were built for it. Run by hand (it prints, it does not judge);
/// `docs/MEASUREMENTS.md` has the numbers and the command.
#[gpui::test]
#[ignore = "a measurement, run by hand: see docs/MEASUREMENTS.md"]
fn measure_the_keyboard_moving_on_its_own_beside_the_chrome(cx: &mut TestAppContext) {
    const MOVES: usize = 400;
    const WARM: usize = 20;
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let sessions = crowd(&view, cx, &studio, 60, 60);
    // Two shells on screen, so the move changes what the window shows.
    let handles: Vec<FocusHandle> = view.read_with(cx, |v, cx| {
        let shown = v.drawn.on_screen.borrow();
        sessions
            .iter()
            .filter(|s| {
                v.layout.tiles().any(|t| {
                    shown.contains(&t.item)
                        && v.item(t).is_some_and(|i| i.kind == ItemKind::Terminal { session: **s })
                })
            })
            .take(2)
            .filter_map(|s| v.terminal(*s).map(|t| t.read(cx).focus_handle(cx)))
            .collect()
    });
    assert_eq!(handles.len(), 2, "two shells on screen");
    let builds =
        |cx: &mut VisualTestContext| view.read_with(cx, |v, _| (v.renders, v.drawn.builds.get()));
    let mut took = Vec::with_capacity(MOVES);
    let start_builds = builds(cx);
    for n in 0..WARM + MOVES {
        let handle = &handles[n % 2];
        let start = Instant::now();
        cx.update(|window, cx| window.focus(handle, cx));
        cx.run_until_parked();
        if n >= WARM {
            took.push(start.elapsed());
        }
    }
    let (workspace, strip) = builds(cx);
    took.sort_unstable();
    let pct = |p: usize| slopty_client::pacing::percentile(&took, p).as_secs_f64() * 1e3;
    println!(
        "MEASURE the keyboard moving on its own beside the chrome, 60 shells + 60 notes, {MOVES} \
         moves: p50 {:.3} ms p95 {:.3} ms; the workspace built {} times, the strip {}",
        pct(50),
        pct(95),
        workspace.saturating_sub(start_builds.0),
        strip.saturating_sub(start_builds.1),
    );
}

/// On a phone the bar is the focused tile's header: a command that starts there retitles the
/// bar at once, the bar drawn again for it. One that starts in a tile out of view leaves the
/// bar on the focused tile.
#[gpui::test]
fn a_command_in_the_focused_tile_retitles_a_phones_bar(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let [(other, _), _, (session, tile)] = three_shells(&view, cx, &studio);
    view.update_in(cx, |v, _w, cx| v.focus_tile(tile, cx));
    cx.simulate_resize(size(px(390.0), px(844.0)));
    cx.run_until_parked();
    let heading = |cx: &mut VisualTestContext| {
        tree(cx).into_iter().find(|n| n.role == "Heading").and_then(|n| n.label)
    };
    // The accessibility tree draws every view, so it is read only once the counts are taken.
    let prompt = SemanticMark::Prompt { exit: None, input: Some(2) };
    // Typed at the prompt first, as the device has it: the command has not started.
    let typed = [("$ cat -v", prompt)];
    view.update_in(cx, |v, _w, cx| v.term_event(session, marked_frame(1, &typed, 0), cx));
    cx.run_until_parked();
    let before = renders(&view, cx);
    let rows = [("$ cat -v", prompt), ("", SemanticMark::Output)];
    view.update_in(cx, |v, _w, cx| v.term_event(session, marked_frame(2, &rows, 1), cx));
    cx.run_until_parked();
    let after = renders(&view, cx);
    assert!(
        after[1] > before[1],
        "the bar draws the focused tile's command: {before:?} → {after:?}"
    );
    assert!(heading(cx).is_some_and(|h| h.starts_with("terminal cat -v")), "{:?}", heading(cx));

    let rows = [("$ make", prompt), ("building", SemanticMark::Output)];
    view.update_in(cx, |v, _w, cx| v.term_event(other, marked_frame(1, &rows, 1), cx));
    cx.run_until_parked();
    assert!(
        heading(cx).is_some_and(|h| h.starts_with("terminal cat -v")),
        "still the focused tile"
    );
}

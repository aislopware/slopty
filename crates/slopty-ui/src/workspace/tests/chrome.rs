//! The frame's chrome in the headless workspace: what an echo costs beside it.

use std::time::{Instant, SystemTime, UNIX_EPOCH};

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
            started_ms: u64::try_from(started).unwrap(),
            ..summary(session, Some(&format!("/Users/me/src/project_{n}")))
        });
        items.push(Item {
            id: ItemId::new(),
            kind: ItemKind::Terminal { session },
            sleeping: false,
            name: None,
        });
    }
    for _ in 0..notes {
        let kind = ItemKind::Note { text: "a note\n".into() };
        items.push(Item { id: ItemId::new(), kind, sleeping: false, name: None });
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
    assert!(cx.debug_bounds("navigator").is_some());
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
    assert!(cx.debug_bounds(selector("item", focused_tile.item)).is_some(), "drawn");
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

/// How many times each region has drawn: the navigator, the title bar, the status bar.
fn renders(view: &Entity<WorkspaceView>, cx: &VisualTestContext) -> [usize; 3] {
    view.read_with(cx, WorkspaceView::chrome_renders)
}

/// An echo draws its terminal and leaves the navigator and both bars as they were drawn; a
/// change to the workspace draws them all again.
#[gpui::test]
fn an_echo_leaves_the_chrome_as_it_was_drawn(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let [_, _, (session, _)] = three_shells(&view, cx, &studio);
    assert!(cx.debug_bounds("navigator").is_some() && cx.debug_bounds("statusbar").is_some());
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
        view.read_with(cx, |v, cx| [first, second].map(|t| v.tile_title(t, v.item(t).unwrap(), cx)))
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
    assert!(lines.iter().any(|(_, meta, _)| meta.contains("make")), "{lines:#?}");
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

/// While agents are at their turn out of sight (their worker folded), *Working* lists the first
/// four under a heading that counts them all, with "Show N more" for the rest. Its turn times tick
/// once a second by the navigator's own clock, which draws the navigator alone.
#[gpui::test]
fn working_lists_the_agents_at_their_turn_and_ticks_their_time(cx: &mut TestAppContext) {
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
    assert!(cx.debug_bounds("nav-working").is_none(), "nobody works yet");
    let since_ms = ms_ago(Duration::from_secs(65));
    view.update_in(cx, |v, _w, cx| {
        for session in &sessions {
            let working =
                AgentEvent { status: AgentStatus::Working, since_ms, ..blocked(*session) };
            v.agent_event(working, cx);
        }
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("nav-working").is_none(), "their rows say it while in view");
    click_at(cx, leak(format!("nav-worker-{}", studio.key)));
    assert!(cx.debug_bounds("nav-working").is_some(), "the heading");
    assert!(cx.debug_bounds("nav-workers").is_some(), "the workers' own heading under it");
    assert_eq!(view.read_with(cx, |v, _| v.navigator_working().len()), 4, "the first four");
    let first = sessions.first().copied().expect("five");
    assert!(cx.debug_bounds(leak(format!("nav-working-time-{first}"))).is_some(), "its time");
    click_at(cx, "nav-working-more");
    assert_eq!(view.read_with(cx, |v, _| v.navigator_working()), sessions, "all, in order");

    let before = renders(&view, cx);
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    let after = renders(&view, cx);
    assert!(after[0] > before[0], "the turn's time ticked: {before:?} → {after:?}");
    assert_eq!(after[1..], before[1..], "the bars did not draw for it");

    click_at(cx, leak(format!("nav-working-{first}")));
    assert_eq!(focused(&view, cx), view.read_with(cx, |v, _| v.tile_of_session(first)));
}

/// With the navigator hidden where it docks, a rail keeps a glyph per worker, marked with what
/// its tiles want; one goes to that worker.
#[gpui::test]
fn the_rail_keeps_the_workers_in_view_when_the_navigator_hides(cx: &mut TestAppContext) {
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
    let badge = leak(format!("nav-rail-rollup-{}", laptop.key));
    assert!(cx.debug_bounds(badge).is_none(), "at rest, no mark");
    view.update_in(cx, |v, _w, cx| v.agent_event(blocked(away), cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds(badge).is_some(), "what waits shows on the rail");
    click_at(cx, leak(format!("nav-rail-{}", laptop.key)));
    assert_eq!(focused(&view, cx), Some(theirs), "the worker's tile");
}

/// The handle is 12 pt centred on the navigator's edge, and a double-click puts the width back.
#[gpui::test]
fn the_handle_straddles_the_edge_and_a_double_click_resets_it(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let _studio = connect(&view, cx, 1, "studio");
    view.update(cx, |v, cx| {
        let nav = v.layout.navigator();
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

/// Workspaces of their own are tabs, a lone one only its name, with no rollup mark (the bell
/// already counts what waits) and no count of what it holds, which the navigator and the
/// overview give. The title bar has no second "+".
#[gpui::test]
fn a_lone_workspace_is_its_name_alone(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let [(asking, _), ..] = three_shells(&view, cx, &studio);
    view.update_in(cx, |v, _w, cx| v.agent_event(blocked(asking), cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("bell-count").is_some(), "the bell counts the one waiting");
    assert!(cx.debug_bounds("ws-rollup-0").is_none(), "a lone name carries no second mark");
    let remote = connect(&view, cx, 2, "remote");
    let _far = opens(&view, cx, &remote, SessionId::new(), remote.me, 1);
    cx.update(|window, _cx| window.set_a11y_active(true));
    view.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();
    let tree = cx.update(|window, _cx| crate::a11y::tree(window));
    let heading = Some("Workspace 1, 4 tiles, 1 needs you");
    assert!(tree.iter().any(|n| n.is("Heading", heading)), "{tree:#?}");
    assert!(cx.debug_bounds("add").is_none(), "one \"+\", for a workspace");
    // The name is whole beside what the workspace holds: a lone name is not held to a tab's
    // width.
    let name = cx.debug_bounds("ws-name-0").expect("the name");
    let whole = cx.update(|window, _cx| {
        let mut style = window.text_style();
        style.font_weight = gpui::FontWeight(slopty_theme::Typography::MEDIUM_WEIGHT);
        let text = "Workspace 1";
        let run = style.to_run(text.len());
        let size = px(Theme::default().typography.ui_size);
        window.text_system().shape_line(text.into(), size, &[run], None).width
    });
    assert!(name.size.width + px(0.5) >= whole, "{name:?}, whole {whole:?}");

    let new = cx.debug_bounds("new-menu").expect("+");
    let gap = f32::from(new.left() - name.right());
    assert!(gap <= Theme::default().spacing.md, "nothing between the name and +: {gap}");

    new_workspace_from_the_bar(cx);
    assert!(cx.debug_bounds("ws-tab-1").is_some(), "two tabs");
}

/// A tab that goes folds its width away where it stood, unless motion is reduced. (The last
/// tab but one going leaves a lone name, which has no tabs to slide.)
#[gpui::test]
fn a_closing_tab_folds_away_unless_motion_is_reduced(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let _shells = three_shells(&view, cx, &studio);
    // A second workspace with a column in it, so two tabs stay when a third goes.
    view.update_in(cx, |v, _w, cx| {
        v.tick();
        v.layout.move_column_to_workspace_down();
        v.after_focus_moved(cx);
        cx.notify();
    });
    cx.run_until_parked();
    view.update(cx, |v, _| v.set_animation(true));
    // Open an empty workspace, then leave it: its tab goes.
    let leave_an_empty_one = |cx: &mut VisualTestContext| {
        new_workspace_from_the_bar(cx);
        let active = view.read_with(cx, |v, _| v.layout().active_workspace());
        let id = view.read_with(cx, |v, _| {
            v.layout().workspaces().get(active).map(slopty_client::layout::Workspace::id)
        });
        click_at(cx, "ws-tab-0");
        cx.debug_bounds(leak(format!("ws-tab-closing-{}", id.unwrap_or_default()))).is_some()
    };
    assert!(leave_an_empty_one(cx), "the empty workspace's tab folds away");
    cx.update(|_w, cx| cx.set_reduce_motion(true));
    assert!(!leave_an_empty_one(cx), "at once under Reduce Motion");
}

/// While the phone's key bar shows, the status bar gives it its row, and takes it back after.
#[gpui::test]
fn the_status_bar_steps_aside_for_the_key_bar(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let strip_h = |cx: &mut VisualTestContext| {
        f32::from(cx.debug_bounds("strip").expect("the strip is drawn").size.height)
    };
    let with_bar = strip_h(cx);
    assert!(cx.debug_bounds("statusbar").is_some());
    view.update(cx, |v, cx| v.set_key_bar_shown(true, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("statusbar").is_none(), "out of the key bar's way");
    assert!(strip_h(cx) > with_bar, "the strip takes its row");
    view.update(cx, |v, cx| v.set_key_bar_shown(false, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("statusbar").is_some(), "back once the keys go");
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
        ["New terminal", "New agent", "Add a window or display", "New note", "New workspace"]
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
            [ClientMsg::OpenSession(OpenSession { cwd: Some(cwd), .. })] if cwd == "/tmp/work"
        ),
        "{sent:?}"
    );
}

/// The "…" menu reads in sections, a hairline between each: where to go, the settings, then
/// the connections, whatever order the app handed its rows in. Its Workers row opens the
/// hosts popover, which the status bar no longer offers while every worker is up.
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
            entry(MenuGroup::Connections, "Add a worker"),
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
    let order = ["Command palette", "Overview", "Stream stats", "Settings", "Add a worker"];
    let tops: Vec<f32> = order.iter().map(|label| top(cx, label)).collect();
    assert!(tops.windows(2).all(|w| w[0] < w[1]), "{order:?} at {tops:?}");
    assert!(top(cx, "Add a worker") < top(cx, "Workers"), "the hosts close the connections");
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

    let workers = Box::leak("menu-Workers".to_owned().into_boxed_str());
    let at = cx.debug_bounds(workers).expect("Workers").center();
    cx.simulate_click(at, Modifiers::none());
    cx.run_until_parked();
    assert!(view.read_with(cx, |v, _| v.hosts_open()), "Workers opens the hosts");
    assert!(cx.debug_bounds("status-workers").is_none(), "with every worker up");
}

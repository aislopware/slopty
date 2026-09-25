//! The frame around the strip in the headless workspace: the navigator, the status bar, the
//! title bar's items and the bell.

use gpui::{AppContext as _, Modifiers, MouseButton};
use slopty_client::layout::Navigator;

use super::*;

/// `debug_bounds` wants a static selector; tests may leak a handful.
fn leak(selector: String) -> &'static str {
    Box::leak(selector.into_boxed_str())
}

/// Click the middle of what `selector` names.
fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let at = cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is not drawn"));
    cx.simulate_click(at.center(), Modifiers::default());
    cx.run_until_parked();
}

/// The labels a screen reader reads in the last frame.
fn labels(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> Vec<String> {
    cx.update(|window, _cx| window.set_a11y_active(true));
    view.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();
    let tree = cx.update(|window, _cx| crate::a11y::tree(window));
    tree.into_iter().filter_map(|n| n.label).collect()
}

/// Within a point: widths are laid out in floats.
fn near(a: f32, b: f32) {
    assert!((a - b).abs() < 0.5, "{a} vs {b}");
}

/// Drag the navigator's handle `dx` points sideways.
fn drag_handle(cx: &mut VisualTestContext, dx: f32) {
    let at = cx.debug_bounds("navigator-handle").expect("the handle is drawn").center();
    let to = point(at.x + px(dx), at.y);
    cx.simulate_mouse_down(at, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(to, Some(MouseButton::Left), Modifiers::default());
    cx.simulate_mouse_up(to, MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
}

fn temp_layout() -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("slopty-frame-{}", ItemId::new()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("layout.json");
    (dir, path)
}

fn saved_navigator(cx: &VisualTestContext, path: &std::path::Path) -> Navigator {
    cx.executor().advance_clock(SAVE_AFTER);
    cx.run_until_parked();
    read_layout(path).expect("written").navigator
}

/// How the navigator sits: docked where the strip keeps a desktop's width beside it, over the
/// strip on an iPad or a window that would leave the strip a phone's, a drawer on a phone.
#[test]
fn the_navigator_docks_only_where_the_strip_keeps_its_room() {
    use navigator::{Mode, mode};
    assert_eq!(mode(1200.0, 248.0, 700.0, false), Mode::Docked);
    assert_eq!(mode(900.0, 248.0, 700.0, false), Mode::Overlay, "the strip would be a phone's");
    assert_eq!(mode(1366.0, 248.0, 700.0, true), Mode::Overlay, "an iPad");
    assert_eq!(mode(390.0, 248.0, 700.0, true), Mode::Drawer, "a phone");
}

/// ⌘B hides the docked navigator and the strip takes its room; ⌘B brings it back. Whether it
/// shows is written with the layout, and a workspace made from that file starts the same way.
#[gpui::test]
fn cmd_b_hides_and_shows_the_navigator_and_the_layout_keeps_it(cx: &mut TestAppContext) {
    let (dir, path) = temp_layout();
    let (view, cx) = workspace(cx);
    view.update(cx, |v, _| v.set_layout_path(path.clone()));
    let studio = connect(&view, cx, 1, "studio");
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let strip_w = |cx: &mut VisualTestContext| {
        f32::from(cx.debug_bounds("strip").expect("the strip is drawn").size.width)
    };
    assert!(cx.debug_bounds("navigator").is_some(), "docked by default on a wide window");
    let beside = strip_w(cx);

    cx.simulate_keystrokes("cmd-b");
    cx.run_until_parked();
    assert!(cx.debug_bounds("navigator").is_none(), "hidden");
    let alone = strip_w(cx);
    assert!((alone - beside - Navigator::DEFAULT_WIDTH).abs() < 1.0, "{beside} → {alone}");
    assert!(!saved_navigator(cx, &path).shown, "the layout keeps it hidden");
    let saved = read_layout(&path).expect("written");
    let restored =
        cx.update(|_w, cx| cx.new(|cx| WorkspaceView::new(Theme::default(), Some(saved), cx)));
    assert!(!restored.read_with(cx, |v, _| v.layout().navigator().shown), "and starts hidden");

    click(cx, "navigator-toggle");
    assert!(cx.debug_bounds("navigator").is_some(), "the title bar's toggle brings it back");
    assert!(saved_navigator(cx, &path).shown);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The handle drags the width, which stops at 200 and 400 however far the pointer goes, and
/// the width is written with the layout and read back from it.
#[gpui::test]
fn dragging_the_handle_resizes_the_navigator_within_its_clamps(cx: &mut TestAppContext) {
    let (dir, path) = temp_layout();
    let (view, cx) = workspace(cx);
    view.update(cx, |v, _| v.set_layout_path(path.clone()));
    let _studio = connect(&view, cx, 1, "studio");
    let width = |cx: &mut VisualTestContext| {
        let drawn = f32::from(cx.debug_bounds("navigator").expect("drawn").size.width);
        (view.read_with(cx, |v, _| v.navigator_width()), drawn)
    };
    let (kept, drawn) = width(cx);
    near(kept, Navigator::DEFAULT_WIDTH);
    near(drawn, Navigator::DEFAULT_WIDTH);
    drag_handle(cx, 60.0);
    let (kept, drawn) = width(cx);
    near(kept, Navigator::DEFAULT_WIDTH + 60.0);
    near(drawn, Navigator::DEFAULT_WIDTH + 60.0);
    drag_handle(cx, 500.0);
    near(width(cx).0, Navigator::MAX_WIDTH);
    drag_handle(cx, -900.0);
    near(width(cx).0, Navigator::MIN_WIDTH);
    near(saved_navigator(cx, &path).width, Navigator::MIN_WIDTH);
    let saved = read_layout(&path).expect("written");
    let restored =
        cx.update(|_w, cx| cx.new(|cx| WorkspaceView::new(Theme::default(), Some(saved), cx)));
    near(restored.read_with(cx, |v, _| v.navigator_width()), Navigator::MIN_WIDTH);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// *Needs you* shows only while an agent waits, above *Workers* and *Workspaces*; each
/// worker lists its tiles beneath it, with an accessible name, until its row folds them.
#[gpui::test]
fn the_navigator_lists_what_needs_you_then_the_workers_then_the_workspaces(
    cx: &mut TestAppContext,
) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let laptop = connect(&view, cx, 2, "laptop");
    let session = SessionId::new();
    let _waiting = opens(&view, cx, &laptop, session, laptop.me, 1);
    let mine = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    assert!(cx.debug_bounds("nav-needs-you").is_none(), "nothing waits yet");
    let top = |cx: &mut VisualTestContext, selector: &'static str| {
        f32::from(cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector}")).origin.y)
    };
    assert!(top(cx, "nav-workers") < top(cx, "nav-workspaces"));
    assert!(cx.debug_bounds(selector("nav-tile", mine.item)).is_some(), "a tile under its worker");

    view.update_in(cx, |v, _w, cx| v.agent_event(blocked(session), cx));
    cx.run_until_parked();
    assert!(top(cx, "nav-needs-you") < top(cx, "nav-workers"), "first while something waits");
    let waiting_row = leak(format!("nav-waiting-{session}"));
    assert!(cx.debug_bounds(waiting_row).is_some());
    let names = labels(&view, cx);
    assert!(names.iter().any(|l| l == "studio"), "a worker that is fine is its name: {names:#?}");
    assert!(names.iter().any(|l| l == "shell, Needs you"), "the waiting tile's row: {names:#?}");
    assert!(names.iter().any(|l| l == "Workspace 1, 2 tiles"), "{names:#?}");

    click(cx, "nav-worker-00000000000000000000000000000001");
    assert!(cx.debug_bounds(selector("nav-tile", mine.item)).is_none(), "folded away");
}

/// On a phone the navigator waits closed; ⌘B slides it in over a scrim, and choosing a tile
/// closes it again with the tile focused. The layout's own setting is left alone.
#[gpui::test]
fn on_a_phone_the_navigator_is_a_drawer_that_closes_on_a_choice(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let [(_, first), _, (_, third)] = three_shells(&view, cx, &fake);
    cx.simulate_resize(size(px(390.0), px(844.0)));
    cx.run_until_parked();
    assert!(cx.debug_bounds("navigator").is_none(), "closed until asked for");
    cx.simulate_keystrokes("cmd-b");
    cx.run_until_parked();
    assert!(cx.debug_bounds("navigator-away").is_some(), "a drawer over the scrim");
    assert_eq!(focused(&view, cx), Some(third));
    click(cx, selector("nav-tile", first.item));
    assert_eq!(focused(&view, cx), Some(first));
    assert!(cx.debug_bounds("navigator").is_none(), "out of the way of what was chosen");
    assert!(view.read_with(cx, |v, _| v.layout().navigator().shown), "the desktop's setting");
}

/// A tile's row focuses its tile and hands it the keyboard; a workspace's row goes there.
#[gpui::test]
fn a_tile_row_focuses_its_tile(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let [(first_session, first), _, (_, third)] = three_shells(&view, cx, &fake);
    assert_eq!(focused(&view, cx), Some(third));
    click(cx, selector("nav-tile", first.item));
    assert_eq!(focused(&view, cx), Some(first));
    assert!(terminal_focused(&view, cx, first_session), "the keyboard goes with it");
    view.update_in(cx, |v, _w, cx| {
        v.tick();
        v.layout.focus_workspace(1);
        v.after_focus_moved(cx);
        cx.notify();
    });
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.layout().active_workspace()), 1);
    click(cx, "nav-workspace-0");
    assert_eq!(view.read_with(cx, |v, _| v.layout().active_workspace()), 0);
    assert_eq!(focused(&view, cx), Some(first));
}

/// The status bar names the focused tile's worker and directory, the round trip there, and
/// the agents; the agents' summary goes to the one waiting.
#[gpui::test]
fn the_status_bar_reads_the_focused_tile_and_its_link(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let laptop = connect(&view, cx, 2, "laptop");
    let session = SessionId::new();
    let waiting = opens(&view, cx, &laptop, session, ClientId::new(), 1);
    let busy = SessionId::new();
    let _busy = opens(&view, cx, &studio, busy, ClientId::new(), 1);
    let _here = opens_in(&view, cx, &studio, SessionId::new(), studio.me, 2, Some("/w/oss/slopty"));
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| {
        v.set_rtt(key, Some(Duration::from_micros(4_240)), cx);
        v.agent_event(blocked(session), cx);
        v.agent_event(AgentEvent { status: AgentStatus::Working, ..blocked(busy) }, cx);
    });
    cx.run_until_parked();
    let names = labels(&view, cx);
    // The same place the header names: the last two directories, a home as `~`.
    for readout in ["studio", "oss/slopty", "rtt 4.2 ms", "1 working · 1 needs you"] {
        assert!(names.iter().any(|l| l == readout), "{readout}: {names:#?}");
    }
    assert!(cx.debug_bounds("rtt").is_none(), "the round trip left the title bar");
    click(cx, "status-agents");
    assert_eq!(focused(&view, cx), Some(waiting), "the summary goes to the one waiting");
}

/// The bell counts what waits and what finished unwatched, and its inbox lists both; a row
/// goes to its tile, which clears what it said.
#[gpui::test]
fn the_bell_counts_the_inbox_and_its_rows_go_there(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let asking = SessionId::new();
    let _asking = opens(&view, cx, &studio, asking, studio.me, 1);
    let built = SessionId::new();
    let built_tile = opens(&view, cx, &studio, built, studio.me, 2);
    let _last = opens(&view, cx, &studio, SessionId::new(), studio.me, 3);
    assert!(cx.debug_bounds("bell-count").is_none(), "nothing to count");
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(blocked(asking), cx);
        let done = Finished {
            command: "cargo build".into(),
            exit: Some(0),
            elapsed: Duration::from_secs(40),
        };
        v.command_finished(built, done, cx);
    });
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.inbox_count()), 2);
    assert!(labels(&view, cx).iter().any(|l| l == "2 new"), "the badge says how many");

    click(cx, "bell");
    assert!(cx.debug_bounds("inbox").is_some(), "the bell opens the inbox");
    assert!(
        cx.debug_bounds("inbox-needs-you").is_some() && cx.debug_bounds("inbox-finished").is_some()
    );
    click(cx, leak(format!("inbox-finished-{built}")));
    assert!(cx.debug_bounds("inbox").is_none(), "a row closes it");
    assert_eq!(focused(&view, cx), Some(built_tile));
    assert_eq!(view.read_with(cx, |v, _| v.inbox_count()), 1, "looked at, it is cleared");
}

/// With the toggle and a long name on the left, the column dots move right of them rather
/// than cover them, and never onto the buttons.
#[gpui::test]
fn the_column_dots_keep_clear_of_the_toggle_and_the_name(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let _shells = three_shells(&view, cx, &fake);
    view.update_in(cx, |v, _w, cx| {
        let name = "The workspace with the longest name anybody ever gave one of them, and more";
        v.layout.set_workspace_name(0, Some(name.to_owned()));
        cx.notify();
    });
    cx.run_until_parked();
    let bounds = |cx: &mut VisualTestContext, selector: &'static str| {
        cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is drawn"))
    };
    let (toggle, name, dots, bell) = (
        bounds(cx, "navigator-toggle"),
        bounds(cx, "workspace-name"),
        bounds(cx, "indicator"),
        bounds(cx, "bell"),
    );
    assert!(toggle.right() <= name.left(), "{toggle:?} {name:?}");
    assert!(name.right() <= dots.left(), "the dots cover the name: {name:?} {dots:?}");
    assert!(dots.right() <= bell.left(), "the dots cover the bell: {dots:?} {bell:?}");
    let centred = (VIEWPORT.0 - f32::from(dots.size.width)) / 2.0;
    assert!(f32::from(dots.left()) > centred, "pushed right of the middle by the name");
}

/// A worker whose link is up says nothing about it: no word and no mark in the navigator, the
/// palette or the empty workspace, only its round trip. Once the link drops, each says what is
/// wrong with the warn mark and a word, and so does the status bar.
#[gpui::test]
fn a_healthy_worker_says_nothing_and_a_lost_one_says_what_is_wrong(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let _laptop = connect(&view, cx, 2, "laptop");
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| v.set_rtt(key, Some(Duration::from_micros(4_240)), cx));
    cx.run_until_parked();
    let names = labels(&view, cx);
    assert!(names.iter().any(|l| l == "studio") && names.iter().any(|l| l == "laptop"));
    assert!(!names.iter().any(|l| l.contains("connected")), "{names:#?}");
    let marks = |cx: &mut VisualTestContext| {
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        tree.into_iter().filter(|n| n.role == "Image").filter_map(|n| n.label).collect::<Vec<_>>()
    };
    assert!(!marks(cx).iter().any(|m| m == "Done"), "no success mark: {:?}", marks(cx));
    let workers = view.read_with(cx, |v, _| {
        v.worker_lines().map(|l| (l.label, l.keys, l.status)).collect::<Vec<_>>()
    });
    assert_eq!(
        workers,
        [
            ("Go to studio".to_owned(), "4.2 ms".to_owned(), None),
            ("Go to laptop".to_owned(), String::new(), None),
        ]
    );

    view.update_in(cx, |v, _w, cx| {
        v.disconnect_worker(key, WorkerStatus::Reconnecting("lost".into()), cx);
    });
    cx.run_until_parked();
    let names = labels(&view, cx);
    assert!(names.iter().any(|l| l == "studio, reconnecting"), "{names:#?}");
    assert!(names.iter().any(|l| l == "reconnecting"), "the status bar says so: {names:#?}");
    assert!(marks(cx).iter().any(|m| m == "Away"), "{:?}", marks(cx));
    let studio_line = view.read_with(cx, |v, _| {
        v.worker_lines().find(|l| l.label == "Go to studio").map(|l| (l.keys, l.status))
    });
    assert_eq!(studio_line, Some(("reconnecting".to_owned(), Some(crate::icons::Status::Away))));
}

/// A worker's tiles are listed by what they want: the one that needs the human, then the one
/// that finished unseen, then the one at work, then the idle ones in reading order.
#[gpui::test]
fn a_workers_tiles_come_in_order_of_attention(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let sessions: [SessionId; 5] = std::array::from_fn(|_| SessionId::new());
    let tiles: Vec<TileRef> = (1_u64..)
        .zip(sessions)
        .map(|(version, session)| opens(&view, cx, &studio, session, studio.me, version))
        .collect();
    let [_idle, busy, built, asking, _last] = sessions;
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(AgentEvent { status: AgentStatus::Working, ..blocked(busy) }, cx);
        v.agent_event(blocked(asking), cx);
        let done = Finished {
            command: "cargo build".into(),
            exit: Some(0),
            elapsed: Duration::from_secs(40),
        };
        v.command_finished(built, done, cx);
    });
    cx.run_until_parked();
    let mut rows: Vec<(f32, usize)> = tiles
        .iter()
        .enumerate()
        .map(|(ix, t)| {
            let at = cx.debug_bounds(selector("nav-tile", t.item)).expect("a row per tile");
            (f32::from(at.origin.y), ix)
        })
        .collect();
    rows.sort_by(|a, b| a.0.total_cmp(&b.0));
    let order: Vec<usize> = rows.into_iter().map(|(_, ix)| ix).collect();
    assert_eq!(order, [3, 2, 1, 0, 4], "needs you, unseen, working, then idle in reading order");
}

/// A tile whose long command finished while the human looked elsewhere carries the unseen
/// dot on its row's status lane. The dot stands aside while the tile is at work, and goes once
/// the tile is looked at.
#[gpui::test]
fn an_unseen_dot_marks_a_finished_tile_until_it_is_looked_at(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let built = SessionId::new();
    let tile = opens(&view, cx, &studio, built, studio.me, 1);
    let _last = opens(&view, cx, &studio, SessionId::new(), studio.me, 2);
    let dot = selector("nav-unseen", tile.item);
    assert!(cx.debug_bounds(dot).is_none(), "nothing unseen yet");
    view.update_in(cx, |v, _w, cx| {
        let done =
            Finished { command: "make".into(), exit: Some(2), elapsed: Duration::from_secs(9) };
        v.command_finished(built, done, cx);
    });
    cx.run_until_parked();
    let (lane, at) = (
        cx.debug_bounds(selector("nav-tile", tile.item)).expect("the row"),
        cx.debug_bounds(dot).expect("the dot"),
    );
    assert!(lane.right() >= at.right() && at.left() > lane.center().x, "on the lane's edge");
    assert!(labels(&view, cx).iter().any(|l| l == "shell, Failed, unseen"));

    view.update_in(cx, |v, _w, cx| {
        v.agent_event(AgentEvent { status: AgentStatus::Working, ..blocked(built) }, cx);
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds(dot).is_none(), "not while it works");
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(AgentEvent { status: AgentStatus::Idle, ..blocked(built) }, cx);
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds(dot).is_some(), "back once it rests");
    click(cx, selector("nav-tile", tile.item));
    assert!(cx.debug_bounds(dot).is_none(), "looked at");
}

/// How many times the workspace draws in one second of a 120 Hz display: each tick delivers
/// whatever frame was asked for, then runs what is due.
fn frames_in_a_second(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> u64 {
    let before = view.read_with(cx, |v, _| v.frames_drawn);
    for _ in 0..120 {
        cx.executor().advance_clock(Duration::from_nanos(8_333_333));
        cx.update(Window::simulate_next_frame);
        cx.run_until_parked();
    }
    view.read_with(cx, |v, _| v.frames_drawn).wrapping_sub(before)
}

/// A working agent in view turns its mark twelve steps a second, and the workspace draws
/// twelve frames a second for it, not one per display refresh. With nothing at work it draws
/// none, and under Reduce Motion the mark stands still and draws none either.
#[gpui::test]
fn a_working_mark_draws_twelve_frames_a_second_and_none_at_rest(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let busy = SessionId::new();
    let _busy = opens(&view, cx, &studio, busy, studio.me, 1);
    assert_eq!(frames_in_a_second(&view, cx), 0, "at rest, nothing draws");

    let working = AgentEvent { status: AgentStatus::Working, ..blocked(busy) };
    view.update_in(cx, |v, _w, cx| v.agent_event(working.clone(), cx));
    cx.run_until_parked();
    let turning = frames_in_a_second(&view, cx);
    assert!((11..=13).contains(&turning), "{turning} frames in a second");

    view.update_in(cx, |v, _w, cx| {
        v.agent_event(AgentEvent { status: AgentStatus::Idle, ..blocked(busy) }, cx);
    });
    cx.run_until_parked();
    assert!(frames_in_a_second(&view, cx) <= 1, "the last step's timer at most");
    assert_eq!(frames_in_a_second(&view, cx), 0, "then nothing");

    cx.update(|_w, cx| cx.set_reduce_motion(true));
    view.update_in(cx, |v, _w, cx| v.agent_event(working, cx));
    cx.run_until_parked();
    assert_eq!(frames_in_a_second(&view, cx), 0, "Reduce Motion: a still mark");
}

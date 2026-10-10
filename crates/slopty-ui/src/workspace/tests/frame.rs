//! The frame around the panes in the headless workspace: the navigator, the title bar's
//! items and the bell.

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
    read_layout(path).ok().flatten().expect("written").navigator
}

/// How the navigator sits: docked where the panes keep a desktop's width beside it, and on an
/// iPad where they keep a regular width; over the panes on an iPad narrower than that or a
/// window that would leave the panes a phone's; a drawer on a phone.
#[test]
fn the_navigator_docks_only_where_the_panes_keep_their_room() {
    use navigator::{Mode, mode};
    assert_eq!(mode(1200.0, 248.0, 700.0, false), Mode::Docked);
    assert_eq!(mode(900.0, 248.0, 700.0, false), Mode::Overlay, "the panes would be a phone's");
    assert_eq!(mode(1376.0, 248.0, 700.0, true), Mode::Docked, "a 13-inch iPad in landscape");
    assert_eq!(mode(1210.0, 248.0, 700.0, true), Mode::Docked, "an 11-inch iPad in landscape");
    assert_eq!(mode(1032.0, 248.0, 700.0, true), Mode::Overlay, "a 13-inch iPad upright");
    assert_eq!(mode(1032.0, 248.0, 700.0, false), Mode::Docked, "a Mac window as wide");
    assert_eq!(mode(390.0, 248.0, 700.0, true), Mode::Home, "a phone");
}

/// A window too narrow to dock the navigator folds it to the rail rather than to nothing, as
/// `MonoCode`'s compact rail: the panes start past it, and the panel opened over them leaves the
/// rail and the panes where they were.
#[gpui::test]
fn a_narrow_window_folds_the_navigator_to_its_rail(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    cx.simulate_resize(size(px(800.0), px(700.0)));
    cx.run_until_parked();
    assert!(cx.debug_bounds("navigator").is_none(), "not docked, not open");
    let rail = cx.debug_bounds("nav-rail").expect("the rail");
    let area = cx.debug_bounds("area").expect("the panes");
    assert!(area.left() >= rail.right() - px(0.5), "the panes past it: {rail:?} {area:?}");
    assert!((f32::from(rail.size.width) - navigator::RAIL_W).abs() < 0.5);
    let machine = leak(format!("nav-rail-{}", studio.key));
    let machine = cx.debug_bounds(machine).expect("the machine its shell is on");
    assert!(rail.contains(&machine.center()), "in the rail: {machine:?}");

    view.update_in(cx, |v, window, cx| v.toggle_navigator(&ToggleNavigator, window, cx));
    cx.run_until_parked();
    let panel = cx.debug_bounds("navigator").expect("open over the panes");
    assert_eq!(cx.debug_bounds("nav-rail"), Some(rail), "the rail stays");
    assert_eq!(cx.debug_bounds("area"), Some(area), "and the panes do not move");
    assert!(panel.right() > area.left(), "over them");

    cx.simulate_resize(size(px(390.0), px(844.0)));
    cx.run_until_parked();
    assert!(cx.debug_bounds("nav-rail").is_none(), "a phone has its drawer, no rail");
}

/// ⌘B hides the docked navigator and the panes take its room but the rail's; ⌘B brings it
/// back. Whether it shows is written with the layout, and a workspace made from that file
/// starts the same way.
#[gpui::test]
fn cmd_b_hides_and_shows_the_navigator_and_the_layout_keeps_it(cx: &mut TestAppContext) {
    let (dir, path) = temp_layout();
    let (view, cx) = workspace(cx);
    view.update(cx, |v, _| v.set_layout_path(path.clone()));
    let studio = connect(&view, cx, 1, "studio");
    let _shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let area_w = |cx: &mut VisualTestContext| {
        f32::from(cx.debug_bounds("area").expect("the panes' area is drawn").size.width)
    };
    assert!(cx.debug_bounds("navigator").is_some(), "docked by default on a wide window");
    let beside = area_w(cx);

    cx.simulate_keystrokes("cmd-b");
    cx.run_until_parked();
    assert!(cx.debug_bounds("navigator").is_none(), "hidden");
    assert!(cx.debug_bounds("nav-rail").is_some(), "the rail in its place");
    let alone = area_w(cx);
    let freed = Navigator::DEFAULT_WIDTH - navigator::RAIL_W;
    assert!((alone - beside - freed).abs() < 1.0, "{beside} → {alone}");
    assert!(!saved_navigator(cx, &path).shown, "the layout keeps it hidden");
    let saved = read_layout(&path).ok().flatten().expect("written");
    let restored =
        cx.update(|_w, cx| cx.new(|cx| WorkspaceView::new(Theme::default(), Some(saved), cx)));
    assert!(!restored.read_with(cx, |v, _| v.navigator().shown), "and starts hidden");

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
    let saved = read_layout(&path).ok().flatten().expect("written");
    let restored =
        cx.update(|_w, cx| cx.new(|cx| WorkspaceView::new(Theme::default(), Some(saved), cx)));
    near(restored.read_with(cx, |v, _| v.navigator_width()), Navigator::MIN_WIDTH);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// *Needs you* shows while an agent waits, in view or not, above *Workers*, whose heading
/// shows only then: alone it would head nothing. The waiting tile's own row says so too, and
/// folding its worker away leaves the section as it was. Each worker lists its tiles beneath it,
/// with an accessible name, until its row folds them.
#[gpui::test]
fn the_navigator_lists_what_needs_you_then_the_workers(cx: &mut TestAppContext) {
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
    assert!(cx.debug_bounds("nav-workers").is_none(), "a lone section has no heading");
    assert!(cx.debug_bounds(selector("nav-tile", mine.item)).is_some(), "a tile under its worker");

    view.update_in(cx, |v, _w, cx| v.agent_event(blocked(session), cx));
    cx.run_until_parked();
    let waiting_row = leak(format!("nav-waiting-{session}"));
    assert!(top(cx, "nav-needs-you") < top(cx, "nav-workers"), "it leads");
    assert!(cx.debug_bounds(waiting_row).is_some(), "in view or not");
    let names = labels(&view, cx);
    assert!(names.iter().any(|l| l == "studio"), "a worker that is fine is its name: {names:#?}");
    assert!(
        names.iter().any(|l| l == "Claude Code, Has a question"),
        "the waiting tile's row, in the state's own word: {names:#?}"
    );

    click(cx, leak(format!("nav-worker-{}", laptop.key)));
    assert!(cx.debug_bounds(waiting_row).is_some(), "folded away, it stays");
    click(cx, leak(format!("nav-worker-{}", laptop.key)));

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
    assert!(view.read_with(cx, |v, _| v.navigator().shown), "the desktop's setting");
}

/// A tile's row focuses its tile and hands it the keyboard, and shows its project first where
/// another is on show.
#[gpui::test]
fn a_tile_row_focuses_its_tile(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let [(first_session, first), _, (_, third)] = three_shells(&view, cx, &fake);
    assert_eq!(focused(&view, cx), Some(third));
    click(cx, selector("nav-tile", first.item));
    assert_eq!(focused(&view, cx), Some(first));
    assert!(terminal_focused(&view, cx, first_session), "the keyboard goes with it");
    let laptop = connect(&view, cx, 2, "laptop");
    let theirs = opens(&view, cx, &laptop, SessionId::new(), ClientId::new(), 1);
    view.update_in(cx, |v, _w, cx| v.focus_tile(theirs, cx));
    cx.run_until_parked();
    let shown = |cx: &VisualTestContext| view.read_with(cx, |v, _| v.layout().shown_index());
    let elsewhere = shown(cx);
    click(cx, selector("nav-tile", first.item));
    assert_ne!(shown(cx), elsewhere, "its project on show");
    assert_eq!(focused(&view, cx), Some(first));
}

/// The navigator names the workers and the breadcrumb the directory; a slow link's round trip
/// is the navigator's, and no readout repeats any of it. The bell counts the one blocked, as
/// its way there.
#[gpui::test]
fn the_frame_says_where_the_focused_tile_is_once(cx: &mut TestAppContext) {
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
        v.set_rtt(key, Some(Duration::from_micros(42_400)), cx);
        v.agent_event(blocked(session), cx);
        v.agent_event(AgentEvent { status: AgentStatus::Working, ..blocked(busy) }, cx);
    });
    cx.run_until_parked();
    let names = labels(&view, cx);
    // Outside a repository the checkout is the directory's own name.
    for readout in ["studio", "slopty"] {
        assert!(names.iter().any(|l| l == readout), "{readout}: {names:#?}");
    }
    assert!(!names.iter().any(|l| l.starts_with("Round trip")), "no readout: {names:#?}");
    assert!(cx.debug_bounds("readouts").is_none(), "the title bar says none of it again");
    assert!(!names.iter().any(|l| l.contains("1 working")), "no agent counts: {names:#?}");
    assert!(cx.debug_bounds("rtt").is_none(), "the round trip left the title bar");
    // The bell stands in the bar while the navigator is hidden.
    assert!(cx.debug_bounds("bell").is_none(), "the navigator's Needs you says it");
    cx.simulate_keystrokes("cmd-b");
    cx.run_until_parked();
    let count = cx.debug_bounds("bell-count").expect("the bell counts the one waiting");
    assert_eq!(count.size, size(px(0.0), px(0.0)), "said, not drawn: no disc on the bell");
    let tree = tree(cx);
    assert!(tree.iter().any(|n| n.is("Status", Some("1 new"))), "the count is said: {tree:#?}");
    let amber = gpui::Background::from(crate::colors::hsla(Theme::default().surfaces.warn_fill));
    let bell = cx.debug_bounds("bell").expect("the bell");
    let disc = cx.update(|window, _| {
        window.painted_quads().into_iter().any(|q| {
            q.background == amber && {
                let scale = window.scale_factor();
                let at = point(px(q.bounds.origin.x.0 / scale), px(q.bounds.origin.y.0 / scale));
                bell.contains(&at)
            }
        })
    });
    assert!(!disc, "no amber disc over the bell");
    cx.simulate_keystrokes("cmd-shift-a");
    cx.run_until_parked();
    assert_eq!(focused(&view, cx), Some(waiting), "⌘⇧A still goes to the one waiting");
}

/// The bell counts what waits and an agent's turn that ended unseen, never a shell's finish;
/// the turn's row under *To review* goes to its tile, which clears what it said.
#[gpui::test]
fn the_bell_counts_what_needs_you_and_a_rows_tile_clears_it(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let asking = SessionId::new();
    let _asking = opens(&view, cx, &studio, asking, studio.me, 1);
    let turned = SessionId::new();
    let turned_tile = opens(&view, cx, &studio, turned, studio.me, 2);
    let built = SessionId::new();
    let _built = opens(&view, cx, &studio, built, studio.me, 3);
    let _last = opens(&view, cx, &studio, SessionId::new(), studio.me, 4);
    assert!(cx.debug_bounds("bell-count").is_none(), "nothing to count");
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(blocked(asking), cx);
        let at = |s: u64| WallMs::from_millis(s.saturating_mul(1000));
        let working =
            AgentEvent { status: AgentStatus::Working, since_ms: at(100), ..blocked(turned) };
        v.agent_event(working, cx);
        let done = AgentEvent { status: AgentStatus::Done, since_ms: at(220), ..blocked(turned) };
        v.agent_event(done, cx);
        let done = Finished {
            command: "cargo build".into(),
            exit: Some(0),
            elapsed: Duration::from_secs(40),
            turn: None,
        };
        v.command_finished(built, done, cx);
    });
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.bell_count()), 2);
    cx.simulate_keystrokes("cmd-b");
    cx.run_until_parked();
    assert!(labels(&view, cx).iter().any(|l| l == "2 new"), "the badge says how many");
    cx.simulate_keystrokes("cmd-b");
    cx.run_until_parked();

    assert!(
        cx.debug_bounds("nav-needs-you").is_some() && cx.debug_bounds("nav-to-review").is_some()
    );
    click(cx, leak(format!("nav-review-{turned}")));
    assert_eq!(focused(&view, cx), Some(turned_tile));
    assert_eq!(view.read_with(cx, |v, _| v.bell_count()), 1, "looked at, it is cleared");
}

/// The bar runs from the docked navigator's right edge, and holds no breadcrumb or bell while the
/// navigator stands; the toggle stays in the navigator's top row, clear of the bar. With the
/// navigator hidden the breadcrumb returns, clear of the bell however long the project's name.
#[gpui::test]
fn the_bar_keeps_clear_of_the_toggle_and_the_breadcrumb(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let _shells = three_shells(&view, cx, &fake);
    let bounds = |cx: &mut VisualTestContext, selector: &'static str| {
        cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is drawn"))
    };
    view.update_in(cx, |v, _w, cx| {
        let name = "The project with the longest name anybody ever gave one of them, and more";
        let home = v.layout.shown_project().map(|p| p.home().clone()).expect("a project");
        v.layout.set_name(&home, Some(name.to_owned()));
        cx.notify();
    });
    cx.run_until_parked();
    let (navigator, toggle, titlebar) =
        (bounds(cx, "navigator"), bounds(cx, "navigator-toggle"), bounds(cx, "titlebar"));
    assert!(toggle.right() <= navigator.right(), "the toggle is the navigator's: {toggle:?}");
    assert!(navigator.right() <= titlebar.left() + px(0.5), "the bar starts at its edge");
    for absent in ["crumb-project", "bell"] {
        assert!(cx.debug_bounds(absent).is_none(), "{absent}: the navigator says it");
    }
    cx.simulate_keystrokes("cmd-b");
    cx.run_until_parked();
    let (crumbs, bell) = (bounds(cx, "crumb-project"), bounds(cx, "bell"));
    assert!(crumbs.right() <= bell.left(), "{crumbs:?} {bell:?}");
}

/// A worker whose link is up says nothing about it: no word and no mark in the navigator, the
/// palette or the empty workspace, and a round trip under `RTT_SHOWN_FROM` is not named. Once the
/// link drops, each says what is wrong with the warn mark and a word.
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
        [("studio".to_owned(), String::new(), None), ("laptop".to_owned(), String::new(), None),]
    );

    view.update_in(cx, |v, _w, cx| {
        v.disconnect_worker(key, WorkerStatus::Reconnecting("lost".into()), cx);
    });
    cx.run_until_parked();
    let names = labels(&view, cx);
    assert!(names.iter().any(|l| l == "studio, reconnecting"), "{names:#?}");
    assert!(marks(cx).iter().any(|m| m == "Away"), "{:?}", marks(cx));
    let studio_line = view.read_with(cx, |v, _| {
        v.worker_lines().find(|l| l.label == "studio").map(|l| (l.keys, l.status))
    });
    assert_eq!(studio_line, Some(("Reconnecting".to_owned(), Some(crate::icons::Status::Away))));
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
            turn: None,
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

/// A tile whose long command finished well while the human looked elsewhere carries the
/// unseen check at its row's end. The check stands aside while the tile is at work, and goes
/// once the tile is looked at. One that failed ends its row with the failure's mark instead, which
/// comes first.
#[gpui::test]
fn an_unseen_check_marks_a_finished_tile_until_it_is_looked_at(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let built = SessionId::new();
    let tile = opens(&view, cx, &studio, built, studio.me, 1);
    let _last = opens(&view, cx, &studio, SessionId::new(), studio.me, 2);
    let check = selector("nav-unseen", tile.item);
    assert!(cx.debug_bounds(check).is_none(), "nothing unseen yet");
    view.update_in(cx, |v, _w, cx| {
        let done = Finished {
            command: "make".into(),
            exit: Some(0),
            elapsed: Duration::from_secs(40),
            turn: None,
        };
        v.command_finished(built, done, cx);
    });
    cx.run_until_parked();
    let (lane, at) = (
        cx.debug_bounds(selector("nav-tile", tile.item)).expect("the row"),
        cx.debug_bounds(check).expect("the check"),
    );
    assert!(lane.right() >= at.right() && at.left() > lane.center().x, "on the lane's edge");
    let said = labels(&view, cx);
    assert!(said.iter().any(|l| l.starts_with("make, ") && l.ends_with(", unseen")), "{said:?}");
    view.update_in(cx, |v, _w, cx| {
        let failed = Finished {
            command: "make".into(),
            exit: Some(2),
            elapsed: Duration::from_secs(40),
            turn: None,
        };
        v.command_finished(built, failed, cx);
    });
    cx.run_until_parked();
    let failed = cx.debug_bounds(selector("nav-state", tile.item)).expect("the failure's mark");
    assert!(cx.debug_bounds(check).is_none(), "the failure comes first");
    assert!(lane.right() >= failed.right() && failed.left() > lane.center().x, "{failed:?}");
    assert!(labels(&view, cx).iter().any(|l| l == "make, Failed, unseen"), "named by what it ran");

    view.update_in(cx, |v, _w, cx| {
        v.agent_event(AgentEvent { status: AgentStatus::Working, ..blocked(built) }, cx);
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds(check).is_none(), "not while it works");
    view.update_in(cx, |v, _w, cx| {
        v.agent_event(AgentEvent { status: AgentStatus::Idle, ..blocked(built) }, cx);
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds(check).is_some(), "back once it rests");
    click(cx, selector("nav-tile", tile.item));
    assert!(cx.debug_bounds(check).is_none(), "looked at");
}

/// How many times the workspace draws in one second of a 120 Hz display: each tick delivers
/// whatever frame was asked for, then runs what is due. Counted by the panes' builds or the
/// title bar strip's, which is a tab's one tile's header, whichever built more: one frame may
/// build both.
fn frames_in_a_second(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext) -> u64 {
    let builds = |cx: &mut VisualTestContext| {
        view.read_with(cx, |v, cx| {
            let strip = u64::try_from(v.chrome.tile_strip.read(cx).renders).unwrap_or(u64::MAX);
            (v.drawn.builds.get(), strip)
        })
    };
    let (area, strip) = builds(cx);
    for _ in 0..120 {
        cx.executor().advance_clock(Duration::from_nanos(8_333_333));
        cx.update(Window::simulate_next_frame);
        cx.run_until_parked();
    }
    let (area_after, strip_after) = builds(cx);
    area_after.wrapping_sub(area).max(strip_after.wrapping_sub(strip))
}

/// A working agent in view turns its mark twelve steps a second, and the workspace draws
/// twelve frames a second for it, not one per display refresh. With nothing at work it draws
/// none, and under Reduce Motion the mark stands upright and breathes, five frames a second.
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
    let breathing = frames_in_a_second(&view, cx);
    assert!((4..=6).contains(&breathing), "Reduce Motion: {breathing} breaths in a second");
}

/// The app holding the workspace in less than its window, as an iPad's Split View does in the
/// self-test.
struct Narrow(Entity<WorkspaceView>);

impl gpui::Render for Narrow {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl gpui::IntoElement {
        use gpui::{ParentElement as _, Styled as _};
        gpui::div().w(px(NARROW)).h_full().child(self.0.clone())
    }
}

/// The width [`Narrow`] gives the workspace: a phone's, in a desktop's window.
const NARROW: f32 = 500.0;

/// The bars and the navigator fit the workspace's own width, not the window's: laid out in
/// 500 points of a 1200-point window, "…" stays inside the workspace's edge, and the navigator
/// is a phone's home that fills it and no more.
#[gpui::test]
fn the_frame_fits_the_workspace_not_the_window(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.bind_keys(key_bindings());
        cx.bind_keys(crate::terminal::key_bindings());
    });
    let (host, cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| {
            let mut view = WorkspaceView::new(Theme::default(), None, cx);
            view.set_animation(false);
            view
        });
        let focus = view.read(cx).focus.clone();
        window.focus(&focus, cx);
        Narrow(view)
    });
    cx.simulate_resize(size(px(VIEWPORT.0), px(VIEWPORT.1)));
    cx.run_until_parked();
    let view = host.read_with(cx, |host, _| host.0.clone());
    let fake = connect(&view, cx, 1, "studio");
    let _shells = three_shells(&view, cx, &fake);

    let (bell, more) = (
        cx.debug_bounds("bell").expect("the bell is drawn"),
        cx.debug_bounds("more").expect("… is drawn"),
    );
    assert!(bell.right() <= more.left(), "the bell runs into …: {bell:?} {more:?}");
    assert!(f32::from(more.right()) <= NARROW, "… is past the edge: {more:?}");

    cx.simulate_keystrokes("cmd-b");
    cx.run_until_parked();
    assert!(cx.debug_bounds("navigator-away").is_some(), "a phone's home, not docked");
    let home = cx.debug_bounds("navigator").expect("drawn");
    assert!((f32::from(home.right()) - NARROW).abs() < 0.5, "the home fits: {home:?}");
}

/// A worker's round trip shows only when it is slow enough to matter, on the row's right edge.
/// Under the pointer the chevron and "+" take the readouts' place; folded, the rollup comes
/// before them. Neither moves the round trip.
#[gpui::test]
fn a_slow_round_trip_shows_on_the_right_edge_and_holds_still(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let laptop = connect(&view, cx, 2, "laptop");
    let busy = SessionId::new();
    let _mine = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let _busy = opens(&view, cx, &laptop, busy, laptop.me, 1);
    let (fast, slow) = (studio.key, laptop.key);
    view.update_in(cx, |v, _w, cx| {
        v.set_rtt(fast, Some(Duration::from_micros(4_240)), cx);
        v.set_rtt(slow, Some(Duration::from_millis(31)), cx);
        v.agent_event(AgentEvent { status: AgentStatus::Working, ..blocked(busy) }, cx);
    });
    let far = point(px(VIEWPORT.0 - 10.0), px(VIEWPORT.1 / 2.0));
    cx.simulate_mouse_move(far, None, Modifiers::default());
    cx.run_until_parked();
    let at = |cx: &mut VisualTestContext, what: &str, key: WorkerKey| {
        let selector = leak(format!("{what}-{key}"));
        cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is drawn"))
    };
    assert!(cx.debug_bounds(leak(format!("nav-rtt-{fast}"))).is_none(), "a fast link is quiet");
    assert!(cx.debug_bounds(leak(format!("nav-count-{slow}"))).is_none(), "the rows count");
    let rtt = at(cx, "nav-rtt", slow);
    let header = at(cx, "nav-worker", slow);
    // The row's wash sits a base unit in from the panel; its content ends on the edge grid.
    let pad = theme().spacing.inset() - theme().spacing.xs;
    near(f32::from(rtt.right()), f32::from(header.right()) - pad);

    cx.simulate_mouse_move(header.center(), None, Modifiers::default());
    cx.run_until_parked();
    let actions = at(cx, "nav-new-shell", slow);
    assert!(
        cx.debug_bounds(leak(format!("nav-rtt-{slow}"))).is_none(),
        "the actions take its place"
    );
    assert!(actions.right() <= rtt.right() + px(0.5), "within the readouts' edge");
    click(cx, leak(format!("nav-worker-{slow}")));
    cx.simulate_mouse_move(far, None, Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds(leak(format!("nav-rollup-{slow}"))).is_some(), "folded, the rollup");
    // Folded, its working agent is listed under *Working* above, which moves the whole row
    // down; within the row the round trip stays put.
    let (moved, row) = (at(cx, "nav-rtt", slow), at(cx, "nav-worker", slow));
    assert_eq!(moved.size, rtt.size, "the rollup before it");
    assert_eq!(moved.origin.x, rtt.origin.x, "back where it was");
    assert_eq!(moved.top() - row.top(), rtt.top() - header.top(), "on the row's line");
}

/// On touch a header keeps only its chevron, at rest after the readouts, since a finger has no
/// hover: a machine's "+" and "…" and a project's "+" are its long press's menu, which leads
/// with "New shell here" and opens one there. With a pointer they wait for it.
#[gpui::test]
fn a_finger_finds_a_headers_actions_in_its_menu(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut studio = connect(&view, cx, 1, "studio");
    let key = studio.key;
    let (_, atlas) = palette::shell_in(&view, cx, &studio, 1, "/w/atlas", true);
    view.update_in(cx, |v, _w, cx| v.set_rtt(key, Some(Duration::from_millis(31)), cx));
    let far = point(px(VIEWPORT.0 - 10.0), px(VIEWPORT.1 / 2.0));
    cx.simulate_mouse_move(far, None, Modifiers::default());
    cx.run_until_parked();
    let group = view
        .read_with(cx, |v, _| v.project_groups().group_of(atlas).map(|g| g.key.clone()))
        .expect("atlas's group");
    let add_here = leak(format!("nav-group-new-shell-{group}"));
    assert!(cx.debug_bounds(add_here).is_none(), "a pointer's wait for it");

    let touch = Theme { density: slopty_theme::Density::TOUCH, ..Theme::default() };
    view.update(cx, |v, cx| v.set_theme(touch, cx));
    cx.run_until_parked();
    let drawn = |cx: &mut VisualTestContext, what: &str| {
        cx.debug_bounds(leak(format!("{what}-{key}"))).is_some()
    };
    assert!(drawn(cx, "nav-rtt"), "the readout at rest");
    assert!(!drawn(cx, "nav-new-shell"), "no \"+\" on a finger's header");
    assert!(!drawn(cx, "nav-machine-menu"), "no \"…\" either");
    assert!(cx.debug_bounds(add_here).is_none(), "nor a project's \"+\"");
    let header = cx.debug_bounds(leak(format!("nav-worker-{key}"))).expect("the header");

    cx.simulate_mouse_down(header.center(), MouseButton::Right, Modifiers::default());
    cx.run_until_parked();
    let rows: Vec<String> =
        tree(cx).into_iter().filter(|n| n.role == "MenuItem").filter_map(|n| n.label).collect();
    assert_eq!(rows.first().map(String::as_str), Some("New shell here"), "{rows:?}");
    let _seen = studio.drain();
    click(cx, "menu-New shell here");
    let sent = studio.drain();
    assert!(
        sent.iter()
            .any(|m| matches!(m, ClientMsg::OpenSession { spec: o, .. } if o.command.is_empty())),
        "a shell on that machine: {sent:?}"
    );
}

/// A pinned round trip is what every readout shows, whatever the link measures, and only for
/// a worker that has one; the link's own figure stays for the predictors and the dump.
#[gpui::test]
fn a_pinned_round_trip_is_what_the_readouts_show(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let laptop = connect(&view, cx, 2, "laptop");
    let _mine = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    let far = point(px(VIEWPORT.0 - 10.0), px(VIEWPORT.1 / 2.0));
    cx.simulate_mouse_move(far, None, Modifiers::default());
    let slow = Duration::from_millis(57);
    view.update_in(cx, |v, _w, cx| {
        v.pin_rtt_readout(Some(Duration::from_millis(1)), cx);
        v.set_rtt(studio.key, Some(slow), cx);
    });
    cx.run_until_parked();
    let readout = |cx: &mut VisualTestContext, key: WorkerKey| {
        cx.debug_bounds(leak(format!("nav-rtt-{key}"))).is_some()
    };
    assert!(!readout(cx, studio.key), "the pinned figure is quiet, not the link's 57 ms");
    assert_eq!(view.read_with(cx, |v, _| v.rtt(studio.key)), Some(slow), "the link's own");
    let shown = view.read_with(cx, |v, _| v.workers.get(&laptop.key).and_then(|w| v.shown_rtt(w)));
    assert_eq!(shown, None, "no figure for a worker that has none");

    view.update_in(cx, |v, _w, cx| v.pin_rtt_readout(Some(Duration::from_millis(31)), cx));
    cx.run_until_parked();
    assert!(readout(cx, studio.key), "a pinned slow figure shows");
    view.update_in(cx, |v, _w, cx| v.pin_rtt_readout(None, cx));
    cx.run_until_parked();
    assert!(readout(cx, studio.key), "unpinned, the link's own");
}

fn theme() -> Theme {
    Theme::default()
}

/// A window narrowed past where the navigator docks is laid out for its new width at once.
/// Were the frame still sized by the window before, the navigator would dock for that frame,
/// the panes would pass through a phone's width, and the view would keep a phone's strut.
#[gpui::test]
fn narrowing_the_window_never_lays_the_panes_out_as_a_phone(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let [(_, first), ..] = three_shells(&view, cx, &fake);
    click(cx, selector("nav-tile", first.item));
    let before = cx.debug_bounds(selector("item", first.item)).expect("drawn");
    let area_left =
        |cx: &mut VisualTestContext| view.read_with(cx, |v, _| v.drawn.viewport.get().left());
    assert_eq!(before.left(), area_left(cx), "the first pane starts the area");

    cx.simulate_resize(size(px(900.0), px(600.0)));
    cx.run_until_parked();
    assert!(cx.debug_bounds("navigator").is_none(), "no room to dock: it waits closed");
    let after = cx.debug_bounds(selector("item", first.item)).expect("drawn");
    assert_eq!(after.left(), area_left(cx), "no strut before it: {after:?}");
}

/// The window is solid: an opaque ground under the title bar, and the panels opaque on it.
/// Nothing shows through from behind the window, and nothing is translucent but the washes.
#[gpui::test]
fn the_window_is_one_opaque_ground(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let shell = opens(&view, cx, &studio, SessionId::new(), studio.me, 1);
    cx.run_until_parked();
    let theme = theme();
    let ground = crate::colors::hsla(theme.surfaces.ground);
    let (scale, quads) = cx.update(|w, _| (w.scale_factor(), w.painted_quads()));
    let bar = cx.debug_bounds("titlebar").expect("the title bar").scale(scale);
    let under_bar = quads.iter().any(|q| {
        q.bounds.origin.y.0 <= bar.origin.y.0 + 0.5
            && q.bounds.size.height.0 >= bar.size.height.0 - 0.5
            && q.background.as_solid() == Some(ground)
    });
    assert!(under_bar, "the ground, opaque, under the title bar");
    assert!((ground.a - 1.0).abs() < f32::EPSILON, "the ground is opaque");
    let tile = cx.debug_bounds(selector("item", shell.item)).expect("the shell").scale(scale);
    let content = crate::colors::hsla(theme.content());
    let panel = quads.iter().find(|q| {
        (q.bounds.origin.x.0 - tile.origin.x.0).abs() < 0.5
            && q.background.as_solid() == Some(content)
    });
    assert!(panel.is_some(), "the panel is opaque");
}

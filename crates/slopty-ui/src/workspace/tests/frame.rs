//! The frame around the strip in the headless workspace: the navigator, the status bar, the
//! title bar's items and the bell.

use gpui::{AppContext as _, Modifiers, MouseButton};
use slopty_client::layout::Navigator;

use super::*;
use crate::workspace::marks;

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

/// How the navigator sits: docked where the strip keeps a desktop's width beside it, and on an
/// iPad where it keeps a regular width; over the strip on an iPad narrower than that or a
/// window that would leave the strip a phone's; a drawer on a phone.
#[test]
fn the_navigator_docks_only_where_the_strip_keeps_its_room() {
    use navigator::{Mode, mode};
    assert_eq!(mode(1200.0, 248.0, 700.0, false), Mode::Docked);
    assert_eq!(mode(900.0, 248.0, 700.0, false), Mode::Overlay, "the strip would be a phone's");
    assert_eq!(mode(1376.0, 248.0, 700.0, true), Mode::Docked, "a 13-inch iPad in landscape");
    assert_eq!(mode(1210.0, 248.0, 700.0, true), Mode::Docked, "an 11-inch iPad in landscape");
    assert_eq!(mode(1032.0, 248.0, 700.0, true), Mode::Overlay, "a 13-inch iPad upright");
    assert_eq!(mode(1032.0, 248.0, 700.0, false), Mode::Docked, "a Mac window as wide");
    assert_eq!(mode(390.0, 248.0, 700.0, true), Mode::Drawer, "a phone");
}

/// ⌘B hides the docked navigator and the strip takes its room but the rail's; ⌘B brings it
/// back. Whether it shows is written with the layout, and a workspace made from that file
/// starts the same way.
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
    assert!(cx.debug_bounds("nav-rail").is_some(), "the rail in its place");
    let alone = strip_w(cx);
    let freed = Navigator::DEFAULT_WIDTH - navigator::RAIL_W;
    assert!((alone - beside - freed).abs() < 1.0, "{beside} → {alone}");
    assert!(!saved_navigator(cx, &path).shown, "the layout keeps it hidden");
    let saved = read_layout(&path).ok().flatten().expect("written");
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
    let saved = read_layout(&path).ok().flatten().expect("written");
    let restored =
        cx.update(|_w, cx| cx.new(|cx| WorkspaceView::new(Theme::default(), Some(saved), cx)));
    near(restored.read_with(cx, |v, _| v.navigator_width()), Navigator::MIN_WIDTH);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// *Needs you* shows only while an agent waits out of sight, above *Workers*, whose heading
/// shows only then: alone it would head nothing. A waiting tile in view says so in its own row,
/// so the section is for one folded away. Each worker lists its tiles beneath it, with an
/// accessible name, until its row folds them. The workspaces are no section of the navigator:
/// they are the title bar's, named with their count.
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
    assert!(cx.debug_bounds("nav-workspaces").is_none(), "the tabs list the workspaces");
    assert!(cx.debug_bounds(selector("nav-tile", mine.item)).is_some(), "a tile under its worker");

    view.update_in(cx, |v, _w, cx| v.agent_event(blocked(session), cx));
    cx.run_until_parked();
    let waiting_row = leak(format!("nav-waiting-{session}"));
    assert!(cx.debug_bounds("nav-needs-you").is_none(), "its own row, in view, says so");
    assert!(cx.debug_bounds(waiting_row).is_none());
    let names = labels(&view, cx);
    assert!(names.iter().any(|l| l == "studio"), "a worker that is fine is its name: {names:#?}");
    assert!(
        names.iter().any(|l| l == "Claude Code, Has a question"),
        "the waiting tile's row, in the state's own word: {names:#?}"
    );

    click(cx, leak(format!("nav-worker-{}", laptop.key)));
    assert!(top(cx, "nav-needs-you") < top(cx, "nav-workers"), "folded away, it leads");
    assert!(cx.debug_bounds(waiting_row).is_some());
    click(cx, leak(format!("nav-worker-{}", laptop.key)));
    assert!(cx.debug_bounds("nav-needs-you").is_none(), "unfolded, the row says it again");

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

/// A tile's row focuses its tile and hands it the keyboard; a workspace's row in the
/// breadcrumb's menu goes there.
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
    click(cx, "crumb-workspace");
    let first_name = view.read_with(cx, |v, _| v.workspace_name_at(0));
    click(cx, leak(format!("menu-{first_name}")));
    assert_eq!(view.read_with(cx, |v, _| v.layout().active_workspace()), 0);
    assert_eq!(focused(&view, cx), Some(first));
}

/// The status bar names the focused tile's worker, the round trip there, and how every
/// worker's agents stand; the breadcrumb names the directory. The bell counts the one blocked
/// too, as its way there.
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
        v.set_rtt(key, Some(Duration::from_micros(42_400)), cx);
        v.agent_event(blocked(session), cx);
        v.agent_event(AgentEvent { status: AgentStatus::Working, ..blocked(busy) }, cx);
    });
    cx.run_until_parked();
    let names = labels(&view, cx);
    // Outside a repository the checkout is the directory's own name.
    let agents = "studio: 1 working; laptop: 1 blocked";
    for readout in ["studio", "slopty", "Round trip 42 ms", agents] {
        assert!(names.iter().any(|l| l == readout), "{readout}: {names:#?}");
    }
    assert!(cx.debug_bounds("rtt").is_none(), "the round trip left the title bar");
    assert!(cx.debug_bounds("bell-count").is_some(), "the bell counts the one waiting");
    cx.simulate_keystrokes("cmd-shift-a");
    cx.run_until_parked();
    assert_eq!(focused(&view, cx), Some(waiting), "⌘⇧A still goes to the one waiting");
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

/// The bar runs from the docked navigator's right edge: the toggle, the breadcrumb and "+",
/// each clear of the next and of the bell, however long the workspace's name.
#[gpui::test]
fn the_bar_keeps_clear_of_the_toggle_and_the_breadcrumb(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let _shells = three_shells(&view, cx, &fake);
    let bounds = |cx: &mut VisualTestContext, selector: &'static str| {
        cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is drawn"))
    };
    view.update_in(cx, |v, _w, cx| {
        let name = "The workspace with the longest name anybody ever gave one of them, and more";
        v.layout.set_workspace_name(0, Some(name.to_owned()));
        cx.notify();
    });
    cx.run_until_parked();
    let (navigator, toggle, crumbs, new, bell) = (
        bounds(cx, "navigator"),
        bounds(cx, "navigator-toggle"),
        bounds(cx, "breadcrumb"),
        bounds(cx, "new-menu"),
        bounds(cx, "bell"),
    );
    assert!(navigator.right() <= toggle.left(), "the bar starts at the navigator's edge");
    assert!(toggle.right() <= crumbs.left(), "{toggle:?} {crumbs:?}");
    assert!(crumbs.right() <= new.left(), "{crumbs:?} {new:?}");
    assert!(new.right() <= bell.left(), "+ covers the bell: {new:?} {bell:?}");
}

/// Where the view is along the strip is a thumb on the strip's bottom edge, never in the title
/// bar: it shows while the strip scrolls and there is somewhere to go, goes once the strip has
/// been still a while, and comes back while the pointer is near that edge.
#[gpui::test]
fn the_strip_thumb_shows_only_while_the_strip_moves(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    view.update(cx, |v, _| v.set_animation(true));
    let fake = connect(&view, cx, 1, "studio");
    for n in 1..=4 {
        let _tile = opens(&view, cx, &fake, SessionId::new(), fake.me, n);
    }
    assert!(
        !view.read_with(cx, |v, _| marks::all_in_view(&v.drawn.strip.borrow())),
        "past the view"
    );
    assert!(cx.debug_bounds("indicator").is_none(), "nothing in the title bar");
    let strip = cx.debug_bounds("strip").expect("the strip");
    let track = cx.debug_bounds("strip-marks").expect("the strip moved to the new column");
    let thumb = cx.debug_bounds("strip-thumb").expect("the view's share");
    assert!((f32::from(track.size.height) - 3.0).abs() < 0.01, "3 pt: {track:?}");
    assert!(
        (f32::from(strip.bottom() - track.bottom()) - 8.0).abs() < 0.5,
        "a base unit over the strip's bottom edge: {track:?} in {strip:?}"
    );
    assert!(thumb.size.width < track.size.width, "the view is a share of the strip");

    let gone = |cx: &mut VisualTestContext| {
        cx.executor().advance_clock(marks::MARKS_HOLD);
        cx.run_until_parked();
        cx.executor().advance_clock(crate::kit::Pace::Fade.duration());
        cx.run_until_parked();
        cx.debug_bounds("strip-marks").is_none()
    };
    // The spring steps by the wall clock: land the strip, as the self-test draws it.
    view.update(cx, |v, _| v.set_animation(false));
    cx.run_until_parked();
    assert!(gone(cx), "still a while, it goes");

    let near = point(strip.center().x, strip.bottom() - px(10.0));
    cx.simulate_mouse_move(near, None, Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("strip-marks").is_some(), "the pointer near the edge brings it");
    cx.executor().advance_clock(marks::MARKS_HOLD);
    cx.run_until_parked();
    assert!(cx.debug_bounds("strip-marks").is_some(), "and keeps it while it stays");
    cx.simulate_mouse_move(strip.center(), None, Modifiers::default());
    assert!(gone(cx), "gone a while after the pointer left");
}

/// The strip's thumb fades out once the pointer has left the edge a while; under Reduce
/// Motion it goes at once, so no frame of a fade is asked for.
#[gpui::test]
fn the_strip_thumb_goes_at_once_under_reduce_motion(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    for n in 1..=4 {
        let _tile = opens(&view, cx, &fake, SessionId::new(), fake.me, n);
    }
    view.update(cx, |v, _| v.set_animation(true));
    let strip = cx.debug_bounds("strip").expect("the strip");
    let near = point(strip.center().x, strip.bottom() - px(10.0));
    let left_a_while = |cx: &mut VisualTestContext| {
        cx.simulate_mouse_move(near, None, Modifiers::default());
        cx.run_until_parked();
        assert!(cx.debug_bounds("strip-marks").is_some(), "the pointer near the edge brings it");
        cx.simulate_mouse_move(strip.center(), None, Modifiers::default());
        cx.executor().advance_clock(marks::MARKS_HOLD);
        cx.run_until_parked();
    };
    left_a_while(cx);
    assert!(cx.debug_bounds("strip-marks").is_some(), "it fades where it was");
    let fading = frames_in_a_second(&view, cx);
    assert!(fading > 1, "a fade draws frames: {fading}");
    cx.update(|_w, cx| cx.set_reduce_motion(true));
    left_a_while(cx);
    assert!(cx.debug_bounds("strip-marks").is_none(), "gone at once");
    let after = frames_in_a_second(&view, cx);
    assert!(after <= 1, "the frame it goes in at most: {after}");
    assert_eq!(frames_in_a_second(&view, cx), 0, "then nothing");
}

/// A worker whose link is up says nothing about it: no word and no mark in the navigator, the
/// palette or the empty workspace, and a round trip under `RTT_SHOWN_FROM` is not named. Once the
/// link drops, each says what is wrong with the warn mark and a word, and so does the status bar.
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
    assert!(names.iter().any(|l| l == "Reconnecting"), "the status bar says so: {names:#?}");
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
    assert!(labels(&view, cx).iter().any(|l| l == "make, Failed, unseen"), "named by what it ran");

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
    let before = view.read_with(cx, |v, _| v.drawn.builds.get());
    for _ in 0..120 {
        cx.executor().advance_clock(Duration::from_nanos(8_333_333));
        cx.update(Window::simulate_next_frame);
        cx.run_until_parked();
    }
    view.read_with(cx, |v, _| v.drawn.builds.get()).wrapping_sub(before)
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
/// is a phone's drawer that fits in it.
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
    assert!(cx.debug_bounds("navigator-away").is_some(), "a phone's drawer, not docked");
    let drawer = cx.debug_bounds("navigator").expect("drawn");
    assert!(f32::from(drawer.right()) < NARROW, "the drawer fits: {drawer:?}");
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
/// the strip would pass through a phone's width, and the view would keep a phone's strut.
#[gpui::test]
fn narrowing_the_window_never_lays_the_strip_out_as_a_phone(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let [(_, first), ..] = three_shells(&view, cx, &fake);
    click(cx, selector("nav-tile", first.item));
    let before = cx.debug_bounds(selector("item", first.item)).expect("drawn");
    let strip_left =
        |cx: &mut VisualTestContext| view.read_with(cx, |v, _| v.drawn.viewport.get().left());
    assert_eq!(before.left(), strip_left(cx), "the first column starts the strip");

    cx.simulate_resize(size(px(900.0), px(600.0)));
    cx.run_until_parked();
    assert!(cx.debug_bounds("navigator").is_none(), "no room to dock: it waits closed");
    let after = cx.debug_bounds(selector("item", first.item)).expect("drawn");
    assert_eq!(after.left(), strip_left(cx), "no strut before it: {after:?}");
}

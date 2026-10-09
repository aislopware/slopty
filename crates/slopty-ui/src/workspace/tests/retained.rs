//! GPUI draws a view again only when something it read changed. After each step of the
//! interactions below, what the window shows must be what the same state drawn from scratch
//! shows ([`crate::retained::stale`]): a view left showing an old state fails here.
//!
//! The workspace's clocks (a spinner, an age) run on its own clock, which these tests hold
//! still between steps, so a frame and the frame drawn from scratch after it are drawn at the
//! same instant.

use gpui::{Modifiers, MouseButton, MouseMoveEvent};
use slopty_proto::screen::VideoCodec;

use super::*;

/// The window shows what a frame drawn from scratch would.
#[track_caller]
fn fresh(cx: &mut VisualTestContext, step: &str) {
    cx.run_until_parked();
    let stale = cx.update(|window, cx| crate::retained::stale(window, cx, 12));
    if let Some(stale) = stale {
        panic!("{step}: the window shows a stale frame. {stale}");
    }
}

/// The next frame the window asked for, drawn: a step of a spring, a clock's tick.
fn next_frame(cx: &mut VisualTestContext) {
    cx.update(Window::simulate_next_frame);
    cx.run_until_parked();
}

/// A shell's output is drawn as it arrives: its echo, a line more, and a command that starts.
#[gpui::test]
fn a_shells_echo_is_drawn_as_from_scratch(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let session = SessionId::new();
    let _tile = opens(&view, cx, &fake, session, fake.me, 1);
    fresh(cx, "the shell opened");
    view.update_in(cx, |v, _w, cx| v.term_event(session, frame(&["~ % "]), cx));
    fresh(cx, "its prompt");
    let echo = marked_frame(2, &[("~ % l", SemanticMark::Unknown)], 0);
    view.update_in(cx, |v, _w, cx| v.term_event(session, echo, cx));
    fresh(cx, "an echo");
    let more =
        marked_frame(3, &[("~ % ls", SemanticMark::Unknown), ("a b c", SemanticMark::Unknown)], 1);
    view.update_in(cx, |v, _w, cx| v.term_event(session, more, cx));
    fresh(cx, "a line more");
    // A command that starts names the shell: its header and its navigator row say so.
    let prompt = SemanticMark::Prompt { exit: None, input: Some(4) };
    let runs = marked_frame(4, &[("~ % make", prompt), ("building", SemanticMark::Output)], 1);
    view.update_in(cx, |v, _w, cx| v.term_event(session, runs, cx));
    fresh(cx, "a command started");
    // The frame just drawn from scratch, against another: the oracle itself draws steadily.
    fresh(cx, "scratch after scratch");
}

/// Each pane key, and the frame it lands on, is the frame drawn from scratch: the focus up and
/// down, the pane's next tab, a zoom and its end.
#[gpui::test]
fn the_pane_keys_are_drawn_as_from_scratch(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let _shells = three_shells(&view, cx, &fake);
    view.update(cx, |v, _| v.hold_clock(Some(Duration::ZERO)));
    fresh(cx, "three shells");
    for (keys, what) in [
        ("cmd-alt-up", "the focus up"),
        ("cmd-alt-down", "the focus down"),
        ("cmd-alt-]", "the pane's next tab"),
        ("cmd-shift-enter", "a zoom"),
        ("cmd-shift-enter", "the zoom's end"),
    ] {
        cx.simulate_keystrokes(keys);
        fresh(cx, what);
    }
}

/// A sash dragged step by step: each step is the frame drawn from scratch, and builds each
/// shell it resizes at most twice, once at its new bounds and once more in the next frame
/// where its grid took new rows ([`crate::terminal::TerminalView::fitted`]), never more.
#[gpui::test]
fn a_sash_drag_is_drawn_as_from_scratch_and_builds_each_shell_once(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let shells = three_shells(&view, cx, &fake);
    let terminals: Vec<Entity<TerminalView>> = shells
        .iter()
        .filter_map(|(session, _)| view.read_with(cx, |v, _| v.terminal(*session).cloned()))
        .collect();
    let renders = |cx: &mut VisualTestContext| -> Vec<u32> {
        terminals.iter().map(|t| t.read_with(cx, |t, _| t.renders())).collect()
    };
    view.update(cx, |v, _| v.hold_clock(Some(Duration::ZERO)));
    fresh(cx, "two panes");
    let at = cx.debug_bounds("sash--0").expect("the sash between the panes").center();
    cx.simulate_mouse_down(at, MouseButton::Left, Modifiers::default());
    fresh(cx, "the sash pressed");
    for step in 1..=8_u8 {
        let before = renders(cx);
        let to = point(at.x, at.y + px(f32::from(step) * 12.0));
        cx.simulate_mouse_move(to, Some(MouseButton::Left), Modifiers::default());
        next_frame(cx);
        let built: Vec<u32> =
            renders(cx).iter().zip(&before).map(|(a, b)| a.saturating_sub(*b)).collect();
        assert!(built.iter().all(|n| *n <= 2), "step {step}: shells built {built:?}");
        fresh(cx, &format!("step {step} of the drag"));
    }
    cx.simulate_mouse_up(point(at.x, at.y + px(96.0)), MouseButton::Left, Modifiers::default());
    fresh(cx, "the sash let go");
}

/// The pointer over a tile, over its header, and away: each hover is drawn as from scratch.
#[gpui::test]
fn a_tile_hovered_is_drawn_as_from_scratch(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let [(_, first), (_, second), _] = three_shells(&view, cx, &fake);
    view.update(cx, |v, _| v.hold_clock(Some(Duration::ZERO)));
    fresh(cx, "three columns");
    for tile in [first, second] {
        let Some(bounds) = cx.debug_bounds(selector("item", tile.item)) else { continue };
        let header = point(bounds.center().x, bounds.top() + px(8.0));
        for (at, what) in [(bounds.center(), "its body"), (header, "its header")] {
            cx.simulate_event(MouseMoveEvent {
                position: at,
                modifiers: Modifiers::default(),
                pressed_button: None,
            });
            fresh(cx, &format!("the pointer over {what}"));
        }
    }
    cx.simulate_event(MouseMoveEvent {
        position: point(px(2.0), px(VIEWPORT.1 - 2.0)),
        modifiers: Modifiers::default(),
        pressed_button: None,
    });
    fresh(cx, "the pointer away");
}

/// A remote window's tile as the window it is drawn in changes size: the picture's place and
/// its overlay follow in the frames drawn, not a frame later.
#[gpui::test]
fn a_remote_window_follows_a_resize_as_from_scratch(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let window = slopty_core::WindowId(7);
    let tile = arrives(&view, cx, &fake, ItemKind::Window { window }, 1);
    let key = fake.key;
    view.update_in(cx, |v, _w, cx| {
        v.screen_event(
            key,
            ScreenEvent::Opened {
                stream: StreamId(1),
                target: CaptureTarget::Window(window),
                codec: VideoCodec::Hevc,
                width: 1280,
                height: 800,
                scale: 2.0,
                stripes: Vec::new(),
            },
            cx,
        );
        v.focus_tile(tile, cx);
    });
    fresh(cx, "the window's tile");
    for (w, h) in [(900.0, 700.0), (1400.0, 900.0), (VIEWPORT.0, VIEWPORT.1)] {
        cx.simulate_resize(size(px(w), px(h)));
        fresh(cx, &format!("the window at {w}×{h}"));
    }
    // A zoom and its end, the docked navigator stepping aside and back: the picture's place is
    // laid out at the size its tile is drawn at.
    let screen = view.read_with(cx, |v, _| v.screen(tile.item).cloned()).expect("streaming");
    screen.update(cx, |s, cx| s.show_picture(picture(1280, 800), cx));
    fresh(cx, "its first picture");
    let mut widths = Vec::new();
    for step in 1..=2 {
        cx.simulate_keystrokes("cmd-shift-enter");
        next_frame(cx);
        // The picture and the pointer are laid out from the body's bounds as the render read
        // them; the painted quads do not show the picture itself. Read before the oracle.
        let (drawn, rendered) = screen.read_with(cx, |s, _| s.bounds_rendered());
        assert_eq!(rendered, drawn, "step {step}: laid out at the bounds it was drawn in");
        fresh(cx, &format!("the zoom, step {step}"));
        widths.push(drawn.size.width);
    }
    // The window resized under a still stream, no new picture coming: the picture is laid out
    // again at the body's new bounds by the frame after the one that measured them.
    for (w, h) in [(900.0, 700.0), (1400.0, 900.0)] {
        cx.simulate_resize(size(px(w), px(h)));
        cx.run_until_parked();
        next_frame(cx);
        // Read before the oracle, whose frames from scratch would lay it out again.
        let (drawn, rendered) = screen.read_with(cx, |s, _| s.bounds_rendered());
        assert_eq!(rendered, drawn, "at {w}×{h}: laid out at the bounds it was drawn in");
        fresh(cx, &format!("a still picture, the window at {w}×{h}"));
        widths.push(drawn.size.width);
    }
    widths.dedup();
    assert!(widths.len() > 3, "the tile changed size each step: {widths:?}");
}

/// A picture of `w` × `h` for a stream to show.
fn picture(w: usize, h: usize) -> core_video::pixel_buffer::CVPixelBuffer {
    core_video::pixel_buffer::CVPixelBuffer::new(
        core_video::pixel_buffer::kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
        w,
        h,
        None,
    )
    .expect("pixel buffer")
}

/// A slow command ends in a shell beside the focused one: its header's mark and the badge that
/// says it finished are drawn as from scratch, and so is the bell that counts it.
#[gpui::test]
fn a_command_that_ends_beside_is_drawn_as_from_scratch(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let (slow, here) = (SessionId::new(), SessionId::new());
    let _slow = opens(&view, cx, &fake, slow, fake.me, 1);
    let here_tile = opens(&view, cx, &fake, here, fake.me, 2);
    view.update_in(cx, |v, _w, cx| v.focus_tile(here_tile, cx));
    view.update(cx, |v, _| v.hold_clock(Some(Duration::ZERO)));
    fresh(cx, "two shells");
    let prompt = |exit| SemanticMark::Prompt { exit, input: Some(4) };
    let runs = marked_frame(2, &[("~ % sleep 6", prompt(None))], 0);
    view.update_in(cx, |v, _w, cx| v.term_event(slow, runs, cx));
    fresh(cx, "a command runs beside");
    let ended = marked_frame(3, &[("~ % sleep 6", prompt(None)), ("~ % ", prompt(Some(0)))], 1);
    view.update_in(cx, |v, _w, cx| {
        v.term_event(slow, ended, cx);
        let done =
            Finished { command: "sleep 6".into(), exit: Some(0), elapsed: Duration::from_secs(6) };
        v.command_finished(slow, done, cx);
    });
    fresh(cx, "it ended");
}

/// The area built again with nothing in it moved (a working mark's turn, the pointer on a
/// tile's chrome) replays every shell, the one typed into included: a typed key, timed once
/// its frame reached the display, is no news for the shell's view.
#[gpui::test]
fn the_area_built_again_replays_a_shell_typed_into(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let shells = three_shells(&view, cx, &fake);
    let [_, (typed, tile), _] = shells;
    view.update_in(cx, |v, _w, cx| v.focus_tile(tile, cx));
    cx.run_until_parked();
    assert!(terminal_focused(&view, cx, typed), "the middle shell has the keyboard");
    cx.simulate_keystrokes("a");
    view.update_in(cx, |v, _w, cx| v.term_event(typed, frame(&["~ % a"]), cx));
    cx.run_until_parked();
    let now = Instant::now();
    let shown = gpui::PresentedFrame { submitted_at: now, presented_at: Some(now) };
    cx.update(|_w, cx| crate::shown::presented(shown, cx));
    let terminals: Vec<Entity<TerminalView>> = shells
        .iter()
        .filter_map(|(session, _)| view.read_with(cx, |v, _| v.terminal(*session).cloned()))
        .collect();
    let renders = |cx: &mut VisualTestContext| -> Vec<u32> {
        terminals.iter().map(|t| t.read_with(cx, |t, _| t.renders())).collect()
    };
    let (area, builds) = view.read_with(cx, |v, _| (v.area_host.entity_id(), v.drawn.builds.get()));
    let before = renders(cx);
    for _ in 0..3 {
        cx.update(|_w, cx| cx.notify(area));
        cx.run_until_parked();
    }
    let built = view.read_with(cx, |v, _| v.drawn.builds.get()).wrapping_sub(builds);
    assert_eq!(built, 3, "the area, built three times");
    assert_eq!(renders(cx), before, "no shell built again");
}

/// A pointer moving over a shell, a frame drawn for something else after each move: once it
/// is in (entering changes what the shell's hover reads found), no shell is built again,
/// since none of them keeps where the pointer is, or reads it, to draw itself.
#[gpui::test]
fn a_pointer_moving_over_the_shells_builds_none_of_them(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let shells = three_shells(&view, cx, &fake);
    let terminals: Vec<Entity<TerminalView>> = shells
        .iter()
        .filter_map(|(session, _)| view.read_with(cx, |v, _| v.terminal(*session).cloned()))
        .collect();
    let renders = |cx: &mut VisualTestContext| -> Vec<u32> {
        terminals.iter().map(|t| t.read_with(cx, |t, _| t.renders())).collect()
    };
    let body = cx.debug_bounds(selector("item", shells[2].1.item)).expect("drawn");
    let area = view.read_with(cx, |v, _| v.area_host.entity_id());
    let move_to = |cx: &mut VisualTestContext, step: u8| {
        let at = body.origin + point(px(12.0) * f32::from(step), body.size.height / 2.0);
        cx.simulate_mouse_move(at, None, Modifiers::default());
        cx.run_until_parked();
        cx.update(|_w, cx| cx.notify(area));
        cx.run_until_parked();
    };
    move_to(cx, 1);
    let before = renders(cx);
    for step in 2..6 {
        move_to(cx, step);
    }
    assert_eq!(renders(cx), before, "no shell built again");
    fresh(cx, "the pointer moved");
}

/// A command that starts in a shell on a tab not shown retitles its navigator row in the
/// frame it starts in: its title is worked out though no tile of it is drawn.
#[gpui::test]
fn an_undrawn_shells_command_retitles_its_row_as_from_scratch(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let far = SessionId::new();
    let tile = opens(&view, cx, &fake, far, fake.me, 1);
    let other = opens(&view, cx, &fake, SessionId::new(), fake.me, 2);
    on_new_tab(&view, cx, other);
    fresh(cx, "the second shell on a tab of its own, shown");
    assert!(cx.debug_bounds(selector("item", tile.item)).is_none(), "the first is not drawn");
    assert!(cx.debug_bounds("navigator").is_some(), "the navigator lists it");
    let prompt = SemanticMark::Prompt { exit: None, input: Some(4) };
    let runs = marked_frame(2, &[("~ % make", prompt), ("building", SemanticMark::Output)], 1);
    view.update_in(cx, |v, _w, cx| v.term_event(far, runs, cx));
    fresh(cx, "a command started on the tab not shown");
    let titles: Vec<String> =
        view.read_with(cx, WorkspaceView::navigator_lines).into_iter().map(|(t, ..)| t).collect();
    assert!(titles.iter().any(|t| t == "make"), "the row is named for the command: {titles:?}");
}

/// A page tile on a device with no web view says so in the frame after it tried: the failure
/// is found while the window draws, and the frame it asks for draws it.
#[gpui::test]
fn a_page_that_cannot_open_says_so_as_from_scratch(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let url = "http://127.0.0.1:5173/";
    let tile = arrives(&view, cx, &fake, ItemKind::Browser { url: url.into() }, 1);
    let page = view.read_with(cx, |v, _| v.browser(tile.item).cloned()).expect("a page view");
    page.update(cx, |page, cx| page.set_local(Some(url.into()), cx));
    fresh(cx, "a page tile, opened as it is drawn");
    let failed = page.read_with(cx, |page, _| page.page().failed.is_some());
    assert!(failed, "no web view in a headless window: the page failed");
}

/// A file tile's caret blinking, or a keystroke in it, is its editor's news: the area is not
/// built again for it, the header reading only whether the edit is on disk.
#[gpui::test]
fn a_file_tiles_caret_blinks_without_building_the_area(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let path = "/w/notes.txt";
    let tile = arrives(&view, cx, &studio, ItemKind::File { path: path.to_owned() }, 1);
    let text = slopty_proto::file::FileRead::Text {
        text: "# Notes".to_owned(),
        size: 8,
        modified_ms: WallMs::from_millis(1_000),
        final_newline: true,
        editorconfig: Vec::new(),
    };
    let key = studio.key;
    view.update_in(cx, |v, _w, cx| {
        v.file_read(key, path, &text, cx);
        v.focus_tile(tile, cx);
    });
    cx.update(|window, _cx| window.activate_window());
    cx.run_until_parked();
    cx.simulate_input("x");
    cx.run_until_parked();
    let builds = |cx: &mut VisualTestContext| view.read_with(cx, |v, _| v.drawn.builds.get());
    let before = builds(cx);
    for _ in 0..4 {
        cx.executor().advance_clock(Duration::from_millis(510));
        cx.run_until_parked();
    }
    cx.simulate_input("y");
    cx.run_until_parked();
    assert_eq!(builds(cx), before, "the area was not built for the caret or the key");
    assert!(says_edited(&view, cx, tile), "the word still shows");
}

/// The keyboard moved by a view of its own (a click in a body, a find bar giving it back)
/// builds neither the workspace nor the area: nothing of theirs reads where the keyboard is
/// as a whole. The two shells it moves between are built again for their carets and rings, a
/// third is not, and every frame is the one drawn from scratch.
#[gpui::test]
fn the_keyboard_moving_on_its_own_builds_neither_the_workspace_nor_the_area(
    cx: &mut TestAppContext,
) {
    let (view, cx) = still_workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let [(first, _), (second, two), (third, _)] = three_shells(&view, cx, &fake);
    let terminals: Vec<Entity<TerminalView>> = [first, second, third]
        .iter()
        .filter_map(|session| view.read_with(cx, |v, _| v.terminal(*session).cloned()))
        .collect();
    let built = |cx: &mut VisualTestContext| -> (usize, u64, Vec<u32>) {
        let (workspace, area) = view.read_with(cx, |v, _| (v.renders, v.drawn.builds.get()));
        let shells = terminals.iter().map(|t| t.read_with(cx, |t, _| t.renders())).collect();
        (workspace, area, shells)
    };
    view.update_in(cx, |v, _w, cx| v.focus_tile(two, cx));
    fresh(cx, "the middle shell has the keyboard");
    for (to, from, left_alone) in [(0, 1, 2), (1, 0, 2)] {
        let before = built(cx);
        let handle = terminals[to].read_with(cx, Focusable::focus_handle);
        cx.update(|window, cx| window.focus(&handle, cx));
        cx.run_until_parked();
        let after = built(cx);
        let step = format!("the keyboard moved from shell {from} to shell {to}");
        assert!(terminal_focused(&view, cx, [first, second][to]), "{step}");
        assert_eq!((after.0, after.1), (before.0, before.1), "{step}: workspace and area");
        assert!(after.2[to] > before.2[to], "{step}: the shell it came to was drawn again");
        assert!(after.2[from] > before.2[from], "{step}: the shell it left was drawn again");
        assert_eq!(after.2[left_alone], before.2[left_alone], "{step}: the third was not");
        fresh(cx, &step);
    }
}

/// The keyboard moving from one shell to another draws the two of them again (the caret each
/// shows, the ring and header that say which has it) and not a shell it never touched: GPUI
/// builds again only the views whose answer to a focus question changed, and the frame after is
/// the one drawn from scratch.
#[gpui::test]
fn the_keyboard_moving_builds_only_the_shells_it_moves_between(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let [(first, _), (second, two), (third, _)] = three_shells(&view, cx, &fake);
    let terminals: Vec<Entity<TerminalView>> = [first, second, third]
        .iter()
        .filter_map(|session| view.read_with(cx, |v, _| v.terminal(*session).cloned()))
        .collect();
    let renders = |cx: &mut VisualTestContext| -> Vec<u32> {
        terminals.iter().map(|t| t.read_with(cx, |t, _| t.renders())).collect()
    };
    view.update_in(cx, |v, _w, cx| v.focus_tile(two, cx));
    cx.run_until_parked();
    assert!(terminal_focused(&view, cx, second));
    fresh(cx, "the middle shell has the keyboard");
    let before = renders(cx);
    // The keyboard alone, as a shell's find bar closing hands it back: the area stays put.
    view.update_in(cx, |v, _w, cx| {
        v.pending_focus = Some(first);
        cx.notify();
    });
    cx.run_until_parked();
    assert!(terminal_focused(&view, cx, first), "the first shell has the keyboard");
    // Counted before the check, which draws every view from scratch to compare.
    let after = renders(cx);
    fresh(cx, "the keyboard moved");
    assert!(after[0] > before[0], "the shell it moved to was drawn again: {before:?} {after:?}");
    assert!(after[1] > before[1], "the shell it left was drawn again: {before:?} {after:?}");
    assert_eq!(after[2], before[2], "the shell it never touched was not built again");
}

/// Title tabs opened one by one until they run past the bar's room, then back and forward
/// between them: each frame is the frame drawn from scratch. What the strip shows of the tabs
/// past its ends is known only once it is laid out, so it is drawn in that same frame, never
/// from what the last frame laid out.
#[gpui::test]
fn title_tabs_that_overflow_are_drawn_as_from_scratch(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let first = SessionId::new();
    let _tile = opens(&view, cx, &fake, first, fake.me, 1);
    fresh(cx, "one shell");
    for n in 2..=12_u64 {
        let tile = opens(&view, cx, &fake, SessionId::new(), fake.me, n);
        on_new_tab(&view, cx, tile);
        fresh(cx, &format!("tab {n}"));
    }
    let overflows = view.read_with(cx, |v, _| v.title_scroll.max_offset().x > px(0.0));
    assert!(overflows, "the tabs run past the bar's room");
    for (keys, what) in [("cmd-[", "back"), ("cmd-]", "forward"), ("cmd-[", "back again")] {
        cx.simulate_keystrokes(keys);
        fresh(cx, what);
    }
}

/// Tabs enough to run past the bar arriving in one snapshot, as a relaunch onto many tabs lays
/// them all out in its first frame: that frame is the frame drawn from scratch, the tab the
/// strip's end cuts and every glyph in it included. A narrower window, the same.
#[gpui::test]
fn title_tabs_that_overflow_at_once_are_drawn_as_from_scratch(cx: &mut TestAppContext) {
    let (view, cx) = still_workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let session = SessionId::new();
    let shell = opens_in(&view, cx, &fake, session, fake.me, 1, Some("/w"));
    fresh(cx, "one shell");
    let shell_item = view.read_with(cx, |v, _| v.item(shell).cloned()).expect("its item");
    let files = (0..12).map(|n| Item {
        id: ItemId::new(),
        kind: ItemKind::File { path: format!("/w/notes-{n}.md") },
        name: None,
        facts: BTreeMap::new(),
    });
    let items: Vec<Item> = std::iter::once(shell_item).chain(files).collect();
    let key = fake.key;
    view.update_in(cx, |v, _w, cx| {
        v.apply_sync(key, ItemSync::Snapshot { version: 2, items }, cx);
    });
    fresh(cx, "twelve tabs at once");
    let tabs = view.read_with(cx, |v, _| v.layout().shown_project().map(|p| p.tabs().len()));
    assert_eq!(tabs, Some(13), "each a tab of the shell's project");
    let overflows = view.read_with(cx, |v, _| v.title_scroll.max_offset().x > px(0.0));
    assert!(overflows, "the tabs run past the bar's room");
    cx.simulate_keystrokes("cmd-[");
    fresh(cx, "back to a tab past the end");
    cx.simulate_resize(size(px(700.0), px(900.0)));
    fresh(cx, "a narrower window");
}

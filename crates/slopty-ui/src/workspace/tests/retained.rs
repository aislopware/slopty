//! GPUI draws a view again only when something it read changed. After each step of the
//! interactions below, what the window shows must be what the same state drawn from scratch
//! shows ([`crate::retained::stale`]): a view left showing an old state fails here.
//!
//! The layout's springs run on the workspace's clock, which these tests hold still between
//! steps, so a frame of motion and the frame drawn from scratch after it are drawn at the same
//! instant.

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
    let (view, cx) = workspace(cx);
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

/// Every frame of the strip's spring, stepping a column left and back, and the frame it lands
/// on, is the frame drawn from scratch at the same instant.
#[gpui::test]
fn the_strip_springs_as_from_scratch(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let _shells = three_shells(&view, cx, &fake);
    view.update(cx, |v, _| {
        v.set_animation(true);
        v.hold_clock(Some(Duration::ZERO));
    });
    fresh(cx, "three columns");
    for (keys, way) in [("cmd-alt-left", "left"), ("cmd-alt-right", "right")] {
        cx.simulate_keystrokes(keys);
        fresh(cx, &format!("the step {way}"));
        for step in 1..=12_u32 {
            view.update(cx, |v, _| v.hold_clock(Some(Duration::from_millis(u64::from(step) * 25))));
            next_frame(cx);
            fresh(cx, &format!("frame {step} of the spring {way}"));
        }
        view.update(cx, |v, _| v.hold_clock(Some(Duration::from_secs(5))));
        next_frame(cx);
        fresh(cx, &format!("landed {way}"));
    }
}

/// A frame of the overview's spring builds each shell it moves once: a shell drawn somewhere
/// else is built again there, and nothing asks for it a second time in the same frame.
#[gpui::test]
fn a_frame_of_the_spring_builds_each_shell_once(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let shells = three_shells(&view, cx, &fake);
    let terminals: Vec<Entity<TerminalView>> = shells
        .iter()
        .filter_map(|(session, _)| view.read_with(cx, |v, _| v.terminal(*session).cloned()))
        .collect();
    let renders = |cx: &mut VisualTestContext| -> Vec<u32> {
        terminals.iter().map(|t| t.read_with(cx, |t, _| t.renders())).collect()
    };
    view.update(cx, |v, _| {
        v.set_animation(true);
        v.hold_clock(Some(Duration::ZERO));
    });
    cx.run_until_parked();
    view.update(cx, |v, cx| {
        v.tick();
        v.layout.set_overview(true);
        cx.notify();
    });
    cx.run_until_parked();
    for step in 1..=8_u32 {
        let before = renders(cx);
        view.update(cx, |v, _| v.hold_clock(Some(Duration::from_millis(u64::from(step) * 25))));
        next_frame(cx);
        let built: Vec<u32> =
            renders(cx).iter().zip(&before).map(|(a, b)| a.saturating_sub(*b)).collect();
        assert!(built.iter().all(|n| *n <= 1), "frame {step}: shells built {built:?}");
        fresh(cx, &format!("frame {step} of the overview opening"));
    }
}

/// A trackpad scroll moves the strip, frame by frame, and each frame is the one drawn from
/// scratch.
#[gpui::test]
fn the_strip_scrolls_as_from_scratch(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let [_, (_, middle), _] = three_shells(&view, cx, &fake);
    view.update(cx, |v, _| v.hold_clock(Some(Duration::ZERO)));
    fresh(cx, "three columns");
    let at = cx.debug_bounds(selector("item", middle.item)).expect("the middle tile").center();
    swipe(cx, at, (-40.0, 0.0), 6, 0);
    fresh(cx, "a swipe");
    for step in 1..=8_u32 {
        view.update(cx, |v, _| v.hold_clock(Some(Duration::from_millis(u64::from(step) * 40))));
        next_frame(cx);
        fresh(cx, &format!("frame {step} after the swipe"));
    }
}

/// The pointer over a tile, over its header, and away: each hover is drawn as from scratch.
#[gpui::test]
fn a_tile_hovered_is_drawn_as_from_scratch(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
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
    let (view, cx) = workspace(cx);
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
    // The overview opens and closes, the window as it was: the picture's place is laid out at
    // the size its tile is drawn at.
    let screen = view.read_with(cx, |v, _| v.screen(tile.item).cloned()).expect("streaming");
    screen.update(cx, |s, cx| s.show_picture(picture(1280, 800), cx));
    fresh(cx, "its first picture");
    let mut widths = Vec::new();
    for (step, overview) in [(1, true), (2, false)] {
        view.update(cx, |v, cx| {
            v.layout.set_overview(overview);
            cx.notify();
        });
        next_frame(cx);
        // The picture and the pointer are laid out from the body's bounds as the render read
        // them; the painted quads do not show the picture itself. Read before the oracle.
        let (drawn, rendered) = screen.read_with(cx, |s, _| s.bounds_rendered());
        assert_eq!(rendered, drawn, "step {step}: laid out at the bounds it was drawn in");
        fresh(cx, &format!("the overview, step {step}"));
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
    let (view, cx) = workspace(cx);
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

/// The strip built again with nothing in it moved (a working mark's turn, the pointer on a
/// tile's chrome) replays every shell, the one typed into included: a typed key, timed once
/// its frame reached the display, is no news for the shell's view.
#[gpui::test]
fn the_strip_built_again_replays_a_shell_typed_into(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
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
    let (strip, builds) =
        view.read_with(cx, |v, _| (v.strip_host.entity_id(), v.drawn.builds.get()));
    let before = renders(cx);
    for _ in 0..3 {
        cx.update(|_w, cx| cx.notify(strip));
        cx.run_until_parked();
    }
    let built = view.read_with(cx, |v, _| v.drawn.builds.get()).wrapping_sub(builds);
    assert_eq!(built, 3, "the strip, built three times");
    assert_eq!(renders(cx), before, "no shell built again");
}

/// A pointer moving over a shell, a frame drawn for something else after each move: once it
/// is in (entering changes what the shell's hover reads found), no shell is built again,
/// since none of them keeps where the pointer is, or reads it, to draw itself.
#[gpui::test]
fn a_pointer_moving_over_the_shells_builds_none_of_them(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let shells = three_shells(&view, cx, &fake);
    let terminals: Vec<Entity<TerminalView>> = shells
        .iter()
        .filter_map(|(session, _)| view.read_with(cx, |v, _| v.terminal(*session).cloned()))
        .collect();
    let renders = |cx: &mut VisualTestContext| -> Vec<u32> {
        terminals.iter().map(|t| t.read_with(cx, |t, _| t.renders())).collect()
    };
    let body = cx.debug_bounds(selector("item", shells[1].1.item)).expect("drawn");
    let strip = view.read_with(cx, |v, _| v.strip_host.entity_id());
    let move_to = |cx: &mut VisualTestContext, step: u8| {
        let at = body.origin + point(px(12.0) * f32::from(step), body.size.height / 2.0);
        cx.simulate_mouse_move(at, None, Modifiers::default());
        cx.run_until_parked();
        cx.update(|_w, cx| cx.notify(strip));
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

/// A command that starts in a shell scrolled off the strip retitles its navigator row in the
/// frame it starts in: its title is worked out though no tile of it is drawn.
#[gpui::test]
fn an_undrawn_shells_command_retitles_its_row_as_from_scratch(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let shells: Vec<(SessionId, TileRef)> = (1..=6)
        .map(|version| {
            let session = SessionId::new();
            (session, opens(&view, cx, &fake, session, fake.me, version))
        })
        .collect();
    let (far, tile) = shells[0];
    fresh(cx, "six shells, the last focused");
    assert!(cx.debug_bounds(selector("item", tile.item)).is_none(), "the first is off the strip");
    assert!(cx.debug_bounds("navigator").is_some(), "the navigator lists it");
    let prompt = SemanticMark::Prompt { exit: None, input: Some(4) };
    let runs = marked_frame(2, &[("~ % make", prompt), ("building", SemanticMark::Output)], 1);
    view.update_in(cx, |v, _w, cx| v.term_event(far, runs, cx));
    fresh(cx, "a command started off the strip");
    let titles: Vec<String> =
        view.read_with(cx, WorkspaceView::navigator_lines).into_iter().map(|(t, ..)| t).collect();
    assert!(titles.iter().any(|t| t == "make"), "the row is named for the command: {titles:?}");
}

/// A page tile on a device with no web view says so in the frame after it tried: the failure
/// is found while the window draws, and the frame it asks for draws it.
#[gpui::test]
fn a_page_that_cannot_open_says_so_as_from_scratch(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let url = "http://127.0.0.1:5173/";
    let tile = arrives(&view, cx, &fake, ItemKind::Browser { url: url.into() }, 1);
    let page = view.read_with(cx, |v, _| v.browser(tile.item).cloned()).expect("a page view");
    page.update(cx, |page, cx| page.set_local(Some(url.into()), cx));
    fresh(cx, "a page tile, opened as it is drawn");
    let failed = page.read_with(cx, |page, _| page.page().failed.is_some());
    assert!(failed, "no web view in a headless window: the page failed");
}

/// A tile dragged into the strip's right edge band and held there scrolls the strip on, one
/// frame of motion asked for per frame however often the pointer moves, and each frame is the
/// one drawn from scratch.
#[gpui::test]
fn a_drag_held_at_the_edge_scrolls_one_frame_at_a_time(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let tiles: Vec<TileRef> = (1..=6)
        .map(|version| opens(&view, cx, &fake, SessionId::new(), fake.me, version))
        .collect();
    view.update(cx, |v, _| {
        v.set_animation(true);
        v.hold_clock(Some(Duration::ZERO));
    });
    let first = tiles[0];
    view.update_in(cx, |v, _w, cx| v.focus_tile(first, cx));
    cx.run_until_parked();
    view.update(cx, |v, _| v.hold_clock(Some(Duration::from_secs(5))));
    next_frame(cx);
    fresh(cx, "the first column in view");
    let header = cx.debug_bounds(selector("title", first.item)).expect("its header");
    // Right of the title's text, left of the controls.
    let grab = point(header.left() + header.size.width * 0.6, header.center().y);
    let strip = view.read_with(cx, |v, _| v.drawn.viewport.get());
    let edge = point(strip.right() - px(4.0), grab.y);
    cx.simulate_mouse_down(grab, MouseButton::Left, Modifiers::default());
    let scroll = |cx: &mut VisualTestContext| {
        let next = tiles[1];
        cx.debug_bounds(selector("item", next.item)).map_or(f32::MAX, |b| f32::from(b.left()))
    };
    let before = scroll(cx);
    let landed = Duration::from_secs(5);
    for step in 1..=12_u32 {
        let wobble = px(if step % 2 == 0 { 0.0 } else { 1.0 });
        let at = point(edge.x - wobble, edge.y);
        cx.simulate_mouse_move(at, Some(MouseButton::Left), Modifiers::default());
        let clock = landed.saturating_add(Duration::from_millis(16).saturating_mul(step));
        view.update(cx, |v, _| v.hold_clock(Some(clock)));
        let ran = cx.update(Window::simulate_next_frame);
        cx.run_until_parked();
        // The strip's own frame, and at most one other (a caret's, a spring's).
        assert!(ran <= 2, "move {step}: {ran} frames of motion asked for in one frame");
        fresh(cx, &format!("frame {step} at the edge"));
    }
    let after = scroll(cx);
    assert!(after < before, "the strip scrolled on: {before} → {after}");
    cx.simulate_mouse_up(edge, MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
}

/// A file tile's caret blinking, or a keystroke in it, is its editor's news: the strip is not
/// built again for it, the header reading only whether the edit is on disk.
#[gpui::test]
fn a_file_tiles_caret_blinks_without_building_the_strip(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let studio = connect(&view, cx, 1, "studio");
    let path = "/w/notes.md";
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
    assert_eq!(builds(cx), before, "the strip was not built for the caret or the key");
    assert!(cx.debug_bounds(selector("unsaved", tile.item)).is_some(), "the dot still shows");
}

/// The keyboard moved by a view of its own (a click in a body, a find bar giving it back)
/// builds neither the workspace nor the strip: nothing of theirs reads where the keyboard is
/// as a whole. The two shells it moves between are built again for their carets and rings, a
/// third is not, and every frame is the one drawn from scratch.
#[gpui::test]
fn the_keyboard_moving_on_its_own_builds_neither_the_workspace_nor_the_strip(
    cx: &mut TestAppContext,
) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let [(first, _), (second, two), (third, _)] = three_shells(&view, cx, &fake);
    let terminals: Vec<Entity<TerminalView>> = [first, second, third]
        .iter()
        .filter_map(|session| view.read_with(cx, |v, _| v.terminal(*session).cloned()))
        .collect();
    let built = |cx: &mut VisualTestContext| -> (usize, u64, Vec<u32>) {
        let (workspace, strip) = view.read_with(cx, |v, _| (v.renders, v.drawn.builds.get()));
        let shells = terminals.iter().map(|t| t.read_with(cx, |t, _| t.renders())).collect();
        (workspace, strip, shells)
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
        assert_eq!((after.0, after.1), (before.0, before.1), "{step}: workspace and strip");
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
    let (view, cx) = workspace(cx);
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
    // The keyboard alone, as a shell's find bar closing hands it back: the strip stays put.
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

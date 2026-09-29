//! GPUI draws a view again only when something it read changed. After each step of the
//! interactions below, what the window shows must be what the same state drawn from scratch
//! shows ([`crate::retained::stale`]): a view left showing an old state fails here.
//!
//! The layout's springs run on the workspace's clock, which these tests hold still between
//! steps, so a frame of motion and the frame drawn from scratch after it are drawn at the same
//! instant.

use gpui::{Modifiers, MouseMoveEvent};
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
        fresh(cx, &format!("the overview, step {step}"));
        // The picture and the pointer are laid out from the body's bounds as the render read
        // them; the painted quads do not show the picture itself.
        let (drawn, rendered) = screen.read_with(cx, |s, _| s.bounds_rendered());
        assert_eq!(rendered, drawn, "step {step}: laid out at the bounds it was drawn in");
        widths.push(drawn.size.width);
    }
    widths.dedup();
    assert!(widths.len() > 1, "the tile changed size: {widths:?}");
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

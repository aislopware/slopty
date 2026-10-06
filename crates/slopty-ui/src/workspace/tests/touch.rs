//! A pinch over the panes in the headless workspace: over a remote picture it zooms the
//! picture; over a terminal it leaves the picture and the layout alone.

use gpui::{Bounds, PinchEvent};
use slopty_proto::screen::VideoCodec;

use super::*;
use crate::screen::Zoom;

/// A whole pinch at `at`: it begins, takes two steps of `step` each, and ends.
fn pinch(cx: &mut VisualTestContext, at: Point<Pixels>, step: f32) {
    for (phase, delta) in [
        (TouchPhase::Started, 0.0),
        (TouchPhase::Moved, step),
        (TouchPhase::Moved, step),
        (TouchPhase::Ended, 0.0),
    ] {
        cx.simulate_event(PinchEvent {
            position: at,
            delta,
            modifiers: Modifiers::default(),
            phase,
        });
    }
    cx.run_until_parked();
}

/// A point on a tile's body: below its middle, clear of the header.
fn on_body(cx: &mut VisualTestContext, tile: TileRef) -> Point<Pixels> {
    let b: Bounds<Pixels> = cx
        .debug_bounds(selector("item", tile.item))
        .unwrap_or_else(|| panic!("{tile:?} is not drawn"));
    point(b.center().x, b.origin.y + b.size.height * 0.7)
}

fn zoom_of(view: &Entity<WorkspaceView>, cx: &VisualTestContext, tile: TileRef) -> Zoom {
    view.read_with(cx, |v, cx| v.screen(tile.item).map(|s| s.read(cx).zoom())).expect("streaming")
}

/// What the layout shows: the tile focused and every pane's place.
fn shown(
    view: &Entity<WorkspaceView>,
    cx: &VisualTestContext,
) -> (Option<TileRef>, Vec<slopty_client::layout::Rect>) {
    view.read_with(cx, |v, _| {
        (v.focused(), v.layout.frame().panes.iter().map(|l| l.rect).collect())
    })
}

/// The focused remote window takes a pinch that begins on its picture: spreading the fingers
/// zooms it, and pinching them together goes no smaller than fit. The same pinch over a
/// shell's body is no gesture of the workspace's: the picture and the layout stay as they were.
#[gpui::test]
fn a_pinch_over_a_stream_zooms_it_and_over_a_shell_leaves_all_alone(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let shell = opens(&view, cx, &fake, SessionId::new(), fake.me, 1);
    let window = slopty_core::WindowId(7);
    let tile = arrives(&view, cx, &fake, ItemKind::Window { window }, 2);
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
    cx.run_until_parked();
    assert_eq!(zoom_of(&view, cx, tile), Zoom::FIT);

    let on_picture = on_body(cx, tile);
    pinch(cx, on_picture, 0.3);
    let zoomed = zoom_of(&view, cx, tile);
    assert!((zoomed.scale() - 1.69).abs() < 1e-3, "two steps of 1.3: {zoomed:?}");
    let readout =
        view.read_with(cx, |v, cx| v.screen(tile.item).and_then(|s| s.read(cx).readout()));
    assert!(
        readout.as_ref().is_some_and(|r| r.ends_with('%') && !r.ends_with(" %")),
        "the readout says the size: {readout:?}"
    );

    pinch(cx, on_picture, -0.4);
    assert_eq!(zoom_of(&view, cx, tile), Zoom::FIT, "no smaller than fit");

    view.update_in(cx, |v, _w, cx| v.focus_tile(shell, cx));
    cx.run_until_parked();
    let on_shell = on_body(cx, shell);
    let before = shown(&view, cx);
    for step in [-0.1, 0.1] {
        pinch(cx, on_shell, step);
        assert_eq!(shown(&view, cx), before, "{step}: the layout is as it was");
        assert_eq!(zoom_of(&view, cx, tile), Zoom::FIT, "{step}: and the picture");
    }
}

/// The palette's "Trackpad mode" reaches the active picture even with the keyboard on the
/// workspace, and running it again turns the mode back off.
#[gpui::test]
fn the_palettes_trackpad_mode_reaches_the_active_picture(cx: &mut TestAppContext) {
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
    cx.run_until_parked();
    let trackpad = |cx: &VisualTestContext| {
        view.read_with(cx, |v, cx| v.screen(tile.item).is_some_and(|s| s.read(cx).trackpad()))
    };
    assert!(!trackpad(cx));
    view.update_in(cx, |v, window, cx| {
        v.toggle_trackpad(&crate::screen::ToggleTrackpad, window, cx);
    });
    assert!(trackpad(cx), "on");
    view.update_in(cx, |v, window, cx| {
        v.toggle_trackpad(&crate::screen::ToggleTrackpad, window, cx);
    });
    assert!(!trackpad(cx), "and off again");
    let listed = palette_items().iter().any(|i| i.label == "Trackpad mode");
    assert!(listed, "the palette offers it");
}

/// The palette's "Gestures to the remote app" reaches the active picture with the keyboard on
/// the workspace: off at first, so a pinch zooms here, then on, then off again.
#[gpui::test]
fn the_palettes_remote_gestures_reach_the_active_picture(cx: &mut TestAppContext) {
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
    cx.run_until_parked();
    let remote = |cx: &VisualTestContext| {
        let screen = |v: &WorkspaceView, cx: &App| {
            v.screen(tile.item).is_some_and(|s| s.read(cx).remote_gestures())
        };
        view.read_with(cx, screen)
    };
    assert!(!remote(cx), "off by default: a pinch zooms here");
    let toggle = |cx: &mut VisualTestContext| {
        view.update_in(cx, |v, window, cx| {
            v.toggle_remote_gestures(&crate::screen::ToggleRemoteGestures, window, cx);
        });
    };
    toggle(cx);
    assert!(remote(cx), "on");
    toggle(cx);
    assert!(!remote(cx), "and off again");
    let listed = palette_items().iter().any(|i| i.label == crate::screen::REMOTE_GESTURES);
    assert!(listed, "the palette offers it");
}

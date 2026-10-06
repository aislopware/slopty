//! Touch in the headless workspace. A pinch over a remote picture zooms the picture; over a
//! terminal it leaves the picture and the layout alone. A phone shows one pane, its title the
//! way to the tab's other panes and the project's tabs; an iPad splits by the touch minimum,
//! and in Split View below 900 pt shows one pane as a phone does.

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

fn leak(selector: String) -> &'static str {
    Box::leak(selector.into_boxed_str())
}

fn click_at(cx: &mut VisualTestContext, selector: &'static str) {
    let at = cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is not drawn"));
    cx.simulate_click(at.center(), Modifiers::default());
    cx.run_until_parked();
}

/// The labels of the menu's rows, in order.
fn menu_rows(cx: &mut VisualTestContext) -> Vec<String> {
    tree(cx).into_iter().filter(|n| n.role == "MenuItem").filter_map(|n| n.label).collect()
}

/// A phone draws the focused pane alone over the tab. Its title opens the tab's panes and the
/// project's tabs, the ones on show ticked; a pane picked is focused and drawn, a tab picked
/// shown. With one pane and one tab the title opens nothing.
#[gpui::test]
fn a_phone_shows_one_pane_and_its_title_goes_to_the_others(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let fake = connect(&view, cx, 1, "studio");
    let solo = opens_in(&view, cx, &fake, SessionId::new(), fake.me, 1, Some("/w/solo"));
    cx.simulate_resize(size(px(390.0), px(760.0)));
    cx.run_until_parked();
    assert!(cx.debug_bounds("phone-title").is_some(), "the phone's title");
    assert!(cx.debug_bounds("phone-switch").is_none(), "nothing else to go to");

    cx.simulate_resize(size(px(VIEWPORT.0), px(VIEWPORT.1)));
    cx.run_until_parked();
    cx.simulate_keystrokes("cmd-d");
    let right = opens_in(&view, cx, &fake, SessionId::new(), fake.me, 2, Some("/w/right"));
    let away = opens_in(&view, cx, &fake, SessionId::new(), fake.me, 3, Some("/w/away"));
    on_new_tab(&view, cx, away);
    view.update(cx, |v, cx| v.focus_tile(right, cx));
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.layout().frame().panes.len()), 2, "two panes wide");

    cx.simulate_resize(size(px(390.0), px(760.0)));
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.layout().frame().panes.len()), 1, "one on a phone");
    assert!(drawn_at(&view, cx, right).is_some() && drawn_at(&view, cx, solo).is_none());

    click_at(cx, "phone-switch");
    let rows = menu_rows(cx);
    assert_eq!(rows.len(), 4, "two panes, then two tabs: {rows:?}");
    let title = |cx: &mut VisualTestContext, t: TileRef| {
        view.read_with(cx, |v, _| v.item(t).map(|i| v.tile_title(i))).expect("an item")
    };
    let (solo_title, right_title, away_title) =
        (title(cx, solo), title(cx, right), title(cx, away));
    assert_eq!(rows[..2], [solo_title.clone(), right_title], "the panes, in order");
    assert_eq!(rows[3], away_title, "then the tabs");
    click_at(cx, leak(format!("menu-{solo_title}")));
    assert_eq!(focused(&view, cx), Some(solo), "the pane picked");
    assert!(drawn_at(&view, cx, solo).is_some() && drawn_at(&view, cx, right).is_none());

    click_at(cx, "phone-switch");
    click_at(cx, leak(format!("menu-{away_title}")));
    assert_eq!(focused(&view, cx), Some(away), "the tab picked, shown");
}

/// An iPad splits by the touch minimum: on a 1024 pt screen a shell opened beside the focus
/// gets a pane of its own to its right, where a pointer's minimum would put it below. In Split
/// View below 900 pt it is drawn as a phone: one pane, the phone's title.
#[gpui::test]
fn an_ipad_splits_by_the_touch_room_and_in_split_view_shows_one_pane(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    view.update(cx, |v, _| v.layout.set_config(TilingConfig::TOUCH));
    cx.simulate_resize(size(px(1024.0), px(768.0)));
    cx.run_until_parked();
    let fake = connect(&view, cx, 1, "studio");
    let first = opens(&view, cx, &fake, SessionId::new(), fake.me, 1);
    view.update(cx, |v, cx| v.open_command(vec!["top".to_owned()], cx));
    let beside = opens(&view, cx, &fake, SessionId::new(), fake.me, 2);
    let area = view.read_with(cx, |v, _| v.layout().area().w);
    assert!(
        area < 2.0 * slopty_client::layout::tree::PANE_MIN_W,
        "too narrow for a pointer's: {area}"
    );
    let (a, b) = (pos_of(&view, cx, first), pos_of(&view, cx, beside));
    assert_eq!(a.tab, b.tab, "in the tab");
    assert_ne!(a.pane, b.pane, "a pane of its own");
    let (left, right) =
        (drawn_at(&view, cx, first).expect("drawn"), drawn_at(&view, cx, beside).expect("drawn"));
    assert!(right.left() >= left.right() - px(0.5), "beside, not below: {left:?} {right:?}");
    assert!(cx.debug_bounds("phone-title").is_none(), "an iPad's bar");

    // Split View's two thirds of an 11-inch iPad: wider than a phone by a pointer's rule.
    cx.simulate_resize(size(px(800.0), px(768.0)));
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |v, _| v.layout().frame().panes.len()), 1, "one pane");
    assert!(cx.debug_bounds("phone-switch").is_some(), "the phone's title, its way to the other");
}

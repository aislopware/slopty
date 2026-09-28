//! A remote tile shown in a window of its own: the same stream moves into the new window and
//! back, with nothing closed or opened on the worker, and the keyboard goes with it.

use slopty_core::WindowId;
use slopty_proto::screen::VideoCodec;

use super::*;
use crate::workspace::actions::ToggleOwnWindow;
use crate::workspace::popout::{BACK_TO_WORKSPACE, OPEN_OWN_WINDOW, PopOutView};

fn palette_has(view: &Entity<WorkspaceView>, cx: &mut VisualTestContext, label: &str) -> bool {
    view.update(cx, |v, cx| v.palette_lines(cx).iter().any(|l| l.label == label))
}

/// Whether anything sent closed or opened a stream.
fn reopened(msgs: &[ClientMsg]) -> bool {
    msgs.iter().any(|m| {
        matches!(
            m,
            ClientMsg::Screen(
                ScreenRequest::Close(_)
                    | ScreenRequest::Open { .. }
                    | ScreenRequest::OpenDisplay { .. }
            )
        )
    })
}

/// "Open in its own window" draws the tile's stream, the same view, in a new window that has
/// the keyboard, and the tile says where it went; "Back to the workspace" closes that window
/// and draws the view in its tile again. The worker hears neither.
#[gpui::test]
fn a_tile_pops_out_into_its_own_window_and_back(cx: &mut TestAppContext) {
    let (view, cx) = workspace(cx);
    let mut fake = connect(&view, cx, 1, "studio");
    let window = WindowId(7);
    let tile = arrives(&view, cx, &fake, ItemKind::Window { window }, 1);
    let key = fake.key;
    view.update_in(cx, |v, _w, cx| {
        let event = ScreenEvent::Opened {
            stream: StreamId(1),
            target: CaptureTarget::Window(window),
            codec: VideoCodec::Hevc,
            width: 1600,
            height: 1000,
            scale: 2.0,
        };
        v.screen_event(key, event, cx);
        v.focus_tile(tile, cx);
    });
    cx.run_until_parked();
    fake.drain();
    let screen = view.read_with(cx, |v, _| v.screen(tile.item).cloned()).expect("streaming");
    assert!(palette_has(&view, cx, OPEN_OWN_WINDOW));
    let placeholder = selector("waiting", tile.item);
    assert!(cx.debug_bounds(placeholder).is_none(), "the tile draws its stream");

    let toggle = |cx: &mut VisualTestContext| {
        view.update_in(cx, |v, window, cx| v.toggle_own_window(&ToggleOwnWindow, window, cx));
        cx.run_until_parked();
    };
    let before = cx.update(|_, cx| cx.windows().len());
    let renders = screen.read_with(cx, |s, _| s.renders());
    toggle(cx);
    let windows = cx.update(|_, cx| cx.windows());
    assert!(windows.len() > before, "a window of its own");
    let popped = windows
        .iter()
        .find_map(gpui::AnyWindowHandle::downcast::<PopOutView>)
        .expect("the tile's own window");
    let (shown, focused) = popped
        .update(cx, |p, window, cx| {
            let shown = p.screen().cloned();
            let focused =
                shown.as_ref().is_some_and(|s| s.read(cx).focus_handle(cx).is_focused(window));
            (shown, focused)
        })
        .expect("open");
    assert_eq!(shown.map(|s| s.entity_id()), Some(screen.entity_id()), "the same stream's view");
    assert!(focused, "the keyboard went with it");
    assert!(screen.read_with(cx, |s, _| s.renders()) > renders, "drawn in the new window");
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    assert!(cx.debug_bounds(placeholder).is_some(), "the tile says where it went");
    assert!(palette_has(&view, cx, BACK_TO_WORKSPACE));
    assert!(view.read_with(cx, |v, _| v.screen(tile.item).is_some()), "the stream stays");

    let renders = screen.read_with(cx, |s, _| s.renders());
    toggle(cx);
    assert_eq!(cx.update(|_, cx| cx.windows().len()), before, "its window is gone");
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    assert!(cx.debug_bounds(placeholder).is_none(), "the tile draws its stream again");
    assert!(screen.read_with(cx, |s, _| s.renders()) > renders, "back in the workspace");
    assert!(palette_has(&view, cx, OPEN_OWN_WINDOW));
    let sent = fake.drain();
    assert!(!reopened(&sent), "no stream was closed or opened: {sent:?}");
}

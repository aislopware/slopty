use gpui::{AppContext as _, Entity, TestAppContext, VisualTestContext, point, px, size};
use slopty_client::ScreenHandle;
use slopty_client::dnd::{Outcome, Read};
use slopty_core::{DisplayId, StreamId};
use slopty_proto::ClientMsg;
use slopty_proto::drag::{DragEvent, DragInput, DragOp, DragOps, Promised};
use slopty_proto::screen::{CaptureTarget, Quality, ScreenInput, ScreenRequest};
use slopty_theme::Theme;
use tokio::sync::mpsc;

use crate::screen::{OUTBOX_DEPTH, Opened, ScreenView};

/// An 800 × 600 display stream in a 400 × 300 window: a point maps to twice itself.
fn windowed(
    cx: &mut TestAppContext,
) -> (Entity<ScreenView>, mpsc::Receiver<ClientMsg>, &mut VisualTestContext) {
    let (out, rx) = mpsc::channel(64);
    let opened = Opened {
        stream: StreamId(4),
        target: CaptureTarget::Display(DisplayId(2)),
        size: (800, 600),
        quality: Quality { scale: 1.0, ..Quality::default() },
    };
    let (view, cx) = cx.add_window_view(|_window, cx| {
        ScreenView::new(opened, ScreenHandle::detached(StreamId(4)), out, Theme::default(), cx)
    });
    cx.simulate_resize(size(px(400.0), px(300.0)));
    cx.run_until_parked();
    (view, rx, cx)
}

/// The drag steps the view sent, in order.
fn drags(rx: &mut mpsc::Receiver<ClientMsg>) -> Vec<DragInput> {
    std::iter::from_fn(|| rx.try_recv().ok())
        .filter_map(|m| match m {
            ClientMsg::Screen(ScreenRequest::Input { input: ScreenInput::Drag(d), .. }) => Some(d),
            _ => None,
        })
        .collect()
}

/// A point of the picture by fractions of its bounds.
fn at(
    view: &Entity<ScreenView>,
    cx: &VisualTestContext,
    fx: f32,
    fy: f32,
) -> gpui::Point<gpui::Pixels> {
    let b = view.read_with(cx, |v, _| v.bounds);
    point(b.origin.x + b.size.width * fx, b.origin.y + b.size.height * fy)
}

/// A drag over the picture reaches the worker in the stream's pixels, its entry first and its
/// moves only when it moved; what a drop would do is the worker's last answer, which the drag
/// shows as its badge.
#[gpui::test]
fn the_badge_is_the_workers_last_answer(cx: &mut TestAppContext) {
    let (view, mut rx, cx) = windowed(cx);
    drop(drags(&mut rx));
    let (quarter, half) = (at(&view, cx, 0.25, 0.5), at(&view, cx, 0.5, 0.5));
    let drag = view.update(cx, |v, cx| v.drag_enter(quarter, &Read::default(), DragOps::COPY, cx));
    assert_eq!(
        view.update(cx, |v, _| v.drag_move(quarter)),
        DragOp::Copy,
        "a copy until it speaks"
    );
    assert_eq!(view.update(cx, |v, _| v.drag_move(half)), DragOp::Copy);
    let sent = drags(&mut rx);
    let [DragInput::Enter { x, y, .. }, DragInput::Move { x: mx, y: my, .. }] = sent.as_slice()
    else {
        panic!("an entry and one move: {sent:?}");
    };
    assert!((x - 200.0).abs() < 1.0 && (y - 300.0).abs() < 1.0, "stream pixels: {x}, {y}");
    assert!((mx - 400.0).abs() < 1.0 && (my - 300.0).abs() < 1.0, "{mx}, {my}");
    view.update(cx, |v, cx| v.drag_heard(&DragEvent::Operation { drag, op: DragOp::None }, cx));
    assert_eq!(view.update(cx, |v, _| v.drag_move(half)), DragOp::None, "the target refuses");
    assert!(!view.read_with(cx, |v, _| v.drag_takes()));
    view.update(cx, |v, cx| v.drag_heard(&DragEvent::Operation { drag, op: DragOp::Copy }, cx));
    assert!(view.read_with(cx, |v, _| v.drag_takes()));
    assert!(drags(&mut rx).is_empty(), "a still drag sends nothing");
}

/// A drop where the worker said nothing takes it is refused here, so it slides back as over a
/// local target that refuses: no drop goes, and the worker's drag ends with the leave.
#[gpui::test]
fn a_refused_drop_slides_back_and_sends_nothing(cx: &mut TestAppContext) {
    let (view, mut rx, cx) = windowed(cx);
    let p = at(&view, cx, 0.5, 0.5);
    let drag = view.update(cx, |v, cx| v.drag_enter(p, &Read::default(), DragOps::COPY, cx));
    view.update(cx, |v, cx| v.drag_heard(&DragEvent::Operation { drag, op: DragOp::None }, cx));
    drop(drags(&mut rx));
    assert!(!view.update(cx, |v, cx| v.drag_drop(p, Vec::new(), cx)));
    assert_eq!(drags(&mut rx), [DragInput::Leave { drag }]);
    assert_eq!(view.read_with(cx, |v, _| v.dragging()), None);
}

/// While a drag is over the tile the worker's pointer is not drawn. Once dropped the ring
/// turns at the point, from a drop that waits for its promised files on, and goes when the
/// worker says how the drop ended, which the tile hands on once.
#[gpui::test]
fn a_held_drop_rings_and_ends_with_the_workers_word(cx: &mut TestAppContext) {
    let (view, mut rx, cx) = windowed(cx);
    let p = at(&view, cx, 0.5, 0.5);
    let drag = view.update(cx, |v, cx| v.drag_enter(p, &Read::default(), DragOps::COPY, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("screen-pointer").is_none(), "the system's drag is the pointer");
    assert!(cx.debug_bounds("screen-drop-ring").is_none(), "no ring while hovering");
    view.update(cx, |v, cx| v.drag_hold(p, cx));
    cx.run_until_parked();
    let ring = cx.debug_bounds("screen-drop-ring").expect("the ring while promises are written");
    assert!((ring.center().x - p.x).abs() < px(1.0) && (ring.center().y - p.y).abs() < px(1.0));
    drop(drags(&mut rx));
    let promised = vec![Promised { item: 0, file: None }];
    assert!(view.update(cx, |v, cx| v.drag_drop(p, promised.clone(), cx)));
    let sent = drags(&mut rx);
    assert!(
        matches!(sent.as_slice(), [DragInput::Drop { promised: p, .. }] if *p == promised),
        "{sent:?}"
    );
    let ended = DragEvent::Ended { drag, op: DragOp::None, error: Some("x: gone".to_owned()) };
    let heard = view.update(cx, |v, cx| v.drag_heard(&ended, cx));
    assert_eq!(heard, Some((drag, Outcome::Failed("x: gone".to_owned()))));
    assert_eq!(view.update(cx, |v, cx| v.drag_heard(&ended, cx)), None, "once");
    cx.run_until_parked();
    assert!(cx.debug_bounds("screen-drop-ring").is_none(), "gone with the end");
}

/// A full outbound queue keeps every step of a drag but its moves, which go as the last one
/// of the drag waiting.
#[gpui::test]
fn a_full_queue_coalesces_a_drags_moves_and_keeps_its_steps(cx: &mut TestAppContext) {
    let (out, mut rx) = mpsc::channel(1);
    let opened = Opened {
        stream: StreamId(4),
        target: CaptureTarget::Display(DisplayId(2)),
        size: (800, 600),
        quality: Quality { scale: 1.0, ..Quality::default() },
    };
    let view = cx.new(|cx| {
        ScreenView::new(opened, ScreenHandle::detached(StreamId(4)), out, Theme::default(), cx)
    });
    let drag = slopty_proto::drag::DragId::new();
    let mv = |x: f32| ScreenInput::Drag(DragInput::Move { drag, x, y: 0.0 });
    let step = |input: DragInput| ScreenInput::Drag(input);
    let enter =
        DragInput::Enter { drag, x: 0.0, y: 0.0, allowed: DragOps::COPY, items: Vec::new() };
    let drop_at = DragInput::Drop { drag, x: 9.0, y: 0.0, promised: Vec::new() };
    view.update(cx, |v, _| {
        v.input(step(enter.clone()));
        for x in 1..=OUTBOX_DEPTH + 4 {
            #[expect(clippy::cast_precision_loss, reason = "a small count")]
            v.input(mv(x as f32));
        }
        v.input(step(drop_at.clone()));
        v.input(step(DragInput::Leave { drag }));
    });
    let mut got = Vec::new();
    loop {
        cx.run_until_parked();
        match rx.try_recv() {
            Ok(ClientMsg::Screen(ScreenRequest::Input { input, .. })) => got.push(input),
            Ok(other) => panic!("{other:?}"),
            Err(_) => break,
        }
    }
    #[expect(clippy::cast_precision_loss, reason = "a small count")]
    let last = mv((OUTBOX_DEPTH + 4) as f32);
    assert_eq!(got, [step(enter), last, step(drop_at), step(DragInput::Leave { drag })]);
}

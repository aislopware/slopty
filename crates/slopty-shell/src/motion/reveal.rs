//! Two small motions every surface shares: a copy control's "Copied" span, and a section
//! that grows into its height instead of arriving at it in one frame
//! (`.research/micro-details-2026-10-16.md`, M3 and M5).
#![allow(
    missing_debug_implementations,
    missing_docs,
    clippy::arithmetic_side_effects,
    clippy::clone_on_ref_ptr,
    clippy::float_cmp,
    clippy::needless_pass_by_ref_mut,
    clippy::return_self_not_must_use,
    clippy::too_long_first_doc_paragraph,
    reason = "shell lint migration: slice D"
)]

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, ElementId, Global, IntoElement, ParentElement as _, RenderOnce, SharedString,
    Styled as _, Window, div, px,
};

use super::{COLLAPSE, FoldFrame, fold_frame, lerp, reduced_motion, speed_scale};

/// How long a copy control shows its check (and its tooltip "Copied") after a copy.
pub const COPIED_FOR: Duration = Duration::from_millis(1500);

/// The copy controls in their "Copied" span, each with the copy that started it (a second copy
/// restarts the span, and the first one's timer then leaves it be).
#[derive(Default)]
struct CopiedMarks {
    marks: HashMap<SharedString, u64>,
    next: u64,
}

impl Global for CopiedMarks {}

/// Starts `key`'s "Copied" span: [`copied`] reads true for [`COPIED_FOR`], then false. Every
/// copy control keeps its size: it swaps COPY for CHECK ([`copy_icon`]), never its words.
pub fn copy_feedback(key: impl Into<SharedString>, cx: &mut App) {
    let key: SharedString = key.into();
    let mark = {
        let marks = cx.default_global::<CopiedMarks>();
        marks.next += 1;
        let mark = marks.next;
        marks.marks.insert(key.clone(), mark);
        mark
    };
    let delay = COPIED_FOR;
    cx.spawn(async move |cx| {
        cx.background_executor().timer(delay).await;
        cx.update(|cx| {
            if cx.try_global::<CopiedMarks>().and_then(|marks| marks.marks.get(&key)) == Some(&mark)
            {
                cx.global_mut::<CopiedMarks>().marks.remove(&key);
            }
        });
    })
    .detach();
}

/// Whether `key` is in its "Copied" span. A view that reads it is built again when it ends.
pub fn copied(key: &str, cx: &App) -> bool {
    cx.try_global::<CopiedMarks>().is_some_and(|marks| marks.marks.contains_key(key))
}

/// The glyph a copy control shows now: the check while copied, else the copy glyph.
pub fn copy_icon(key: &str, cx: &App) -> &'static str {
    if copied(key, cx) { crate::icons::CHECK } else { crate::icons::COPY }
}

/// A section that grows from nothing to its height over [`COLLAPSE`] as `open` turns true and
/// shrinks back as it turns false, so what sits under it slides rather than jumps. Pass the same
/// `key` every frame; keep passing the child while it closes, or it vanishes at once. Reduced
/// motion snaps. A section mounted open grows in.
pub fn reveal_height(
    key: impl Into<ElementId>,
    open: bool,
    child: impl IntoElement,
) -> RevealHeight {
    RevealHeight { key: key.into(), open, fold: false, child: child.into_any_element() }
}

#[derive(IntoElement)]
pub struct RevealHeight {
    key: ElementId,
    open: bool,
    /// Moves as the one fold ([`fold_frame`]) instead of [`COLLAPSE`].
    fold: bool,
    child: AnyElement,
}

impl RevealHeight {
    /// Moves as the one fold groups, cards and trays share ([`fold_frame`];
    /// design-inspiration item 7): the height opens first and the contents fade in and rise
    /// after it; closing, the contents are gone before the height lands.
    pub const fn fold(mut self) -> Self {
        self.fold = true;
        self
    }
}

struct RevealState {
    open: bool,
    /// The child's own height at its last layout.
    natural: Rc<Cell<f32>>,
    tween: Option<(f32, Instant)>,
}

impl RevealState {
    /// The height drawn now, the contents' opacity and rise, and whether a tween is still
    /// running.
    fn fold(&mut self, now: Instant, reduced: bool) -> (Option<f32>, FoldFrame, bool) {
        let settled = FoldFrame { height: 1.0, opacity: 1.0, rise: 0.0 };
        let target = if self.open { self.natural.get() } else { 0.0 };
        if let Some((from, started)) = self.tween
            && !reduced
        {
            let frame = fold_frame(self.open, now.saturating_duration_since(started), false);
            let share = if self.open { frame.height } else { 1.0 - frame.height };
            let landed = share >= 1.0
                && (if self.open { frame.opacity >= 1.0 } else { frame.opacity <= 0.0 });
            if !landed {
                return (Some(lerp(from, target, share)), frame, true);
            }
        }
        self.tween = None;
        (if self.open { None } else { Some(0.0) }, settled, false)
    }

    /// The height drawn now, and whether a tween is still running.
    fn height(&mut self, now: Instant, reduced: bool) -> (Option<f32>, bool) {
        let target = if self.open { self.natural.get() } else { 0.0 };
        if let Some((from, started)) = self.tween
            && !reduced
        {
            let total = COLLAPSE.total().mul_f32(speed_scale()).as_secs_f32();
            let raw =
                now.saturating_duration_since(started).as_secs_f32() / total.max(f32::EPSILON);
            if raw < 1.0 {
                return (Some(lerp(from, target, COLLAPSE.progress(raw))), true);
            }
        }
        self.tween = None;
        (if self.open { None } else { Some(0.0) }, false)
    }
}

impl RenderOnce for RevealHeight {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let reduced = reduced_motion(cx);
        let open = self.open;
        let state = window.use_keyed_state(self.key.clone(), cx, |_, _| RevealState {
            open: false,
            natural: Rc::new(Cell::new(0.0)),
            tween: None,
        });
        // The frame's moment, not the wall clock: a frame drawn again from scratch at the same
        // moment (the retained check, the self-test's dump) stands the section where it stood,
        // and a tween's last frame short of its end is drawn where it was.
        let now = window.frame_time();
        let fold = self.fold;
        let (height, frame, running, natural) = state.update(cx, |state, _| {
            let at = |state: &mut RevealState| {
                if fold {
                    state.fold(now, reduced)
                } else {
                    let (height, running) = state.height(now, reduced);
                    (height, FoldFrame { height: 1.0, opacity: 1.0, rise: 0.0 }, running)
                }
            };
            if state.open != open {
                let (drawn, ..) = at(state);
                let drawn = drawn.unwrap_or_else(|| state.natural.get());
                state.tween = Some((drawn, now));
                state.open = open;
            }
            let (height, frame, running) = at(state);
            (height, frame, running, state.natural.clone())
        });
        if running {
            window.request_animation_frame();
        }
        let shown = open || running;
        let child = self.child;
        div()
            .on_children_prepainted(move |bounds, _, _| {
                if let Some(child) = bounds.first() {
                    natural.set(f32::from(child.size.height));
                }
            })
            .when_some(height, |el, height| el.h(px(height)).overflow_hidden())
            .when(shown, |el| {
                el.child(
                    div()
                        .flex_none()
                        .w_full()
                        .when(frame.opacity < 1.0, |el| el.opacity(frame.opacity))
                        .when(frame.rise > 0.0, |el| el.mt(px(frame.rise)))
                        .child(child),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use gpui::{Context, InteractiveElement as _, Render, TestAppContext};

    use super::*;

    #[gpui::test]
    fn a_copy_span_ends_on_its_own(cx: &mut TestAppContext) {
        cx.update(|cx| {
            copy_feedback("copy-message", cx);
            assert!(copied("copy-message", cx));
            assert_eq!(copy_icon("copy-message", cx), crate::icons::CHECK);
            assert!(!copied("copy-plan", cx));
        });
        cx.executor().advance_clock(COPIED_FOR / 2);
        // A second copy restarts the span; the first timer leaves it be.
        cx.update(|cx| copy_feedback("copy-message", cx));
        cx.executor().advance_clock(COPIED_FOR / 2 + Duration::from_millis(10));
        cx.run_until_parked();
        cx.update(|cx| assert!(copied("copy-message", cx)));
        cx.executor().advance_clock(COPIED_FOR);
        cx.run_until_parked();
        cx.update(|cx| {
            assert!(!copied("copy-message", cx));
            assert_eq!(copy_icon("copy-message", cx), crate::icons::COPY);
        });
    }

    struct Section {
        open: bool,
    }

    impl Render for Section {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().w(px(100.0)).flex().flex_col().child(
                div().debug_selector(|| "section".into()).child(reveal_height(
                    "notice",
                    self.open,
                    div().h(px(40.0)),
                )),
            )
        }
    }

    #[gpui::test]
    fn a_section_snaps_under_reduced_motion_and_closes_to_nothing(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (view, cx) = cx.add_window_view(|_, _| Section { open: true });
        let height = |cx: &mut gpui::VisualTestContext| {
            cx.update(|window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
            });
            // A second frame reads the height the first one measured.
            cx.update(|window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
            });
            f32::from(cx.debug_bounds("section").expect("drawn").size.height)
        };
        assert_eq!(height(cx), 40.0);
        view.update(cx, |section, cx| {
            section.open = false;
            cx.notify();
        });
        assert_eq!(height(cx), 0.0);
    }

    /// A fold opens its height first and fades its contents in after, risen; closing,
    /// the contents are gone before the height lands.
    #[test]
    fn a_fold_opens_its_height_before_its_contents() {
        let start = Instant::now();
        let mut state = RevealState {
            open: true,
            natural: Rc::new(Cell::new(40.0)),
            tween: Some((0.0, start)),
        };
        let (height, frame, running) = state.fold(start + Duration::from_millis(60), false);
        assert!(running);
        assert!(
            height.is_some_and(|h| h > 10.0 && h < 40.0),
            "the height is on its way: {height:?}"
        );
        assert!(frame.opacity == 0.0 && frame.rise > 0.0, "the contents wait, below their place");
        let (_, frame, _) = state.fold(start + Duration::from_millis(200), false);
        assert!(frame.opacity > 0.0 && frame.opacity < 1.0, "then fade in");
        let (height, frame, running) = state.fold(start + Duration::from_millis(400), false);
        assert!(!running && height.is_none() && frame.opacity == 1.0 && frame.rise == 0.0);

        state.open = false;
        state.tween = Some((40.0, start));
        let (height, frame, _) = state.fold(start + Duration::from_millis(150), false);
        assert!(frame.opacity <= 0.0, "the contents are gone");
        assert!(height.is_some_and(|h| h > 0.0), "before the height lands");
        let (height, _, running) = state.fold(start + Duration::from_millis(300), false);
        assert!(!running && height == Some(0.0));
    }
}

//! slopty: two small motions every surface shares: a copy control's "Copied" span, and a section
//! that grows into its height instead of arriving at it in one frame
//! (`.research/micro-details-2026-10-16.md`, M3 and M5).

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, ElementId, Global, IntoElement, ParentElement as _, RenderOnce, SharedString,
    Styled as _, Window, div, prelude::FluentBuilder as _, px,
};

use super::{COLLAPSE, lerp, reduced_motion, speed_scale};

/// How long a copy control shows its check (and its tooltip "Copied") after a copy.
pub const COPIED_FOR: Duration = Duration::from_millis(1200);

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
            if cx.try_global::<CopiedMarks>().and_then(|marks| marks.marks.get(&key)) == Some(&mark) {
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
    RevealHeight {
        key: key.into(),
        open,
        child: child.into_any_element(),
    }
}

#[derive(IntoElement)]
pub struct RevealHeight {
    key: ElementId,
    open: bool,
    child: AnyElement,
}

struct RevealState {
    open: bool,
    /// The child's own height at its last layout.
    natural: Rc<Cell<f32>>,
    tween: Option<(f32, Instant)>,
}

impl RevealState {
    /// The height drawn now, and whether a tween is still running.
    fn height(&mut self, now: Instant, reduced: bool) -> (Option<f32>, bool) {
        let target = if self.open { self.natural.get() } else { 0.0 };
        if let Some((from, started)) = self.tween
            && !reduced
        {
            let total = COLLAPSE.total().mul_f32(speed_scale()).as_secs_f32();
            let raw = now.saturating_duration_since(started).as_secs_f32() / total.max(f32::EPSILON);
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
        let now = Instant::now();
        let (height, running, natural) = state.update(cx, |state, _| {
            if state.open != open {
                let (drawn, _) = state.height(now, reduced);
                let drawn = drawn.unwrap_or_else(|| state.natural.get());
                state.tween = Some((drawn, now));
                state.open = open;
            }
            let (height, running) = state.height(now, reduced);
            (height, running, state.natural.clone())
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
            .when(shown, |el| el.child(div().flex_none().w_full().child(child)))
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
}

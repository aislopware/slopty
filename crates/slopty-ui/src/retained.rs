//! The retained-mode oracle: what the window shows against what a frame drawn from scratch
//! shows.
//!
//! GPUI draws a view again only when something it read changed, and draws everything else from
//! the last frame. A view whose state changed without it being notified keeps showing what it
//! showed. [`stale`] catches that: it reads what the window last painted, draws the same state
//! again with every view built from scratch, and says where the two differ. The headless tests
//! run it after each step of an interaction; the self-test runs it in every `dump`.

use gpui::{App, Window};

/// What the window last painted, one line per primitive, sorted.
///
/// A scroll layer the frame composited counts as the content its tiles were rasterized from,
/// moved to where the tiles show it and clipped to its viewport: a frame drawn from scratch may
/// paint that content directly, and the two show the same pixels. Each line holds a primitive's
/// bounds, clip and colour, and its depth: one more than the deepest primitive painted before it
/// that it overlaps. Two frames that paint the same things in the same places, each above what
/// it was above, give the same lines however they numbered or ordered their draws.
#[must_use]
pub fn painted(window: &Window) -> Vec<String> {
    let mut lines = window.painted_primitives();
    lines.sort_unstable();
    lines
}

/// Where the frame the window shows differs from the same state drawn from scratch.
///
/// Gives the lines only one of them painted, at most `limit` of each, or `None` when they
/// agree. Draws a frame from scratch, which the window then shows. When they differ it draws
/// another: something that moves on the wall clock (a fade, a spinner) paints differently a
/// few milliseconds later however the frame was drawn, so two frames from scratch that differ
/// say the window is in motion. Then only what both paint alike, what holds still, is judged:
/// the frame shown must paint it too.
pub fn stale(window: &mut Window, cx: &mut App, limit: usize) -> Option<String> {
    // GPUI's fades run on the wall clock: a slow machine can paint one half way and draw the
    // frames from scratch after it has landed, and no comparison then holds.
    #[cfg(test)]
    assert!(cx.reduce_motion(), "a frame is judged only under Reduce Motion");
    let shown = painted(window);
    let scratch = from_scratch(window, cx);
    if shown == scratch {
        return None;
    }
    let again = from_scratch(window, cx);
    if again == scratch {
        return diff(&shown, &scratch, limit);
    }
    let still = common(&scratch, &again);
    let missed = only(&still, &shown);
    (!missed.is_empty()).then(|| {
        format!(
            "in motion, {} that holds still in a frame from scratch was not painted:\n  {}",
            missed.len(),
            cut(&missed, limit)
        )
    })
}

/// The lines of sorted `a` that sorted `b` does not hold, a repeat counting once per copy.
fn only(a: &[String], b: &[String]) -> Vec<String> {
    let mut rest = b.iter().peekable();
    let mut out = Vec::new();
    for line in a {
        while rest.next_if(|other| *other < line).is_some() {}
        if rest.next_if(|other| *other == line).is_none() {
            out.push(line.clone());
        }
    }
    out
}

/// The lines two sorted paintings both hold, a repeat as often as both hold it.
fn common(a: &[String], b: &[String]) -> Vec<String> {
    let mut rest = b.iter().peekable();
    let mut out = Vec::new();
    for line in a {
        while rest.next_if(|other| *other < line).is_some() {}
        if rest.next_if(|other| *other == line).is_some() {
            out.push(line.clone());
        }
    }
    out
}

/// `lines`, at most `limit` of them, one to a line.
fn cut(lines: &[String], limit: usize) -> String {
    let mut text: Vec<&str> = lines.iter().take(limit).map(String::as_str).collect();
    if lines.len() > limit {
        text.push("…");
    }
    text.join("\n  ")
}

/// What a frame drawn with every view built from scratch paints.
fn from_scratch(window: &mut Window, cx: &mut App) -> Vec<String> {
    window.refresh();
    window.draw(cx).clear(cx);
    painted(window)
}

/// The lines of two sorted paintings that only one holds, at most `limit` of each side.
fn diff(shown: &[String], scratch: &[String], limit: usize) -> Option<String> {
    if shown == scratch {
        return None;
    }
    let (drawn, missed) = (only(shown, scratch), only(scratch, shown));
    Some(format!(
        "{} painted that a frame from scratch does not:\n  {}\n{} a frame from scratch paints that were not:\n  {}",
        drawn.len(),
        cut(&drawn, limit),
        missed.len(),
        cut(&missed, limit)
    ))
}

#[cfg(test)]
mod tests {
    use gpui::{
        Context, IntoElement, ParentElement as _, Render, Styled as _, TestAppContext, Window, div,
        px,
    };

    use super::{diff, stale};

    /// A bar as wide as its count.
    struct Bar(f32);

    impl Render for Bar {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(div().w(px(self.0)).h(px(4.0)).bg(gpui::black()))
        }
    }

    /// A view changed without being told is caught, and once told it agrees again: the oracle
    /// can fail, and a frame from scratch after a frame from scratch never does.
    #[gpui::test]
    fn a_view_changed_untold_is_stale_until_told(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (bar, cx) = cx.add_window_view(|_, _| Bar(10.0));
        cx.run_until_parked();
        assert_eq!(cx.update(|window, cx| stale(window, cx, 4)), None, "drawn as it was");
        assert_eq!(cx.update(|window, cx| stale(window, cx, 4)), None, "scratch after scratch");
        bar.update(cx, |bar, _| bar.0 = 20.0);
        cx.run_until_parked();
        let found = cx.update(|window, cx| stale(window, cx, 4));
        assert!(found.is_some(), "a bar widened untold still shows 10 wide");
        bar.update(cx, |bar, cx| {
            bar.0 = 30.0;
            cx.notify();
        });
        cx.run_until_parked();
        assert_eq!(cx.update(|window, cx| stale(window, cx, 4)), None, "told, it is drawn anew");
    }

    /// A mark that stands somewhere new every time it is drawn, as a spinner on the wall clock.
    struct Spinner;

    impl Render for Spinner {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            gpui::canvas(
                |_, _, _| (),
                |bounds, (), window, _| {
                    let turn = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |d| d.subsec_nanos() % 97);
                    let at = bounds.origin
                        + gpui::point(px(f32::from(u8::try_from(turn).unwrap_or(0))), px(0.0));
                    window.paint_quad(gpui::fill(
                        gpui::Bounds::new(at, gpui::size(px(2.0), px(2.0))),
                        gpui::black(),
                    ));
                    window.request_animation_frame();
                },
            )
            .size_full()
        }
    }

    /// Beside something in motion, a view changed without being told is still caught: what
    /// holds still in the frames from scratch must be in the frame shown.
    #[gpui::test]
    fn a_view_changed_untold_is_caught_beside_motion(cx: &mut TestAppContext) {
        use gpui::AppContext as _;
        cx.update(|cx| cx.set_reduce_motion(true));
        let (both, cx) =
            cx.add_window_view(|_, cx| Both(cx.new(|_| Bar(10.0)), cx.new(|_| Spinner)));
        let bar = both.read_with(cx, |both, _| both.0.clone());
        cx.run_until_parked();
        assert_eq!(cx.update(|window, cx| stale(window, cx, 4)), None, "drawn as it is");
        bar.update(cx, |bar, _| bar.0 = 20.0);
        cx.run_until_parked();
        let found = cx.update(|window, cx| stale(window, cx, 4));
        assert!(found.is_some(), "the bar widened untold, beside the spinner");
    }

    /// A bar and a spinner side by side.
    struct Both(gpui::Entity<Bar>, gpui::Entity<Spinner>);

    impl Render for Both {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div().size_full().flex().child(self.0.clone()).child(self.1.clone())
        }
    }

    /// A list of 200 rows 20 px tall, each a coloured bar with its number.
    struct Rows;

    impl Render for Rows {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div().size_full().bg(gpui::white()).child(
                gpui::uniform_list(
                    "rows",
                    200,
                    cx.processor(|_, range: std::ops::Range<usize>, _, _| {
                        range
                            .map(|row| {
                                div()
                                    .h(px(20.0))
                                    .w(px(160.0))
                                    .bg(gpui::rgb(
                                        u32::try_from(row).unwrap_or(0).wrapping_mul(0x0001_0101),
                                    ))
                                    .child(format!("row {row}"))
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .w(px(160.0))
                .h(px(200.0)),
            )
        }
    }

    /// A frame that composites a list's scroll layer from its tiles agrees with the same state
    /// drawn from scratch, which may paint the rows directly: what a tile holds is what is
    /// compared, not the tile.
    #[gpui::test]
    fn a_frame_composited_from_a_layer_agrees_with_one_from_scratch(cx: &mut TestAppContext) {
        use gpui::{Modifiers, ScrollDelta, ScrollWheelEvent, TouchPhase, point};
        cx.update(|cx| cx.set_reduce_motion(true));
        let (_rows, cx) = cx.add_window_view(|_, _| Rows);
        cx.run_until_parked();
        let wheel = |cx: &mut gpui::VisualTestContext| {
            cx.simulate_event(ScrollWheelEvent {
                position: point(px(40.0), px(40.0)),
                delta: ScrollDelta::Pixels(point(px(0.0), px(-15.0))),
                modifiers: Modifiers::default(),
                touch_phase: TouchPhase::Moved,
                momentum_phase: None,
            });
            cx.run_until_parked();
        };
        for _ in 0..4 {
            wheel(cx);
        }
        cx.update(|window, _| window.reset_layout_stats());
        wheel(cx);
        let composited = cx.update(|window, _| window.layout_stats().layer_frames_composited);
        assert_eq!(composited, 1, "the frame shown composites the list's layer");
        assert_eq!(cx.update(|window, cx| stale(window, cx, 8)), None);
    }

    fn lines(text: &[&str]) -> Vec<String> {
        let mut out: Vec<String> = text.iter().map(|s| (*s).to_owned()).collect();
        out.sort_unstable();
        out
    }

    #[test]
    fn agreeing_paintings_have_no_difference() {
        assert_eq!(diff(&lines(&["a", "b"]), &lines(&["b", "a"]), 4), None);
    }

    #[test]
    fn a_difference_names_each_side_and_counts_repeats() {
        let shown = lines(&["a", "b", "b", "stale"]);
        let scratch = lines(&["a", "b", "fresh"]);
        let text = diff(&shown, &scratch, 4).unwrap_or_default();
        assert!(
            text.starts_with("2 painted that a frame from scratch does not:\n  b\n  stale"),
            "{text}"
        );
        assert!(text.ends_with("1 a frame from scratch paints that were not:\n  fresh"), "{text}");
    }

    #[test]
    fn a_long_difference_is_cut_to_the_limit() {
        let shown = lines(&["1", "2", "3"]);
        let text = diff(&shown, &[], 2).unwrap_or_default();
        assert!(
            text.contains("3 painted that a frame from scratch does not:\n  1\n  2\n  …"),
            "{text}"
        );
    }
}

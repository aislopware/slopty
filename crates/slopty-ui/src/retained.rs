//! The retained-mode oracle: what the window shows against what a frame drawn from scratch
//! shows.
//!
//! GPUI draws a view again only when something it read changed, and draws everything else from
//! the last frame. A view whose state changed without it being notified keeps showing what it
//! showed. [`stale`] catches that: it reads what the window last painted, draws the same state
//! again with every view built from scratch, and says where the two differ. The headless tests
//! run it after each step of an interaction; the self-test runs it in every `dump`.

use gpui::{App, Window};

/// What the window last painted, one line per quad, glyph, icon, image and underline.
///
/// Each line holds its bounds, clip and colour, and the lines are sorted: two frames that paint
/// the same things in the same places give the same lines whatever order they were drawn in.
#[must_use]
pub fn painted(window: &Window) -> Vec<String> {
    let mut lines: Vec<String> = window
        .painted_quads()
        .iter()
        .map(|q| {
            format!(
                "quad {:?} {:?} {:?} {:?} {:?} {:?} {:?}",
                q.bounds,
                q.content_mask,
                q.background,
                q.border_color,
                q.corner_radii,
                q.border_widths,
                q.border_style
            )
        })
        .chain(window.painted_sprites())
        .collect();
    lines.sort_unstable();
    lines
}

/// Where the frame the window shows differs from the same state drawn from scratch.
///
/// Gives the lines only one of them painted, at most `limit` of each, or `None` when they
/// agree. Draws a frame from scratch, which the window then shows. When they differ it draws
/// another: something that moves on the wall clock (a fade, a spinner) paints differently a
/// few milliseconds later however the frame was drawn, so two frames from scratch that differ
/// say the window is in motion, and a frame in motion is not judged.
pub fn stale(window: &mut Window, cx: &mut App, limit: usize) -> Option<String> {
    let shown = painted(window);
    let scratch = from_scratch(window, cx);
    if shown == scratch || from_scratch(window, cx) != scratch {
        return None;
    }
    diff(&shown, &scratch, limit)
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
    let only = |a: &[String], b: &[String]| -> Vec<String> {
        let mut rest = b.iter().peekable();
        let mut out = Vec::new();
        for line in a {
            while rest.next_if(|other| *other < line).is_some() {}
            if rest.next_if(|other| *other == line).is_none() {
                out.push(line.clone());
            }
        }
        out
    };
    let (drawn, missed) = (only(shown, scratch), only(scratch, shown));
    let show = |lines: &[String]| {
        let mut text: Vec<&str> = lines.iter().take(limit).map(String::as_str).collect();
        if lines.len() > limit {
            text.push("…");
        }
        text.join("\n  ")
    };
    Some(format!(
        "{} painted that a frame from scratch does not:\n  {}\n{} a frame from scratch paints that were not:\n  {}",
        drawn.len(),
        show(&drawn),
        missed.len(),
        show(&missed)
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

//! Motion keyed on a value changing, and figures that roll to their new value.
//!
//! [`on_change`] counts how often a value changed since its element first drew, so an
//! animation keyed on the count plays once per change and never on the first paint. A figure
//! that changes ([`Rolling`]) rolls each digit to its new place, as a counter does, so the eye
//! sees that it moved and by how much; a figure drawn for the first time, or under Reduce
//! Motion, simply stands.
//!
//! Ported from Ely GPUI Components (`src/motion/changes.rs`, `src/typography/rolling.rs`),
//! Copyright (c) 2026 Ely GPUI Component contributors, MIT OR Apache-2.0. Reworked onto
//! Slopty's motion tokens and [`super::motion`], with no `unwrap`, and the old figure taken
//! from what was drawn rather than parsed back.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    AnimationExt as _, AnyElement, App, ElementId, IntoElement, ParentElement as _, Pixels,
    RenderOnce, SharedString, Styled as _, Window, div,
};

use super::Pace;

/// A rolling figure's line as a share of its size: tight, as a counter's, so a digit's column
/// shows one digit and no edge of the next.
const FIGURE_LEADING: f32 = 1.25;

/// What a keyed element last drew, and how often that changed.
struct Changes<T> {
    last: T,
    count: u64,
}

/// How often `value` changed since the element `id` first drew it.
///
/// 0 on the first paint, one more on each change. Key an animation on it so it plays once per
/// change and is still when the element first appears; it never re-keys what is inside, so focus
/// and hover there hold.
pub fn on_change<T: Clone + PartialEq + 'static>(
    id: impl Into<ElementId>,
    value: &T,
    window: &mut Window,
    cx: &mut App,
) -> u64 {
    let state = window.use_keyed_state(id, cx, |_, _| Changes { last: value.clone(), count: 0 });
    if state.read(cx).last != *value {
        state.update(cx, |state, _| {
            state.last = value.clone();
            state.count = state.count.saturating_add(1);
        });
    }
    state.read(cx).count
}

/// A figure in words (`"3"`, `"99+"`, `"34%"`) and the one before it.
struct Rolled {
    from: String,
    to: String,
    count: u64,
}

/// A figure (`"3"`, `"99+"`, `"34%"`) that rolls to each new value from what it drew before.
///
/// Each digit slides along a column of 0–9 to its new place over [`Pace::Settle`], one line
/// tall, in tabular figures. Anything not a digit (a sign, a `%`, a `+`) stands. The first
/// paint, and every paint under Reduce Motion, draws the figure as it is.
///
/// The line is snapped to whole device pixels, since GPUI snaps the rows apart and a column a
/// fraction off drifts by a pixel as it rolls.
#[derive(IntoElement, Debug)]
pub struct Rolling {
    id: ElementId,
    text: String,
    line: Pixels,
}

impl Rolling {
    /// `text` under `id`, set at `size` (already zoomed): its line is `FIGURE_LEADING`
    /// times that, which the words beside it share by sitting in the same row.
    #[must_use]
    pub fn new(id: impl Into<ElementId>, text: impl Into<String>, size: Pixels) -> Self {
        Self { id: id.into(), text: text.into(), line: size * FIGURE_LEADING }
    }
}

impl RenderOnce for Rolling {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self { id, text, line } = self;
        let state = window.use_keyed_state(id, cx, |_, _| Rolled {
            from: text.clone(),
            to: text.clone(),
            count: 0,
        });
        if state.read(cx).to != text {
            state.update(cx, |rolled, _| {
                rolled.from = std::mem::replace(&mut rolled.to, text.clone());
                rolled.count = rolled.count.saturating_add(1);
            });
        }
        let (from, count) = {
            let rolled = state.read(cx);
            (rolled.from.clone(), rolled.count)
        };
        let row = super::tabular(div()).flex().flex_none().line_height(line);
        if count == 0 || !super::motion(cx) || from == text {
            return row.child(SharedString::from(text));
        }
        let scale = window.scale_factor();
        let line = Pixels::from((f32::from(line) * scale).round() / scale);
        let (before, after) = align(&from, &text);
        if !same_shape(&before, &after) {
            return row.child(SharedString::from(text));
        }
        let cells = before.into_iter().zip(after).enumerate().map(move |(ix, (old, new))| {
            let Some(end) = new.to_digit(10) else {
                return div().child(SharedString::from(new.to_string())).into_any_element();
            };
            let start = old.to_digit(10).unwrap_or(0);
            if start == end {
                return div().child(SharedString::from(new.to_string())).into_any_element();
            }
            let stack = div().flex().flex_col().children(
                (0..10_u32).map(|digit| div().h(line).child(SharedString::from(digit.to_string()))),
            );
            #[expect(clippy::cast_precision_loss, reason = "a digit's place, 0 to 9")]
            let (start, end) = (start as f32, end as f32);
            let key = ElementId::Name(SharedString::from(format!("roll-{count}-{ix}")));
            div()
                .h(line)
                .overflow_hidden()
                .child(stack.with_animation(key, Pace::Settle.animation(), move |stack, t| {
                    stack.mt(-(line * (end - start).mul_add(t, start)))
                }))
                .into_any_element()
        });
        row.h(line).overflow_hidden().children(cells)
    }
}

/// What a gliding value last drew, where it set out from, and how often it changed.
struct Glide {
    to: f32,
    from: f32,
    drawn: Rc<Cell<f32>>,
    count: u64,
}

/// A value drawn by `draw` that glides to each new value over [`Pace::Settle`].
///
/// A meter's fill or a ring's arc glides rather than jumping. A change while it glides sets out
/// from where it is drawn, so it turns back rather than jumping to its last end first. The first
/// paint, and every paint under Reduce Motion, draws the value as it is.
///
/// From Ely's `gliding` (`src/data_display/measures.rs`), which restarted from its last end.
pub struct Gliding {
    id: ElementId,
    value: f32,
    draw: Rc<dyn Fn(f32) -> AnyElement>,
    fill: bool,
}

impl std::fmt::Debug for Gliding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Gliding")
            .field("id", &self.id)
            .field("value", &self.value)
            .finish_non_exhaustive()
    }
}

impl Gliding {
    /// `value` under `id`, drawn by `draw` at each step of its glide.
    #[must_use]
    pub fn new(
        id: impl Into<ElementId>,
        value: f32,
        draw: impl Fn(f32) -> AnyElement + 'static,
    ) -> Self {
        Self { id: id.into(), value, draw: Rc::new(draw), fill: false }
    }

    /// Take the whole of the room it is given, for a drawing sized against it (a bar's fill).
    #[must_use]
    pub const fn fill(mut self) -> Self {
        self.fill = true;
        self
    }
}

impl IntoElement for Gliding {
    type Element = AnyElement;

    fn into_element(self) -> Self::Element {
        GlideHost(self).into_any_element()
    }
}

/// [`Gliding`] as an element: it reads its keyed state as it renders.
#[derive(IntoElement)]
struct GlideHost(Gliding);

impl RenderOnce for GlideHost {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Gliding { id, value, draw, fill } = self.0;
        let host = |el: gpui::Div| if fill { el.size_full() } else { el.flex_none() };
        let state = window.use_keyed_state(id, cx, |_, _| Glide {
            to: value,
            from: value,
            drawn: Rc::new(Cell::new(value)),
            count: 0,
        });
        if state.read(cx).to.to_bits() != value.to_bits() {
            state.update(cx, |glide, _| {
                glide.from = glide.drawn.get();
                glide.to = value;
                glide.count = glide.count.saturating_add(1);
            });
        }
        let (from, drawn, count) = {
            let glide = state.read(cx);
            (glide.from, Rc::clone(&glide.drawn), glide.count)
        };
        if count == 0 || !super::motion(cx) {
            drawn.set(value);
            return host(div()).child(draw(value));
        }
        let key = ElementId::Name(SharedString::from(format!("glide-{count}")));
        host(div()).child(host(div()).with_animation(
            key,
            Pace::Settle.animation(),
            move |el, t| {
                let now = (value - from).mul_add(t, from);
                drawn.set(now);
                el.child(draw(now))
            },
        ))
    }
}

/// The two figures padded on the left to one width, so their digits pair by place.
fn align(from: &str, to: &str) -> (Vec<char>, Vec<char>) {
    let width = from.chars().count().max(to.chars().count());
    let pad = |text: &str| {
        let mut chars = vec![' '; width.saturating_sub(text.chars().count())];
        chars.extend(text.chars());
        chars
    };
    (pad(from), pad(to))
}

/// Whether two aligned figures differ only in their digits (a pad may become a digit), so
/// rolling them reads as one figure moving; "2 running" to "1 finished" is new words instead.
fn same_shape(before: &[char], after: &[char]) -> bool {
    let digitish = |c: char| c == ' ' || c.is_ascii_digit();
    before.iter().zip(after).all(|(a, b)| a == b || (digitish(*a) && digitish(*b)))
}

#[cfg(test)]
mod tests {
    use super::{align, same_shape};

    /// Only a figure whose words stay the same rolls.
    #[test]
    fn only_the_digits_may_change() {
        let shape = |a: &str, b: &str| {
            let (a, b) = align(a, b);
            same_shape(&a, &b)
        };
        assert!(shape("2 running", "13 running"));
        assert!(!shape("99+", "12"), "a sign that comes or goes is a new figure");
        assert!(!shape("2 running", "1 finished"));
    }

    /// Two figures of different widths pair their digits by place, the shorter padded first.
    #[test]
    fn figures_pair_by_place() {
        let (from, to) = align("99", "100");
        assert_eq!(from.iter().collect::<String>(), " 99");
        assert_eq!(to.iter().collect::<String>(), "100");
        let (from, to) = align("34%", "8%");
        assert_eq!(from.iter().collect::<String>(), "34%");
        assert_eq!(to.iter().collect::<String>(), " 8%");
    }
}

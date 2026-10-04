//! A label that fades at its tail when it runs past its room, where one cut for an ellipsis
//! loses its last letters to three dots: a calmer row, and the start of every word kept.
//!
//! The row is laid out whole and kept from scrolling; its right edge fades through GPUI's
//! per-pixel `edge_fade` only as far as text lies past it (`hidden_by_scroll`), so a label that
//! fits is drawn sharp to its last letter. The whole text is what a screen reader hears, and a
//! label that is cut shows it whole in a hint. After Zeron's `sidebar_faded_label`
//! (`.research/gpui-references-2026-10-03.md`, T8).

use std::rc::Rc;

use gpui::{
    App, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use slopty_theme::Theme;

/// `text` under `id`, fading at its tail when it overflows; it grows to fill its row unless
/// [`FitLabel::fixed`].
#[must_use]
pub fn fit_label(
    id: impl Into<SharedString>,
    text: impl Into<SharedString>,
    theme: &Theme,
) -> FitLabel {
    FitLabel { id: id.into(), text: text.into(), theme: theme.clone(), grow: true }
}

/// A label that fades at its tail when it overflows ([`fit_label`]).
#[derive(IntoElement)]
pub struct FitLabel {
    id: SharedString,
    text: SharedString,
    theme: Theme,
    grow: bool,
}

impl std::fmt::Debug for FitLabel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FitLabel")
            .field("id", &self.id)
            .field("text", &self.text)
            .field("grow", &self.grow)
            .finish_non_exhaustive()
    }
}

impl FitLabel {
    /// Takes only the room its text needs, up to what its row leaves it.
    #[must_use]
    pub const fn fixed(mut self) -> Self {
        self.grow = false;
        self
    }
}

impl RenderOnce for FitLabel {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self { id, text, theme, grow } = self;
        let fade = gpui::Edges { right: px(theme.spacing.lg), ..gpui::Edges::default() };
        let scroll = window
            .use_keyed_state(
                ElementId::Name(SharedString::from(format!("{id}-fit"))),
                cx,
                |_, _| ScrollHandle::new(),
            )
            .read(cx)
            .clone();
        // What the last frame laid out: a label cut then is offered whole in a hint.
        let cut = scroll.max_offset().x > px(0.0);
        let selector = id.to_string();
        let words_selector = format!("{id}-words");
        let row = div()
            .id(ElementId::Name(id))
            .debug_selector(move || selector)
            .min_w_0()
            .flex()
            .overflow_hidden()
            .track_scroll(&scroll)
            .whitespace_nowrap()
            // Its own width, so the row scrolls by what runs past it.
            .child(div().debug_selector(move || words_selector).flex_none().child(text.clone()));
        let row = if grow { row.flex_1() } else { row };
        let row = if cut {
            let words = text;
            super::hint_timing(row).tooltip(move |_window, cx| {
                let theme = Rc::new(theme.clone());
                gpui::AppContext::new(cx, |_| super::Hint::new(words.clone(), "", theme)).into()
            })
        } else {
            row
        };
        gpui::edge_fade(row, gpui::EdgeFade::new(fade)).hidden_by_scroll(&scroll)
    }
}

#[cfg(test)]
mod tests {
    use gpui::{
        Context, IntoElement, ParentElement as _, Render, Styled as _, TestAppContext, Window, div,
        px,
    };
    use slopty_theme::Theme;

    use super::fit_label;

    struct Two;

    impl Render for Two {
        fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let theme = Theme::default();
            let row = |label| div().w(px(120.0)).flex().child(label);
            div().flex().flex_col().child(row(fit_label("short", "Fits", &theme))).child(row(
                fit_label(
                    "long",
                    "A title far too long for the hundred and twenty points it is given",
                    &theme,
                ),
            ))
        }
    }

    /// A label keeps its whole width inside its row, so the row's edge fade
    /// (`hidden_by_scroll`) has only what runs past the room to fade: a long one runs past its
    /// 120 points and a short one ends inside them. The test platform paints no glyphs, so the
    /// fade itself is GPUI's to prove; what it is given is proved here.
    #[gpui::test]
    fn only_a_label_past_its_room_runs_past_its_edge(cx: &mut TestAppContext) {
        let (_view, cx) = cx.add_window_view(|_, _| Two);
        cx.run_until_parked();
        let width = |cx: &mut gpui::VisualTestContext, id: &'static str| {
            cx.debug_bounds(id).unwrap_or_else(|| panic!("{id} drawn")).size.width
        };
        assert_eq!(width(cx, "long"), px(120.0), "the row keeps its room");
        assert!(width(cx, "long-words") > px(120.0), "the long one runs past it");
        assert!(width(cx, "short-words") < px(120.0), "the short one ends inside it");
    }
}

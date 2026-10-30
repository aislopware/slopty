// Derived from zeron (MIT, github.com/zeronsh/zeron @ 75b7260), see THIRD_PARTY_NOTICES.md
//! Shared metrics for sidebar surface toolbars and their controls.

use gpui::{Div, div, prelude::*, px};

use crate::theme::Theme;
use crate::motion::HoverFade as _;

pub(crate) const HEADER_HEIGHT: f32 = Theme::TITLEBAR_HEIGHT;
pub(crate) const CONTROL_SIZE: f32 = 24.0;
pub(crate) const CONTROL_RADIUS: f32 = Theme::CONTROL_RADIUS;
pub(crate) const ICON_SIZE: f32 = 14.0;
pub(crate) const CONTROL_GAP: f32 = Theme::SPACE_XS;
pub(crate) const EDGE_INSET: f32 = 8.0;

/// Shared field treatment for the file search and browser address controls.
pub(crate) fn input() -> Div {
    div()
        .h(px(CONTROL_SIZE))
        .min_w_0()
        .flex_1()
        .px(px(8.0))
        .rounded(px(CONTROL_RADIUS))
        .bg(crate::theme::ink(0.035))
        .flex()
        .items_center()
        .gap(px(Theme::SPACE_SNUG))
        // On the type ladder, and with the font-size setting (M28).
        .text_size(crate::typography::ui_rems(12.0))
}

// One icon button for every pane toolbar and strip (Claude desktop's ghost button, T3's
// `variant="ghost"`): five recipes had drawn the same 24 px control side by side.
/// The pane's icon button: 24 square, r 6, a 14 glyph in `text_muted`, its hover wash faded in
/// and its press a step deeper; Tab reaches it and the ring shows it. The caller names it
/// (`hint`/`action_hint`) and gives it a click.
pub(crate) fn icon_button(
    id: impl Into<gpui::SharedString>,
    glyph: &'static str,
    theme: &Theme,
) -> gpui::Stateful<Div> {
    icon_button_frame(id, false, theme)
        .child(crate::icons::icon(glyph).size(px(ICON_SIZE)).text_color(theme.text_muted))
}

/// [`icon_button`] for a toggle: while `on` it rests on the selected wash with its glyph in
/// `theme.text`, hover still answering a step deeper, and a screen reader hears its state.
pub(crate) fn icon_toggle(
    id: impl Into<gpui::SharedString>,
    glyph: &'static str,
    on: bool,
    theme: &Theme,
) -> gpui::Stateful<Div> {
    icon_button_frame(id, on, theme)
        .aria_toggled(if on { gpui::Toggled::True } else { gpui::Toggled::False })
        .child(
            crate::icons::icon(glyph)
                .size(px(ICON_SIZE))
                .text_color(if on { theme.text } else { theme.text_muted }),
        )
}

/// The frame [`icon_button`] and [`icon_toggle`] share, for a caller-drawn glyph: a keyboard
/// stop ([`key_stop`]), so a press leaves the keyboard where it was.
pub(crate) fn icon_button_frame(
    id: impl Into<gpui::SharedString>,
    on: bool,
    theme: &Theme,
) -> gpui::Stateful<Div> {
    let id: gpui::SharedString = id.into();
    let fade = format!("{id}-fade");
    let (rest, hover) = if on {
        (crate::theme::selected_wash(), crate::theme::selected_hover_wash())
    } else {
        (crate::theme::wash(0.0), crate::theme::control_hover())
    };
    div()
        .id(gpui::ElementId::Name(id))
        .size(px(CONTROL_SIZE))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(CONTROL_RADIUS))
        .cursor_pointer()
        .role(gpui::Role::Button)
        .bg(crate::motion::hover_blend(&fade, rest, hover))
        .hover_fade(fade)
        .active(|style| style.bg(crate::theme::pressed_wash()))
        .map(|el| key_stop(el, theme))
}

/// A view's header band: [`HEADER_HEIGHT`], and on a touch screen tall enough that its buttons'
/// finger presses ([`TOUCH_TARGET`]) fit between its two 1 pt hairlines.
pub(crate) fn toolbar(theme: &Theme) -> Div {
    div()
        .h(px(if crate::shell::touch_screen() { TOUCH_TARGET + 2.0 } else { HEADER_HEIGHT }))
        .w_full()
        .flex_none()
        .px(px(EDGE_INSET))
        .flex()
        .items_center()
        .gap(px(CONTROL_GAP))
        .border_t_1()
        .border_b_1()
        .border_color(theme.border)
        .bg(if theme.is_glass() {
            theme.surface.opacity(0.26)
        } else {
            theme.surface
        })
}

// One keyboard ring for every control (M8): 2 px of the accent drawn 2 px clear of the
// control, as Claude desktop's `outline: 2px solid; outline-offset: 2px`. It is paint only, so
// focus never moves a control or its neighbours, and a control at rest holds no edge for it.
/// The ring's thickness.
pub const RING_WIDTH: f32 = 2.0;
/// The gap between a control's edge and its ring.
pub const RING_OFFSET: f32 = 2.0;

/// The shell's keyboard ring in `theme`'s accent, clear of the control by `offset`.
#[must_use]
pub fn ring(theme: &Theme, offset: f32) -> gpui::Outline {
    gpui::Outline {
        color: theme.accent,
        width: px(RING_WIDTH),
        offset: px(offset),
    }
}

/// Gives `el` the shell's focus ring. The control also needs a `tab_index` to take focus.
pub fn focus_ring<E: InteractiveElement + Styled>(el: E, theme: &Theme) -> E {
    let ring = ring(theme, RING_OFFSET);
    el.focus_visible(move |style| style.outline_ring(ring))
}

/// [`focus_ring`] for a control shown only on hover: the keyboard shows it too.
pub fn focus_ring_revealing<E: InteractiveElement + Styled>(el: E, theme: &Theme) -> E {
    let ring = ring(theme, RING_OFFSET);
    el.focus_visible(move |style| style.outline_ring(ring).opacity(1.0))
}

/// [`focus_ring`] drawn just inside the edge, for a row that fills a clipped list (a file
/// tree's row, a list's entry), where a ring outside it would be cut off.
pub fn focus_ring_inset<E: InteractiveElement + Styled>(el: E, theme: &Theme) -> E {
    let ring = ring(theme, -RING_WIDTH);
    el.focus_visible(move |style| style.outline_ring(ring))
}

// A pane's clickable control is a keyboard stop (M8). A pointer press leaves the
// keyboard where it was, as a Mac's buttons do, so clicking a pane's button never takes the
// keyboard from the editor or the composer.
/// Makes the clickable `el` a keyboard stop: Tab reaches it, Enter or Space presses it, and
/// [`focus_ring`] shows it. The caller gives it a role and a name.
pub fn key_stop<E: InteractiveElement + Styled>(el: E, theme: &Theme) -> E {
    focus_ring(el.tab_index(0), theme).on_mouse_down(gpui::MouseButton::Left, |_, window, _| {
        window.prevent_default();
    })
}

/// [`key_stop`] with [`focus_ring_inset`], for a row that fills a clipped list.
pub fn key_stop_inset<E: InteractiveElement + Styled>(el: E, theme: &Theme) -> E {
    focus_ring_inset(el.tab_index(0), theme).on_mouse_down(
        gpui::MouseButton::Left,
        |_, window, _| window.prevent_default(),
    )
}

/// Neutral navigation chip shared by the right-panel strip and PR navigation.
pub(crate) fn tab_frame(
    id: impl Into<gpui::ElementId>,
    selected: bool,
    theme: &Theme,
) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .h(px(CONTROL_SIZE))
        .flex_none()
        .rounded(px(CONTROL_RADIUS))
        .flex()
        .items_center()
        .gap(px(CONTROL_GAP))
        .cursor_pointer()
        .role(gpui::Role::Button)
        .aria_selected(selected)
        .tab_index(0)
        .text_size(crate::typography::ui_rems(12.0))
        .text_color(if selected {
            theme.text
        } else {
            theme.text_muted
        })
        // A ring that never widens the chip (`focus_ring`).
        .map(|el| focus_ring(el, theme))
        .when(selected, |el| el.bg(crate::theme::selected_wash()))
}

pub(crate) fn tab(
    id: impl Into<gpui::ElementId>,
    selected: bool,
    theme: &Theme,
) -> gpui::Stateful<Div> {
    tab_frame(id, selected, theme)
        .when(!selected, |el| el.hover(|style| style.bg(crate::theme::hover_wash())))
        // Pressed reads a step deeper (M22).
        .active(|style| style.bg(crate::theme::pressed_wash()))
}

// A modal takes the keyboard as it opens and gives it back as it closes, so Escape and
// Tab are the dialog's and the page behind it never holds the keyboard under it.
/// The keyboard of one modal dialog: its card tracks [`Self::handle`], and its owner calls
/// [`Self::sync`] each render with whether the dialog shows.
pub(crate) struct ModalFocus {
    focus: gpui::FocusHandle,
    /// What had the keyboard when the dialog took it; `Some` while the dialog holds it.
    restore: Option<Option<gpui::FocusHandle>>,
}

impl ModalFocus {
    pub(crate) fn new(cx: &mut gpui::App) -> Self {
        Self { focus: cx.focus_handle(), restore: None }
    }

    /// The handle the dialog's card tracks.
    pub(crate) fn handle(&self) -> &gpui::FocusHandle {
        &self.focus
    }

    /// The dialog shows (`shown`) or not, this render: as it opens it takes the keyboard, as
    /// it closes it gives the keyboard back to what had it (or to nothing, when that is gone).
    pub(crate) fn sync(&mut self, shown: bool, window: &mut gpui::Window, cx: &mut gpui::App) {
        match (shown, self.restore.is_some()) {
            (true, false) => {
                self.restore = Some(window.focused(cx));
                window.focus(&self.focus, cx);
            }
            (false, true) => {
                if self.focus.contains_focused(window, cx) || window.focused(cx).is_none() {
                    match self.restore.take().flatten() {
                        Some(restore) => window.focus(&restore, cx),
                        None => window.blur(cx),
                    }
                }
                self.restore = None;
            }
            _ => {}
        }
    }
}

/// A modal's card as a screen reader and the keyboard take it: a Dialog named `title` that
/// tracks `focus` (its [`ModalFocus`]); Escape inside it runs `close`.
pub(crate) fn modal_card(
    card: Div,
    id: &'static str,
    focus: &gpui::FocusHandle,
    title: &'static str,
    close: impl Fn(&mut gpui::Window, &mut gpui::App) + 'static,
) -> gpui::Stateful<Div> {
    card.id(id)
        .track_focus(focus)
        .role(gpui::Role::Dialog)
        .aria_label(title)
        .on_key_down(move |event: &gpui::KeyDownEvent, window, cx| {
            if event.keystroke.key == "escape" {
                close(window, cx);
                cx.stop_propagation();
            }
        })
}

// A small control takes presses past what it draws (M25).
/// The least a pointer's press target measures, either way.
pub(crate) const POINTER_TARGET: f32 = 24.0;
/// The least a finger's press target measures, either way.
pub(crate) const TOUCH_TARGET: f32 = 44.0;

/// How far past each edge a control drawn `drawn` pt takes presses: nothing under a
/// pointer, and on a touch screen what brings it to [`TOUCH_TARGET`] (#23). The control's hit
/// area reaches that far, its negative margins giving it back, while what it draws stays.
pub(crate) fn touch_reach(drawn: f32) -> f32 {
    if crate::shell::touch_screen() { ((TOUCH_TARGET - drawn) / 2.0).max(0.0) } else { 0.0 }
}

/// A text row's height drawn `drawn` pt tall: on a touch screen at least
/// [`TOUCH_TARGET`], a finger's row, where reaching past it would take its neighbours' presses.
pub(crate) fn touch_row_height(drawn: f32) -> f32 {
    if crate::shell::touch_screen() { drawn.max(TOUCH_TARGET) } else { drawn }
}

/// [`focus_ring`] for a hit area reaching `reach` past the control it draws
/// ([`touch_reach`]): the ring stands around the control, not around the hit area.
pub(crate) fn focus_ring_within<E: InteractiveElement + Styled>(el: E, theme: &Theme, reach: f32) -> E {
    let ring = ring(theme, RING_OFFSET - reach);
    el.focus_visible(move |style| style.outline_ring(ring))
}

/// The press target around a small control drawn `drawn` pt square: [`POINTER_TARGET`] (or
/// [`TOUCH_TARGET`] on a touch screen) when it draws smaller, centred on it, its margins giving
/// the extra back so the layout is the drawn size. The caller puts the drawn control in it and
/// fades that one's hover with the target's group.
pub(crate) fn press_target(id: impl Into<gpui::ElementId>, drawn: f32) -> gpui::Stateful<Div> {
    let least = if crate::shell::touch_screen() { TOUCH_TARGET } else { POINTER_TARGET };
    let size = drawn.max(least);
    div()
        .id(id)
        .flex_none()
        .size(px(size))
        .m(px(-(size - drawn) / 2.0))
        .flex()
        .items_center()
        .justify_center()
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use gpui::{
        Background, Bounds, Context, Hsla, Modifiers, Pixels, Render, VisualTestContext, Window,
        point,
    };

    use super::*;

    /// Which ring a [`Stops`] window's first control wears.
    #[derive(Clone, Copy)]
    enum Wear {
        Ring,
        Inset,
    }

    /// Two 40 × 20 controls in a row, red and blue, the first a keyboard stop wearing `wear`;
    /// a third, green, holds the keyboard first for the pointer test.
    struct Stops {
        wear: Wear,
        presses: Rc<Cell<usize>>,
        holder: gpui::FocusHandle,
    }

    const RED: Hsla = Hsla { h: 0.0, s: 1.0, l: 0.5, a: 1.0 };
    const BLUE: Hsla = Hsla { h: 0.66, s: 1.0, l: 0.5, a: 1.0 };

    impl Render for Stops {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let theme = Theme::dark();
            let presses = self.presses.clone();
            let first = div()
                .id("first")
                .w(px(40.0))
                .h(px(20.0))
                .rounded(px(6.0))
                .bg(RED)
                .on_click(move |_, _, _| presses.set(presses.get() + 1));
            let first = match self.wear {
                Wear::Ring => key_stop(first, &theme),
                Wear::Inset => key_stop_inset(first, &theme),
            };
            div()
                .p(px(10.0))
                .flex()
                .gap(px(8.0))
                .child(first)
                .child(div().w(px(40.0)).h(px(20.0)).bg(BLUE))
                .child(
                    div()
                        .id("holder")
                        .track_focus(&self.holder)
                        .size(px(10.0))
                        .bg(gpui::green()),
                )
        }
    }

    /// A [`Stops`] window: its presses, the handle that holds the keyboard first, the window.
    fn stops(
        wear: Wear,
        cx: &mut gpui::TestAppContext,
    ) -> (Rc<Cell<usize>>, gpui::FocusHandle, &mut VisualTestContext) {
        cx.update(|cx| cx.set_global(Theme::dark()));
        let presses = Rc::new(Cell::new(0));
        let (view, cx) = cx.add_window_view({
            let presses = presses.clone();
            move |_, cx| Stops { wear, presses, holder: cx.focus_handle() }
        });
        let holder = cx.update(|_, cx| view.read(cx).holder.clone());
        (presses, holder, cx)
    }

    fn draw(cx: &mut VisualTestContext) -> (Vec<gpui::Quad>, f32) {
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
            (window.painted_quads(), window.scale_factor())
        })
    }

    /// The quad painted in `fill`, in points.
    fn filled(quads: &[gpui::Quad], fill: Hsla, scale: f32) -> Option<Bounds<Pixels>> {
        quads.iter().find(|quad| quad.background == Background::from(fill)).map(|quad| {
            let b = quad.bounds;
            Bounds::new(
                point(px(b.origin.x.0 / scale), px(b.origin.y.0 / scale)),
                gpui::size(px(b.size.width.0 / scale), px(b.size.height.0 / scale)),
            )
        })
    }

    /// The ring the frame painted in the accent, in points, with its thickness.
    fn ring(quads: &[gpui::Quad], scale: f32) -> Option<(Bounds<Pixels>, f32)> {
        let accent = Theme::dark().accent;
        quads.iter().find(|quad| quad.border_color == accent).map(|quad| {
            let b = quad.bounds;
            (
                Bounds::new(
                    point(px(b.origin.x.0 / scale), px(b.origin.y.0 / scale)),
                    gpui::size(px(b.size.width.0 / scale), px(b.size.height.0 / scale)),
                ),
                quad.border_widths.top.0 / scale,
            )
        })
    }

    /// Focus the first stop from the keyboard, as Tab does.
    fn tab_in(cx: &mut VisualTestContext) {
        cx.simulate_keystrokes("a");
        cx.update(|window, cx| window.focus_next(cx));
    }

    /// The keyboard ring is 2 px of the accent 2 px clear of the control, and drawing it moves
    /// neither the control nor its neighbour (M8).
    #[gpui::test]
    fn a_keyboard_stop_rings_clear_of_its_edge_and_moves_nothing(cx: &mut gpui::TestAppContext) {
        let (_, _, cx) = stops(Wear::Ring, cx);
        let (quads, scale) = draw(cx);
        assert!(ring(&quads, scale).is_none(), "no ring at rest");
        let (red, blue) = (filled(&quads, RED, scale).unwrap(), filled(&quads, BLUE, scale).unwrap());
        tab_in(cx);
        let (quads, scale) = draw(cx);
        let (ring, width) = ring(&quads, scale).expect("the keyboard rings the stop");
        assert_eq!(ring, red.dilate(px(RING_OFFSET + RING_WIDTH)));
        assert_eq!(width, RING_WIDTH);
        assert_eq!(filled(&quads, RED, scale).unwrap(), red, "the control holds its place");
        assert_eq!(filled(&quads, BLUE, scale).unwrap(), blue, "its neighbour holds its place");
    }

    /// Every focus style of the window's frame (the shell, its sidebar and Settings) is the
    /// ring, paint only: none adds a border, which would grow its control and shift its row.
    #[test]
    fn the_frames_focus_styles_are_the_ring() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = vec![src.join("shell.rs"), src.join("settings.rs")];
        for dir in ["shell", "settings"] {
            for entry in std::fs::read_dir(src.join(dir)).unwrap() {
                files.push(entry.unwrap().path());
            }
        }
        let mut bordered = Vec::new();
        for file in files.iter().filter(|f| f.extension().is_some_and(|e| e == "rs")) {
            let text = std::fs::read_to_string(file).unwrap();
            for (ix, line) in text.lines().enumerate() {
                if line.contains(".focus_visible(") && !line.contains("outline_ring") {
                    bordered.push(format!("{}:{}", file.display(), ix + 1));
                }
            }
        }
        assert!(bordered.is_empty(), "use focus_ring: {bordered:#?}");
    }

    /// A row's ring sits just inside its edge, where a list's clip cannot cut it.
    #[gpui::test]
    fn an_inset_ring_sits_inside_the_edge(cx: &mut gpui::TestAppContext) {
        let (_, _, cx) = stops(Wear::Inset, cx);
        let (quads, scale) = draw(cx);
        let red = filled(&quads, RED, scale).unwrap();
        tab_in(cx);
        let (quads, scale) = draw(cx);
        let (ring, width) = ring(&quads, scale).expect("the keyboard rings the row");
        assert_eq!(ring, red);
        assert_eq!(width, RING_WIDTH);
    }

    /// Enter and Space press a stop; a pointer press presses it too but leaves the keyboard
    /// where it was.
    #[gpui::test]
    fn a_stop_answers_its_keys_and_a_click_keeps_the_keyboard(cx: &mut gpui::TestAppContext) {
        let (presses, holder, cx) = stops(Wear::Ring, cx);
        draw(cx);
        tab_in(cx);
        for key in ["enter", "space"] {
            cx.simulate_keystrokes(key);
            cx.simulate_event(gpui::KeyUpEvent {
                keystroke: gpui::Keystroke::parse(key).unwrap(),
            });
        }
        assert_eq!(presses.get(), 2, "Enter and Space each press once");

        cx.update(|window, cx| window.focus(&holder, cx));
        let (quads, scale) = draw(cx);
        let red = filled(&quads, RED, scale).unwrap();
        cx.simulate_click(red.center(), Modifiers::none());
        assert_eq!(presses.get(), 3, "the pointer presses it");
        assert!(
            cx.update(|window, _| holder.is_focused(window)),
            "the keyboard stays where it was"
        );
    }

    /// A 20 pt control in a [`press_target`] takes a press 2 pt past its edge and keeps its
    /// place in the row.
    #[gpui::test]
    fn a_press_target_reaches_past_a_small_control(cx: &mut gpui::TestAppContext) {
        struct Row(Rc<Cell<usize>>);
        impl Render for Row {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                let presses = self.0.clone();
                div().p(px(10.0)).flex().child(div().w(px(30.0)).h(px(20.0))).child(
                    press_target("target", 20.0)
                        .debug_selector(|| "target".to_owned())
                        .on_click(move |_, _, _| presses.set(presses.get() + 1))
                        .child(div().debug_selector(|| "drawn".to_owned()).size(px(20.0))),
                )
            }
        }
        cx.update(|cx| cx.set_global(Theme::dark()));
        let presses = Rc::new(Cell::new(0));
        let (_, cx) = cx.add_window_view({
            let presses = presses.clone();
            move |_, _| Row(presses)
        });
        draw(cx);
        let drawn = cx.debug_bounds("drawn").unwrap();
        let target = cx.debug_bounds("target").unwrap();
        assert_eq!(f32::from(target.size.width), POINTER_TARGET);
        assert_eq!(target.center(), drawn.center());
        assert_eq!(f32::from(drawn.left()), 40.0, "the row lays it out at its drawn size");
        cx.simulate_click(point(drawn.right() + px(1.5), drawn.top() - px(1.5)), Modifiers::none());
        assert_eq!(presses.get(), 1);
    }
}

// Derived from zeron (MIT, github.com/zeronsh/zeron @ 75b7260), see THIRD_PARTY_NOTICES.md
//! Shared metrics for sidebar surface toolbars and their controls.

use gpui::{Div, div, prelude::*, px};

use crate::theme::Theme;

pub(crate) const HEADER_HEIGHT: f32 = Theme::TITLEBAR_HEIGHT;
pub(crate) const CONTROL_SIZE: f32 = 24.0;
pub(crate) const CONTROL_RADIUS: f32 = 6.0;
pub(crate) const ICON_SIZE: f32 = 14.0;
pub(crate) const CONTROL_GAP: f32 = 4.0;
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
        .gap(px(6.0))
        .text_size(px(11.5))
}

pub(crate) fn toolbar(theme: &Theme) -> Div {
    div()
        .h(px(HEADER_HEIGHT))
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

// slopty: one keyboard ring for every control (M8): 2 px of the accent drawn 2 px clear of the
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

// slopty: a pane's clickable control is a keyboard stop (M8). A pointer press leaves the
// keyboard where it was, as a Mac's buttons do, so clicking a pane's button never takes the
// keyboard from the editor or the composer.
/// Makes the clickable `el` a keyboard stop: Tab reaches it, Enter or Space presses it, and
/// [`focus_ring`] shows it. The caller gives it a role and a name.
pub fn key_stop<E: InteractiveElement + Styled>(el: E, theme: &Theme) -> E {
    focus_ring(el.tab_index(0), theme).on_mouse_down(gpui::MouseButton::Left, |_, window, _| {
        window.prevent_default();
    })
}

/// [`key_stop`] for a control shown only on hover: the keyboard shows it too.
pub fn key_stop_revealing<E: InteractiveElement + Styled>(el: E, theme: &Theme) -> E {
    focus_ring_revealing(el.tab_index(0), theme).on_mouse_down(
        gpui::MouseButton::Left,
        |_, window, _| window.prevent_default(),
    )
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
        .text_size(px(12.0))
        .text_color(if selected {
            theme.text
        } else {
            theme.text_muted
        })
        // slopty: a ring that never widens the chip (`focus_ring`).
        .map(|el| focus_ring(el, theme))
        .when(selected, |el| el.bg(crate::theme::wash(0.10)))
}

pub(crate) fn tab(
    id: impl Into<gpui::ElementId>,
    selected: bool,
    theme: &Theme,
) -> gpui::Stateful<Div> {
    tab_frame(id, selected, theme)
        .when(!selected, |el| {
            el.hover(|style| style.bg(crate::theme::wash(0.06)))
        })
        // slopty: pressed reads a step deeper (M22).
        .active(|style| style.bg(crate::theme::wash(0.14)))
}

/// Fixed pane tab width. Drag reordering (drop-index quantisation and slide
/// offsets) assumes uniform chips.
pub(crate) const TAB_CHIP_WIDTH: f32 = 112.0;
pub(crate) const TAB_CHIP_HEIGHT: f32 = 24.0;
/// A chip plus the strip's own gap.
pub(crate) const TAB_CHIP_SLOT: f32 = TAB_CHIP_WIDTH + CONTROL_GAP;
/// Width of the fade over tabs scrolled out of a strip.
pub(crate) const TAB_STRIP_FADE: f32 = 36.0;

/// One pane tab chip, as drawn in the titlebar band above a side pane.
pub(crate) struct TabChip {
    /// Element id prefix; `{id}-{ix}` is also the chip's debug selector.
    pub id: &'static str,
    /// Element id prefix of the close slot, debug selector `{close_id}-{ix}`.
    pub close_id: &'static str,
    pub ix: usize,
    pub active: bool,
    pub title: gpui::SharedString,
    /// Shows the unsaved dot in the close slot while the tab is not hovered.
    pub dirty: bool,
    /// The leading 18px slot's content, usually [`tab_chip_icon`].
    pub leading: gpui::AnyElement,
}

/// A pane tab: the leading icon, the truncated title, and a trailing slot
/// whose ✕ fades in on hover over the unsaved dot (same slot, no width jump).
/// Callers add activation, menus, tooltips and drag behaviour.
pub(crate) fn tab_chip(
    chip: TabChip,
    theme: &Theme,
    on_close: impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> gpui::Stateful<Div> {
    let TabChip {
        id,
        close_id,
        ix,
        active,
        title,
        dirty,
        leading,
    } = chip;
    let selector: gpui::SharedString = format!("{id}-{ix}").into();
    let close_fade = format!("{close_id}-{ix}-fade");
    // slopty: the ✕ fades in with the tab's hover, the unsaved dot out (M23).
    let tab_fade = format!("{id}-{ix}-reveal");
    let shown = crate::motion::hover_t(&tab_fade);
    tab((id, ix), active, theme)
        .debug_selector(move || selector.to_string())
        .on_hover(crate::motion::hover_listener(tab_fade))
        .h(px(TAB_CHIP_HEIGHT))
        .w(px(TAB_CHIP_WIDTH))
        .flex_none()
        // slopty: 4 from the edge; the ring is drawn inside it, as the strip clips.
        .px(px(4.0))
        .map(|el| focus_ring_inset(el, theme))
        .rounded(px(CONTROL_RADIUS))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(3.0))
        .cursor_pointer()
        .role(gpui::Role::Button)
        // The old session-tab strip's solved carve-out: NOT `.occlude()` — a
        // BlockMouse hitbox ends the hit test, so the scroll container behind
        // the tabs never saw wheel events and an overflowing strip could not
        // be scrolled (tabs tile the whole region). ExceptScroll keeps the
        // titlebar drag-region carve-out and lets the strip scroll.
        .block_mouse_except_scroll()
        .on_mouse_down(gpui::MouseButton::Left, |_, window, _| {
            window.prevent_default()
        })
        .child(
            div()
                .flex_none()
                .size(px(18.0))
                .flex()
                .items_center()
                .justify_center()
                .child(leading),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(crate::typography::ui_rems(11.5))
                .text_color(if active { theme.text } else { theme.text_muted })
                .child(title),
        )
        .child(
            // Trailing slot: the unsaved dot normally, ✕ on tab hover — two
            // stacked layers crossfaded by the tab's hover.
            div()
                .id((close_id, ix))
                .debug_selector(move || format!("{close_id}-{ix}"))
                .flex_none()
                .size(px(18.0))
                // slopty: r 6 and a faded wash, as the strip's other controls; the tab in front
                // names ⌘W, which closes it.
                .rounded(px(CONTROL_RADIUS))
                .relative()
                .role(gpui::Role::Button)
                .aria_label("Close tab")
                .tooltip(if active {
                    Box::new(crate::settings::widgets::action_tooltip(
                        "Close tab",
                        crate::settings::widgets::Keys::Action(Box::new(crate::app_menus::CloseTab)),
                    )) as Box<dyn Fn(&mut gpui::Window, &mut gpui::App) -> gpui::AnyView>
                } else {
                    Box::new(crate::settings::widgets::text_tooltip("Close tab"))
                })
                .tooltip_show_delay(crate::settings::widgets::TOOLTIP_DELAY)
                .bg(crate::motion::hover_blend(
                    &close_fade,
                    crate::theme::wash(0.0),
                    crate::theme::wash(0.11),
                ))
                .on_hover(crate::motion::hover_listener(close_fade))
                // The tab owns a drag payload. Claim the close press before
                // it reaches that parent or GPUI starts a tab drag instead of
                // delivering the close click.
                .on_mouse_down(gpui::MouseButton::Left, |_, window, cx| {
                    window.prevent_default();
                    cx.stop_propagation();
                })
                .on_click(move |event, window, cx| {
                    cx.stop_propagation();
                    on_close(event, window, cx);
                })
                .when(dirty, |slot| {
                    slot.child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .opacity(1.0 - shown)
                            .child(div().size(px(6.0)).rounded_full().bg(theme.text_muted)),
                    )
                })
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .opacity(shown)
                        .child(
                            crate::icons::icon(crate::icons::CLOSE)
                                .size(px(12.0))
                                .text_color(theme.text_muted),
                        ),
                ),
        )
}

/// A chip's leading glyph, quieter on inactive tabs.
pub(crate) fn tab_chip_icon(path: &'static str, active: bool, theme: &Theme) -> gpui::AnyElement {
    crate::icons::icon(path)
        .size(px(12.0))
        .text_color(if active {
            theme.text_muted
        } else {
            theme.text_muted.opacity(0.7)
        })
        .into_any_element()
}

/// Wrap a horizontally scrolling tab strip in its region, fading whichever
/// side hides tabs. Glass: a per-glyph EdgeFade scope over the chips' own
/// opacity ramps; opaque: painted gradients in the shell surface tone.
pub(crate) fn tab_strip_region(
    strip: impl IntoElement,
    scroll: &gpui::ScrollHandle,
    theme: &Theme,
) -> gpui::AnyElement {
    // Fade flags from the LAST frame's scroll state (invisible lag).
    let scrolled = -f32::from(scroll.offset().x);
    let max_scroll = f32::from(scroll.max_offset().x);
    let fade_left = scrolled > 1.0;
    let fade_right = scrolled < max_scroll - 1.0;
    let glass = theme.is_glass();
    let bar_bg = theme.surface;
    let edge = |angle: f32| {
        div()
            .absolute()
            .top_0()
            .bottom_0()
            .w(px(TAB_STRIP_FADE))
            .bg(gpui::linear_gradient(
                angle,
                gpui::linear_color_stop(bar_bg, 0.0),
                gpui::linear_color_stop(bar_bg.opacity(0.0), 1.0),
            ))
    };
    let region = div()
        .relative()
        .min_w_0()
        .size_full()
        .flex()
        .items_center()
        .child(strip)
        .when(fade_left && !glass, |el| el.child(edge(90.0).left_0()))
        .when(fade_right && !glass, |el| el.child(edge(270.0).right_0()));
    if glass {
        crate::edge_fade::edge_faded(TAB_STRIP_FADE, false, false, region)
            .fade_left(fade_left)
            .fade_right(fade_right)
            .into_any_element()
    } else {
        region.into_any_element()
    }
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
        Revealing,
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
                Wear::Revealing => key_stop_revealing(first.opacity(0.0), &theme),
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

    /// A control shown only on hover shows, ringed, while it has the keyboard.
    #[gpui::test]
    fn a_revealed_control_shows_while_it_has_the_keyboard(cx: &mut gpui::TestAppContext) {
        let (_, _, cx) = stops(Wear::Revealing, cx);
        let (quads, scale) = draw(cx);
        assert!(filled(&quads, RED, scale).is_none(), "hidden at rest");
        tab_in(cx);
        let (quads, scale) = draw(cx);
        assert!(filled(&quads, RED, scale).is_some(), "shown with the keyboard");
        assert!(ring(&quads, scale).is_some(), "and ringed");
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

    struct Chip {
        dirty: bool,
    }

    impl Render for Chip {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let theme = Theme::dark();
            div().p(px(10.0)).child(tab_chip(
                TabChip {
                    id: "chip",
                    close_id: "chip-close",
                    ix: 0,
                    active: false,
                    title: "notes.md".into(),
                    dirty: self.dirty,
                    leading: div().into_any_element(),
                },
                &theme,
                |_, _, _| {},
            ))
        }
    }

    /// A tab's ✕ fades in with its hover (M23): the unsaved dot it stands over fades out with
    /// the same hover, so with motion reduced the dot is gone once the pointer is over the tab.
    #[gpui::test]
    fn a_tabs_close_fades_with_its_hover(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            cx.set_global(Theme::dark());
            cx.set_reduce_motion(true);
        });
        let (_, cx) = cx.add_window_view(|_, _| Chip { dirty: true });
        let dot = Theme::dark().text_muted;
        let (quads, scale) = draw(cx);
        let shown = filled(&quads, dot, scale).expect("the unsaved dot at rest");
        cx.simulate_mouse_move(
            point(shown.center().x - px(40.0), shown.center().y),
            None,
            Modifiers::none(),
        );
        let (quads, scale) = draw(cx);
        assert!(filled(&quads, dot, scale).is_none(), "the hover hands the slot to the ✕");
    }
}

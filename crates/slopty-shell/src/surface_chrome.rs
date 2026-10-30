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
        // slopty: on the type ladder, and with the font-size setting (M28).
        .text_size(crate::typography::ui_rems(12.0))
}

// slopty: one icon button for every pane toolbar and strip (Claude desktop's ghost button, T3's
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

/// [`icon_button`] for a menu's trigger: while its menu is `open` it holds the selected wash with
/// its glyph in full ink and takes no hover over it (opencode's `[data-expanded]`), so the
/// trigger reads as the menu's own. The caller sets `aria_expanded`.
pub(crate) fn icon_menu_button(
    id: impl Into<gpui::SharedString>,
    glyph: &'static str,
    open: bool,
    theme: &Theme,
) -> gpui::Stateful<Div> {
    icon_button_frame(id, open, theme)
        .when(open, |el| el.bg(crate::theme::selected_wash()))
        .child(
            crate::icons::icon(glyph)
                .size(px(ICON_SIZE))
                .text_color(if open { theme.text } else { theme.text_muted }),
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
        .text_size(crate::typography::ui_rems(12.0))
        .text_color(if selected {
            theme.text
        } else {
            theme.text_muted
        })
        // slopty: a ring that never widens the chip (`focus_ring`).
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
        // slopty: pressed reads a step deeper (M22).
        .active(|style| style.bg(crate::theme::pressed_wash()))
}

// slopty: a pane's tabs fit their titles (T3's content-sized chips), so "Working tree" reads
// whole beside an empty strip. One width per strip, the widest title's, keeps the drag maths
// (drop-index quantisation, slide offsets) uniform.
/// The narrowest a pane tab draws.
pub(crate) const TAB_CHIP_MIN_WIDTH: f32 = 96.0;
/// The widest a pane tab draws; a longer title is cut and shown whole in its tooltip.
pub(crate) const TAB_CHIP_MAX_WIDTH: f32 = 168.0;
pub(crate) const TAB_CHIP_HEIGHT: f32 = 24.0;
/// A tab title's size, at the 16 px baseline.
pub(crate) const TAB_TITLE_SIZE: f32 = crate::typography::ladder::LABEL;
/// What a chip spends besides its title: its two 4 px pads, the 18 px glyph and close slots,
/// the two gaps between them, and 4 px so a measured title never meets its fade.
const TAB_CHIP_FRAME: f32 = 2.0 * 4.0 + 2.0 * 18.0 + 2.0 * CONTROL_GAP + 4.0;
/// Width of the fade over tabs scrolled out of a strip.
pub(crate) const TAB_STRIP_FADE: f32 = 36.0;

/// One strip's tab width, and for each title whether that width cuts it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TabWidths {
    /// Every chip's width in the strip.
    pub chip: f32,
    /// Per title, in order: whether the chip cuts it.
    pub cut: Vec<bool>,
}

impl TabWidths {
    /// A chip plus the strip's own gap: one drag slot.
    pub(crate) fn slot(&self) -> f32 {
        self.chip + CONTROL_GAP
    }
}

/// Fits a strip's chips to its widest title (in `cx`'s interface face and size), between
/// [`TAB_CHIP_MIN_WIDTH`] and [`TAB_CHIP_MAX_WIDTH`].
pub(crate) fn tab_widths<'a>(
    titles: impl IntoIterator<Item = &'a str>,
    cx: &gpui::App,
) -> TabWidths {
    let size = px(crate::typography::font_size(cx).pixels() * TAB_TITLE_SIZE / 16.0);
    let text = cx.text_system();
    let font = text.resolve_font(&gpui::font(crate::typography::effective_family_name(cx)));
    let widths: Vec<f32> = titles
        .into_iter()
        .map(|title| {
            title
                .chars()
                .map(|ch| {
                    text.advance(font, size, ch)
                        .map_or(f32::from(size) * 0.6, |advance| f32::from(advance.width))
                })
                .sum()
        })
        .collect();
    let widest = widths.iter().copied().fold(0.0_f32, f32::max);
    let chip = (widest + TAB_CHIP_FRAME).clamp(TAB_CHIP_MIN_WIDTH, TAB_CHIP_MAX_WIDTH).ceil();
    let room = chip - TAB_CHIP_FRAME;
    TabWidths { chip, cut: widths.into_iter().map(|width| width > room).collect() }
}

// slopty: a strip keeps its chips' width after a close by pointer, so the next ✕ stays under the
// pointer (opencode's TUI and every browser): until the pointer leaves the strip or five seconds
// pass, then the chips ease to their new width over `TAB_SLIDE`.
/// How long a strip holds its width after a close with the pointer still over it.
pub(crate) const TAB_HOLD: std::time::Duration = std::time::Duration::from_secs(5);

/// The chip width a strip holds after a close by pointer, and its ease back once let go.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct HeldTabWidth(Option<Hold>);

#[derive(Debug, Clone, Copy)]
enum Hold {
    Holding { width: f32, at: std::time::Instant },
    Easing { from: f32, at: std::time::Instant },
}

impl HeldTabWidth {
    /// A tab was closed by pointer while the chips stood `width` wide.
    pub(crate) fn hold(&mut self, width: f32, now: std::time::Instant) {
        self.0 = Some(Hold::Holding { width, at: now });
    }

    /// The pointer left the strip: the held width eases to the natural one. Whether it held.
    pub(crate) fn release(&mut self, now: std::time::Instant) -> bool {
        match self.0 {
            Some(Hold::Holding { width, .. }) => {
                self.0 = Some(Hold::Easing { from: width, at: now });
                true
            }
            _ => false,
        }
    }

    /// The width to draw the chips at instead of `natural`, and whether it is still moving (the
    /// caller asks for frames while it is). Reduced motion lands at once.
    pub(crate) fn width(&mut self, natural: f32, now: std::time::Instant, reduced: bool) -> (f32, bool) {
        if let Some(Hold::Holding { width, at }) = self.0 {
            if now.saturating_duration_since(at) < TAB_HOLD {
                return (width, false);
            }
            self.0 = Some(Hold::Easing { from: width, at: at + TAB_HOLD });
        }
        let Some(Hold::Easing { from, at }) = self.0 else {
            return (natural, false);
        };
        let total = crate::motion::TAB_SLIDE.total().as_secs_f32();
        let raw = now.saturating_duration_since(at).as_secs_f32() / total;
        if reduced || raw >= 1.0 {
            self.0 = None;
            return (natural, false);
        }
        let eased = crate::motion::TAB_SLIDE.progress(raw);
        (crate::motion::lerp(from, natural, eased), true)
    }
}

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
    /// The strip's chip width ([`tab_widths`]).
    pub width: f32,
    /// The close button's tooltip ("Close tab", with ⌘W on the tab in front).
    pub close_hint: &'static str,
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
        width,
        close_hint,
    } = chip;
    // slopty: each ✕ is named for its tab, so a screen reader tells them apart (T3's
    // `Close ${title}`); its tooltip keeps the action's words and key.
    let close_name: gpui::SharedString = format!("Close {title}").into();
    let selector: gpui::SharedString = format!("{id}-{ix}").into();
    let close_fade = format!("{close_id}-{ix}-fade");
    // slopty: the ✕ fades in with the tab's hover, the unsaved dot out (M23).
    let tab_fade = format!("{id}-{ix}-reveal");
    let shown = crate::motion::hover_t(&tab_fade);
    // slopty: on a touch screen the ✕ is there at rest (opencode's `(hover: hover)` gate); an
    // inactive tab with unsaved edits keeps its dot there instead.
    let revealed = if dirty && !active { shown } else { crate::motion::revealed_by_hover(&tab_fade) };
    // slopty: the hover wash fades with the same hover as the ✕ (M23 remainder).
    let rest = if active { crate::theme::selected_wash() } else { crate::theme::wash(0.0) };
    let hover = if active { rest } else { crate::theme::hover_wash() };
    tab_frame((id, ix), active, theme)
        .bg(crate::motion::mix(rest, hover, shown))
        .active(|style| style.bg(crate::theme::pressed_wash()))
        .debug_selector(move || selector.to_string())
        .hover_fade(tab_fade)
        .h(px(TAB_CHIP_HEIGHT))
        .w(px(width))
        .flex_none()
        // slopty: 4 from the edge; the ring is drawn inside it, as the strip clips.
        .px(px(4.0))
        .map(|el| focus_ring_inset(el, theme))
        .rounded(px(CONTROL_RADIUS))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(CONTROL_GAP))
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
                .text_size(crate::typography::ui_rems(TAB_TITLE_SIZE))
                .text_color(if active { theme.text } else { theme.text_muted })
                .child(title),
        )
        .child({
            // slopty: the press reaches past the 18 pt slot it draws, to 24 pt under a pointer
            // and 44 tall on a touch screen (M25); its margins give the extra back, so the
            // tab's layout is the slot's.
            let (hit_w, hit_h) = close_hit_size(crate::shell::touch_screen());
            let close_label = format!("{close_id}-{ix}");
            div()
                .id((close_id, ix))
                .debug_selector(move || format!("{close_label}-hit"))
                .flex_none()
                .w(px(hit_w))
                .h(px(hit_h))
                .mx(px(-(hit_w - CLOSE_SLOT) / 2.0))
                .my(px(-(hit_h - CLOSE_SLOT) / 2.0))
                .flex()
                .items_center()
                .justify_center()
                .role(gpui::Role::Button)
                .aria_label(close_name)
                .tooltip(if active {
                    Box::new(crate::settings::widgets::action_tooltip(
                        close_hint,
                        crate::settings::widgets::Keys::Action(Box::new(crate::app_menus::CloseTab)),
                    )) as Box<dyn Fn(&mut gpui::Window, &mut gpui::App) -> gpui::AnyView>
                } else {
                    Box::new(crate::settings::widgets::text_tooltip(close_hint))
                })
                .tooltip_show_delay(crate::settings::widgets::TOOLTIP_DELAY)
                .hover_fade(close_fade.clone())
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
                .child(
                    // Trailing slot: the unsaved dot normally, ✕ on tab hover — two
                    // stacked layers crossfaded by the tab's hover.
                    div()
                        .debug_selector(move || format!("{close_id}-{ix}"))
                        .flex_none()
                        .size(px(CLOSE_SLOT))
                        // slopty: r 6 and a faded wash, as the strip's other controls; the tab in
                        // front names ⌘W, which closes it.
                        .rounded(px(CONTROL_RADIUS))
                        .relative()
                        .bg(crate::motion::hover_blend(
                            &close_fade,
                            crate::theme::wash(0.0),
                            crate::theme::control_hover(),
                        ))
                        .when(dirty, |slot| {
                            slot.child(
                                div()
                                    .absolute()
                                    .inset_0()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .opacity(1.0 - revealed)
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
                                .opacity(revealed)
                                .child(
                                    crate::icons::icon(crate::icons::CLOSE)
                                        .size(px(12.0))
                                        .text_color(theme.text_muted),
                                ),
                        ),
                )
        })
}

// slopty: a modal takes the keyboard as it opens and gives it back as it closes, so Escape and
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

// slopty: a small control takes presses past what it draws (M25).
/// The least a pointer's press target measures, either way.
pub(crate) const POINTER_TARGET: f32 = 24.0;
/// The least a finger's press target measures, either way.
pub(crate) const TOUCH_TARGET: f32 = 44.0;

/// slopty: how far past each edge a control drawn `drawn` pt takes presses: nothing under a
/// pointer, and on a touch screen what brings it to [`TOUCH_TARGET`] (#23). The control's hit
/// area reaches that far, its negative margins giving it back, while what it draws stays.
pub(crate) fn touch_reach(drawn: f32) -> f32 {
    if crate::shell::touch_screen() { ((TOUCH_TARGET - drawn) / 2.0).max(0.0) } else { 0.0 }
}

/// slopty: a text row's height drawn `drawn` pt tall: on a touch screen at least
/// [`TOUCH_TARGET`], a finger's row, where reaching past it would take its neighbours' presses.
pub(crate) fn touch_row_height(drawn: f32) -> f32 {
    if crate::shell::touch_screen() { drawn.max(TOUCH_TARGET) } else { drawn }
}

/// slopty: [`focus_ring`] for a hit area reaching `reach` past the control it draws
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

/// The slot a tab's ✕ draws in.
const CLOSE_SLOT: f32 = 18.0;

/// How far a tab's ✕ takes a press, width and height: 24 pt under a pointer; on a touch screen
/// 44 tall, the finger's size, and only as wide as leaves the title and the next tab their own.
const fn close_hit_size(touch: bool) -> (f32, f32) {
    if touch { (30.0, 44.0) } else { (24.0, 24.0) }
}

/// A dragged tab at the pointer: the strip's chip width, its title on the raised surface.
pub(crate) fn tab_ghost(title: gpui::SharedString, width: f32, theme: &Theme) -> Div {
    div()
        .h(px(TAB_CHIP_HEIGHT))
        .w(px(width))
        .px(px(8.0))
        .flex()
        .items_center()
        .rounded(px(CONTROL_RADIUS))
        .bg(theme.surface_raised)
        .border_1()
        .border_color(theme.border_strong)
        .text_size(crate::typography::ui_rems(TAB_TITLE_SIZE))
        .text_color(theme.text)
        .opacity(0.85)
        .child(div().truncate().child(title))
}

/// A chip's leading glyph, quieter on inactive tabs.
pub(crate) fn tab_chip_icon(path: &'static str, active: bool, theme: &Theme) -> gpui::AnyElement {
    // slopty: a tab's leading glyph is 14, as the file icons beside it (M48, item 21).
    crate::icons::icon(path)
        .size(px(ICON_SIZE))
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
    let (fade_left, fade_right) = strip_fades(scroll);
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
        // slopty: the lag above lasts one frame, never the window's rest.
        .child(strip_fades_follow(scroll.clone(), (fade_left, fade_right)))
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

/// Which sides of the strip hide tabs, by its scroll state: left, right.
fn strip_fades(scroll: &gpui::ScrollHandle) -> (bool, bool) {
    let scrolled = -f32::from(scroll.offset().x);
    let max_scroll = f32::from(scroll.max_offset().x);
    (scrolled > 1.0, scrolled < max_scroll - 1.0)
}

/// slopty: nothing drawn. Laid out after the strip, it reads the strip's scroll as this frame
/// left it (a tab opened, closed or scrolled to), and when that calls for other fades than the
/// ones `drawn`, it asks for the next frame, which draws them: a window at rest never keeps a
/// fade over a tab that no longer hides.
fn strip_fades_follow(scroll: gpui::ScrollHandle, drawn: (bool, bool)) -> impl IntoElement {
    gpui::canvas(
        move |_, window, _| {
            if strip_fades(&scroll) != drawn {
                window.request_animation_frame();
            }
        },
        |_, (), _, _| {},
    )
    .absolute()
    .size_0()
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
        closes: Rc<Cell<usize>>,
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
                    width: TAB_CHIP_MIN_WIDTH,
                    close_hint: "Close tab",
                },
                &theme,
                {
                    let closes = self.closes.clone();
                    move |_, _, _| closes.set(closes.get() + 1)
                },
            ))
        }
    }

    /// A tab's ✕ fades in with its hover (M23): the unsaved dot it stands over fades out with
    /// the same hover, so with motion reduced the dot is gone once the pointer is over the tab.
    /// After a close by pointer the strip holds its width while the pointer stays, eases to the
    /// new width once it leaves, and lets go by itself after five seconds.
    #[test]
    fn a_strip_holds_its_width_after_a_close_until_the_pointer_leaves() {
        let t0 = std::time::Instant::now();
        let ms = |m: u64| t0 + std::time::Duration::from_millis(m);
        let mut hold = HeldTabWidth::default();
        assert_eq!(hold.width(120.0, t0, false), (120.0, false), "nothing held");
        hold.hold(150.0, t0);
        assert_eq!(hold.width(120.0, ms(1000), false), (150.0, false), "held under the pointer");
        assert!(hold.release(ms(1000)));
        let (mid, moving) = hold.width(120.0, ms(1075), false);
        assert!(moving && mid < 150.0 && mid > 120.0, "easing: {mid}");
        assert_eq!(hold.width(120.0, ms(1200), false), (120.0, false), "settled");
        assert!(!hold.release(ms(1300)), "nothing left to let go");

        hold.hold(150.0, t0);
        assert_eq!(hold.width(120.0, ms(4999), false).0, 150.0);
        assert!(hold.width(120.0, ms(5050), false).1, "five seconds on, it eases by itself");
        hold.hold(150.0, t0);
        hold.release(ms(10));
        assert_eq!(hold.width(120.0, ms(11), true), (120.0, false), "reduced motion lands at once");
    }

    /// A strip of `tabs` 60 pt tabs in 200 pt.
    struct Strip {
        tabs: usize,
        scroll: gpui::ScrollHandle,
    }

    impl Render for Strip {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let tabs = (0..self.tabs).map(|_| div().flex_none().w(px(60.0)).h(px(20.0)));
            let strip = div()
                .id("strip")
                .flex()
                .min_w_0()
                .overflow_x_scroll()
                .track_scroll(&self.scroll)
                .children(tabs);
            // Opaque: its fades are gradients, where glass's ramp the chips' own glyphs.
            let mut theme = Theme::dark();
            theme.surface_treatment = slopty_shell_theme::SurfaceTreatment::Opaque;
            div().w(px(200.0)).h(px(20.0)).child(tab_strip_region(strip, &self.scroll, &theme))
        }
    }

    /// slopty: the strip fades the side that hides tabs from its last frame's scroll, and the
    /// frame that changes what hides asks for the next: tabs coming to overflow it, then going,
    /// leave a window at rest as the same state draws from scratch.
    #[gpui::test]
    fn a_tab_strip_at_rest_fades_what_it_hides_now(cx: &mut gpui::TestAppContext) {
        let (strip, cx) = cx.add_window_view(|_, _| Strip { tabs: 1, scroll: gpui::ScrollHandle::new() });
        let rest = |cx: &mut VisualTestContext| {
            cx.run_until_parked();
            for _ in 0..4 {
                cx.update(|window, cx| window.simulate_next_frame(cx));
                cx.run_until_parked();
            }
            cx.update(|window, cx| {
                let mut shown = window.painted_primitives();
                shown.sort_unstable();
                window.repeat_frame_time();
                window.refresh();
                window.draw(cx).clear(cx);
                let mut scratch = window.painted_primitives();
                scratch.sort_unstable();
                shown == scratch
            })
        };
        let fades = |cx: &mut VisualTestContext| {
            cx.update(|window, _| {
                let quads = window.painted_quads();
                quads.iter().filter(|quad| format!("{:?}", quad.background).starts_with("LinearGradient")).count()
            })
        };
        draw(cx);
        for (tabs, faded) in [(10, 1), (1, 0)] {
            strip.update(cx, |strip, cx| {
                strip.tabs = tabs;
                cx.notify();
            });
            assert!(rest(cx), "{tabs} tabs at rest draw as from scratch");
            assert_eq!(fades(cx), faded, "{tabs} tabs: the right side fades only while it hides");
        }
    }

    #[gpui::test]
    fn a_tabs_close_fades_with_its_hover(cx: &mut gpui::TestAppContext) {
        crate::motion::forget_for_test(); // slopty: no hover from the last seed
        cx.update(|cx| {
            cx.set_global(Theme::dark());
            cx.set_reduce_motion(true);
        });
        let (_, cx) = cx.add_window_view(|_, _| Chip { dirty: true, closes: Rc::default() });
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

    /// A tab's ✕ draws in 18 pt but takes a press over 24 (44 tall on a touch screen), and the
    /// larger target moves nothing: the slot sits where it always did.
    #[gpui::test]
    fn a_tabs_close_takes_a_press_past_what_it_draws(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| cx.set_global(Theme::dark()));
        let closes = Rc::new(Cell::new(0));
        let (_, cx) = cx.add_window_view({
            let closes = closes.clone();
            move |_, _| Chip { dirty: false, closes }
        });
        draw(cx);
        let tab = cx.debug_bounds("chip-0").expect("the tab");
        let slot = cx.debug_bounds("chip-close-0").expect("the drawn slot");
        let hit = cx.debug_bounds("chip-close-0-hit").expect("the press target");
        assert_eq!(f32::from(slot.size.width), CLOSE_SLOT);
        assert!(f32::from(hit.size.width) >= 24.0 && f32::from(hit.size.height) >= 24.0, "{hit:?}");
        assert_eq!(hit.center(), slot.center(), "the target centres on the slot");
        assert_eq!(slot.right(), tab.right() - px(4.0), "the slot holds its place");
        // Two points outside the slot, inside the target.
        cx.simulate_click(point(slot.left() - px(2.0), slot.center().y), Modifiers::none());
        cx.simulate_click(point(slot.center().x, slot.bottom() + px(2.0)), Modifiers::none());
        assert_eq!(closes.get(), 2);
        assert_eq!(close_hit_size(true).1, 44.0, "a finger's height on a touch screen");
    }
}

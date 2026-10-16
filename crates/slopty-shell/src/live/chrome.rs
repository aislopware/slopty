//! The shell's own parts as Slopty's views draw them: a terminal's find bar and pills, a remote
//! window's controls and notices (`docs/decisions/ui.md`, "Slopty's own views draw with the
//! shell's helpers").
//!
//! Most of what they need is public where zeron keeps it: [`crate::popover`] (cards, menus,
//! buttons, the rail), [`crate::icons`], [`crate::frost`], [`crate::motion`], [`crate::loaders`],
//! [`crate::settings::widgets`] and [`floating_pill`]. What zeron keeps to the
//! crate (the surface toolbars' control metrics) is handed out here, unchanged, so nothing is
//! drawn twice.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Div, EntityId, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, div, px,
};

use crate::theme::Theme;

/// A toolbar control's side: zeron's icon buttons, fields and chips are this tall.
pub const CONTROL_SIZE: f32 = crate::surface_chrome::CONTROL_SIZE;
/// A toolbar control's corners.
pub const CONTROL_RADIUS: f32 = crate::surface_chrome::CONTROL_RADIUS;
/// A toolbar control's icon.
pub const ICON_SIZE: f32 = crate::surface_chrome::ICON_SIZE;
/// The room between a toolbar's controls.
pub const CONTROL_GAP: f32 = crate::surface_chrome::CONTROL_GAP;

thread_local! {
    /// The shell's default theme, for a view drawn where no shell runs (a crate's own tests).
    static FALLBACK: &'static Theme = Box::leak(Box::new(Theme::default()));
}

/// The shell's theme now; its default where no shell has set one (a view's own tests).
#[must_use]
pub fn theme(cx: &App) -> &Theme {
    cx.try_global::<Theme>().unwrap_or_else(|| FALLBACK.with(|theme| *theme))
}

/// The field a surface's search sits in (the Files search, the browser's address): 24 tall,
/// its ink wash, 11.5 pt words.
#[must_use]
pub fn field() -> Div {
    crate::surface_chrome::input()
}

/// A chip that is on or off (a find's match case), as a surface's tab chip: 24 tall, 12 pt,
/// the selected wash when on and a hover wash when off.
#[must_use]
pub fn chip(id: impl Into<gpui::ElementId>, on: bool, theme: &Theme) -> gpui::Stateful<Div> {
    // slopty: never narrower than tall, so a one-letter chip ("W") is a touch target on an iPad.
    crate::surface_chrome::tab(id, on, theme)
        .min_w(px(CONTROL_SIZE))
        .justify_center()
        .map(keep_keyboard)
}

/// `el` pressed by the pointer without taking the keyboard: a float over a terminal or a remote
/// window (a pill, a find bar's toggle, a paste's buttons) leaves the keyboard where it was, as a
/// Mac's buttons do. Tab still reaches it, and its ring shows then.
pub fn keep_keyboard<E: gpui::InteractiveElement>(el: E) -> E {
    el.on_mouse_down(gpui::MouseButton::Left, |_ev, window, _cx| window.prevent_default())
}

/// A square button round one icon, a surface toolbar's: [`CONTROL_SIZE`] at [`CONTROL_RADIUS`],
/// the icon at [`ICON_SIZE`] in `text_muted`, its hover fading in an ink wash, and `label` as its
/// accessible name and its tooltip. `id` is also its debug selector and its hover fade's key, so
/// it is unique app-wide.
#[must_use]
pub fn icon_button(
    id: impl Into<SharedString>,
    icon: &'static str,
    label: impl Into<SharedString>,
    theme: &Theme,
) -> gpui::Stateful<Div> {
    let id: SharedString = id.into();
    let label: SharedString = label.into();
    let selector = id.to_string();
    div()
        .id(gpui::ElementId::Name(id.clone()))
        .debug_selector(move || selector)
        .role(gpui::Role::Button)
        .aria_label(label.clone())
        .tab_index(0)
        .size(px(CONTROL_SIZE))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(CONTROL_RADIUS))
        .cursor_pointer()
        .bg(crate::motion::hover_blend(
            &id,
            gpui::transparent_black(),
            crate::theme::ink(0.05),
        ))
        .on_hover(crate::motion::hover_listener(id))
        .focus_visible(|style| style.border_2().border_color(theme.accent))
        .map(keep_keyboard)
        .tooltip(crate::settings::widgets::text_tooltip(label))
        .child(crate::icons::icon(icon).size(px(ICON_SIZE)).text_color(theme.text_muted))
}

/// How loud a [`button`] is: the shell's dialog buttons.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tone {
    /// The one action the surface is for (`popover::btn_primary`): the text's solid fill.
    Primary,
    /// A way out or aside (`popover::btn_ghost`): quiet words, a wash under the pointer.
    Ghost,
    /// The press that removes or ends something for good (`popover::btn_danger`).
    Danger,
}

/// A text button as the shell's dialogs draw it, 13 pt, 8 round, its `label` its accessible
/// name; `id` is also its debug selector and its hover fade's key, so it is unique app-wide.
#[must_use]
pub fn button(
    id: impl Into<SharedString>,
    label: &str,
    tone: Tone,
    theme: &Theme,
) -> gpui::Stateful<Div> {
    let id: SharedString = id.into();
    let selector = id.to_string();
    let plate = match tone {
        Tone::Primary => crate::popover::btn_primary(theme, label),
        Tone::Ghost => crate::popover::btn_ghost(theme, label, id.clone()),
        Tone::Danger => crate::popover::btn_danger(theme, label),
    };
    plate
        .id(gpui::ElementId::Name(id))
        .debug_selector(move || selector)
        .role(gpui::Role::Button)
        .aria_label(SharedString::from(label.to_owned()))
        .tab_index(0)
        .flex_none()
        .whitespace_nowrap()
        .focus_visible(|style| style.border_2().border_color(theme.accent))
        .map(keep_keyboard)
}

/// A [`floating_pill`]'s glyph: a Solar icon at 14 in the popup's `text_muted`.
#[must_use]
pub fn pill_glyph(icon: &'static str, popup: &Theme) -> AnyElement {
    crate::icons::icon(icon)
        .size(px(ICON_SIZE))
        .flex_none()
        .text_color(popup.text_muted)
        .into_any_element()
}

/// A [`floating_pill`]'s words: 13 pt in the popup's text.
#[must_use]
pub fn pill_words(words: impl Into<SharedString>, popup: &Theme) -> Div {
    div()
        .text_size(crate::typography::ui_rems(13.0))
        .text_color(popup.text)
        .child(words.into())
}

/// A surface with nothing to show yet saying so in its middle: 13 pt in `text_muted` (a remote
/// window's "Connecting…" and "Waiting for the first frame…" both).
#[must_use]
pub fn note(text: impl Into<SharedString>, theme: &Theme) -> Div {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .p(px(16.0))
        .text_size(crate::typography::ui_rems(13.0))
        .text_color(theme.text_muted)
        .child(text.into())
}

/// A surface on its way, zeron's loading anatomy (the Changes pane's "Preparing diff…"): the
/// gradient spinner over a 12 pt caption in `text_faint`, in its middle. `view` keeps its pulse.
pub fn loading(
    caption: impl Into<SharedString>,
    theme: &Theme,
    view: EntityId,
    cx: &mut App,
) -> Div {
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(Theme::SPACE_SM))
        .child(crate::loaders::gradient_spinner("live-loading", theme, 3.0, view, cx))
        .child(
            div()
                .text_size(px(12.0))
                .text_color(theme.text_faint)
                .child(caption.into()),
        )
}

/// What a surface says when it cannot go on, zeron's empty-state anatomy (the "Add a project"
/// canvas): an optional mark 48 tall at 9 % of the text, a 16 pt medium headline 24 under it, a
/// 13 pt line in `text_muted` at 0.7 6 under that, and an optional action 20 lower, fading in.
pub fn empty_state(
    id: impl Into<gpui::ElementId>,
    mark: Option<&'static str>,
    headline: impl Into<SharedString>,
    line: Option<SharedString>,
    action: Option<AnyElement>,
    theme: &Theme,
) -> AnyElement {
    let marked = mark.is_some();
    let body = div()
        .flex()
        .flex_col()
        .items_center()
        .max_w(px(360.0))
        .px(px(16.0))
        .text_center()
        .when_some(mark, |el, mark| {
            el.child(crate::icons::icon(mark).size(px(48.0)).text_color(theme.text.opacity(0.09)))
        })
        .child(
            div()
                .when(marked, |el| el.mt(px(24.0)))
                .text_size(crate::typography::ui_rems(16.0))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme.text)
                .child(headline.into()),
        )
        .when_some(line, |el, line| {
            el.child(
                div()
                    .mt(px(6.0))
                    .text_size(crate::typography::ui_rems(13.0))
                    .text_color(theme.text_muted.opacity(0.7))
                    .child(line),
            )
        })
        .when_some(action, |el, action| el.child(div().mt(px(20.0)).child(action)));
    crate::motion::fade_in(id, body).into_any_element()
}

/// The transcript's "Scroll to bottom" pill as a free function, so Slopty's own views
/// float theirs with it (a terminal's lines below, a remote window's controls): 30 tall and
/// round, the popup plane inside the hairline (frosted, or raised under `shadow_md`). `content`,
/// given the popup's theme, is its glyph and its words, 6 apart; it enters by `dialog_in`.
/// `hover_key` names its hover fade (unique app-wide); `None` keeps the plate still, for a pill
/// that only says something or holds buttons. `interact` hangs the caller's click on it.
pub fn floating_pill(
    anim_key: impl Into<SharedString>,
    hover_key: Option<SharedString>,
    theme: &Theme,
    content: impl FnOnce(&Theme) -> (Option<AnyElement>, AnyElement),
    interact: impl FnOnce(gpui::Stateful<gpui::Div>) -> gpui::Stateful<gpui::Div>,
) -> AnyElement {
    let theme = theme.for_popup();
    let (glyph, words) = content(&theme);
    let anim_key: SharedString = anim_key.into();
    let glass = theme.is_frost();
    let base = match (&hover_key, glass) {
        (_, true) => crate::popover::surface_bg(&theme),
        (Some(key), false) => {
            crate::motion::hover_blend(key, theme.surface_raised, theme.surface_raised_hover)
        }
        (None, false) => theme.surface_raised,
    };
    let wash = match (&hover_key, glass) {
        (Some(key), true) => {
            crate::motion::hover_blend(key, gpui::transparent_black(), theme.glass_hover())
        }
        _ => gpui::transparent_black(),
    };
    let lead = if glyph.is_some() { 11.0 } else { 13.0 };
    let mut pill = div()
        .id(anim_key.clone())
        .h(px(30.0))
        .flex_none()
        .rounded_full()
        .border_1()
        .border_color(theme.border)
        .when(!glass, |el| el.shadow_md())
        .bg(base);
    if let Some(key) = hover_key {
        pill = pill.on_hover(crate::motion::hover_listener(key));
    }
    let pill = interact(pill).child(
        // The hover wash rides an inner full-height layer so it
        // composites over the tint (a div has one bg).
        div()
            .h_full()
            .rounded_full()
            .flex()
            .items_center()
            .gap(px(6.0))
            .pl(px(lead))
            .pr(px(13.0))
            .whitespace_nowrap()
            .bg(wash)
            .children(glyph)
            .child(words),
    );
    // Frost OUTSIDE the entry animation (the composer pill's exact
    // composition): one scene layer — blur, then the pill's quads, then
    // glyphs — so the pill always composes over the content scrolling under
    // it, and never loses its washes to the kind-sorted draw order (frost.rs
    // module docs).
    let pill = crate::motion::dialog_in(anim_key, pill);
    crate::frost::frosted(15.0, crate::frost::MENU_BLUR, pill).into_any_element()
}

/// A [`crate::settings::widgets::TextTooltip`]'s frosted card, for what Slopty's own views show
/// where a hint shows (a terminal link's target under ⌘).
pub fn tooltip_card(text: impl Into<SharedString>, theme: &Theme) -> crate::frost::Frosted {
    let card = div()
        .max_w(px(320.0))
        .px(px(9.0))
        .py(px(6.0))
        .rounded(px(6.0))
        .border_1()
        .border_color(theme.border)
        .bg(crate::popover::surface_bg(theme))
        .text_size(px(11.0))
        .text_color(theme.text_muted)
        .child(text.into());
    crate::frost::frosted(6.0, crate::frost::MENU_BLUR, card)
}

// slopty: a float's controls are touch targets on an iPad: none under 24 pt either way.
#[cfg(test)]
mod tests {
    use gpui::{Render, div};

    use super::*;

    /// What the iPad's floats are built of: a toolbar button, a toggle chip with one letter, the
    /// shortest dialog button and a pill.
    struct Floats;

    impl Render for Floats {
        fn render(&mut self, _: &mut gpui::Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
            let theme = theme(cx).clone();
            div()
                .flex()
                .items_start()
                .gap(px(8.0))
                .child(icon_button("float-close", crate::icons::CLOSE, "Close", &theme))
                .child(chip("float-chip", false, &theme).debug_selector(|| "float-chip".into()).child("W"))
                .child(button("float-ok", "OK", Tone::Ghost, &theme))
                .child(floating_pill(
                    "float-pill",
                    None,
                    &theme,
                    |popup| (None, pill_words("Back", popup).into_any_element()),
                    |pill| pill.debug_selector(|| "float-pill".into()),
                ))
        }
    }

    #[gpui::test]
    fn a_float_s_controls_are_touch_targets(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
        });
        let (_floats, cx) = cx.add_window_view(|_, _| Floats);
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        for id in ["float-close", "float-chip", "float-ok", "float-pill"] {
            let bounds = cx.debug_bounds(id).unwrap_or_else(|| panic!("{id} drawn"));
            let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
            assert!(w >= 24.0 && h >= 24.0, "{id} is {w}x{h}");
        }
    }
}

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

use crate::motion::HoverFade as _;
use crate::theme::Theme;

/// A toolbar control's side: zeron's icon buttons, fields and chips are this tall.
pub const CONTROL_SIZE: f32 = crate::surface_chrome::CONTROL_SIZE;
/// A toolbar control's corners.
pub const CONTROL_RADIUS: f32 = crate::surface_chrome::CONTROL_RADIUS;
/// A toolbar control's icon.
pub const ICON_SIZE: f32 = crate::surface_chrome::ICON_SIZE;
/// The room between a toolbar's controls.
pub const CONTROL_GAP: f32 = crate::surface_chrome::CONTROL_GAP;
/// The shell's keyboard focus ring (2 px of the accent, 2 px clear), for Slopty's own
/// controls.
pub use crate::surface_chrome::focus_ring;

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
    // Never narrower than tall, so a one-letter chip ("W") is a touch target on an iPad.
    crate::surface_chrome::tab(id, on, theme)
        .min_w(px(CONTROL_SIZE))
        .justify_center()
        .map(keep_keyboard)
}

/// `el` pressed by the pointer without taking the keyboard.
///
/// A float over a terminal or a remote window (a pill, a find bar's toggle, a paste's buttons)
/// leaves the keyboard where it was, as a Mac's buttons do. Tab still reaches it, and its ring
/// shows then.
pub fn keep_keyboard<E: gpui::InteractiveElement>(el: E) -> E {
    el.on_mouse_down(gpui::MouseButton::Left, |_ev, window, _cx| window.prevent_default())
}

/// A square button round one icon, a surface toolbar's.
///
/// It is the pane's one icon button (`crate::surface_chrome::icon_button`), `label` as its
/// accessible name and its tooltip at the house delay. `id` is also its debug selector and keys
/// its hover fade, so it is unique app-wide. A press leaves the keyboard where it was.
// One recipe for every pane button; this one drew its own ink washes and showed its
// tooltip at gpui's 500 ms.
#[must_use]
pub fn icon_button(
    id: impl Into<SharedString>,
    icon: &'static str,
    label: impl Into<SharedString>,
    theme: &Theme,
) -> gpui::Stateful<Div> {
    use crate::settings::widgets::ActionHint as _;
    let id: SharedString = id.into();
    let selector = id.to_string();
    crate::surface_chrome::icon_button(id, icon, theme).debug_selector(move || selector).hint(label)
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
        .map(|el| focus_ring(el, theme))
        // Pressed reads a step deeper (M22).
        .map(|el| match tone {
            Tone::Ghost => el.active(|style| style.bg(crate::theme::pressed_wash())),
            Tone::Primary | Tone::Danger => el.active(|style| style.opacity(0.85)),
        })
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
    div().text_size(crate::typography::ui_rems(13.0)).text_color(popup.text).child(words.into())
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

// One loading voice for every surface, and a quick load never flashes (the 150 ms grace,
// `loaders::pending`; `docs/decisions/ui.md`, "A load waits 200 ms before it says so", now 150).
/// A surface on its way, in its middle: [`crate::loaders::pending`]'s spinner and words, shown
/// once the load has run its grace. `theme` and `view` are the caller's; the loader reads both
/// itself.
pub fn loading(
    caption: impl Into<SharedString>,
    _theme: &Theme,
    _view: EntityId,
    _cx: &mut App,
) -> Div {
    let caption: SharedString = caption.into();
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .child(crate::loaders::pending(format!("live-loading-{caption}"), caption))
}

/// What a pane says when it has nothing to show or cannot go on, in opencode's quiet voice.
///
/// An optional 20 pt mark in `text_faint`, a 14 pt medium headline 8 under it, a 13 pt line in
/// `text_muted` 8 under that (282 to 360 wide), and an optional action 16 lower, fading in. It
/// fills its region and sits at the optical centre, lifted by the smaller of 18 % of the region
/// and 160 (opencode's `padding-bottom: 160px`), not the middle.
pub fn empty_state(
    id: impl Into<gpui::ElementId>,
    mark: Option<&'static str>,
    headline: impl Into<SharedString>,
    line: Option<SharedString>,
    action: Option<AnyElement>,
    theme: &Theme,
) -> AnyElement {
    let body = div()
        .flex_none()
        .flex()
        .flex_col()
        .items_center()
        .max_w(px(EMPTY_STATE_WIDTH))
        .px(px(16.0))
        .text_center()
        .gap(px(8.0))
        .when_some(mark, |el, mark| {
            el.child(crate::icons::icon(mark).size(px(20.0)).text_color(theme.text_faint))
        })
        .child(
            div()
                .text_size(crate::typography::ui_rems(crate::typography::ladder::HEADING))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme.text)
                .child(headline.into()),
        )
        .when_some(line, |el, line| {
            el.child(
                div()
                    .text_size(crate::typography::ui_rems(crate::typography::ladder::BODY))
                    .text_color(theme.text_muted)
                    .child(line),
            )
        })
        .when_some(action, |el, action| el.child(div().mt(px(8.0)).child(action)))
        .debug_selector(|| "empty-state".to_owned());
    optically_centred(crate::motion::fade_in(id, body)).into_any_element()
}

/// The widest an empty state's words run (opencode's 360).
const EMPTY_STATE_WIDTH: f32 = 360.0;
/// The most an empty state is lifted above the middle, as bottom room (opencode's 160).
const OPTICAL_LIFT_MAX: f32 = 160.0;
/// An empty state's lift as a share of its region.
const OPTICAL_LIFT_SHARE: f32 = 0.18;

/// `content` in a region it fills, centred across and a little above the middle: the room under
/// it gains the smaller of 18 % of the region and 160, so the eye finds it where it looks first.
pub fn optically_centred(content: impl IntoElement) -> Div {
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .child(div().flex_1())
        .child(content)
        .child(div().flex_1())
        .child(div().h(gpui::relative(OPTICAL_LIFT_SHARE)).max_h(px(OPTICAL_LIFT_MAX)))
}

/// The transcript's "Scroll to bottom" pill as a free function.
///
/// Slopty's own views float theirs with it (a terminal's lines below, a remote window's
/// controls): 30 tall and round, the popup plane inside the hairline (frosted, or raised under
/// `shadow_md`). `content`, given the popup's theme, is its glyph and its words, 6 apart; it enters
/// by `dialog_in`. `hover_key` names its hover fade (unique app-wide); `None` keeps the plate
/// still, for a pill that only says something or holds buttons. `interact` hangs the caller's click
/// on it.
pub fn floating_pill(
    anim_key: impl Into<SharedString>,
    hover_key: Option<SharedString>,
    theme: &Theme,
    content: impl FnOnce(&Theme) -> (Option<AnyElement>, AnyElement),
    interact: impl FnOnce(gpui::Stateful<Div>) -> gpui::Stateful<Div>,
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
        .when(!glass, gpui::Styled::shadow_md)
        .bg(base);
    if let Some(key) = hover_key {
        pill = pill.hover_fade(key);
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
    tooltip_card_keyed(text, None, theme)
}

/// [`tooltip_card`] naming an action and, after it, its key ("Show files panel  ⌘E"): the key
/// in the menus' and palette's voice, one step under the label.
pub fn tooltip_card_keyed(
    text: impl Into<SharedString>,
    keys: Option<SharedString>,
    theme: &Theme,
) -> crate::frost::Frosted {
    let text: SharedString = text.into();
    tooltip_frame(
        theme,
        (text.clone(), keys.clone()),
        div()
            .flex()
            .flex_row()
            .items_baseline()
            .gap(px(TOOLTIP_KEY_GAP))
            .child(div().min_w_0().child(text))
            .children(keys.map(|keys| tooltip_keys(theme, keys))),
    )
}

/// Between a tooltip's words and its key (Claude desktop's `gap-2`).
pub const TOOLTIP_KEY_GAP: f32 = 8.0;
/// The tooltip's corners, which its frost masks to.
pub const TOOLTIP_RADIUS: f32 = 6.0;
/// The tooltip's words, at the 16 px baseline.
pub const TOOLTIP_TEXT_SIZE: f32 = crate::typography::ladder::LABEL;
/// The widest a tooltip draws before its words wrap.
pub const TOOLTIP_MAX_WIDTH: f32 = 240.0;

// Legible as Claude desktop's and T3's: 12 pt words in the primary ink, on the spacing ladder,
// 240 wide at most; frosted, since an inverted chip would be a seam beside the frosted menus.
/// The chip every tooltip is.
///
/// Frosted, ringed, 12 pt in the primary ink at the font-size setting, entering with
/// [`crate::motion::tooltip_in`] (none under reduced motion). It is one `Tooltip` node for
/// assistive tech, `spoken` as its label and value (the words, then the key).
pub fn tooltip_frame(
    theme: &Theme,
    spoken: (SharedString, Option<SharedString>),
    content: impl IntoElement,
) -> crate::frost::Frosted {
    let (label, value) = spoken;
    let card = div()
        .id("tooltip")
        .role(gpui::Role::Tooltip)
        .aria_label(label)
        .max_w(px(TOOLTIP_MAX_WIDTH))
        .px(px(Theme::SPACE_SM))
        .py(px(6.0)) // the border's pixel, now the ring draws inside the edge.
        .rounded(px(TOOLTIP_RADIUS))
        // A ring, the lit top edge on dark and a short shadow, not a 1 px border
        // (opencode's and Amp's floats; `Theme::elevation`).
        .shadow(theme.elevation(crate::theme::Elevation::Raised))
        .bg(crate::popover::surface_bg(theme))
        .text_size(crate::typography::ui_rems(TOOLTIP_TEXT_SIZE))
        .text_color(theme.text)
        .when_some(value, gpui::StatefulInteractiveElement::aria_value)
        .child(content);
    crate::frost::frosted(
        TOOLTIP_RADIUS,
        crate::frost::MENU_BLUR,
        crate::motion::tooltip_in("tooltip-in", card),
    )
}

/// A tooltip's keys, each drawn as its own cap (opencode's keybind: 14 px tall, at least as wide,
/// 4 px each side, radius 2, a faint fill, the glyph at 11 px in the system face so ⇧ ⌥ ⏎ draw).
#[expect(clippy::needless_pass_by_value, reason = "shell lint migration: seam, callers in slice D")]
pub fn tooltip_keys(theme: &Theme, keys: SharedString) -> Div {
    div().flex_none().flex().items_center().gap(px(2.0)).children(key_caps(&keys).into_iter().map(
        |cap| {
            div()
                .h(px(KEY_CAP_SIZE))
                .min_w(px(KEY_CAP_SIZE))
                .px(px(4.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(2.0))
                .bg(crate::theme::ink(0.10))
                .shadow(vec![cap_lip(1.0)]) // the cap's lip (design-inspiration item 9).
                .whitespace_nowrap()
                .font_family(crate::popover::KEY_GLYPH_FONT)
                .text_size(crate::typography::ui_rems(11.0))
                .text_color(theme.text_muted)
                .child(cap)
        },
    ))
}

/// A key cap's height and least width.
const KEY_CAP_SIZE: f32 = 14.0;

// Key caps press with the real key (HextaUI's `Kbd` with `useHeldKeys`; design-inspiration
// item 9). A cap stands on a 1 px lip; while its key is down in the focused palette or picker it
// sinks 1 px onto it, its fill deepening to `ink(0.16)` and the lip gone, over 100 ms. The palette
// or picker's root records what is held ([`track_held_keys`]) and forgets it as it opens
// ([`release_held_keys`]); auto-repeat keeps a key down.

/// How long a cap takes to press or come back up.
pub const CAP_PRESS: crate::motion::MotionSpec =
    crate::motion::MotionSpec::new(100, crate::motion::EASE_OUT);
/// A pressed cap's fill.
const CAP_PRESSED_FILL: f32 = 0.16;
/// A cap's lip at rest.
const CAP_LIP: f32 = 0.14;

/// A cap's 1 px lip along its bottom edge, drawn inside it so it takes no room; `shown` is 1 at
/// rest and 0 pressed.
fn cap_lip(shown: f32) -> gpui::BoxShadow {
    gpui::BoxShadow {
        color: crate::theme::ink(CAP_LIP * shown),
        offset: gpui::point(px(0.0), px(-1.0)),
        blur_radius: px(0.0),
        spread_radius: px(0.0),
        inset: true,
    }
}

/// The keys held down in the focused palette or picker, each with when it went down or came up.
#[derive(Debug, Default)]
pub struct HeldKeys {
    keys: std::collections::HashMap<SharedString, (bool, std::time::Instant)>,
}

impl gpui::Global for HeldKeys {}

impl HeldKeys {
    fn set(key: &str, down: bool, window: &mut gpui::Window, cx: &mut App) {
        let now = crate::clock::now(cx);
        let held = cx.default_global::<Self>();
        let was = held.keys.get(key).is_some_and(|(down, _)| *down);
        if was != down {
            held.keys.insert(SharedString::from(key.to_owned()), (down, now));
            window.refresh();
        }
    }

    /// How far a cap for `key` (a gpui key name) is pressed: 0 up, 1 down, between while it moves.
    fn pressed(key: &str, cx: &App) -> (f32, bool) {
        let Some(&(down, at)) = cx.try_global::<Self>().and_then(|held| held.keys.get(key)) else {
            return (0.0, false);
        };
        if crate::motion::reduced_motion(cx) {
            return (if down { 1.0 } else { 0.0 }, false);
        }
        let elapsed = crate::clock::now(cx).saturating_duration_since(at);
        let total = CAP_PRESS.total().mul_f32(crate::motion::speed_scale());
        let moved = CAP_PRESS.progress(elapsed.as_secs_f32() / total.as_secs_f32());
        (if down { moved } else { 1.0 - moved }, elapsed < total)
    }
}

/// `root`, a palette's or picker's focused root, recording which keys are held for its caps.
pub fn track_held_keys<E: gpui::InteractiveElement>(root: E) -> E {
    root.capture_key_down(|event: &gpui::KeyDownEvent, window, cx| {
        HeldKeys::set(&event.keystroke.key, true, window, cx);
    })
    .capture_key_up(|event: &gpui::KeyUpEvent, window, cx| {
        HeldKeys::set(&event.keystroke.key, false, window, cx);
    })
}

/// Forgets every held key: a palette or picker calls it as it opens, since the key that closed
/// the last one (↵, Esc) came up where no root was listening.
pub fn release_held_keys(cx: &mut App) {
    if cx.try_global::<HeldKeys>().is_some_and(|held| !held.keys.is_empty()) {
        cx.set_global(HeldKeys::default());
    }
}

/// The gpui key a cap's glyph or word stands for.
fn cap_key(cap: &str) -> Option<&'static str> {
    Some(match cap {
        "↑" => "up",
        "↓" => "down",
        "←" => "left",
        "→" => "right",
        "⏎" | "↵" | "Return" | "Enter" | "return" | "enter" => "enter",
        "Esc" | "esc" | "⎋" => "escape",
        "Tab" | "tab" | "⇥" => "tab",
        _ => return None,
    })
}

/// `cap`, a key cap resting on `fill` (an `ink` alpha) for `keys` (glyphs or words split by
/// spaces, "↑ ↓" one cap for both), drawn with its lip, sunk while one of its keys is held.
pub fn pressable_cap(cap: Div, keys: &str, fill: f32, window: &gpui::Window, cx: &App) -> Div {
    let (pressed, moving) = keys
        .split_whitespace()
        .filter_map(cap_key)
        .map(|key| HeldKeys::pressed(key, cx))
        .fold((0.0_f32, false), |(p, m), (q, n)| (p.max(q), m || n));
    if moving {
        window.request_animation_frame();
    }
    cap.relative()
        .top(px(pressed))
        .bg(crate::theme::ink(crate::motion::lerp(fill, CAP_PRESSED_FILL, pressed)))
        .shadow(vec![cap_lip(1.0 - pressed)])
}

/// The caps a shortcut's words draw as: each modifier glyph its own cap, then the key; a chord
/// written with spaces or `+` splits there.
fn key_caps(keys: &str) -> Vec<String> {
    const MODIFIERS: [char; 5] = ['⌘', '⇧', '⌥', '⌃', '⇪'];
    let mut caps = Vec::new();
    for word in keys.split([' ', '+']).filter(|word| !word.is_empty()) {
        let key = word.trim_start_matches(MODIFIERS);
        let modifiers = word.strip_suffix(key).unwrap_or_default();
        caps.extend(modifiers.chars().map(String::from));
        if !key.is_empty() {
            caps.push(key.to_owned());
        }
    }
    caps
}

// A float's controls are touch targets on an iPad: none under 24 pt either way.
#[cfg(test)]
mod tests {
    use gpui::{Render, div};

    use super::*;

    /// A shortcut draws one cap per modifier and one for its key, whichever way it is written.
    #[test]
    fn a_shortcut_draws_a_cap_per_key() {
        assert_eq!(key_caps("⌘⇧E"), ["⌘", "⇧", "E"]);
        assert_eq!(key_caps("⌘↵"), ["⌘", "↵"]);
        assert_eq!(key_caps("Ctrl L"), ["Ctrl", "L"]);
        assert_eq!(key_caps("Ctrl+Shift+Tab"), ["Ctrl", "Shift", "Tab"]);
        assert_eq!(key_caps("⌘K ⌘S"), ["⌘", "K", "⌘", "S"]);
        assert_eq!(key_caps("Esc"), ["Esc"]);
    }

    struct Empty;

    impl Render for Empty {
        fn render(
            &mut self,
            _: &mut gpui::Window,
            cx: &mut gpui::Context<Self>,
        ) -> impl IntoElement {
            let theme = theme(cx).clone();
            div().size_full().child(empty_state(
                "empty",
                None,
                "Nothing here",
                Some("A line under it.".into()),
                None,
                &theme,
            ))
        }
    }

    /// An empty state sits above the middle of its region by half its lift, the smaller of 18 %
    /// of the region and 160.
    #[gpui::test]
    fn an_empty_state_sits_at_the_optical_centre(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
        });
        let (_view, cx) = cx.add_window_view(|_, _| Empty);
        cx.simulate_resize(gpui::size(px(500.0), px(800.0)));
        // Drawn once to start its fade-in, then again once the 4 px rise has settled.
        for _ in 0..2 {
            cx.update(|window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
            });
            cx.executor().advance_clock(std::time::Duration::from_secs(1));
        }
        let body = cx.debug_bounds("empty-state").expect("drawn");
        let centre = f32::from(body.center().y);
        let lift = (0.18_f32 * 800.0).min(160.0);
        assert!((centre - (400.0 - lift / 2.0)).abs() < 1.5, "centre at {centre}");
    }

    /// What the iPad's floats are built of: a toolbar button, a toggle chip with one letter, the
    /// shortest dialog button and a pill.
    struct Floats;

    impl Render for Floats {
        fn render(
            &mut self,
            _: &mut gpui::Window,
            cx: &mut gpui::Context<Self>,
        ) -> impl IntoElement {
            let theme = theme(cx).clone();
            div()
                .flex()
                .items_start()
                .gap(px(8.0))
                .child(icon_button("float-close", crate::icons::CLOSE, "Close", &theme))
                .child(
                    chip("float-chip", false, &theme)
                        .debug_selector(|| "float-chip".into())
                        .child("W"),
                )
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
    /// A palette footer: the arrows' cap, in a root that records held keys.
    struct Footer {
        focus: gpui::FocusHandle,
    }

    impl Render for Footer {
        fn render(
            &mut self,
            window: &mut gpui::Window,
            cx: &mut gpui::Context<Self>,
        ) -> impl IntoElement {
            let cap = div().h(px(22.0)).w(px(30.0)).debug_selector(|| "arrows-cap".into());
            track_held_keys(div().track_focus(&self.focus).size_full().p(px(10.0)))
                .child(pressable_cap(cap, "↑ ↓", 0.05, window, cx))
        }
    }

    /// A cap sinks 1 px while its key is held (auto-repeat too) and comes back up as it is let go;
    /// a key no cap shows moves nothing.
    #[gpui::test]
    fn a_key_cap_presses_with_the_real_key(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            cx.set_global(Theme::default());
        });
        let (footer, cx) = cx.add_window_view(|_, cx| Footer { focus: cx.focus_handle() });
        let focus = footer.read_with(cx, |footer, _| footer.focus.clone());
        cx.update(|window, cx| window.focus(&focus, cx));
        let top = |cx: &mut gpui::VisualTestContext| {
            cx.executor().advance_clock(CAP_PRESS.total().saturating_mul(2));
            cx.update(|window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
            });
            f32::from(cx.debug_bounds("arrows-cap").expect("drawn").origin.y)
        };
        let rest = top(cx);
        let key = |key: &'static str| gpui::Keystroke::parse(key).unwrap();
        let down = |key: &'static str, is_held: bool| gpui::KeyDownEvent {
            keystroke: gpui::Keystroke::parse(key).unwrap(),
            is_held,
            prefer_character_input: false,
        };
        cx.simulate_event(down("tab", false));
        assert_eq!(top(cx), rest, "Tab has no cap here");
        cx.simulate_event(down("down", false));
        assert_eq!(top(cx), rest + 1.0, "pressed");
        cx.simulate_event(down("down", true));
        assert_eq!(top(cx), rest + 1.0, "held through the repeat");
        cx.simulate_event(gpui::KeyUpEvent { keystroke: key("down") });
        assert_eq!(top(cx), rest, "let go");
        cx.simulate_event(down("up", false));
        assert_eq!(top(cx), rest + 1.0, "either arrow");
        cx.update(|_, cx| release_held_keys(cx));
        assert_eq!(top(cx), rest, "forgotten as a palette opens");
    }
}

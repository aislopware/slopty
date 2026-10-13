//! gpui-kit's theme on Slopty's tokens.
//!
//! The widgets borrowed from gpui-kit (inputs, the composer's textarea, Markdown `TextView`)
//! colour themselves from gpui-kit's own global [`gpui_kit::component::Theme`]. Rather than
//! keep two palettes, [`sync`] switches that theme to the matching mode and then writes the
//! Slopty surface tokens over the colours those widgets read, so a text field, a code block
//! or a link looks like the chrome around it in both variants.

use std::rc::Rc;
use std::sync::{Arc, LazyLock};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Animation, App, BoxShadow, Div, FontFeatures, FontWeight, Hsla, InteractiveElement as _,
    IntoElement, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled, div,
    point, px,
};
use gpui_kit::base::text::TextViewDefaults;
use gpui_kit::component::{Theme as KitTheme, ThemeMode};
use slopty_theme::{Motion, Rgb, Theme, Variant, alpha, stroke};

use crate::colors::{hsla, hsla_alpha};

mod change;
pub mod find;
pub mod menu;
pub mod progress;
mod spark;
pub use change::on_change;
pub use find::FindBar;
pub use menu::{Menu, MenuItem, MenuPanel};
pub use spark::Spark;

/// Figures of one width: OpenType `tnum`, which the system UI font and the terminal face both
/// carry. Built once; each use clones an `Arc`.
static TABULAR: LazyLock<FontFeatures> =
    LazyLock::new(|| FontFeatures(Arc::new(vec![("tnum".to_owned(), 1)])));

/// The font features that set a number's figures to one width, for text that is not a
/// [`Styled`] element (a shaped run, a `TextStyle`).
#[must_use]
pub fn tabular_figures() -> FontFeatures {
    TABULAR.clone()
}

/// `el` with tabular figures: every digit one width.
///
/// A count, a round trip, an age or a progress readout that changes then does not shift what
/// follows it, and a column of them lines up. The system font's figures are proportional by
/// default, where a `1` is narrower than an `8`. The children inherit it, `ChromeText`
/// included.
#[must_use]
pub fn tabular<E: Styled>(el: E) -> E {
    el.font_features(tabular_figures())
}

/// How long chrome takes to appear: an overlay, a menu, the palette, a hint. Short enough
/// that a key typed at once still lands in what opened (focus does not wait for it).
pub const FADE: std::time::Duration = Motion::DEFAULT.fade;

/// The curve of everything that moves but a sheet: [`Motion::ease_out`], fast out of the gate
/// and a long soft landing.
pub fn ease_out() -> impl Fn(f32) -> f32 {
    move |t| Motion::DEFAULT.ease_out.at(t)
}

/// Whether chrome may move: not while GPUI's Reduce Motion flag is set.
///
/// The flag is the one answer for every animation, Slopty's and GPUI's own (gpui-kit's
/// among them): the app sets it from the system as it launches, as the setting changes and
/// each time it comes forward, and a test sets it with [`App::set_reduce_motion`]. Nothing reads
/// the system on its own, so no view can move while the rest of the window holds still.
#[must_use]
pub fn motion(cx: &App) -> bool {
    !cx.reduce_motion()
}

/// The one-shot GPUI animation a fill, a width or a fold settles on: [`Motion::settle`],
/// eased out. Gate it on [`motion`]: under Reduce Motion the caller draws the end state instead.
#[must_use]
pub fn settle() -> Animation {
    let m = Motion::DEFAULT;
    let curve = m.ease_out;
    Animation::new(m.settle).with_easing(move |t| curve.at(t))
}

/// `el`'s fill and hairline following its states as a native control's do.
///
/// Into a hover or a press and back to rest over [`Motion::feedback`], eased out; Zed's land at
/// once, so with no time to take nothing is eased. Only colours ease; the state itself, and
/// layout, change at once, and under Reduce Motion GPUI applies every change at once. Every kit
/// control that answers the pointer wears it, so the durations live here and nowhere else.
///
/// Not on the rows of a list GPUI composites as a scroll layer: a fill easing out under rows
/// that scroll past the pointer repaints the layer, and measured headless it composited 2 of 30
/// scroll frames where the list composites all 30.
pub fn eased<E: gpui::StatefulInteractiveElement>(el: E) -> E {
    let m = Motion::DEFAULT;
    if m.feedback.is_zero() {
        return el;
    }
    el.transition(gpui::StateTransition::new(m.feedback).enter(m.feedback).with_easing(ease_out()))
}

/// A pill's height in points: T3's badge (`h-5`), a notch over Linear's 18.
///
/// Fixed rather than grown from a pad round the text, so it sits centred in a 27 pt header with
/// room above and below. Padded, the agent's pill stood 23 pt tall and touched the hairline.
pub const PILL_HEIGHT: f32 = 20.0;

/// A pill's shape without its fill.
///
/// [`PILL_HEIGHT`] tall, the text centred on it at `small()`, `spacing.sm` at each end, a chip
/// at `radii.sm` as `MonoCode`'s state chips are: its glyph and its word.
/// Capsules are kept for the switch, count badges, dots and the scrollbar's thumb. A header's
/// words that act (Take, Mute) wear it bare, so they stand as tall as a state's pill beside
/// them and their hover takes the same chip. Key caps keep their 4 pt corners: they
/// are keys.
#[must_use]
pub fn pill_frame(theme: &Theme) -> Div {
    div()
        .flex()
        .items_center()
        .h(px(PILL_HEIGHT))
        .gap(px(theme.spacing.xs))
        .px(px(theme.spacing.sm))
        .rounded(px(theme.radii.sm))
        .overflow_hidden()
        .whitespace_nowrap()
        .text_size(px(theme.typography.small()))
}

/// How long something took or has run, the one way chrome says it: "850 ms", "6.2 s" (the
/// tenth under ten seconds, left off when it is zero), "35 s", "1m 5s", "1h 4m".
///
/// Compact as Claude Code's own "1m 5s" and cargo's "1m 04s" are, so a line that sets one
/// beside the other does not read two formats. A clock that ticks each second passes whole
/// seconds and so never shows a tenth. Set it in [`tabular`] figures.
#[must_use]
pub fn duration(elapsed: std::time::Duration) -> String {
    let (secs, ms) = (elapsed.as_secs(), elapsed.subsec_millis());
    match secs {
        0 => format!("{ms} ms"),
        1..10 if ms >= 100 => format!("{secs}.{} s", ms / 100),
        1..60 => format!("{secs} s"),
        60..3_600 => format!("{}m {}s", secs / 60, secs % 60),
        _ => format!("{}h {}m", secs / 3_600, (secs % 3_600) / 60),
    }
}

/// A clock that ticks once a second, as a turn at work and a long command count it.
///
/// Whole seconds in [`duration`]'s one form, from "1 s", so a frame never shows a fraction the
/// next tick undoes, nor "0 ms" while the clock has only begun.
#[must_use]
pub fn clock(elapsed: std::time::Duration) -> String {
    duration(std::time::Duration::from_secs(elapsed.as_secs().max(1)))
}

/// A size in bytes as a person reads it: "812 B", "240 KB", "1.2 MB". Whole kilobytes, since a
/// tenth of one is noise; a tenth of a megabyte is still a size worth telling apart.
#[must_use]
pub fn size_label(bytes: u64) -> String {
    #[expect(clippy::cast_precision_loss, reason = "a label, not arithmetic")]
    let n = bytes as f64;
    match bytes {
        0..1_024 => format!("{bytes} B"),
        1_024..1_048_576 => format!("{:.0} KB", n / 1_024.0),
        _ => format!("{:.1} MB", n / 1_048_576.0),
    }
}

/// How large an overlay grows on a desktop.
///
/// A phone gets whatever the margins leave. Two sizes, because a list of commands and a file
/// being edited want different room, and a third would be a size nobody could name.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Overlay {
    /// A list typed at: the command palette, the pickers.
    List,
    /// A document edited: the settings (about 800 × 600 where the window has room), the
    /// project search.
    Editor,
}

impl Overlay {
    /// Width and height ceilings, in points.
    #[must_use]
    pub const fn bounds(self) -> (f32, f32) {
        match self {
            Self::List => (560.0, 520.0),
            Self::Editor => (800.0, 720.0),
        }
    }
}

/// A float's shadow, as GPUI draws it: Zed's two crisp layers
/// ([`slopty_theme::Elevation::float`]), as its menus, popovers and palette wear them.
#[must_use]
pub fn elevation(theme: &Theme) -> Vec<BoxShadow> {
    theme
        .elevation
        .float
        .iter()
        .filter(|l| l.shows())
        .map(|&layer| drop_shadow(theme, layer))
        .collect()
}

/// One layer of the shade falling under a surface.
fn drop_shadow(theme: &Theme, layer: slopty_theme::Shadow) -> BoxShadow {
    BoxShadow {
        color: hsla_alpha(theme.elevation.shade, layer.alpha),
        offset: point(px(0.0), px(layer.y)),
        blur_radius: px(layer.blur),
        spread_radius: px(layer.spread),
        inset: false,
    }
}

/// `el` raised off its plane at rest: what stands up from where it lies (a card, a tray's head,
/// a notice's disc, a chip over a body).
///
/// The work's ground inside the one structural `stroke` line, with no shadow: a resting thing is
/// told by its edge, as Delta's cards rest on its page (`border` rings controls only). A hover or
/// selection wash is never a resting surface, so the kit lint `a_resting_fill_is_raised_or_sunk`
/// keeps the washes to the pointer and the selection. The caller keeps its own radius.
#[must_use]
pub fn raised<E: Styled>(el: E, theme: &Theme) -> E {
    raised_part(el, theme, true, true)
}

/// One part of a [`raised`] thing cut across its width: `first` takes the top edge and `last`
/// the bottom one. In order the parts read as one.
#[must_use]
pub fn raised_part<E: Styled>(el: E, theme: &Theme, first: bool, last: bool) -> E {
    let el = el.bg(hsla(theme.surfaces.ground)).border_l(HAIR).border_r(HAIR);
    let el = if first { el.border_t(HAIR) } else { el };
    let el = if last { el.border_b(HAIR) } else { el };
    el.border_color(hsla(theme.surfaces.stroke))
}

/// `el` lifted off the chrome, Zed's elevated surface: the `elevated` plane inside the lighter
/// `stroke` line (Zed's `border.variant`) under the float's shadow.
///
/// Everything that floats wears it: the palette, a menu, a popover, a hint, a find bar, a chip
/// over a body. The caller keeps its own radius: `radii.lg` for a menu, a popover, a hint or a
/// toast, the control's own for a chip.
#[must_use]
pub fn elevate<E: Styled>(el: E, theme: &Theme) -> E {
    el.bg(hsla(theme.surfaces.elevated))
        .border(HAIR)
        .border_color(hsla(theme.surfaces.stroke))
        .shadow(elevation(theme))
}

/// The scrim under a modal: the window dimmed by the elevation's shade.
#[must_use]
pub fn scrim(theme: &Theme) -> Hsla {
    hsla_alpha(theme.elevation.shade, theme.elevation.scrim)
}

/// How loud a [`button`] is, Zed's button styles. One primary per surface; the rest are
/// secondary, or ghost where a ring would crowd a bar.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ButtonKind {
    /// The one action the surface is for: Zed's tinted accent, the accent's quiet ground inside
    /// its line under the text's words ([`tinted`]). Zed has no white or black solid.
    Primary,
    /// The press that removes or ends something for good (Remove a machine): Zed's tinted
    /// error ([`destructive`]), behind a confirm. Never a surface's only way on, and never
    /// beside a [`Self::Primary`].
    Destructive,
    /// Another way on: Zed's outlined, clear inside the lighter line, the heavier one under
    /// the pointer.
    Secondary,
    /// A way out (Cancel): Zed's subtle, words only until the pointer is on it.
    Ghost,
    /// A way aside (the other way in, Open in editor): the text's own tone with no pad and no
    /// fill, so its words start on the same edge as the text above them; underlined under
    /// the pointer.
    Link,
}

/// `el` tinted as the one action a surface is for, Zed's tinted accent: the accent's quiet
/// ground inside its line, the text's words on it.
///
/// Also what is on (a ticked box, an armed key, the send and stop square). The theme's tests
/// hold the words to AA on that ground over every plane.
pub fn tinted<E: Styled>(el: E, theme: &Theme) -> E {
    let s = theme.surfaces;
    el.bg(hsla(s.info_background))
        .border(HAIR)
        .border_color(hsla(s.info_border))
        .text_color(hsla(s.text))
}

/// [`tinted`] that answers the pointer, flat: its ground a shade stronger under the pointer
/// and stronger again while pressed, Zed's tinted hover.
pub fn tinted_pressable(el: gpui::Stateful<Div>, theme: &Theme) -> gpui::Stateful<Div> {
    let (hovered, pressed) = tinted_states(theme);
    eased(tinted(el, theme))
        .hover(move |el| el.bg(hsla(hovered)))
        .active(move |el| el.bg(hsla(pressed)))
}

/// The accent's quiet ground under the pointer and pressed: its ink moved [`alpha::FAINT`] and
/// [`alpha::DIM`] toward the accent's mark, and half as much more opacity where it is a wash.
fn tinted_states(theme: &Theme) -> (slopty_theme::Tint, slopty_theme::Tint) {
    let s = theme.surfaces;
    let ground = s.info_background;
    let deeper = |by: f32| slopty_theme::Tint {
        ink: ground.ink.mix(s.accent_fill, by),
        alpha: ground.alpha.saturating_add(slopty_theme::Tint::of(s.accent_fill, by).alpha / 2),
    };
    (deeper(alpha::FAINT), deeper(alpha::DIM))
}

/// `el` tinted as the one press that removes or ends something for good.
///
/// The error's quiet ground inside its line under the error's word, as Zed's tinted error
/// button is. The theme's tests hold the word to AA on that ground over every plane.
pub fn destructive<E: Styled>(el: E, theme: &Theme) -> E {
    let s = theme.surfaces;
    el.bg(hsla(s.error_background))
        .border(HAIR)
        .border_color(hsla(s.error_border))
        .text_color(hsla(s.error))
}

/// [`destructive`] that answers the pointer: its line takes the error's mark under the pointer
/// and while pressed, its ground and word unchanged.
pub fn destructive_pressable(el: gpui::Stateful<Div>, theme: &Theme) -> gpui::Stateful<Div> {
    let fill = theme.surfaces.error_fill;
    eased(destructive(el, theme))
        .hover(move |el| el.border_color(hsla(fill)))
        .active(move |el| el.border_color(hsla(fill)))
}

/// `el` drawn as a secondary button ([`ButtonKind::Secondary`]), Zed's outlined.
///
/// Clear at rest inside the lighter `stroke` line, the heavier `border` and the hover wash
/// under the pointer, the pressed one while held, so a press reads apart from a hover.
pub fn secondary(el: gpui::Stateful<Div>, theme: &Theme) -> gpui::Stateful<Div> {
    let s = theme.surfaces;
    eased(el)
        .border(HAIR)
        .border_color(hsla(s.stroke))
        .text_color(hsla(s.text))
        .hover(move |el| el.bg(hsla(s.hover)).border_color(hsla(s.border)))
        .active(move |el| el.bg(hsla(s.pressed)).border_color(hsla(s.border)))
}

/// A text button, the one every dialog, panel and empty state draws.
///
/// Four had been written by hand, and the secondary among them filled itself with `raised`,
/// which on the light theme's `canvas` is one step from `canvas` itself: "Add a window" and
/// the phone's "Paste" were words floating on a smudge. Every kind stands the density's
/// control height, so they line up side by side and fill a row. Its words are at the regular
/// weight, Zed's: the style, not the weight, says it is a button.
#[must_use]
pub fn button(
    theme: &Theme,
    id: &'static str,
    label: impl Into<SharedString>,
    kind: ButtonKind,
) -> gpui::Stateful<Div> {
    let s = theme.surfaces;
    let label = label.into();
    let el = div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .role(gpui::accesskit::Role::Button)
        .aria_label(label.clone())
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .h(px(theme.density.control))
        .when(kind != ButtonKind::Link, |el| el.px(px(theme.spacing.md)))
        .rounded(px(theme.radii.sm))
        .text_size(px(theme.roles().action.size))
        .cursor_pointer()
        .child(label);
    let el = match kind {
        ButtonKind::Primary => tinted_pressable(el, theme),
        ButtonKind::Destructive => destructive_pressable(el, theme),
        ButtonKind::Secondary => secondary(el, theme),
        ButtonKind::Ghost => eased(el)
            .text_color(hsla(s.text))
            .hover(move |el| el.bg(hsla(s.hover)).text_color(hsla(s.text)))
            .active(move |el| el.bg(hsla(s.pressed))),
        ButtonKind::Link => el.text_color(hsla(s.text)).hover(Styled::underline),
    };
    crate::a11y::tab_stop(el, s.focus)
}

/// The side of an [`icon_button`]: the density's button, Zed's 22 pt square round the
/// 16 pt icon.
///
/// It is never under the density's hit target, so a finger gets 44 pt round the same icon.
/// A strip that holds icon buttons in turn with something else sizes itself from it.
#[must_use]
pub const fn icon_button_side(theme: &Theme) -> f32 {
    theme.density.button.max(theme.density.hit)
}

/// A list row, Zed's list item: its density's height, starting on the edge grid
/// and ending 6 in ([`slopty_theme::Spacing::inset_trailing`]), its parts centred.
///
/// Its parts sit [`slopty_theme::Spacing::row_gap`] apart, Zed's `gap_1p5`. The menus draw
/// their rows from it, so one density switch moves them all.
/// The trailing end is usually an icon button or a count, which brings its own room.
#[must_use]
pub fn row(theme: &Theme) -> Div {
    div()
        .pl(px(theme.spacing.inset()))
        .pr(px(theme.spacing.inset_trailing()))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(theme.spacing.row_gap()))
        .h(px(theme.density.row))
}

/// How wide chrome draws its lines: [`stroke::LINE`], one point.
///
/// Every border chrome draws is this wide: a sheet's edge, a pane's sash, a rule under a bar, a
/// ring just inside a button. One point weighs the same at 1x and 2x, where half a point at
/// twice the share halves on a Retina screen (`docs/MEASUREMENTS.md`, "The structural line at
/// 1x and 2x"), so the line's shares are set for this width. A line that marks something (a
/// failed block's bar) is a stroke of its own.
pub const HAIR: gpui::Pixels = px(stroke::LINE);

/// [`HAIR`] for a quad painted by hand at `scale` device pixels a point.
///
/// Whole device pixels, never under one, as GPUI snaps a border. A painted box's edges round on
/// their own, so a width that is not whole device pixels would come out thinner or thicker by
/// where it fell.
#[must_use]
pub fn hair_painted(scale: f32) -> gpui::Pixels {
    let device = stroke::LINE.mul_add(scale, -0.5).ceil().max(1.0);
    px(device / scale)
}

/// The pad round the rows of a floating sheet (a menu, the palette's list, the inbox): Zed's
/// `p_1`, 4.
#[must_use]
pub const fn sheet_pad(theme: &Theme) -> f32 {
    theme.spacing.xs
}

/// A row inside a floating sheet padded by [`sheet_pad`].
///
/// A [`row`] whose fill (hover, selection) is rounded at [`slopty_theme::Radii::sm`], 4 inside
/// the sheet's 8, as Zed's menu entries are; its leading words still on the edge grid measured
/// from the sheet's own edge, and its trailing end a row's trailing inset in.
#[must_use]
pub fn sheet_row(theme: &Theme) -> Div {
    let pad = sheet_pad(theme);
    row(theme).pl(px(theme.spacing.inset() - pad)).rounded(px(theme.radii.sm))
}

/// `el` set in a type role ([`Theme::roles`]): its size, its line and its weight.
#[must_use]
pub fn typed<E: Styled>(el: E, role: slopty_theme::TypeRole) -> E {
    el.text_size(px(role.size)).line_height(px(role.line)).font_weight(FontWeight(role.weight))
}

/// A square button around one icon: a bar's actions, a tile's close and split.
///
/// Zed's icon button: the density's button square (22) at [`slopty_theme::Radii::sm`] round
/// the 16 pt icon in `icon_muted`, the hover wash under the pointer and the ink unchanged. `label`
/// is its accessible name and its hint, since the icon alone names nothing to a screen reader.
#[must_use]
pub fn icon_button(
    theme: &Theme,
    id: impl Into<SharedString>,
    icon: crate::icons::Symbol,
    label: &'static str,
) -> gpui::Stateful<Div> {
    let s = theme.surfaces;
    eased(square_icon(theme, id.into(), icon, label, s.icon_muted))
        .hover(move |el| el.bg(hsla(s.hover)))
}

/// The square an icon button is drawn in, its icon in `ink`, before its hover.
fn square_icon(
    theme: &Theme,
    id: SharedString,
    icon: crate::icons::Symbol,
    label: &'static str,
    ink: Rgb,
) -> gpui::Stateful<Div> {
    let s = theme.surfaces;
    let selector = id.to_string();
    let side = icon_button_side(theme);
    let el = div()
        .id(gpui::ElementId::Name(id))
        .debug_selector(move || selector)
        .role(gpui::accesskit::Role::Button)
        .aria_label(label)
        .flex_none()
        .size(px(side))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(theme.radii.sm))
        .cursor_pointer()
        .text_color(hsla(ink))
        .active(move |el| el.bg(hsla(s.pressed)))
        .child(crate::icons::Drawn::new(theme, icon).slot(px(theme.icon()), hsla(ink)));
    crate::a11y::tab_stop(el, s.focus)
}

/// A toggle that is its own short words, `face` ("Aa", "W", ".*"), in an icon button's square:
/// a find's match case, whole word and pattern, the way Xcode and the terminals label them.
///
/// A glyph for "case sensitive" or "regex" has to be learnt; the letters say it. The face is at
/// the caption size, in the secondary ink until on, when it takes the text
/// and the selected wash. Its name, said to a screen reader and in its
/// hint, is `label`.
#[must_use]
pub fn text_toggle(
    theme: &Theme,
    id: impl Into<SharedString>,
    face: &'static str,
    label: &'static str,
    on: bool,
) -> gpui::Stateful<Div> {
    let s = theme.surfaces;
    let id: SharedString = id.into();
    let selector = id.to_string();
    let ink = if on { s.text } else { s.text_secondary };
    let el = div()
        .id(gpui::ElementId::Name(id))
        .debug_selector(move || selector)
        .role(gpui::accesskit::Role::Button)
        .aria_label(label)
        .aria_toggled(if on {
            gpui::accesskit::Toggled::True
        } else {
            gpui::accesskit::Toggled::False
        })
        .flex_none()
        .size(px(icon_button_side(theme)))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(theme.radii.sm))
        .cursor_pointer()
        .font_family(theme.typography.ui_family.clone())
        .text_size(px(theme.typography.caption()))
        .text_color(hsla(ink))
        .active(move |el| el.bg(hsla(s.pressed)))
        .child(face);
    let el = crate::a11y::tab_stop(el, s.focus);
    if on {
        el.bg(hsla(s.selected))
    } else {
        eased(el).hover(move |el| el.bg(hsla(s.hover)).text_color(hsla(s.text)))
    }
}

/// The side of an empty state's mark, in points.
///
/// It holds a symbol at the page heading's size (`roles().page_heading`) at the
/// light weight and large scale, as the system's own empty states draw theirs.
pub const NOTICE_MARK: f32 = 28.0;

/// What a tile's body says when it has nothing to show, as one block in its middle.
///
/// A mark on its own, [`NOTICE_MARK`] square ([`notice_mark`]) with no plate under it, a line in
/// the task title role in the text's ink saying what is so, and under it an optional line at
/// the chrome size in `text_secondary` saying why or where. Where the state has one obvious
/// next step the caller adds it under them; its identity too.
///
/// The words keep to a measure of [`NOTICE_MEASURE`] ems where the tile is wide and to the
/// tile less its margins where it is narrow, wrapping and never cut: the block fits any room.
///
/// A remote window on its way, a file that cannot be opened here and an empty or missing
/// folder all say it this way. Before, the title was a footnote under its mark (12 pt in
/// `text_secondary`), and a file printed its summary alone ("binary, 2 MB"), each a lowercase
/// sentence adrift in the body.
#[must_use]
pub fn notice(
    theme: &Theme,
    mark: impl IntoElement,
    title: impl Into<SharedString>,
    detail: Option<SharedString>,
) -> Div {
    let s = &theme.surfaces;
    let roles = theme.roles();
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(theme.spacing.xs))
        .max_w_full()
        .px(px(theme.spacing.inset()))
        .font_family(theme.typography.ui_family.clone())
        .text_center()
        .child(
            div()
                .flex_none()
                .min_h(px(NOTICE_MARK))
                .mb(px(theme.spacing.xs))
                .flex()
                .items_center()
                .justify_center()
                .child(mark),
        )
        .child(
            typed(div(), roles.task_title)
                .max_w(px(theme.typography.ui_size * NOTICE_MEASURE))
                .text_color(hsla(s.text))
                .child(title.into()),
        )
        .children(detail.map(|detail| {
            typed(div(), roles.chrome)
                .max_w(px(theme.typography.ui_size * NOTICE_MEASURE))
                .text_color(hsla(s.text_secondary))
                .child(detail)
        }))
}

/// How wide a [`notice`]'s words run at most, in ems of the chrome's size: a short paragraph's
/// measure, so a wide tile does not stretch them to one long line.
pub const NOTICE_MEASURE: f32 = 26.0;

/// A [`notice`]'s mark: a kind's glyph across [`NOTICE_MARK`], in `text_muted`.
#[must_use]
pub fn notice_mark(theme: &Theme, icon: crate::icons::Symbol) -> Div {
    let ink = hsla(theme.surfaces.text_muted);
    crate::icons::Drawn::new(theme, icon).slot(px(NOTICE_MARK), ink)
}

thread_local! {
    /// When the last hint on screen went, for the next to know whether the group is warm.
    static HINT_LEFT: std::cell::Cell<Option<std::time::Instant>> =
        const { std::cell::Cell::new(None) };
}

/// The UI's monospace family: a port's number and the settings file are set in it. Paths in
/// chrome are context, in the UI face.
#[must_use]
pub fn mono_family(theme: &Theme) -> SharedString {
    theme.typography.mono_families.first().cloned().unwrap_or_default().into()
}

/// Point gpui-kit's theme at `theme`: mode, then the colours its widgets read.
pub fn sync(theme: &Theme, cx: &mut App) {
    let mode = match theme.variant() {
        Variant::Dark => ThemeMode::Dark,
        Variant::Light => ThemeMode::Light,
    };
    KitTheme::change(mode, None, cx);
    let s = &theme.surfaces;
    let kit = KitTheme::global_mut(cx);
    // Its fields, tooltips, editors and diffs set the bundled faces, as the chrome does.
    kit.font_family = theme.typography.ui_family.clone().into();
    kit.mono_font_family = mono_family(theme);
    let c = &mut kit.colors;
    c.background = hsla(s.ground);
    c.foreground = hsla(s.text);
    c.border = hsla(s.border);
    c.input = hsla(s.border);
    // A focused field shows its caret, not a ring: the accent spent on every focused field's
    // hairline made each form one more place the accent shouted.
    c.ring = hsla(s.border);
    // The caret and a field's selection are neutral, as the terminal's cursor and selection
    // are: colour is kept for meaning.
    c.caret = hsla(s.text);
    c.selection = hsla_alpha(s.text, alpha::TINT);
    // gpui-kit's hovers and wells are the same washes, so they ride on the plane under them.
    c.accent = hsla(s.hover);
    c.accent_foreground = hsla(s.text);
    c.muted = hsla(s.hover);
    c.muted_foreground = hsla(s.text_muted);
    c.secondary = hsla(s.hover);
    c.secondary_foreground = hsla(s.text);
    c.secondary_hover = hsla(s.selected);
    c.secondary_active = hsla(s.pressed);
    // gpui-kit's primary (its switches, checkboxes and radios) is the accent's mark, as a Zed
    // switch is on, given a little toward the content under the pointer and more when pressed.
    let content = theme.content();
    c.primary = hsla(s.accent_fill);
    c.primary_hover = hsla(s.accent_fill.mix(content, alpha::FAINT));
    c.primary_active = hsla(s.accent_fill.mix(content, alpha::DIM));
    c.primary_foreground = hsla(s.accent_ink);
    // Its primary button (a questionnaire's Next and Submit) is Zed's tinted accent, as
    // [`ButtonKind::Primary`] is: the accent's quiet ground under the text's words. Zed has no
    // solid primary.
    let (hovered, pressed) = tinted_states(theme);
    c.button_primary = hsla(s.info_background);
    c.button_primary_hover = hsla(hovered);
    c.button_primary_active = hsla(pressed);
    c.button_primary_foreground = hsla(s.text);
    c.link = hsla(s.accent);
    c.link_hover = hsla(s.accent);
    c.link_active = hsla(s.accent);
    c.popover = hsla(s.elevated);
    c.popover_foreground = hsla(s.text);
    // The bars and the tab rows on the ground, the side panel on the sidebar's plane, every
    // edge the one line; the shown tab a selected pill, as `workspace::tab_look` draws them.
    c.title_bar = hsla(s.ground);
    c.title_bar_border = hsla(s.stroke);
    c.status_bar = hsla(s.ground);
    c.status_bar_border = hsla(s.stroke);
    c.sidebar = hsla(s.sidebar);
    c.sidebar_foreground = hsla(s.text);
    c.sidebar_border = hsla(s.stroke);
    c.tab_bar = hsla(s.ground);
    c.tab = hsla(s.ground);
    c.tab_foreground = hsla(s.text_secondary);
    c.tab_active = hsla(s.selected.over(s.ground));
    c.tab_active_foreground = hsla(s.text);
    c.table_row_border = hsla(s.stroke);
    c.list = hsla(s.ground);
    c.list_hover = hsla(s.hover);
    c.list_active = hsla(s.selected);
    c.danger = hsla(s.error);
    c.success = hsla(s.success);
    c.warning = hsla(s.warn);
    c.scrollbar_thumb = hsla_alpha(s.text_muted, alpha::PRESSED);
    // gpui-kit's ring is a wider halo painted outside a focused field's border.
    kit.focus_ring = false;
    // The editor's gutter would take the input background (the panel), a grey band beside a
    // body on the content surface; it belongs to the body.
    let mut highlight = (*kit.highlight_theme).clone();
    highlight.style.editor_gutter_background = Some(hsla(theme.content()));
    kit.highlight_theme = Arc::new(highlight);
    // The components paint from the tokens, which a write to the colours here leaves as they
    // were: they follow the colours whole.
    kit.tokens = gpui_kit::component::ThemeTokens::from(&kit.colors);
    KitTheme::sync_base(cx);
    // `sync_base` reinstalls the Markdown defaults, so the code colouring goes on after it.
    TextViewDefaults::global(cx)
        .with_code_block_highlighter(crate::highlight::code_block(theme.clone()))
        .install(cx);
    // The components' own icons (a questionnaire's check, a field's chevron) are the chrome's
    // symbols too; one the chrome has no symbol for keeps the kit's drawing.
    let drawn = theme.clone();
    cx.set_global(gpui_kit::component::IconPainter(Rc::new(move |path, side, ink| {
        let symbol = crate::icons::kit_symbol(path)?;
        Some(crate::icons::symbol(&drawn, symbol, side, ink))
    })));
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use super::*;

    /// A size reads the one way everywhere (a file tile, a download, a picture): whole
    /// kilobytes, a tenth of a megabyte.
    #[test]
    fn a_size_reads_as_a_person_says_it() {
        assert_eq!(size_label(812), "812 B");
        assert_eq!(size_label(245_760), "240 KB");
        assert_eq!(size_label(1_536), "2 KB");
        assert_eq!(size_label(1_258_291), "1.2 MB");
    }

    /// The destructive button's word reads AA on its ground, over the work and over a float,
    /// in both variants.
    #[test]
    fn the_destructive_button_reads_in_every_state() {
        for variant in [Variant::Light, Variant::Dark] {
            let theme = Theme::new(variant);
            let s = theme.surfaces;
            for plane in [theme.content(), s.elevated] {
                let ratio = s.error.contrast(s.error_background.over(plane));
                assert!(ratio >= 4.5, "{variant:?}: the word reads {ratio:.2}");
            }
        }
    }

    /// One duration format: milliseconds under a second, a tenth under ten seconds unless it
    /// is zero, then whole seconds, minutes and seconds, hours and minutes.
    #[test]
    fn a_duration_reads_one_way() {
        let ms = std::time::Duration::from_millis;
        let cases = [
            (850, "850 ms"),
            (6_040, "6 s"),
            (6_250, "6.2 s"),
            (6_000, "6 s"),
            (9_990, "9.9 s"),
            (10_400, "10 s"),
            (35_000, "35 s"),
            (65_000, "1m 5s"),
            (192_000, "3m 12s"),
            (3_840_000, "1h 4m"),
        ];
        for (millis, said) in cases {
            assert_eq!(duration(ms(millis)), said, "{millis} ms");
        }
    }

    /// A ticking clock reads in whole seconds in the one duration form, from its first second.
    #[test]
    fn a_clock_ticks_in_whole_seconds() {
        let ms = std::time::Duration::from_millis;
        assert_eq!(clock(ms(300)), "1 s");
        assert_eq!(clock(ms(2_500)), "2 s");
        assert_eq!(clock(ms(9_999)), "9 s");
        assert_eq!(clock(ms(64_000)), "1m 4s");
        assert_eq!(clock(ms(3_720_000)), "1h 2m");
    }

    /// Under the touch density an icon button is a finger's 44 pt round the same icon, and the
    /// rows grow with it; under the compact one they are Zed's: a 22 pt button, 24 pt rows.
    #[test]
    fn density_sizes_the_targets_not_the_icons() {
        let mut theme = Theme::default();
        assert!((icon_button_side(&theme) - 22.0).abs() < f32::EPSILON, "compact: Zed's 22");
        assert!((theme.density.row - 28.0).abs() < f32::EPSILON, "Delta's row");
        theme.density = slopty_theme::Density::TOUCH;
        assert!(icon_button_side(&theme) >= 44.0, "touch: a finger's target");
        assert!(theme.density.row >= 44.0);
        assert!(theme.icon() < icon_button_side(&theme), "the icon stays its size");
    }

    /// A float is Zed's elevated surface: the elevated plane inside the lighter `stroke` line
    /// (`border.variant`) under the shadow. Its rows are Zed's list items: 24 pt, their parts
    /// 4 apart, rounded 4 inside the float's 8, and its groups parted by a plain rule.
    #[test]
    fn a_float_and_its_rows_are_zeds() {
        for variant in [Variant::Dark, Variant::Light] {
            let theme = Theme::new(variant);
            let s = theme.surfaces;
            let style = elevate(div(), &theme).style().clone();
            assert_eq!(style.background, Some(gpui::Fill::from(hsla(s.elevated))), "{variant:?}");
            assert_eq!(style.border_color, Some(hsla(s.stroke)), "{variant:?}: border.variant");
            assert!(style.box_shadow.is_some_and(|l| l.len() == 2), "{variant:?}: two layers");
            let mut sheet = sheet_row(&theme);
            let row = sheet.style();
            let radius = px(theme.radii.sm);
            assert_eq!(row.corner_radii.top_left, Some(radius.into()), "{variant:?}");
            assert_eq!(row.gap.width, Some(px(theme.spacing.row_gap()).into()), "{variant:?}: 6");
            assert_eq!(row.size.height, Some(px(28.0).into()), "{variant:?}: Delta's menu row");
        }
    }

    /// A painted line is whole device pixels, never under one, as GPUI snaps a border: one point
    /// at 1x, 2x and 3x.
    #[test]
    fn a_painted_line_is_one_point_in_whole_device_pixels() {
        for scale in [1.0_f32, 2.0, 3.0] {
            let device = f32::from(hair_painted(scale)) * scale;
            assert!((device - scale).abs() < 1e-4, "{scale}x: {device} device pixels");
        }
    }

    /// A tabular element carries `tnum` in the text style its children inherit, and nothing
    /// else: no other feature of the UI font is switched on or off with it.
    #[test]
    fn tabular_figures_set_tnum_on_the_text_style() {
        let mut el = tabular(div());
        let features = el.text_style().font_features.clone();
        assert_eq!(features.map(|f| f.0.as_ref().clone()), Some(vec![("tnum".to_owned(), 1)]));
        assert_eq!(tabular_figures(), tabular_figures(), "one value, cloned");
    }

    /// Chrome moves unless Reduce Motion is asked for, and then it lands at once.
    #[gpui::test]
    fn chrome_holds_still_under_reduce_motion(cx: &TestAppContext) {
        assert!(cx.update(|cx| motion(cx)), "motion by default");
        cx.update(|cx| cx.set_reduce_motion(true));
        assert!(!cx.update(|cx| motion(cx)), "still under Reduce Motion");
    }

    /// The two overlay sizes differ in both directions, and a list is the smaller of them: a
    /// size that is nearly another size is a size nobody chose.
    #[test]
    fn the_overlay_sizes_are_two_and_they_differ() {
        let (lw, lh) = Overlay::List.bounds();
        let (ew, eh) = Overlay::Editor.bounds();
        assert!(lw < ew && lh < eh, "a list is the smaller overlay");
        assert!(ew - lw >= 40.0 && eh - lh >= 40.0, "far enough apart to tell apart");
    }
}

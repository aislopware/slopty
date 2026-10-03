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
    Animation, AnimationExt as _, App, BoxShadow, Context, Div, FontFeatures, FontWeight, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled, Window, div, point, px,
};
use gpui_kit::base::text::TextViewDefaults;
use gpui_kit::component::{Theme as KitTheme, ThemeMode};
use slopty_theme::{Motion, Rgb, Theme, Typography, Variant, alpha};

use crate::colors::{hsla, hsla_alpha};

/// What a find bar says before anything is typed. The terminal and the file tile share it: the
/// same bar, the same word.
pub const FIND_PLACEHOLDER: &str = "Find";

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

/// A sheet's curve: [`Motion::drawer`], a phone's palette sheet, the iPad's drawer.
pub fn drawer() -> impl Fn(f32) -> f32 {
    move |t| Motion::DEFAULT.drawer.at(t)
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

/// `el` fading in over [`FADE`] the first time it is drawn under `id`, eased out.
///
/// Under Reduce Motion it lands at once. Wrap the root of an overlay, a menu or a popover in
/// it. Only opacity moves, so the element takes the pointer and the keys from its first frame.
pub fn fade_in<E>(el: E, id: impl Into<gpui::ElementId>, cx: &App) -> gpui::AnyElement
where
    E: IntoElement + Styled + 'static,
{
    if !motion(cx) {
        return el.into_any_element();
    }
    el.with_animation(id, Animation::new(FADE).with_easing(ease_out()), Styled::opacity)
        .into_any_element()
}

/// How long a move takes and how it lands: one of [`Motion`]'s durations with its curve.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pace {
    /// An overlay, a menu, a toast or a pill arriving: [`Motion::fade`], eased out.
    Fade,
    /// A fill, a width or a fold settling: [`Motion::settle`], eased out.
    Settle,
    /// A sheet, a drawer, the composer turning into an approval: [`Motion::sheet`] on the
    /// drawer's curve.
    Sheet,
    /// An overlay, a menu or a popover leaving: [`Motion::exit`], eased out, shorter than its
    /// way in, since what is dismissed is no longer looked at.
    Exit,
}

impl Pace {
    /// How long it takes.
    #[must_use]
    pub const fn duration(self) -> std::time::Duration {
        let m = Motion::DEFAULT;
        match self {
            Self::Fade => m.fade,
            Self::Settle => m.settle,
            Self::Sheet => m.sheet,
            Self::Exit => m.exit,
        }
    }

    /// The curve it follows.
    #[must_use]
    pub const fn curve(self) -> slopty_theme::Curve {
        let m = Motion::DEFAULT;
        match self {
            Self::Fade | Self::Settle | Self::Exit => m.ease_out,
            Self::Sheet => m.drawer,
        }
    }

    /// The one-shot GPUI animation on this pace. Gate it on [`motion`]: under Reduce Motion
    /// the caller draws the end state instead.
    #[must_use]
    pub fn animation(self) -> Animation {
        let curve = self.curve();
        Animation::new(self.duration()).with_easing(move |t| curve.at(t))
    }
}

/// `el` arriving from `from` points below its place (above, when negative) as it fades in
/// from clear, on `pace`, the first time it is drawn under `id`.
///
/// Opacity and a small travel are all that move, never the size of text: the palette rising
/// 4 pt, a menu dropping 4 pt from its button, a toast, a pill. Under Reduce Motion it is drawn
/// in place and opaque at once, with no fade either. `el` sits in the flow: it is moved as a
/// `relative` element by its `top`, so nothing round it shifts while it travels, and an
/// absolutely placed element keeps its own placement by being wrapped in one that is not.
pub fn slide_fade<E>(
    el: E,
    id: impl Into<gpui::ElementId>,
    from: f32,
    pace: Pace,
    cx: &App,
) -> gpui::AnyElement
where
    E: IntoElement + Styled + 'static,
{
    if !motion(cx) {
        return el.into_any_element();
    }
    el.relative()
        .with_animation(id, pace.animation(), move |el, t| el.top(px(from * (1.0 - t))).opacity(t))
        .into_any_element()
}

/// `el`'s fill and hairline following its states as a native control's do.
///
/// Into a hover or a press at once ([`Motion::hover`]), and back to rest over
/// [`Motion::unhover`], eased out. Only colours ease; the state itself, and layout, change at once,
/// and under Reduce Motion GPUI applies every change at once. Every kit control that answers the
/// pointer wears it, so the durations live here and nowhere else.
///
/// Not on the rows of a list GPUI composites as a scroll layer (the navigator's): a fill easing
/// out under rows that scroll past the pointer repaints the layer, and measured headless it
/// composited 2 of 30 scroll frames where the list composites all 30
/// (`nav_list::a_scroll_of_the_navigator_composites_its_layer`).
pub fn eased<E: gpui::StatefulInteractiveElement>(el: E) -> E {
    let m = Motion::DEFAULT;
    el.transition(gpui::StateTransition::new(m.unhover).enter(m.hover).with_easing(ease_out()))
}

/// How long a dismissed overlay stays drawn while it leaves.
///
/// [`Pace::Exit`], or `None` under Reduce Motion, where it goes on the frame it is dismissed.
/// Its owner keeps drawing it
/// through [`fade_out`] for this long and then drops it; the keyboard has already gone back.
#[must_use]
pub fn exit_time(cx: &App) -> Option<std::time::Duration> {
    motion(cx).then(|| Pace::Exit.duration())
}

/// `el` leaving: fading out where it is over [`Pace::Exit`], with no travel.
///
/// It fades the first time it is drawn under `id`. It takes no pointer while it goes: a blocker
/// over it holds the clicks within its own bounds, so a row in a fading menu never runs and nothing
/// under it is hit by a press aimed at the menu. Its owner draws it only for [`exit_time`].
pub fn fade_out<E>(el: E, id: impl Into<gpui::ElementId>) -> gpui::AnyElement
where
    E: IntoElement + Styled + gpui::ParentElement + 'static,
{
    el.child(div().absolute().inset_0().occlude())
        .with_animation(id, Pace::Exit.animation(), |el, t| el.opacity(1.0 - t))
        .into_any_element()
}

/// How far a gpui-kit field at the medium size sets its text in from its edge, in points,
/// with its frame or without it (`Size::Medium.input_px()`).
///
/// What has to line up with a field's text starts this far in: the approval that takes the
/// composer's place, the foot under the composer's field. The approval sat at the shell's pad
/// while the field's text sat this much further in, so the morph from one to the other jumped.
pub const FIELD_INSET: f32 = 10.0;

/// The sign of lines taken away: the figure dash, as wide as a digit, so a column of sizes
/// keeps its figures on one grid, as `MonoCode` sets `+25 ‒15`.
pub const REMOVED_SIGN: &str = "\u{2012}";

/// What parts the two sides of a size: a thin space, closer than a word's.
pub const SIDES_APART: &str = "\u{2009}";

/// A diff's size as words, `+12 ‒3`: the side that is zero left out, and nothing for no
/// change. For a line of plain text (a fold's summary, a tool's facts); a readout draws
/// [`changes`].
#[must_use]
pub fn changes_text(added: u32, removed: u32) -> Option<String> {
    match (added, removed) {
        (0, 0) => None,
        (a, 0) => Some(format!("+{a}")),
        (0, r) => Some(format!("{REMOVED_SIGN}{r}")),
        (a, r) => Some(format!("+{a}{SIDES_APART}{REMOVED_SIGN}{r}")),
    }
}

/// A diff's size, `+12 ‒3`: only the signs in the diff's tones, the figures in
/// `text_secondary` and tabular, the side that is zero left out, the two sides a thin space
/// apart. `None` for no change.
///
/// The one way a count of changed lines is drawn: in a tile's header, a diff's head, a fold, a
/// navigator row and the status bar. A figure all in red read as an error, and a red "‒0" as
/// an error about nothing. The caller sets the size and adds an identity and a spoken label.
///
/// Its parts are text, the thin space between the sides included, so they take the chrome's
/// zoom from the size the caller sets.
#[must_use]
pub fn changes(theme: &Theme, added: u32, removed: u32) -> Option<Div> {
    let s = &theme.surfaces;
    let side = |sign: &'static str, tone: Rgb, n: u32| {
        (n > 0).then(|| {
            div()
                .flex()
                .child(div().text_color(hsla(tone)).child(sign))
                .child(SharedString::from(n.to_string()))
        })
    };
    (added > 0 || removed > 0).then(|| {
        tabular(div())
            .flex_none()
            .flex()
            .items_center()
            .whitespace_nowrap()
            .text_color(hsla(s.text_secondary))
            .children(side("+", s.success, added))
            .when(added > 0 && removed > 0, |el| el.child(SIDES_APART))
            .children(side(REMOVED_SIGN, s.error, removed))
    })
}

/// The dot between two facts on one line ("Default · Opus 5.5", a bar's readouts): a middle
/// dot in the separator's faint ink, so it parts the facts without reading as one. The caller's
/// gap spaces it.
#[must_use]
pub fn separator(theme: &Theme) -> Div {
    div().flex_none().text_color(crate::palette::separator_ink(theme)).child("\u{b7}")
}

/// A pill's height at zoom 1, in points: T3's badge (`h-5`), a notch over Linear's 18.
///
/// Fixed rather than grown from a pad round the text, so it sits centred in a 27 pt header with
/// room above and below. Padded, the agent's pill stood 23 pt tall and touched the hairline.
pub const PILL_HEIGHT: f32 = 20.0;

/// A pill's shape without its fill, at the chrome's zoom `k`.
///
/// [`PILL_HEIGHT`] tall, the text centred on it at `small()`, `spacing.sm` at each end, a
/// capsule: a pill says a state, and every reference draws a state as a capsule, where a 4 pt
/// box at this height read as a 2018 tag or a button. A header's words that act (Take, Mute,
/// the hooks' offer) wear it bare, so they stand as tall as the state's [`pill`] beside them
/// and their hover takes the same capsule. Key caps keep their 4 pt corners: they are keys.
#[must_use]
pub fn pill_frame(theme: &Theme, k: f32) -> Div {
    div()
        .flex()
        .items_center()
        .h(px(PILL_HEIGHT * k))
        .gap(px(theme.spacing.xs * k))
        .px(px(theme.spacing.sm * k))
        .rounded(px(theme.radii.full))
        .overflow_hidden()
        .whitespace_nowrap()
        .text_size(px(theme.typography.small() * k))
}

/// A state's pill at the chrome's zoom `k`: [`pill_frame`] filled with `tone` at
/// [`pill_fill`], its words in `tone` at the medium weight.
///
/// A header holds one of these at most, the state's (an agent waiting, working), so it is the
/// one shape there that stands out. The caller adds the identity, the role and the words.
#[must_use]
pub fn pill(theme: &Theme, tone: Rgb, k: f32) -> Div {
    pill_frame(theme, k)
        .bg(hsla_alpha(tone, pill_fill(theme)))
        .text_color(hsla(tone))
        .font_weight(FontWeight(Typography::MEDIUM_WEIGHT))
}

/// How strongly a pill's tone fills it: [`alpha::FAINT`], or [`alpha::FAINT_ON_PAPER`] in
/// light, where a saturated tone reads heavier on white.
#[must_use]
pub const fn pill_fill(theme: &Theme) -> f32 {
    match theme.variant() {
        Variant::Dark => alpha::FAINT,
        Variant::Light => alpha::FAINT_ON_PAPER,
    }
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

/// The first line of `text` with something on it, trimmed.
#[must_use]
pub fn first_line(text: &str) -> &str {
    text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default()
}

/// How large an overlay grows on a desktop.
///
/// A phone gets whatever the margins leave. Two sizes, because a list of commands and a file
/// being edited want different room, and a third would be a size nobody could name.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Overlay {
    /// A list typed at: the command palette, the pickers.
    List,
    /// A document edited: the settings file.
    Editor,
}

impl Overlay {
    /// Width and height ceilings, in points.
    #[must_use]
    pub const fn bounds(self) -> (f32, f32) {
        match self {
            Self::List => (560.0, 520.0),
            Self::Editor => (640.0, 720.0),
        }
    }
}

/// The floating elevation's shadow, as GPUI draws it: a tight contact layer and a soft one, from
/// [`slopty_theme::Elevation::shadow`], and in dark the lit top edge
/// ([`slopty_theme::Rim::float`]).
#[must_use]
pub fn elevation(theme: &Theme) -> Vec<BoxShadow> {
    let e = &theme.elevation;
    let drop = e.shadow.iter().map(|&layer| drop_shadow(theme, layer));
    let edge = e.rim.float.map(|a| rim(theme, a, theme.hair()));
    drop.chain(edge).collect()
}

/// `el` on the resting elevation.
///
/// For a raised thing that keeps its own fill and hairline (the composer and the tray over
/// it): the rim ([`slopty_theme::Rim::rest`]) and, in light, a
/// contact shadow ([`slopty_theme::Elevation::rest`]). A stack cut into parts passes `first`
/// and `last` as [`card_part`] does, so only the lit edge's part takes the rim and only the
/// last part casts the contact. `el` wears a hairline border; the rim lands just inside it.
#[must_use]
pub fn rests<E: Styled>(el: E, theme: &Theme, first: bool, last: bool) -> E {
    el.shadow(rest_layers(theme, theme.hair(), first, last))
}

/// The resting elevation's layers for one part of a stack whose border is `border` wide.
fn rest_layers(theme: &Theme, border: f32, first: bool, last: bool) -> Vec<BoxShadow> {
    let e = &theme.elevation;
    let contact = e.rest.filter(|_| last).map(|layer| drop_shadow(theme, layer));
    let lit = if e.rim.top { first } else { last };
    let edge = lit.then(|| rim(theme, e.rim.rest, border));
    contact.into_iter().chain(edge).collect()
}

/// One layer of the shade falling under a surface.
fn drop_shadow(theme: &Theme, layer: slopty_theme::Shadow) -> BoxShadow {
    BoxShadow {
        color: hsla_alpha(theme.elevation.shade, layer.alpha),
        offset: point(px(0.0), px(layer.y)),
        blur_radius: px(layer.blur),
        spread_radius: px(0.0),
        inset: false,
    }
}

/// The rim at `alpha`: a point of the rim's ink inside the lit edge, past a border `border`
/// wide.
///
/// GPUI paints an inset shadow under the element's border, so the rim reaches the border and a
/// point in: the border covers the first part and the point shows, just inside the edge.
fn rim(theme: &Theme, alpha: f32, border: f32) -> BoxShadow {
    let r = theme.elevation.rim;
    let depth = border + RIM_DEPTH;
    BoxShadow {
        color: hsla_alpha(r.ink, alpha),
        offset: point(px(0.0), px(if r.top { depth } else { -depth })),
        blur_radius: px(0.0),
        spread_radius: px(0.0),
        inset: true,
    }
}

/// How far a rim reaches in past its surface's border: one point.
const RIM_DEPTH: f32 = 1.0;

/// A card: what rests on a plane and holds a thing of its own (a board's task, a settings
/// group, an expanded tool group, a question's option), on the resting elevation.
///
/// `radii.md`, the rim, and in light the floating surface's white with a hairline round it
/// and a contact shadow, so it reads on the quiet well under it ([`well`]). In dark it is the
/// hover wash, a step over whatever plane it rests on (+4.7 L on the content, the floating
/// step's own), with its lit top edge, as coss's cards are; so a card on a sheet still rises
/// from the sheet. Under Increase Contrast its hairline is drawn in dark too. The caller adds
/// the identity and pads it.
#[must_use]
pub fn card(theme: &Theme) -> Div {
    card_part(theme, true, true)
}

/// One part of a [`card`] cut across its width.
///
/// For a card whose rows are each a child of their own (a settings group, so finding a row
/// scrolls to it): `first` takes the top corners
/// and the top edge, `last` the bottom ones. In order the parts read as one card.
#[must_use]
pub fn card_part(theme: &Theme, first: bool, last: bool) -> Div {
    let light = theme.variant() == Variant::Light;
    let ringed = light || theme.contrast == slopty_theme::Contrast::Increased;
    let r = px(theme.radii.md);
    let border = if ringed { theme.hair() } else { 0.0 };
    div()
        .map(|el| {
            if light {
                el.bg(hsla(theme.surfaces.elevated))
            } else {
                el.bg(hsla(theme.surfaces.hover))
            }
        })
        .when(first, |el| el.rounded_t(r))
        .when(last, |el| el.rounded_b(r))
        .when(ringed, |el| {
            el.border_x(hair(theme))
                .when(first, |el| el.border_t(hair(theme)))
                .when(last, |el| el.border_b(hair(theme)))
                .border_color(hsla(theme.surfaces.border))
        })
        .shadow(rest_layers(theme, border, first, last))
}

/// How far a segmented control's track holds its options in from its edge.
pub const TRACK_PAD: f32 = 2.0;

/// A segmented control's thumb at `bounds`, painted as a [`card`] is drawn.
///
/// The floating surface on the resting elevation (its rim, and in light its hairline and contact),
/// at the radius nested in the track's `radii.sm` past [`TRACK_PAD`]. The selection plate paints it
/// as it slides from option to option.
pub fn paint_thumb(theme: &Theme, bounds: gpui::Bounds<gpui::Pixels>, window: &mut Window) {
    let radius = gpui::Corners::all(px(thumb_radius(theme)));
    let ringed =
        theme.variant() == Variant::Light || theme.contrast == slopty_theme::Contrast::Increased;
    let border = if ringed { hair_painted(theme, window.scale_factor()) } else { px(0.0) };
    let layers = rest_layers(theme, f32::from(border), true, true);
    window.paint_drop_shadows(bounds, radius, &layers);
    let quad = gpui::fill(bounds, hsla(theme.surfaces.elevated)).corner_radii(radius);
    window.paint_quad(if ringed {
        quad.border_widths(border).border_color(hsla(theme.surfaces.border))
    } else {
        quad
    });
    window.paint_inset_shadows(bounds, radius, &layers);
}

/// A segmented thumb's radius: concentric with its track's.
#[must_use]
pub fn thumb_radius(theme: &Theme) -> f32 {
    slopty_theme::Radii::nested(theme.radii.sm, 0.0, TRACK_PAD)
}

/// The ground cards rest on.
///
/// `band` in light, a hair under white, so a white card reads as the one thing raised
/// (`HeroUI`'s background under its surfaces, Linear's content under its elevated). In dark the
/// plane itself, which the cards already rise from.
#[must_use]
pub fn well<E: Styled>(el: E, theme: &Theme) -> E {
    if theme.variant() == Variant::Light { el.bg(hsla(theme.surfaces.band)) } else { el }
}

/// `el` lifted off the chrome: the `elevated` surface, the `border` hairline, the shadow and,
/// in dark, the lit top edge.
///
/// Everything that floats wears it: a dialog, the palette, a menu, a popover, a hint, a find
/// bar, a pill over a body. The caller keeps its own radius: `radii.lg` for a sheet (a dialog,
/// a menu, the inbox, a toast), the control's own for a hint or a pill.
///
/// A floating layer on `panel` sat below the content it covered (darker in dark, grey on white
/// in light), so it read as a hole, not a sheet.
#[must_use]
pub fn elevate<E: Styled>(el: E, theme: &Theme) -> E {
    el.bg(hsla(theme.surfaces.elevated))
        .border(hair(theme))
        .border_color(hsla(theme.surfaces.border))
        .shadow(elevation(theme))
}

/// The scrim under a modal: the window dimmed by the elevation's shade.
#[must_use]
pub fn scrim(theme: &Theme) -> Hsla {
    hsla_alpha(theme.elevation.shade, theme.elevation.scrim)
}

/// Where every overlay's top sits, as a share of the window's height below its safe area.
///
/// The palette, the pickers, the settings and the add-worker dialog open at one place, so
/// moving from one to the next does not make the eye hunt for it.
pub const MODAL_ANCHOR: f32 = 0.2;

/// The layer an overlay is laid out on: the dialog [`MODAL_ANCHOR`] down the window, centred.
///
/// Near the top so a phone's keyboard, which rises from the bottom, covers fewer of its rows;
/// under the window's safe area, so a phone's status bar and Dynamic Island never sit on it.
/// A column, so the dialog in it is laid out along the height it has: with a phone's keyboard
/// up, that is less than the dialog's ceiling.
///
/// Nothing dims the window: a list typed at (the palette, a picker) floats over the work, as
/// Zed's and Warp's do, and is gone with a keystroke. A modal takes [`backdrop`].
///
/// The caller adds the identity, the key handling and the dismiss on a click through.
#[must_use]
pub fn anchor(theme: &Theme, window: &Window) -> Div {
    let safe = window.insets().effective();
    let height = f32::from(window.viewport_size().height);
    div()
        .absolute()
        .inset_0()
        .flex()
        .flex_col()
        .items_center()
        .px(px(theme.spacing.md))
        .pt(px(height * MODAL_ANCHOR) + safe.top)
}

/// The [`anchor`] under the [`scrim`]: for a modal that holds the window until it is answered
/// (the settings, adding a worker).
#[must_use]
pub fn backdrop(theme: &Theme, window: &Window) -> Div {
    anchor(theme, window).bg(scrim(theme))
}

/// The shell every overlay wears: the floating radius, [`elevate`]d, the UI font.
///
/// `min_w_0` so an unwrapped title cannot hold the box wider than a phone, and `min_h_0` so it
/// gives up height to what is under the [`backdrop`] (a phone's keyboard and key bar) rather
/// than run under it: its list scrolls, and its field and its foot stay in view.
///
/// The caller adds the identity, the accessibility role and label, and the children.
#[must_use]
pub fn dialog(theme: &Theme, size: Overlay) -> Div {
    let (w, h) = size.bounds();
    elevate(div(), theme)
        .w_full()
        .min_w_0()
        .max_w(px(w))
        .max_h(px(h))
        .min_h_0()
        .mb(px(theme.spacing.xl))
        .flex()
        .flex_col()
        .rounded(px(theme.radii.lg))
        .text_size(px(theme.typography.ui_size))
        .font_family(theme.typography.ui_family.clone())
        .text_color(hsla(theme.surfaces.text))
        .on_mouse_down(gpui::MouseButton::Left, |_ev, _window, cx| cx.stop_propagation())
}

/// A dialog's or a panel's title: the title size at the strong weight, in `text`.
///
/// The strong weight is for titles like this one and for headings; a row, a tab or a name that
/// has to stand out takes the medium weight.
#[must_use]
pub fn title(theme: &Theme, text: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(theme.typography.title()))
        .font_weight(FontWeight(Typography::STRONG_WEIGHT))
        .text_color(hsla(theme.surfaces.text))
        .child(text.into())
}

/// How loud a [`button`] is. One primary per surface; the rest are secondary, or ghost where
/// a fill would crowd a bar.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ButtonKind {
    /// The one action the surface is for: the neutral [`solid`], white on dark and near-black
    /// on light, as `MonoCode`'s stop and Commit buttons are. Never the accent: green says
    /// "live" or "done", not "press here".
    Primary,
    /// Another way on: a neutral fill, the hover step, with no hairline. A row of bordered
    /// boxes beside the primary read as a form.
    Secondary,
    /// A way out (Cancel): text only until the pointer is on it.
    Ghost,
    /// A way aside (the other way in, Open in editor): the text's own tone with no pad and no
    /// fill, so its words start on the same edge as the text above them; underlined under
    /// the pointer.
    Link,
}

/// `el` filled with the neutral solid, its words in the solid's ink: a primary button, the
/// send and stop disc, a ticked box, a switch that is on, a key that is armed.
///
/// One per surface at most: two solids side by side leave nothing to say which leads.
pub fn solid<E: Styled>(el: E, theme: &Theme) -> E {
    let s = theme.surfaces;
    el.bg(hsla(s.solid)).text_color(hsla(s.solid_ink))
}

/// [`solid`] that answers the pointer, with its finish.
///
/// Under the pointer the solid gives a little toward the content, and while pressed more, its point
/// gone and a shade inside its top, so it presses in as a native button does
/// ([`slopty_theme::Finish`]).
pub fn solid_pressable(el: gpui::Stateful<Div>, theme: &Theme) -> gpui::Stateful<Div> {
    let (hovered, pressed) = solid_states(theme);
    let (rest, held) = solid_finish(theme);
    eased(solid(el, theme))
        .shadow(rest)
        .hover(move |el| el.bg(hsla(hovered)))
        .active(move |el| el.bg(hsla(pressed)).shadow(held))
}

/// The solid's finish at rest and held: its point and contact, then the shade pressed in.
fn solid_finish(theme: &Theme) -> (Vec<BoxShadow>, Vec<BoxShadow>) {
    let f = theme.elevation.finish;
    let point_in = |ink: Rgb, a: f32, top: bool| BoxShadow {
        color: hsla_alpha(ink, a),
        offset: point(px(0.0), px(if top { RIM_DEPTH } else { -RIM_DEPTH })),
        blur_radius: px(0.0),
        spread_radius: px(0.0),
        inset: true,
    };
    let rest = f
        .contact
        .map(|layer| drop_shadow(theme, layer))
        .into_iter()
        .chain([point_in(f.ink, f.alpha, f.top)])
        .collect();
    (rest, vec![point_in(theme.elevation.shade, f.pressed, true)])
}

/// The solid under the pointer and pressed: given [`alpha::FAINT`] and [`alpha::DIM`] toward
/// the content. The theme's tests hold the solid's ink to AA on the pressed one.
fn solid_states(theme: &Theme) -> (Rgb, Rgb) {
    let (solid, content) = (theme.surfaces.solid, theme.content());
    (solid.mix(content, alpha::FAINT), solid.mix(content, alpha::DIM))
}

/// `el` drawn as a secondary button ([`ButtonKind::Secondary`]): a step past its plane at rest,
/// another under the pointer, a third while held.
///
/// At rest the hover wash with a hairline just inside its edge and the text's own ink; under the
/// pointer the selected wash; held, the pressed one, so a press reads apart from a hover.
///
/// The hairline is what makes it a button on a card (a board's card, this Mac's checklist):
/// there the fill alone is near the card's own tone and the button read as words.
pub fn secondary(el: gpui::Stateful<Div>, theme: &Theme) -> gpui::Stateful<Div> {
    let s = theme.surfaces;
    let mut layers = vec![ring_inside(theme)];
    layers.push(rim(theme, theme.elevation.rim.rest, theme.hair()));
    eased(el)
        .bg(hsla(s.hover))
        .shadow(layers)
        .text_color(hsla(s.text))
        .hover(move |el| el.bg(hsla(s.selected)))
        .active(move |el| el.bg(hsla(s.pressed)))
}

/// `el` drawn as the selected row of a list, live while that list has the keyboard.
///
/// `focused`: the selected wash with a hairline ring just inside its edge. A list without the
/// keyboard keeps its selection at the hover's wash, with no ring, so only one list in the window
/// shows a live selection (Apple's key and non-key selection, in Slopty's neutrals).
///
/// The ring lets a selection read on a surface whose tone sits near the fill (a menu, a
/// selected row on the bars) and hold its shape on any background. A hover is the wash alone.
pub fn selected<E: Styled>(el: E, theme: &Theme, focused: bool) -> E {
    if focused {
        el.bg(hsla(theme.surfaces.selected)).shadow(vec![ring_inside(theme)])
    } else {
        el.bg(hsla(theme.surfaces.hover))
    }
}

/// A hairline ring just inside an element's edge, in the border's tone: it holds a shape on a
/// surface whose tone sits near its fill, and takes no room.
fn ring_inside(theme: &Theme) -> BoxShadow {
    BoxShadow {
        color: hsla(theme.surfaces.border),
        offset: point(px(0.0), px(0.0)),
        blur_radius: px(0.0),
        spread_radius: hair(theme),
        inset: true,
    }
}

/// A text button, the one every dialog, panel and empty state draws.
///
/// Four had been written by hand, and the secondary among them filled itself with `raised`,
/// which on the light theme's `canvas` is one step from `canvas` itself: "Add a window" and
/// the phone's "Paste" were words floating on a smudge. Every kind stands the density's
/// control height, so they line up side by side and fill a row. Its words are at the medium
/// weight: a button is an action, and its label reads as one against the prose round it.
#[must_use]
pub fn button(
    theme: &Theme,
    id: &'static str,
    label: &'static str,
    kind: ButtonKind,
) -> gpui::Stateful<Div> {
    let s = theme.surfaces;
    let el = div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .role(gpui::accesskit::Role::Button)
        .aria_label(label)
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .h(px(theme.density.control))
        .when(kind != ButtonKind::Link, |el| el.px(px(theme.spacing.md)))
        .rounded(px(theme.radii.sm))
        .text_size(px(theme.typography.ui_size))
        .font_weight(FontWeight(Typography::MEDIUM_WEIGHT))
        .cursor_pointer()
        .child(label);
    let el = match kind {
        ButtonKind::Primary => solid_pressable(el, theme),
        ButtonKind::Secondary => secondary(el, theme),
        ButtonKind::Ghost => eased(el)
            .text_color(hsla(s.text_secondary))
            .hover(move |el| el.bg(hsla(s.hover)).text_color(hsla(s.text)))
            .active(move |el| el.bg(hsla(s.pressed))),
        ButtonKind::Link => el.text_color(hsla(s.text)).hover(Styled::underline),
    };
    crate::a11y::tab_stop(el, s.accent)
}

/// The side of an [`icon_button`] at zoom 1: the large icon size and a small pad round it.
///
/// It is never under the density's hit target, so a finger gets 44 pt round the same icon.
/// A strip that holds icon buttons in turn with something else sizes itself from it.
#[must_use]
pub fn icon_button_side(theme: &Theme) -> f32 {
    2.0_f32.mul_add(theme.spacing.xs, theme.typography.icon_large()).max(theme.density.hit)
}

/// How many lines a list row holds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Row {
    /// A title alone: a worker's header, a palette or menu row, a row of *Needs you*.
    One,
    /// A title over its meta line: a tile in the navigator, an inbox entry.
    Two,
}

impl Row {
    /// The row's height under the theme's density.
    #[must_use]
    pub const fn height(self, theme: &Theme) -> f32 {
        match self {
            Self::One => theme.density.row,
            Self::Two => theme.density.row_two_line,
        }
    }
}

/// A list row: its density's height, starting on the edge grid ([`inset_x`]) and ending
/// half as far in ([`slopty_theme::Spacing::inset_trailing`]), its parts centred.
///
/// Its parts sit a base unit apart. The navigator, the palette, the inbox and the menus draw
/// their rows from it, so one density switch moves them all. The trailing end is usually an
/// icon button or a count, which brings its own room.
#[must_use]
pub fn row(theme: &Theme, lines: Row) -> Div {
    div()
        .pl(px(theme.spacing.inset()))
        .pr(px(theme.spacing.inset_trailing()))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(theme.spacing.sm))
        .h(px(lines.height(theme)))
}

/// How wide chrome draws a hairline: [`Theme::hair`], one device pixel, or a full point under
/// Increase Contrast.
///
/// Every border chrome draws is this wide: a sheet's edge, a pane's divider, a rule under a bar,
/// a ring just inside a button. A full point was two device pixels on a Retina screen, so the
/// chrome read ruled; the hairline shares are set for this width. A line that marks something
/// (the focus line, a failed block's bar) is a stroke of its own, not a hairline.
#[must_use]
pub const fn hair(theme: &Theme) -> gpui::Pixels {
    px(theme.hair())
}

/// [`hair`] for a quad painted by hand at `scale` device pixels a point.
///
/// Whole device pixels, never under one, as GPUI snaps a border. A painted box's edges round on
/// their own, so half a point at 3x would come out one pixel or two by where it fell.
#[must_use]
pub fn hair_painted(theme: &Theme, scale: f32) -> gpui::Pixels {
    let device = theme.hair().mul_add(scale, -0.5).ceil().max(1.0);
    px(device / scale)
}

/// A horizontal rule: a hairline in `tint` across its parent, taking its own width in the flow.
///
/// Drawn as a border rather than a box of the hairline's height, since GPUI snaps a border to
/// at least one device pixel where a box half a point tall rounds to nothing on a 1x screen.
#[must_use]
pub fn rule(theme: &Theme, tint: slopty_theme::Tint) -> Div {
    div().flex_none().w_full().border_t(hair(theme)).border_color(hsla(tint))
}

/// A rule parting a list's or a menu's groups.
///
/// A [`rule`] in `border_subtle` that fades out over its last
/// [`spacing.xl`](slopty_theme::Spacing::xl) at each end, a base unit above and below it, so it
/// reads as a pause between groups rather than a line drawn across (Raycast's list separators). The
/// fade is GPUI's per-pixel `edge_fade` on the rule alone, never a gradient wash.
#[must_use]
pub fn list_rule(theme: &Theme) -> gpui::AnyElement {
    div()
        .flex_none()
        .w_full()
        .py(px(theme.spacing.xs))
        .child(gpui::edge_fade(
            rule(theme, theme.surfaces.border_subtle),
            gpui::EdgeFade::x(px(theme.spacing.xl)),
        ))
        .into_any_element()
}

/// A vertical rule: [`rule`] standing up, the height of its parent.
#[must_use]
pub fn rule_v(theme: &Theme, tint: slopty_theme::Tint) -> Div {
    div().flex_none().h_full().border_l(hair(theme)).border_color(hsla(tint))
}

/// The pad round the rows of a floating sheet (a menu, the palette's list, the inbox).
///
/// Inside its hairline: the sheet's radius less its rows' and the hairline, so a row's corner
/// shares the sheet's centre (6 inside 12).
#[must_use]
pub fn sheet_pad(theme: &Theme) -> f32 {
    theme.radii.lg - theme.radii.sm - theme.hair()
}

/// A row inside a floating sheet padded by [`sheet_pad`].
///
/// A [`row`] whose fill (hover, selection) is rounded to nest in the sheet's corners, its
/// leading words still on the edge grid measured from the sheet's own edge, and its trailing
/// end a row's trailing inset in.
#[must_use]
pub fn sheet_row(theme: &Theme, lines: Row) -> Div {
    let pad = sheet_pad(theme);
    row(theme, lines).pl(px(theme.spacing.inset() - pad)).rounded(px(slopty_theme::Radii::nested(
        theme.radii.lg,
        theme.hair(),
        pad,
    )))
}

/// `el` padded in to the one edge grid on both sides: [`slopty_theme::Spacing::inset`]. A
/// panel's rows, a header, the palette, the inbox and the status bar all start there.
#[must_use]
pub fn inset_x<E: Styled>(el: E, theme: &Theme) -> E {
    el.px(px(theme.spacing.inset()))
}

/// `el` set as meta text: a row's second line, a bar's readout, a status word. The meta size
/// in `text_muted`; a status word then takes its tone's colour over it.
#[must_use]
pub fn meta<E: Styled>(el: E, theme: &Theme) -> E {
    el.text_size(px(theme.typography.small())).text_color(hsla(theme.surfaces.text_muted))
}

/// A section's label: quiet, so the rows under it lead.
///
/// `small()` in `text_muted`, the regular weight, never upper case. The strong weight stays for
/// one thing per region; a semibold heading over a semibold name was two strong lines stacked.
#[must_use]
pub fn label(theme: &Theme, text: impl Into<SharedString>) -> Div {
    div()
        .flex_none()
        .text_size(px(theme.typography.small()))
        .font_weight(FontWeight::NORMAL)
        .text_color(hsla(theme.surfaces.text_muted))
        .child(text.into())
}

/// A square button around one icon: a bar's actions, a tile's close and split.
///
/// Ghost like a bar's text buttons, with `label` as its accessible name and its hint, since the
/// icon alone names nothing to a screen reader.
#[must_use]
pub fn icon_button(
    theme: &Theme,
    id: impl Into<SharedString>,
    icon: crate::icons::IconName,
    label: &'static str,
) -> gpui::Stateful<Div> {
    icon_button_at(theme, id, icon, label, 1.0)
}

/// [`icon_button`] at the chrome's zoom `k`: a tile's header shrinks with the overview, and a
/// button drawn at full size there would outgrow the bar it sits in.
#[must_use]
pub fn icon_button_at(
    theme: &Theme,
    id: impl Into<SharedString>,
    icon: crate::icons::IconName,
    label: &'static str,
    k: f32,
) -> gpui::Stateful<Div> {
    let s = theme.surfaces;
    eased(square_icon(theme, id.into(), icon, label, k, s.text_secondary))
        .hover(move |el| el.bg(hsla(s.hover)).text_color(hsla(s.text)))
}

/// The square an icon button is drawn in, its icon in `ink`, before its hover.
fn square_icon(
    theme: &Theme,
    id: SharedString,
    icon: crate::icons::IconName,
    label: &'static str,
    k: f32,
    ink: Rgb,
) -> gpui::Stateful<Div> {
    let s = theme.surfaces;
    let selector = id.to_string();
    let side = icon_button_side(theme) * k;
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
        .rounded(px(theme.radii.sm * k))
        .cursor_pointer()
        .text_color(hsla(ink))
        .active(move |el| el.bg(hsla(s.pressed)))
        .child(
            crate::icons::icon(theme, icon, crate::icons::IconSize::Inline, hsla(ink))
                .size(px(theme.typography.icon() * k)),
        );
    crate::a11y::tab_stop(el, s.accent)
}

/// [`icon_button_at`] that stays on until pressed again: a tile's trackpad mode.
///
/// One name whatever its state, said as pressed or not (`aria_toggled`), the way iOS and
/// Zed say a toggle. A label that flipped with the state ("Use as a trackpad", then "Touch the
/// picture directly") named one control two ways and the palette's command a third. On, it
/// rests on the selected wash with its icon in `text`, and the pointer over it keeps that
/// wash: the hover's fainter one read as the toggle letting go.
#[must_use]
pub fn icon_toggle(
    theme: &Theme,
    id: impl Into<SharedString>,
    icon: crate::icons::IconName,
    label: &'static str,
    on: bool,
    k: f32,
) -> gpui::Stateful<Div> {
    if !on {
        return icon_button_at(theme, id, icon, label, k)
            .aria_toggled(gpui::accesskit::Toggled::False);
    }
    let s = theme.surfaces;
    square_icon(theme, id.into(), icon, label, k, s.text)
        .aria_toggled(gpui::accesskit::Toggled::True)
        .bg(hsla(s.selected))
}

/// The side of the disc an empty state's mark sits in, in points at zoom 1: `HeroUI`'s small
/// empty state's, the glyph at the inline icon size in its middle.
pub const NOTICE_DISC: f32 = 40.0;

/// What a tile's body says when it has nothing to show, as one block in its middle.
///
/// A mark seated in a [`NOTICE_DISC`] of the hover wash, which gives the empty body a centre of
/// mass without decoration (a bare grey glyph floated in it), a line at `small()` in the medium
/// weight saying what is so, and under it an optional muted line saying why or where. The
/// caller adds the identity and its actions under it.
///
/// A remote window on its way, a file that cannot be opened here and an empty or missing
/// folder all say it this way. Before, a file printed its summary alone ("binary, 2 MB") and a
/// reason ran as one clause ("Too large to edit here: 40 MB, past 16 MB"), each a lowercase
/// sentence adrift in the body.
#[must_use]
pub fn notice(
    theme: &Theme,
    k: f32,
    mark: impl IntoElement,
    title: impl Into<SharedString>,
    detail: Option<SharedString>,
) -> Div {
    let s = &theme.surfaces;
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(theme.spacing.xs * k))
        .max_w_full()
        .px(px(theme.spacing.inset() * k))
        .font_family(theme.typography.ui_family.clone())
        .text_center()
        .child(
            div()
                .flex_none()
                .size(px(NOTICE_DISC * k))
                .mb(px(theme.spacing.xs * k))
                .rounded(px(theme.radii.full))
                .bg(hsla(s.hover))
                .flex()
                .items_center()
                .justify_center()
                .child(mark),
        )
        .child(
            div()
                .text_size(px(theme.typography.small() * k))
                .font_weight(FontWeight(Typography::MEDIUM_WEIGHT))
                .text_color(hsla(s.text_secondary))
                .child(title.into()),
        )
        .children(detail.map(|detail| {
            meta(div(), theme).text_size(px(theme.typography.small() * k)).child(detail)
        }))
}

/// A [`notice`]'s mark: a kind's icon at the inline size in `text_secondary`, for its disc.
#[must_use]
pub fn notice_mark(theme: &Theme, icon: crate::icons::IconName, k: f32) -> gpui::Svg {
    let ink = hsla(theme.surfaces.text_secondary);
    crate::icons::icon(theme, icon, crate::icons::IconSize::Inline, ink)
        .size(px(theme.typography.icon() * k))
}

/// What the app is called where it names itself.
pub const APP_NAME: &str = "Slopty";

/// The side of the app's mark, in points.
pub const BRAND_MARK: f32 = 40.0;

/// The app icon, `assets/icon.svg`, the one the bundles are built from. Declared at the mark's
/// side at 3x rather than the source's 1024 px: an image is rasterised at its declared size and
/// sampled down with no mipmaps, so 1024 px drawn at 40 pt would shimmer at its edges.
static APP_ICON: LazyLock<Arc<gpui::Image>> = LazyLock::new(|| {
    let side = BRAND_MARK * 3.0;
    let svg = include_str!("../../../assets/icon.svg").replacen(
        r#"width="1024" height="1024" viewBox"#,
        &format!(r#"width="{side}" height="{side}" viewBox"#),
        1,
    );
    Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Svg, svg.into_bytes()))
});

/// The app's mark where it names itself: the first run, the settings' About.
///
/// Its own icon, the one the Dock and the home screen show, at [`BRAND_MARK`] and `radii.lg`, then
/// its name at the title size in the strong weight. A bare grey word there was the plainest thing
/// on the plainest screen; an invented glyph on an accent tile read as a web page's logo over a
/// form. `under` stands under the name, beside the mark (About's version line).
#[must_use]
pub fn brand(theme: &Theme, under: Option<gpui::AnyElement>) -> gpui::Stateful<Div> {
    let mark = gpui::img(APP_ICON.clone())
        .id("app-mark")
        .debug_selector(|| "app-mark".to_owned())
        .flex_none()
        .size(px(BRAND_MARK))
        .rounded(px(theme.radii.lg));
    div()
        .id("app-brand")
        .debug_selector(|| "app-brand".to_owned())
        .role(gpui::accesskit::Role::Label)
        .aria_label(APP_NAME)
        .flex()
        .items_center()
        .gap(px(theme.spacing.sm))
        .child(mark)
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(theme.spacing.xxs))
                .child(title(theme, APP_NAME))
                .children(under),
        )
}

/// A key cap: the keys on a small plate, the way the empty workspace teaches its chords and the
/// palette's foot names its keys. `keys` comes from the key tables, or is a lone key.
///
/// The plate is the selected wash with no hairline: a ring round every cap made a row of them
/// read as a row of buttons.
#[must_use]
pub fn key_cap(theme: &Theme, keys: impl Into<SharedString>) -> Div {
    let s = &theme.surfaces;
    div()
        .flex_none()
        .px(px(theme.spacing.xs))
        .rounded(px(theme.radii.xs))
        .bg(hsla(s.selected))
        .text_size(px(theme.typography.small()))
        .text_color(hsla(s.text_secondary))
        .child(SharedString::from(crate::palette::drawn_keys(&keys.into())))
}

/// What a bar button does, and the key that does it, shown after a pause on the pointer.
///
/// The bar prints no keys of its own. Six ⌘ chords across one strip was most of the text up
/// there and none of it answered what a button does; zed and warp print none and keep the index
/// in the command palette, which Slopty already has. A hint needs a pointer to hover, so this is
/// the Mac's affordance — on a phone the palette is the only one, as it always was.
///
/// Hints come as a warm group: the first waits [`HINT_DELAY`] under a resting pointer, and
/// while one was on screen in the last [`HINT_WARM`] the next shows at once, so running the
/// pointer along a bar reads each button without waiting at each (Vercel's and Raycast's
/// timing). GPUI is told to build it at once ([`hint_timing`]) and the hint keeps the wait.
#[derive(Debug)]
pub struct Hint {
    what: SharedString,
    key: SharedString,
    theme: Rc<Theme>,
    /// `what` is text to read exactly (an address), set in the monospace face.
    mono: bool,
    /// The wait is over and the hint is drawn.
    shown: bool,
    /// The wait under way, dropped with the hint when the pointer moves on first.
    wait: Option<gpui::Task<()>>,
}

/// How long the pointer rests on a control before the first hint shows.
pub const HINT_DELAY: std::time::Duration = std::time::Duration::from_millis(400);

/// How long after a hint goes the next one still shows at once.
pub const HINT_WARM: std::time::Duration = std::time::Duration::from_secs(1);

thread_local! {
    /// When the last hint on screen went, for the next to know whether the group is warm.
    static HINT_LEFT: std::cell::Cell<Option<std::time::Instant>> =
        const { std::cell::Cell::new(None) };
}

/// Whether a hint showing at `now` follows one that went at `left` closely enough to show at
/// once.
fn hint_warm(left: Option<std::time::Instant>, now: std::time::Instant) -> bool {
    left.is_some_and(|left| now.saturating_duration_since(left) < HINT_WARM)
}

/// `el` building its hint the moment the pointer rests on it: the [`Hint`] keeps the wait
/// itself, so a warm group can skip it. Every control with a hint wears it.
pub fn hint_timing<E: gpui::StatefulInteractiveElement>(el: E) -> E {
    el.tooltip_show_delay(std::time::Duration::ZERO)
}

impl Drop for Hint {
    fn drop(&mut self) {
        if self.shown {
            HINT_LEFT.set(Some(std::time::Instant::now()));
        }
    }
}

impl Hint {
    /// `what` the button does ("New shell"), and the `key` that does it ("⌘T").
    #[must_use]
    pub fn new(
        what: impl Into<SharedString>,
        key: impl Into<SharedString>,
        theme: Rc<Theme>,
    ) -> Self {
        Self { what: what.into(), key: key.into(), theme, mono: false, shown: false, wait: None }
    }

    /// `what` in the monospace face: an address or a path, read character by character.
    #[must_use]
    pub const fn mono(mut self) -> Self {
        self.mono = true;
        self
    }
}

impl Render for Hint {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.shown && self.wait.is_none() {
            if hint_warm(HINT_LEFT.get(), std::time::Instant::now()) {
                self.shown = true;
            } else {
                self.wait = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(HINT_DELAY).await;
                    let _gone = this.update(cx, |this, cx| {
                        this.shown = true;
                        cx.notify();
                    });
                }));
            }
        }
        if !self.shown {
            return gpui::Empty.into_any_element();
        }
        let theme = &self.theme;
        let s = &theme.surfaces;
        let hint = elevate(div(), theme)
            .flex()
            .items_center()
            .gap(px(theme.spacing.sm))
            .px(px(theme.spacing.sm))
            .py(px(theme.spacing.xxs))
            .rounded(px(theme.radii.sm))
            .text_size(px(theme.typography.small()))
            .font_family(theme.typography.ui_family.clone())
            .child(
                div()
                    .text_color(hsla(s.text))
                    .when(self.mono, |el| el.font_family(crate::palette::mono_family(theme)))
                    .child(self.what.clone()),
            )
            .when(!self.key.is_empty(), |el| {
                el.child(div().text_color(hsla(s.text_muted)).child(self.key.clone()))
            });
        fade_in(hint, "hint", cx)
    }
}

#[cfg(test)]
mod hint_tests {
    use std::time::{Duration, Instant};

    use super::{HINT_WARM, hint_warm};

    /// The first hint waits; one that follows another within the warm second shows at once,
    /// and the group cools after it.
    #[test]
    fn hints_come_as_a_warm_group() {
        let now = Instant::now();
        assert!(!hint_warm(None, now), "the first one waits");
        assert!(hint_warm(Some(now), now + Duration::from_millis(300)), "the next at once");
        assert!(!hint_warm(Some(now), now + HINT_WARM), "cold again after a second");
    }
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
    let c = &mut kit.colors;
    c.background = hsla(s.panel);
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
    // gpui-kit's primary (its buttons, switches, checkboxes) is Slopty's: the neutral solid.
    let (hovered, pressed) = solid_states(theme);
    c.primary = hsla(s.solid);
    c.primary_hover = hsla(hovered);
    c.primary_active = hsla(pressed);
    c.primary_foreground = hsla(s.solid_ink);
    c.link = hsla(s.accent);
    c.link_hover = hsla(s.accent);
    c.link_active = hsla(s.accent);
    c.popover = hsla(s.elevated);
    c.popover_foreground = hsla(s.text);
    // The surface order: bars on `canvas`, side panels on `panel`, content above both.
    c.title_bar = hsla(s.canvas);
    c.title_bar_border = hsla(s.border);
    c.status_bar = hsla(s.canvas);
    c.status_bar_border = hsla(s.border);
    c.sidebar = hsla(s.panel);
    c.sidebar_foreground = hsla(s.text);
    c.sidebar_border = hsla(s.border);
    c.tab_bar = hsla(s.panel);
    c.tab = hsla(s.panel);
    c.tab_foreground = hsla(s.text_muted);
    c.tab_active = hsla(theme.content());
    c.tab_active_foreground = hsla(s.text);
    c.table_row_border = hsla(s.border_subtle);
    c.list = hsla(s.panel);
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
    KitTheme::sync_base(cx);
    // `sync_base` reinstalls the Markdown defaults, so the code colouring goes on after it.
    TextViewDefaults::global(cx)
        .with_code_block_highlighter(crate::highlight::code_block(theme.clone()))
        .install(cx);
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

    /// The first line is the first with something on it, trimmed.
    #[test]
    fn the_first_line_skips_blank_ones() {
        assert_eq!(first_line("\n  \n  cargo build  \nnext"), "cargo build");
        assert_eq!(first_line(""), "");
    }

    /// Chrome text is sentence case, whatever it is doing: a label, a button's accessible name,
    /// a field's placeholder and an empty state are all written the same way. The stream HUD and
    /// the agent pill are readouts of a number or a state, not chrome, and stay lowercase.
    #[test]
    fn chrome_text_is_sentence_case() {
        let chrome = [
            FIND_PLACEHOLDER,
            crate::workspace::INSTALL_HOOKS,
            crate::workspace::TAKE_OVER,
            crate::workspace::HOOKS,
            crate::workspace::TAKE,
            crate::workspace::MUTE,
            crate::workspace::ATTACHING,
            crate::workspace::SLEEPING,
            crate::workspace::PAUSED,
            crate::workspace::OPENING,
            crate::workspace::READING,
            crate::workspace::NOTE,
            crate::note::WRITE_PLACEHOLDER,
            crate::file::CHANGED_ON_DISK,
            crate::file::RELOAD,
            crate::file::OVERWRITE,
            crate::folder::EMPTY_FOLDER,
            crate::folder::NOT_A_FOLDER,
            crate::folder::CANNOT_LIST,
            crate::file::TOO_LARGE,
            crate::file::NOT_TEXT,
            crate::file::CANNOT_READ,
            crate::file::OPEN_IN_EDITOR,
            crate::file::GO_TO_LINE,
            crate::file::OPEN_IN_PAGER,
            crate::screen::TRACKPAD_MODE,
            crate::folder::ENCLOSING_FOLDER,
            crate::settings_form::PRESS_KEYS,
            crate::settings_form::NO_KEYS,
            crate::settings_form::RESET_KEYS,
            crate::workspace::COPY_COMMAND,
            crate::palette::NO_COMMAND_MATCHES,
            crate::picker::FILTER_PLACEHOLDER,
            crate::picker::NOTHING_MATCHES,
            crate::picker::NOTHING_TO_JUMP_TO,
            crate::picker::LOADING_WINDOWS,
            crate::terminal::BACK_TO_LIVE,
            crate::workspace::RECONNECTING,
            crate::workspace::SESSION_ENDED,
            crate::workspace::CLOSE_TILE,
            crate::workspace::FULLSCREEN_TILE,
            crate::workspace::ASK,
            crate::workspace::NO_WORKERS,
            crate::workspace::NO_WORKERS_NEXT,
            crate::workspace::ADD_WORKER,
            crate::workspace::NEW_WORKSPACE,
            crate::workspace::UNTITLED_NOTE,
            crate::workspace::CHROME_WORDS[0],
            crate::workspace::CHROME_WORDS[1],
            crate::screen::waiting_text(slopty_proto::screen::SourceState::Idle),
            crate::screen::waiting_text(slopty_proto::screen::SourceState::Live),
        ];
        for text in chrome {
            let first = text.chars().next().unwrap_or(' ');
            assert!(first.is_uppercase(), "chrome text starts lowercase: {text:?}");
            let rest: String = text.chars().skip(1).collect();
            assert!(!rest.contains(char::is_uppercase), "chrome text is title case: {text:?}");
        }
    }

    /// Both variants land in gpui-kit's theme: mode and the token colours.
    #[gpui::test]
    fn the_kit_theme_follows_the_tokens(cx: &TestAppContext) {
        cx.update(gpui_kit::init);
        for variant in [Variant::Light, Variant::Dark] {
            let theme = Theme::new(variant);
            cx.update(|cx| sync(&theme, cx));
            cx.update(|cx| {
                let kit = KitTheme::global(cx);
                assert_eq!(kit.mode.is_dark(), variant == Variant::Dark);
                assert_eq!(kit.colors.background, hsla(theme.surfaces.panel));
                assert_eq!(kit.colors.foreground, hsla(theme.surfaces.text));
                assert_eq!(kit.colors.primary, hsla(theme.surfaces.solid), "the neutral solid");
                assert_eq!(kit.colors.primary_foreground, hsla(theme.surfaces.solid_ink));
                assert_eq!(kit.colors.caret, hsla(theme.surfaces.text), "a neutral caret");
                assert_eq!(kit.colors.border, hsla(theme.surfaces.border));
                assert_eq!(kit.colors.ring, hsla(theme.surfaces.border), "no accent on a field");
                assert_eq!(kit.colors.title_bar, hsla(theme.surfaces.canvas));
                assert_eq!(kit.colors.sidebar, hsla(theme.surfaces.panel));
                assert_eq!(kit.colors.tab_active, hsla(theme.content()));
                assert_eq!(kit.colors.table_row_border, hsla(theme.surfaces.border_subtle));
                assert_eq!(kit.colors.popover, hsla(theme.surfaces.elevated), "popovers float");
                assert!(!kit.focus_ring, "a focused field is one hairline, not a halo");
                assert!(
                    TextViewDefaults::global(cx).has_code_block_highlighter(),
                    "fenced code is coloured after the sync"
                );
            });
        }
    }

    /// The chrome under `dir`, without its test modules: every line of Rust up to a
    /// `#[cfg(test)]` over a `mod`, with its file and one-based line number. A test-only
    /// accessor gated on its own does not end the file's scan.
    fn chrome_lines(dir: &str) -> Vec<(String, usize, String)> {
        fn walk(dir: &std::path::Path, out: &mut Vec<(String, usize, String)>) {
            let entries = std::fs::read_dir(dir).expect("the crate's sources are readable");
            for entry in entries.flatten() {
                let path = entry.path();
                // Test modules in files of their own are not chrome either.
                let is_tests = path.file_stem().is_some_and(|n| n == "tests");
                if is_tests {
                    continue;
                }
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let text = std::fs::read_to_string(&path).expect("a source file reads");
                    let name = path.display().to_string();
                    let lines: Vec<&str> = text.lines().collect();
                    for (ix, line) in lines.iter().enumerate() {
                        let next = lines.get(ix.saturating_add(1)).copied().unwrap_or_default();
                        if line.trim() == "#[cfg(test)]" && next.trim_start().starts_with("mod ") {
                            break;
                        }
                        out.push((name.clone(), ix.saturating_add(1), (*line).to_owned()));
                    }
                }
            }
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("the crate sits in the workspace")
            .join(dir);
        let mut out = Vec::new();
        walk(&root, &mut out);
        assert!(!out.is_empty(), "found no sources under {dir}");
        out
    }

    /// A padding, margin or gap written as a literal rather than taken from `theme.spacing`.
    ///
    /// Only these: a literal `w`/`h`/`size` is a measurement of something real (a hairline, an
    /// icon box, a panel that has to be some width), while a literal pad is a rhythm chosen in
    /// one place and nowhere else, which is how a scale of 2/4/8/12/16/24 quietly becomes a
    /// scale of every number. Returns the offending call for the message.
    fn literal_spacing(line: &str) -> Option<String> {
        const PAD: [&str; 13] = [
            ".p(", ".px(", ".py(", ".pt(", ".pb(", ".pl(", ".pr(", ".m(", ".mx(", ".my(", ".gap(",
            ".gap_x(", ".gap_y(",
        ];
        PAD.iter()
            .find(|call| {
                line.split(*call).skip(1).any(|rest| {
                    rest.strip_prefix("px(")
                        .is_some_and(|n| n.starts_with(|c: char| c.is_ascii_digit()))
                })
            })
            .map(|call| format!("`{call}px(N)`"))
    }

    /// A floating layer lifted by hand: a shadow of its own, or a scrim that is not the
    /// elevation's.
    fn own_elevation(line: &str) -> Option<&'static str> {
        if line.trim_start().starts_with("//") {
            return None;
        }
        let named = [".shadow_", "::shadow_"]
            .iter()
            .any(|call| line.split(call).skip(1).any(|rest| !rest.starts_with("none")));
        if named || line.contains(".shadow(") {
            return Some("a shadow of its own, not `kit::elevate`, `kit::card` or `kit::rests`");
        }
        let dim = ["alpha::SCRIM", "elevation.scrim", "elevation.shade"];
        dim.iter()
            .any(|token| line.contains(token))
            .then_some("a scrim of its own, not `kit::scrim`")
    }

    #[test]
    fn the_elevation_check_knows_a_lift_from_a_token() {
        assert!(own_elevation(".bg(hsla(s.panel)).shadow_sm()").is_some());
        assert!(own_elevation(".shadow(vec![shadow])").is_some());
        assert!(own_elevation(".bg(hsla_alpha(s.canvas, alpha::SCRIM))").is_some());
        assert!(own_elevation("kit::elevate(div(), theme).rounded(px(r))").is_none());
        assert!(own_elevation(".bg(kit::scrim(theme))").is_none());
        assert!(own_elevation(".when(floats, gpui::Styled::shadow_sm)").is_some());
        assert!(own_elevation(".shadow_none()").is_none(), "taking a shadow off is fine");
        assert!(own_elevation("/// no `.shadow_sm()` here").is_none(), "a comment");
        let dimmed = ".bg(hsla_alpha(theme.elevation.shade, alpha::DIM))";
        assert!(own_elevation(dimmed).is_some(), "the shade dimmed by hand");
        assert!(own_elevation(".bg(hsla_alpha(s.canvas, t.elevation.scrim))").is_some());
    }

    /// What floats wears the floating elevation and what rests the resting one: [`elevate`],
    /// [`card`], [`rests`] and [`scrim`] are the only places a shadow, a rim or a modal's
    /// dim is chosen, so an overlay cannot sit below the content again nor a card grow a
    /// shadow of its own.
    #[test]
    fn a_floating_layer_wears_the_one_elevation() {
        let mut wrong = Vec::new();
        for dir in ["slopty-ui/src", "slopty-app/src"] {
            for (file, line_no, line) in chrome_lines(dir) {
                let waived = file.ends_with("slopty-ui/src/kit.rs");
                if let Some(why) = own_elevation(&line).filter(|_| !waived) {
                    wrong.push(format!("{file}:{line_no}: {why}"));
                }
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// Chrome lines under `slopty-ui/src` and `slopty-app/src` that `wrong` flags, outside
    /// `kit.rs` and the files in `awaiting` (call sites the next wave moves onto the kit), as
    /// `file:line: why`.
    fn flagged(awaiting: &[&str], wrong: impl Fn(&str) -> Option<&'static str>) -> Vec<String> {
        ["slopty-ui/src", "slopty-app/src"]
            .into_iter()
            .flat_map(chrome_lines)
            .filter(|(file, ..)| {
                !file.ends_with("slopty-ui/src/kit.rs")
                    && !awaiting.iter().any(|f| file.ends_with(f))
            })
            .filter_map(|(file, line_no, line)| {
                wrong(&line).map(|why| format!("{file}:{line_no}: {why}: {}", line.trim()))
            })
            .collect()
    }

    /// A diff's size spelled by hand: a minus sign written before a formatted figure, or drawn
    /// apart from its figure. A diff's own sign column (`"\u{2212}"` beside its tone) is not a
    /// size, and neither is a comment.
    fn hand_rolled_changes(line: &str) -> Option<&'static str> {
        let signed = literals(line)
            .iter()
            .any(|l| ["\\u{2212}{", "−{", "\\u{2012}{", "‒{"].iter().any(|sign| l.contains(sign)));
        let apart =
            [".child(\"\\u{2212}\")", ".child(\"−\")", ".child(\"\\u{2012}\")", ".child(\"‒\")"]
                .iter()
                .any(|c| line.contains(c));
        (signed || apart).then_some("a diff's size by hand, not `kit::changes`")
    }

    #[test]
    fn the_changes_check_knows_a_size_from_a_sign() {
        assert!(hand_rolled_changes(r#"format!("+{added} \u{2212}{removed}")"#).is_some());
        assert!(hand_rolled_changes(r#"format!("\u{2212}{}", changes.removed)"#).is_some());
        assert!(
            hand_rolled_changes(r#".child(div().text_color(red).child("\u{2212}"))"#).is_some()
        );
        assert!(hand_rolled_changes(r#"Kind::Removed => (wash, "\u{2212}", s.error),"#).is_none());
        assert!(hand_rolled_changes("/// `+a −r` in the diff's tones").is_none(), "a comment");
        assert!(hand_rolled_changes("kit::changes(theme, added, removed)").is_none());
    }

    /// A count of changed lines is drawn by [`changes`] (or said by [`changes_text`]): the
    /// signs in the diff's tones, the figures quiet, a zero side left out. Written by hand it
    /// came out two ways, one of them a red "−0".
    #[test]
    fn a_diff_size_is_drawn_by_kit_changes() {
        const AWAITING: [&str; 0] = [];
        let wrong = flagged(&AWAITING, hand_rolled_changes);
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// A duration spelled by hand: a format with a figure then seconds (`"{secs} s"`,
    /// `"{:.1}s"`), or minutes or hours followed by a second figure (`"{} m {:02} s"`). A
    /// latency in milliseconds is a readout, not a duration, and an age ("5m") is its own
    /// format; neither is flagged.
    fn hand_rolled_duration(line: &str) -> Option<&'static str> {
        let spelled = |text: &String| {
            let chars: Vec<char> = text.chars().collect();
            chars.iter().enumerate().filter(|(_, c)| **c == '}').any(|(ix, _)| {
                let at = |n: usize| chars.get(ix.saturating_add(n)).copied();
                let skip = usize::from(at(1) == Some(' '));
                let unit = at(skip.saturating_add(1));
                let after = at(skip.saturating_add(2));
                let seconds = unit == Some('s') && !after.is_some_and(char::is_alphanumeric);
                let compound = matches!(unit, Some('m' | 'h'))
                    && after == Some(' ')
                    && at(skip.saturating_add(3)) == Some('{');
                seconds || compound
            })
        };
        literals(line)
            .iter()
            .any(spelled)
            .then_some("a duration spelled by hand, not `kit::duration`")
    }

    #[test]
    #[expect(
        clippy::literal_string_with_formatting_args,
        reason = "the check's cases are format strings as they stand in the source"
    )]
    fn the_duration_check_knows_a_duration_from_a_readout() {
        assert!(hand_rolled_duration(r#"1..60 => format!("{secs} s"),"#).is_some());
        assert!(hand_rolled_duration(r#"format!("{:.1} s", elapsed.as_secs_f64())"#).is_some());
        assert!(hand_rolled_duration(r#"format!("{} m {:02} s", secs / 60, secs % 60)"#).is_some());
        assert!(hand_rolled_duration(r#"format!("{}h {}m", h, m)"#).is_some());
        assert!(hand_rolled_duration(r#"format!("rtt {:.1} ms", ms(d))"#).is_none(), "latency");
        assert!(hand_rolled_duration(r#"format!("{}m", secs / 60)"#).is_none(), "an age");
        assert!(hand_rolled_duration(r#"format!("{n} steps")"#).is_none());
        assert!(hand_rolled_duration(r#"/// "Worked for 3 m 12 s""#).is_none(), "a comment");
        assert!(hand_rolled_duration("kit::duration(elapsed)").is_none());
    }

    /// How long something took is said by [`duration`]: three hand-made formats had put
    /// "1 m 05 s" beside cargo's "1m 04s" on one line, and "6.0 s" in a header.
    #[test]
    fn a_duration_is_kit_duration() {
        const AWAITING: [&str; 0] = [];
        let wrong = flagged(&AWAITING, hand_rolled_duration);
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// A pill made by hand: its height grown from a pad at the chrome's zoom, or a tone's
    /// faint fill at rest (a hover is not a pill).
    fn hand_rolled_pill(line: &str) -> Option<&'static str> {
        if line.trim_start().starts_with("//") {
            return None;
        }
        let squeezed: String = line.split_whitespace().collect();
        if squeezed.contains(".py(px(theme.spacing.xxs*k))") {
            return Some("a pill's height from its pad, not `kit::pill_frame`");
        }
        let fill = squeezed.contains(".bg(hsla_alpha(") && squeezed.contains("alpha::FAINT");
        (fill && !squeezed.contains(".hover(")).then_some("a tone's pill by hand, not `kit::pill`")
    }

    #[test]
    fn the_pill_check_knows_a_pill_from_a_hover() {
        assert!(hand_rolled_pill(".py(px(theme.spacing.xxs * k))").is_some());
        assert!(hand_rolled_pill(".bg(hsla_alpha(color, alpha::FAINT))").is_some());
        let hover = ".hover(move |el| el.bg(hsla_alpha(quiet, alpha::FAINT)))";
        assert!(hand_rolled_pill(hover).is_none(), "a hover fill");
        assert!(hand_rolled_pill("kit::pill(theme, s.warn, k)").is_none());
        assert!(hand_rolled_pill(".py(px(theme.spacing.xxs))").is_none(), "a hint, unzoomed");
        assert!(hand_rolled_pill("// .py(px(theme.spacing.xxs * k))").is_none(), "a comment");
    }

    /// A header's pill is [`pill`] (or [`pill_frame`] for its bare words): [`PILL_HEIGHT`]
    /// tall whatever its text, so it sits centred in the header rather than filling it.
    #[test]
    fn a_pill_is_kit_pill() {
        const AWAITING: [&str; 0] = [];
        let wrong = flagged(&AWAITING, hand_rolled_pill);
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// A pill is its fixed height at any zoom and a capsule, and the state's pill is the frame
    /// filled at the faint step, a notch fainter on white.
    #[test]
    fn a_pill_is_a_twenty_point_capsule_at_its_zoom() {
        for (variant, step) in
            [(Variant::Dark, alpha::FAINT), (Variant::Light, alpha::FAINT_ON_PAPER)]
        {
            let theme = Theme::new(variant);
            for k in [1.0, 0.5] {
                let mut frame = pill_frame(&theme, k);
                let height = frame.style().size.height;
                assert_eq!(height, Some(px(PILL_HEIGHT * k).into()), "zoom {k}");
                let corner = frame.style().corner_radii.top_left;
                assert_eq!(corner, Some(px(theme.radii.full).into()), "a capsule");
                let mut filled = pill(&theme, theme.surfaces.warn, k);
                assert_eq!(filled.style().size.height, Some(px(PILL_HEIGHT * k).into()));
                let fill = filled.style().background.clone();
                let faint = gpui::Fill::from(hsla_alpha(theme.surfaces.warn, step));
                assert_eq!(fill, Some(faint), "{variant:?}: the tone at the faint step");
            }
        }
        let mut cap = key_cap(&Theme::default(), "K");
        let corner = cap.style().corner_radii.top_left;
        assert_eq!(corner, Some(px(Theme::default().radii.xs).into()), "a key stays a key");
    }

    /// A share written with a space before its sign: "150 %" beside the "34%" of the context
    /// and the "↑ 42%" of an upload.
    fn spaced_percent(line: &str) -> Option<&'static str> {
        literals(line)
            .iter()
            .any(|l| l.contains("} %"))
            .then_some("a share as \"{n} %\", where chrome writes \"{n}%\"")
    }

    #[test]
    #[expect(
        clippy::literal_string_with_formatting_args,
        reason = "the check's cases are format strings as they stand in the source"
    )]
    fn the_percent_check_knows_a_share_from_a_modulo() {
        assert!(spaced_percent(r#"format!("{percent:.0} %")"#).is_some());
        assert!(spaced_percent(r#"format!("{percent:.0}%")"#).is_none());
        assert!(spaced_percent("let rest = secs % 60;").is_none(), "arithmetic");
        assert!(spaced_percent(r#"// format!("{n} %")"#).is_none(), "a comment");
    }

    /// One way to write a share: the figure and the sign together, as macOS and the context
    /// readout write it.
    #[test]
    fn a_percent_sits_against_its_figure() {
        const AWAITING: [&str; 0] = [];
        let wrong = flagged(&AWAITING, spaced_percent);
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// A toggle is one control with one name: off it is a quiet icon button, on it rests on
    /// the selected wash with its icon in `text`, and its width does not change.
    #[test]
    fn a_toggle_rests_on_the_selected_fill_only_while_on() {
        let theme = Theme::default();
        let icon = crate::icons::IconName::MousePointer2;
        let mut off = icon_toggle(&theme, "t", icon, "Trackpad mode", false, 1.0);
        let mut on = icon_toggle(&theme, "t", icon, "Trackpad mode", true, 1.0);
        assert_eq!(off.style().background, None, "off is bare");
        let selected = Some(gpui::Fill::from(hsla(theme.surfaces.selected)));
        assert_eq!(on.style().background, selected, "on rests on the selected wash");
        assert_eq!(on.style().size.width, off.style().size.width, "one size either way");
    }

    /// The primary is the neutral solid, the secondary the hover wash and the rest bare, all
    /// one control's height, in both variants: no button wears the accent. A selection is the
    /// selected wash with a hairline ring inside it.
    #[test]
    fn a_button_is_neutral_and_one_height() {
        for variant in [Variant::Dark, Variant::Light] {
            let theme = Theme::new(variant);
            let s = theme.surfaces;
            let fill = |kind| button(&theme, "b", "Go", kind).style().background.clone();
            assert_eq!(fill(ButtonKind::Primary), Some(gpui::Fill::from(hsla(s.solid))));
            assert_eq!(fill(ButtonKind::Secondary), Some(gpui::Fill::from(hsla(s.hover))));
            assert_eq!(fill(ButtonKind::Ghost), None);
            assert_eq!(fill(ButtonKind::Link), None);
            for kind in [ButtonKind::Primary, ButtonKind::Secondary, ButtonKind::Ghost] {
                let height = button(&theme, "b", "Go", kind).style().size.height;
                assert_eq!(height, Some(px(theme.density.control).into()), "{kind:?}");
            }
            let mut picked = selected(div(), &theme, true);
            assert_eq!(picked.style().background, Some(gpui::Fill::from(hsla(s.selected))));
            let ring = picked.style().box_shadow.clone().unwrap_or_default();
            assert!(ring.iter().all(|l| l.inset && l.spread_radius == hair(&theme)), "{ring:?}");
            let mut away = selected(div(), &theme, false);
            assert_eq!(away.style().background, Some(gpui::Fill::from(hsla(s.hover))), "not key");
            assert!(away.style().box_shadow.is_none(), "no ring off the keyboard");
        }
    }

    /// The first run and About lead with one mark at one size.
    #[test]
    fn the_brand_is_the_app_icon_at_its_side() {
        assert_ne!(APP_ICON.bytes(), b"");
        let svg = String::from_utf8_lossy(APP_ICON.bytes());
        let side = BRAND_MARK * 3.0;
        assert!(svg.contains(&format!(r#"width="{side}""#)), "declared at 3x the mark");
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

    /// Chrome draws no gradient, `kit` included: a gradient is a wash, the first tell of
    /// generated UI. Where something scrolls past an edge, the content itself fades per pixel
    /// (`gpui::edge_fade`), over whatever lies behind it.
    #[test]
    fn chrome_draws_no_gradient() {
        let wrong: Vec<String> = ["slopty-ui/src", "slopty-app/src"]
            .into_iter()
            .flat_map(chrome_lines)
            .filter(|(.., line)| {
                let code = !line.trim_start().starts_with("//");
                code && ["linear_gradient(", "linear_color_stop("].iter().any(|g| line.contains(g))
            })
            .map(|(file, line_no, line)| format!("{file}:{line_no}: a gradient: {}", line.trim()))
            .collect();
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// A field's text inset is [`FIELD_INSET`], gpui-kit's medium field padding, and nothing
    /// keeps a copy of it, so what lines up with a field's text cannot drift from it.
    #[test]
    fn the_field_inset_is_the_kits() {
        const AWAITING: [&str; 0] = [];
        let medium = gpui_kit::component::Size::Medium.input_px();
        assert!((f32::from(medium) - FIELD_INSET).abs() < f32::EPSILON, "{medium:?}");
        let copy = |line: &str| line.contains("const FIELD_INSET").then_some("a second inset");
        let wrong = flagged(&AWAITING, copy);
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// A focused element told by an accent border: the field that turns its hairline blue
    /// when it takes the caret. The keyboard's ring is `a11y::tab_stop`'s outline, not a border.
    fn accent_focus_border(line: &str) -> bool {
        let squeezed: String = line.split_whitespace().collect();
        let accent = ["hsla(s.accent)", "hsla(theme.surfaces.accent)", "{s.accent}else"];
        squeezed.contains(".border_color(")
            && accent.iter().any(|a| squeezed.contains(a))
            && squeezed.contains("focus")
            && !squeezed.trim_start().starts_with("//")
    }

    #[test]
    fn the_focus_border_check_knows_a_field_from_a_ring() {
        assert!(accent_focus_border(".when(focused, |el| el.border_color(hsla(s.accent)))"));
        assert!(accent_focus_border(
            ".border_color(hsla(if focused { s.accent } else { s.border }))"
        ));
        assert!(!accent_focus_border(
            ".border_color(hsla(if picked { s.accent } else { s.border }))"
        ));
        assert!(!accent_focus_border(".when(focused, |el| el.border_color(hsla(s.border)))"));
        assert!(!accent_focus_border(
            ".when(active, |el| el.border_2().border_color(hsla(s.accent)))"
        ));
    }

    /// The accent's text tone is never a fill: it is lifted for reading, not for carrying
    /// words. Nor is it a focused field's border: a field shows focus by its caret, and the
    /// accent is kept for the keyboard's ring, links in prose and the marks that say "live".
    #[test]
    fn the_accent_text_tone_is_never_a_fill() {
        const AWAITING: [&str; 0] = [];
        let mut wrong = Vec::new();
        for dir in ["slopty-ui/src", "slopty-app/src"] {
            for (file, line_no, line) in chrome_lines(dir) {
                let squeezed: String = line.split_whitespace().collect();
                let fills =
                    [".bg(hsla(s.accent))", ".bg(hsla(theme.surfaces.accent))", "{s.accent}else"];
                if fills.iter().any(|f| squeezed.contains(f)) && squeezed.contains(".bg(") {
                    wrong.push(format!("{file}:{line_no}: {line}"));
                }
                let waived = AWAITING.iter().any(|f| file.ends_with(f));
                if accent_focus_border(&line) && !waived {
                    wrong.push(format!("{file}:{line_no}: a focused field in the accent: {line}"));
                }
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// A control in the green: words or a glyph in `accent_ink`, which only a green fill
    /// carries, so the line paints a button, a ticked box, a switch or a key green.
    fn green_control(line: &str) -> Option<&'static str> {
        let code = !line.trim_start().starts_with("//");
        (code && line.contains("accent_ink"))
            .then_some("a control in the accent; a primary or an on state takes `kit::solid`")
    }

    #[test]
    fn the_green_control_check_knows_a_control_from_a_mark() {
        assert!(green_control(".bg(hsla(s.accent_fill)).text_color(hsla(s.accent_ink))").is_some());
        assert!(green_control("if lit { (s.accent_fill, s.accent_ink) } else { x }").is_some());
        assert!(green_control(".size(px(dot)).rounded_full().bg(hsla(s.accent_fill))").is_none());
        assert!(green_control("kit::solid(el, theme)").is_none());
        assert!(green_control("// `accent_ink` was the ink").is_none(), "a comment");
    }

    /// The accent is a mark, never a control's fill: the primary action, a ticked box, a
    /// switch that is on and an armed key take the neutral [`solid`]. Green on a control read
    /// as a second brand colour, and made green mean "press here" as well as "live, chosen,
    /// done". The one green that carries words is a badge's count (the inbox bell's, for
    /// what finished).
    #[test]
    fn the_accent_is_never_a_control() {
        const RULED: [&str; 1] = ["slopty-ui/src/workspace/titlebar.rs"];
        // Call sites whose owners move them onto `kit::solid` in their next change.
        const AWAITING: [&str; 0] = [];
        let waived: Vec<&str> = RULED.iter().chain(&AWAITING).copied().collect();
        let wrong = flagged(&waived, green_control);
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// A colour picked at a call site rather than taken from the theme: a hex or channel
    /// literal handed to GPUI. The tokens derive every colour from the content, so a literal
    /// is the one colour a custom background or Increase Contrast cannot move.
    fn raw_colour(line: &str) -> Option<&'static str> {
        let code = !line.trim_start().starts_with("//");
        let literal = ["Rgb::hex(", "gpui::rgb(", "gpui::rgba(", "gpui::hsla(", "Rgb { r:"];
        (code && literal.iter().any(|l| line.contains(l)))
            .then_some("a colour literal, not a theme token")
    }

    #[test]
    fn the_colour_check_knows_a_literal_from_a_token() {
        assert!(raw_colour(".bg(gpui::rgb(0x00ff_0000))").is_some());
        assert!(raw_colour("let white = Rgb::hex(0xff_ffff);").is_some());
        assert!(raw_colour(".bg(hsla(s.hover))").is_none());
        assert!(raw_colour("/// not `Rgb::hex(0)`").is_none(), "a comment");
    }

    /// Chrome paints in the theme's colours only. The waived lines compute a colour from the
    /// theme's (a cursor's ink against the cursor the program chose), or are this file's lit
    /// edge, which is white by definition.
    #[test]
    fn chrome_has_no_colour_literals() {
        const RULED: [&str; 1] = ["slopty-app/src/settings.rs"];
        let wrong = flagged(&RULED, raw_colour);
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// A text size, a radius or a control's height written as a literal: a type size off the
    /// scale, a corner off the radii, a row off the density.
    fn raw_size(line: &str) -> Option<&'static str> {
        let code = !line.trim_start().starts_with("//");
        let squeezed: String = line.split_whitespace().collect();
        let literal = |call: &str| {
            squeezed.split(call).skip(1).any(|rest| {
                rest.strip_prefix("px(")
                    .is_some_and(|n| n.starts_with(|c: char| c.is_ascii_digit() && c != '0'))
            })
        };
        if !code {
            return None;
        }
        if literal(".text_size(") {
            return Some("a text size off the type scale");
        }
        [".rounded(", ".rounded_t(", ".rounded_b(", ".rounded_l(", ".rounded_r("]
            .iter()
            .any(|call| literal(call))
            .then_some("a radius off `theme.radii`")
    }

    #[test]
    fn the_size_check_knows_a_literal_from_a_token() {
        assert!(raw_size(".text_size(px(13.0))").is_some());
        assert!(raw_size(".rounded(px(6.0))").is_some());
        assert!(raw_size(".rounded_t(px(0.0))").is_none(), "square is no radius");
        assert!(raw_size(".text_size(px(theme.typography.small()))").is_none());
        assert!(raw_size(".rounded(px(theme.radii.sm * k))").is_none());
        assert!(raw_size("// .text_size(px(13.0))").is_none(), "a comment");
    }

    /// Chrome's type sizes come from the type scale and its corners from the radii.
    #[test]
    fn chrome_sizes_come_from_the_scale() {
        const AWAITING: [&str; 0] = [];
        let wrong = flagged(&AWAITING, raw_size);
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// What floats as a sheet (a dialog, a menu, the inbox, the workers' popover, a toast, the
    /// add-worker panel) is rounded at `radii.lg`, the rows' radius plus the pad round them. A
    /// hint and a pill keep their control's radius: at 12 a 20 pt hint is a lozenge.
    #[test]
    fn a_floating_surface_is_rounded_lg() {
        const SHEETS: [(&str, &str); 7] = [
            ("slopty-ui/src/kit.rs", "pub fn dialog("),
            ("slopty-ui/src/conversation/view/parts.rs", "\"composer-shell\""),
            ("slopty-ui/src/conversation/view/parts.rs", ".id(\"conversation-find\")"),
            ("slopty-ui/src/workspace/titlebar.rs", "fn menu_panel("),
            ("slopty-ui/src/workspace/statusbar.rs", ".id(\"hosts\")"),
            ("slopty-ui/src/workspace/inbox.rs", ".id(\"inbox\")"),
            ("slopty-app/src/lib.rs", ".id(\"add-worker\")"),
        ];
        let lines: Vec<_> =
            ["slopty-ui/src", "slopty-app/src"].into_iter().flat_map(chrome_lines).collect();
        for (sheet, marker) in SHEETS {
            let start = lines
                .iter()
                .position(|(f, _, l)| f.ends_with(sheet) && l.contains(marker))
                .unwrap_or_else(|| panic!("{sheet}: no {marker}"));
            let rounded = lines
                .iter()
                .skip(start)
                .take(30)
                .find(|(f, _, l)| f.ends_with(sheet) && l.contains(".rounded("))
                .unwrap_or_else(|| panic!("{sheet}: {marker} is not rounded"));
            assert!(rounded.2.contains("radii.lg"), "{}:{}: {}", rounded.0, rounded.1, rounded.2);
        }
    }

    /// The strong weight is for titles: a dialog's or a panel's (`kit::title` or the title
    /// size), a page's heading, and the first run's wordmark. A name, a row or a tab that has
    /// to stand out takes the medium weight; with 600 on the workspace's name, the worker's name
    /// and the overview's names, nothing between a label and the loudest line could say
    /// "this one".
    #[test]
    fn strong_weight_is_for_titles() {
        let mut wrong = Vec::new();
        for dir in ["slopty-ui/src", "slopty-app/src"] {
            let lines = chrome_lines(dir);
            for (ix, (file, line_no, line)) in lines.iter().enumerate() {
                if !line.contains("STRONG_WEIGHT") || line.trim_start().starts_with("//") {
                    continue;
                }
                let near = lines
                    .iter()
                    .skip(ix.saturating_sub(8))
                    .take(12)
                    .filter(|(f, ..)| f == file)
                    .map(|(_, _, l)| l.as_str());
                let title = [
                    "typography.title()",
                    "typography.display()",
                    "Role::Heading",
                    "APP_NAME",
                    "pub fn title(",
                ];
                if !near.into_iter().any(|l| title.iter().any(|t| l.contains(t))) {
                    wrong.push(format!("{file}:{line_no}: the strong weight off a title"));
                }
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// Chrome context (a header's directory, the status bar's path, the palette's column, a
    /// row's second line) is in the UI face. The mono face is for a port's number, the
    /// settings file, and an address read to judge it (a held-back page's hint), and nothing
    /// else calls for it.
    #[test]
    fn the_mono_face_is_for_ports_and_the_settings_file() {
        let uses: Vec<String> = ["slopty-ui/src", "slopty-app/src"]
            .into_iter()
            .flat_map(chrome_lines)
            .filter(|(_, _, line)| {
                line.contains("mono_family(") && !line.contains("fn mono_family")
            })
            .map(|(file, line_no, _)| format!("{file}:{line_no}"))
            .collect();
        let allowed = |at: &String| {
            [
                "slopty-ui/src/workspace/tile.rs",
                "slopty-ui/src/settings_editor.rs",
                "slopty-ui/src/kit.rs",
            ]
            .iter()
            .any(|file| at.contains(file))
        };
        assert!(uses.iter().all(allowed), "mono outside ports and settings: {uses:#?}");
        assert_eq!(uses.len(), 3, "the port pill, the settings field, the address: {uses:#?}");
    }

    /// A list typed at (the palette, a picker) floats on the bare [`anchor`]; the modals that
    /// hold the window until answered (the settings, adding a worker) dim it with the
    /// [`backdrop`]. Both open at one place.
    #[test]
    fn a_list_floats_and_a_modal_dims() {
        let calls = |file: &str, call: &str| {
            let (ui, app) = (chrome_lines("slopty-ui/src"), chrome_lines("slopty-app/src"));
            ui.into_iter().chain(app).any(|(f, _, l)| f.ends_with(file) && l.contains(call))
        };
        for list in ["slopty-ui/src/palette.rs", "slopty-ui/src/picker.rs"] {
            assert!(calls(list, "kit::anchor("), "{list} lays out on the anchor");
            assert!(!calls(list, "kit::backdrop("), "{list} dims nothing");
        }
        for modal in ["slopty-ui/src/settings_editor.rs", "slopty-app/src/lib.rs"] {
            assert!(calls(modal, "kit::backdrop("), "{modal} is a modal");
        }
    }

    /// Under the touch density an icon button is a finger's 44 pt round the same icon, and the
    /// rows grow with it; under the compact one nothing moved from the sizes it replaced.
    #[test]
    fn density_sizes_the_targets_not_the_icons() {
        let mut theme = Theme::default();
        assert!((icon_button_side(&theme) - 24.0).abs() < f32::EPSILON, "compact: 16 + 2 × 4");
        assert!((Row::One.height(&theme) - 28.0).abs() < f32::EPSILON);
        assert!((Row::Two.height(&theme) - 40.0).abs() < f32::EPSILON);
        theme.density = slopty_theme::Density::TOUCH;
        assert!(icon_button_side(&theme) >= 44.0, "touch: a finger's target");
        assert!(Row::One.height(&theme) >= 44.0 && Row::Two.height(&theme) > 44.0);
        assert!(theme.typography.icon() < icon_button_side(&theme), "the icon stays its size");
    }

    /// The elevation reaches GPUI as the theme says: two layers of the shade, falling down, the
    /// soft one wider, and in dark a third, inset: white at `alpha::EDGE` along the top, the
    /// edge a dark sheet needs to be seen on a near-black window.
    #[test]
    fn the_elevation_is_two_layers_of_the_shade_and_a_lit_edge_in_dark() {
        for variant in [Variant::Dark, Variant::Light] {
            let theme = Theme::new(variant);
            let dark = variant == Variant::Dark;
            let layers = elevation(&theme);
            assert_eq!(layers.len(), 2 + usize::from(dark), "{variant:?}");
            let (drop, edge): (Vec<_>, Vec<_>) = layers.iter().partition(|l| !l.inset);
            assert!(drop.iter().all(|l| l.offset.y > px(0.0)));
            assert!(drop.first().map(|l| l.blur_radius) < drop.last().map(|l| l.blur_radius));
            let white = hsla_alpha(Rgb::hex(0xff_ffff), alpha::EDGE);
            assert!(edge.iter().all(|l| l.color == white && l.offset.y > px(0.0)), "{edge:?}");
            assert_eq!(edge.len(), usize::from(dark), "{variant:?}: the lit edge is dark's");
            assert!((scrim(&theme).a - theme.elevation.scrim).abs() < f32::EPSILON);
        }
    }

    /// A card rests: in light white with a hairline round it, a contact shadow and a shade
    /// along its bottom edge; in dark the hover wash, no shadow, light along its top edge, and
    /// a hairline only under Increase Contrast. A card cut into parts lights only the edge its
    /// rim is on and casts its contact only under the last part.
    #[test]
    fn a_card_rests_on_its_rim() {
        for variant in [Variant::Dark, Variant::Light] {
            let theme = Theme::new(variant);
            let s = theme.surfaces;
            let light = variant == Variant::Light;
            let mut whole = card(&theme);
            let style = whole.style();
            let fill = if light { hsla(s.elevated) } else { hsla(s.hover) };
            assert_eq!(style.background, Some(gpui::Fill::from(fill)), "{variant:?}");
            assert_eq!(style.border_widths.top.is_some(), light, "{variant:?}: ringed in light");
            let layers = style.box_shadow.clone().unwrap_or_default();
            let (drop, rims): (Vec<_>, Vec<_>) = layers.iter().partition(|l| !l.inset);
            assert_eq!(drop.len(), usize::from(light), "{variant:?}: a contact in light only");
            let [rim] = rims.as_slice() else { panic!("{variant:?}: one rim, {rims:?}") };
            assert_eq!(rim.offset.y > px(0.0), !light, "{variant:?}: lit from above in dark");
            assert!((rim.color.a - alpha::RIM).abs() < 1e-3, "{variant:?}: {:?}", rim.color);
            let lit = |first, last| {
                card_part(&theme, first, last)
                    .style()
                    .box_shadow
                    .clone()
                    .unwrap_or_default()
                    .iter()
                    .any(|l| l.inset)
            };
            assert_eq!((lit(true, false), lit(false, true)), (!light, light), "{variant:?}");
            assert!(!lit(false, false), "{variant:?}: a middle part has no rim");
        }
        let theme = Theme { contrast: slopty_theme::Contrast::Increased, ..Theme::default() };
        assert!(card(&theme).style().border_widths.top.is_some(), "ringed at more contrast");
    }

    /// The primary solid has its finish: a point inside its lit edge (the top on light's dark
    /// solid, the bottom on dark's white one) and a contact in light; held, the point goes and
    /// a shade presses in along its top.
    #[test]
    fn the_primary_presses_in() {
        for variant in [Variant::Dark, Variant::Light] {
            let theme = Theme::new(variant);
            let (rest, held) = solid_finish(&theme);
            let point = rest.iter().find(|l| l.inset).expect("a point");
            assert_eq!(point.offset.y > px(0.0), variant == Variant::Light, "{variant:?}");
            let contact = rest.iter().filter(|l| !l.inset).count();
            assert_eq!(contact, usize::from(variant == Variant::Light), "{variant:?}");
            let [shade] = held.as_slice() else { panic!("{variant:?}: one shade, {held:?}") };
            assert!(shade.inset && shade.offset.y > px(0.0), "pressed in from the top");
            let mut b = button(&theme, "b", "Go", ButtonKind::Primary);
            assert_eq!(
                b.style().box_shadow.as_ref(),
                Some(&rest),
                "{variant:?}: the button wears it"
            );
        }
    }

    /// The secondary button rests too: its hairline ring and the rim inside it.
    #[test]
    fn a_secondary_button_catches_the_light() {
        for variant in [Variant::Dark, Variant::Light] {
            let theme = Theme::new(variant);
            let mut b = button(&theme, "b", "Go", ButtonKind::Secondary);
            let layers = b.style().box_shadow.clone().unwrap_or_default();
            let rim = theme.elevation.rim;
            let rimmed = layers.iter().any(|l| {
                l.inset && l.spread_radius == px(0.0) && (l.offset.y > px(0.0)) == rim.top
            });
            assert!(rimmed, "{variant:?}: {layers:?}");
        }
    }

    /// The motion curves are the theme's: they start at rest and land, and ease out, ahead of
    /// a straight line half way.
    #[test]
    fn the_curves_ease_out_and_land() {
        let (out, sheet) = (ease_out(), drawer());
        let curves: [&dyn Fn(f32) -> f32; 2] = [&out, &sheet];
        for curve in curves {
            assert!(curve(0.0).abs() < 1e-3 && (curve(1.0) - 1.0).abs() < 1e-3);
            assert!(curve(0.5) > 0.5, "eased out: {}", curve(0.5));
        }
        assert_eq!(FADE, Motion::DEFAULT.fade);
    }

    /// A border drawn a full point by hand (`border_1()`, `border_t_1()`, a `Styled::border_b_1`
    /// handed on), or a box a point wide filled with a hairline's tint.
    fn thick_hairline(line: &str) -> Option<&'static str> {
        if line.trim_start().starts_with("//") {
            return None;
        }
        let squeezed: String = line.split_whitespace().collect();
        let preset = ["border_1()", "border_2()", "Styled::border_1", "Styled::border_2"]
            .iter()
            .any(|p| squeezed.contains(p))
            || ["t", "b", "l", "r", "x", "y"]
                .iter()
                .any(|side| squeezed.contains(&format!("border_{side}_1")));
        let boxed = (squeezed.contains(".h(px(1.0))") || squeezed.contains(".w(px(1.0))"))
            && squeezed.contains(".bg(hsla(")
            && squeezed.contains("border");
        (preset || boxed).then_some("a hairline a point wide, not `kit::hair` or `kit::rule`")
    }

    #[test]
    fn the_hairline_check_knows_a_hairline_from_a_point() {
        assert!(thick_hairline(".border_b_1()").is_some());
        assert!(thick_hairline(".border_1().border_color(hsla(s.border))").is_some());
        assert!(thick_hairline(".when(first, gpui::Styled::border_t_1)").is_some());
        assert!(thick_hairline("div().h(px(1.0)).bg(hsla(s.border_subtle))").is_some());
        assert!(thick_hairline(".border_b(kit::hair(theme))").is_none());
        assert!(thick_hairline(".border(px(slopty_theme::stroke::EDGE))").is_none(), "an edge");
        assert!(thick_hairline(".border_x_0()").is_none(), "no border");
        assert!(thick_hairline(".when(x, gpui::Styled::border_dashed)").is_none(), "a style");
        assert!(thick_hairline("// .border_1()").is_none(), "a comment");
    }

    /// Every border chrome draws is a hairline, one device pixel ([`hair`], a point under
    /// Increase Contrast), or a named stroke that marks something (`stroke::EDGE`,
    /// `stroke::MARK`). At a full point each rule was two pixels on a Retina screen.
    #[test]
    fn a_chrome_border_is_kit_hair() {
        // Call sites whose owners move them onto `kit::hair` in their next change.
        const AWAITING: [&str; 3] =
            ["slopty-ui/src/project/", "slopty-app/src/lib.rs", "slopty-app/src/ssh.rs"];
        let wrong: Vec<String> = ["slopty-ui/src", "slopty-app/src"]
            .into_iter()
            .flat_map(chrome_lines)
            .filter(|(file, ..)| {
                !file.ends_with("slopty-ui/src/kit.rs")
                    && !AWAITING.iter().any(|f| file.contains(f))
            })
            .filter_map(|(file, line_no, line)| {
                thick_hairline(&line).map(|why| format!("{file}:{line_no}: {why}: {}", line.trim()))
            })
            .collect();
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// Every hint keeps the warm group's timing: GPUI builds it at once ([`hint_timing`]) and
    /// the [`Hint`] holds the wait, so a tooltip built on GPUI's own half-second delay would
    /// wait twice and never join the group.
    #[test]
    fn a_hint_keeps_the_warm_timing() {
        // Call sites whose owners move them onto `kit::hint_timing` in their next change.
        const AWAITING: [&str; 1] = ["slopty-ui/src/project/"];
        let mut wrong = Vec::new();
        for dir in ["slopty-ui/src", "slopty-app/src"] {
            let lines = chrome_lines(dir);
            for (ix, (file, line_no, line)) in lines.iter().enumerate() {
                if !line.contains(".tooltip(") || AWAITING.iter().any(|f| file.contains(f)) {
                    continue;
                }
                let before = ix.checked_sub(1).and_then(|b| lines.get(b)).map(|l| l.2.as_str());
                if !line.contains("hint_timing")
                    && !before.unwrap_or_default().contains("hint_timing")
                {
                    wrong.push(format!("{file}:{line_no}: {}", line.trim()));
                }
            }
        }
        assert!(wrong.is_empty(), "a hint on GPUI's own delay:\n{}", wrong.join("\n"));
    }

    /// A state painted as a solid step mixed for the content (`raised`, `overlay`), where the
    /// washes (`hover`, `selected`, `pressed`) ride on whatever plane is under them.
    fn solid_state(line: &str) -> Option<&'static str> {
        let code = !line.trim_start().starts_with("//");
        let solid = ["s.raised", "surfaces.raised", "s.overlay", "surfaces.overlay"];
        (code && solid.iter().any(|t| line.contains(t)))
            .then_some("a state's solid step, not its wash (`hover`, `selected`, `pressed`)")
    }

    #[test]
    fn the_state_check_knows_a_step_from_a_wash() {
        assert!(solid_state(".hover(move |el| el.bg(hsla(s.raised)))").is_some());
        assert!(solid_state(".bg(hsla(theme.surfaces.overlay))").is_some());
        assert!(solid_state(".hover(move |el| el.bg(hsla(s.hover)))").is_none());
        assert!(solid_state("// `raised` was s.raised").is_none(), "a comment");
    }

    /// Hover, selection and press are washes of the ink over the plane under them, so a menu's
    /// rows, a card's and the navigator's answer the pointer alike, and a float can rise +5 L
    /// over the content with its rows' hover still showing.
    #[test]
    fn a_state_is_a_wash_over_its_plane() {
        const AWAITING: [&str; 3] =
            ["slopty-ui/src/project/", "slopty-app/src/lib.rs", "slopty-app/src/ssh.rs"];
        let wrong: Vec<String> = ["slopty-ui/src", "slopty-app/src"]
            .into_iter()
            .flat_map(chrome_lines)
            .filter(|(file, ..)| !AWAITING.iter().any(|f| file.contains(f)))
            .filter_map(|(file, line_no, line)| {
                solid_state(&line).map(|why| format!("{file}:{line_no}: {why}: {}", line.trim()))
            })
            .collect();
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// One size has one name: `small()` and `title()`, not `meta()` and `prose()`, which were
    /// the same sizes under second names, as a scale drifts.
    #[test]
    fn one_name_per_type_size() {
        const AWAITING: [&str; 2] = ["slopty-ui/src/project/", "slopty-app/src/lib.rs"];
        let wrong: Vec<String> = ["slopty-ui/src", "slopty-app/src"]
            .into_iter()
            .flat_map(chrome_lines)
            .filter(|(file, ..)| !AWAITING.iter().any(|f| file.contains(f)))
            .filter(|(.., line)| {
                ["typography.meta()", "typography.prose()"].iter().any(|n| line.contains(n))
            })
            .map(|(file, line_no, line)| format!("{file}:{line_no}: {}", line.trim()))
            .collect();
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// A painted hairline is whole device pixels, never under one, as GPUI snaps a border: one
    /// at 1x, 2x and 3x, and a point's worth under Increase Contrast.
    #[test]
    fn a_painted_hairline_is_one_device_pixel() {
        let mut theme = Theme::default();
        for scale in [1.0_f32, 2.0, 3.0] {
            let device = f32::from(hair_painted(&theme, scale)) * scale;
            assert!((device - 1.0).abs() < 1e-4, "{scale}x: {device} device pixels");
        }
        theme.contrast = slopty_theme::Contrast::Increased;
        let device = f32::from(hair_painted(&theme, 2.0)) * 2.0;
        assert!((device - 2.0).abs() < 1e-4, "a point at 2x: {device}");
    }

    /// The ruling in `docs/decisions/ui.md`, as a check rather than a paragraph: chrome takes
    /// its transparencies from `alpha` and wears one elevation. A raw opacity or a second
    /// shadow is how a design system becomes a pile of one-offs, so neither compiles.
    #[test]
    fn chrome_paints_from_the_tokens() {
        // Every step is written `0.` or `1.0`, so a digit after the comma is a raw opacity.
        let raw_alpha = |line: &str| {
            line.contains("hsla_alpha(")
                && (line.contains(", 0.") || line.contains(", 1.0") || line.contains("{ 1.0 }"))
        };
        let mut wrong = Vec::new();
        for dir in ["slopty-ui/src", "slopty-app/src"] {
            for (file, line_no, line) in chrome_lines(dir) {
                if raw_alpha(&line) {
                    wrong.push(format!("{file}:{line_no}: a raw opacity, not an `alpha` step"));
                }
                for other in ["shadow_md()", "shadow_lg()", "shadow_xl()", "shadow_2xl()"] {
                    if line.contains(other) {
                        wrong.push(format!("{file}:{line_no}: a second elevation ({other})"));
                    }
                }
                if let Some(call) = literal_spacing(&line) {
                    wrong.push(format!("{file}:{line_no}: {call} off the spacing scale"));
                }
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// The string literals on a line of Rust, escapes left as written.
    fn literals(line: &str) -> Vec<String> {
        let code = line.trim_start();
        if code.starts_with("//") {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut current: Option<String> = None;
        let mut escaped = false;
        for c in code.chars() {
            match current.as_mut() {
                None if c == '"' => current = Some(String::new()),
                None => {}
                Some(text) if escaped => {
                    text.push(c);
                    escaped = false;
                }
                Some(text) if c == '\\' => {
                    text.push(c);
                    escaped = true;
                }
                Some(_) if c == '"' => out.extend(current.take()),
                Some(text) => text.push(c),
            }
        }
        out
    }

    /// A chord written into chrome text: a literal holding a modifier glyph and anything
    /// else. A lone glyph is a key cap on the phone's bar, not a chord.
    fn literal_chord(line: &str) -> Option<String> {
        literals(line)
            .into_iter()
            .find(|text| text.contains(['⌘', '⌥', '⌃', '⇧']) && text.chars().count() > 1)
    }

    /// Where a string literal on a line is drawn as chrome text or read out as an accessible
    /// name: the first literal after one of these calls. The helpers whose first argument is
    /// an element id (`pill`, a text button, a key cap) draw a later literal instead.
    const DRAWN: [&str; 8] = [
        ".child(\"",
        "ChromeText::new(\"",
        ".aria_label(\"",
        ".placeholder(\"",
        "muted_line(\"",
        "picture_wait(\"",
        "notice(\"",
        "title(\"",
    ];
    const DRAWN_AFTER_ID: [&str; 5] = ["pill(", "button(", "heading(", "key_cap(", "bar_key("];
    /// The helpers whose first argument is the theme draw the first literal after it.
    const DRAWN_AFTER_THEME: [&str; 2] = ["kit::label(", "kit::title("];

    /// A literal drawn as chrome text that starts lowercase: `"take"` on a pill, `"opening…"`
    /// in a body. Sentence case is checked on the text drawn, not only on the names a screen
    /// reader reads, so a lowercase label cannot come back unseen.
    fn lowercase_label(line: &str) -> Option<String> {
        let code = line.trim_start();
        if code.starts_with("//") {
            return None;
        }
        // A word, not a name: a file (`settings.toml`), a host (`mac-studio`) or a number is
        // written as it is spelled.
        let lower = |text: &String| {
            let word = text.split_whitespace().next().unwrap_or_default();
            word.chars().next().is_some_and(char::is_lowercase)
                && !word.contains(['.', '-', '_', '/'])
                && !word.contains(|c: char| c.is_ascii_digit())
        };
        let drawn = DRAWN.iter().filter_map(|call| {
            line.split_once(call)
                .and_then(|(_, rest)| literals(&format!("\"{rest}")).into_iter().next())
        });
        let after_id = DRAWN_AFTER_ID.iter().flat_map(|call| {
            line.split_once(call)
                .map(|(_, rest)| literals(rest).into_iter().skip(1).collect::<Vec<_>>())
                .unwrap_or_default()
        });
        let after_theme = DRAWN_AFTER_THEME.iter().filter_map(|call| {
            line.split_once(call).and_then(|(_, rest)| literals(rest).into_iter().next())
        });
        drawn.chain(after_id).chain(after_theme).find(lower)
    }

    /// Whether `text` names a computer as a worker: "worker" or "workers", any case, as a word
    /// of its own, not part of an identifier (`slopty-worker`, `{worker}`, `nav-worker-`).
    fn says_worker(text: &str) -> bool {
        let edge =
            |c: Option<char>| c.is_none_or(|c| !(c.is_alphanumeric() || "_-.{}".contains(c)));
        let lower = text.to_lowercase();
        lower.match_indices("worker").any(|(at, word)| {
            let before = lower.get(..at).and_then(|b| b.chars().next_back());
            let rest = lower.get(at.saturating_add(word.len())..).unwrap_or_default();
            let rest = rest.strip_prefix('s').unwrap_or(rest);
            edge(before) && edge(rest.chars().next())
        })
    }

    /// The person's word for a computer Slopty runs on is "machine", as the tailnet's is: no
    /// string the workspace's chrome or the palette shows says "worker". Code, wire and process
    /// names keep it. A literal shaped as a key (lowercase, no space: an element's id, a
    /// `userInfo` key, a selector's part) is not shown, nor is a `Debug` name or a log line.
    #[test]
    fn chrome_says_machine_not_worker() {
        assert!(says_worker("Add a worker") && says_worker("Workers") && says_worker("workers'"));
        assert!(!says_worker("slopty-worker") && !says_worker("on {worker}"));
        assert!(!says_worker("nav-worker-{key}") && !says_worker("workerd"));
        let mut lines = chrome_lines("slopty-ui/src/workspace");
        for file in ["workspace.rs", "palette.rs", "keymap.rs"] {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(file);
            let text = std::fs::read_to_string(&path).expect("a source file reads");
            let name = path.display().to_string();
            let kept = text.lines().take_while(|l| l.trim() != "#[cfg(test)]");
            lines.extend(kept.enumerate().map(|(ix, l)| (name.clone(), ix + 1, l.to_owned())));
        }
        assert!(lines.iter().any(|(_, _, l)| l.contains("\"Machines\"")), "the chrome is scanned");
        let unshown = ["tracing::", ".id(", "debug_tuple(", "debug_struct(", ".field("];
        let mut said = Vec::new();
        for (file, line_no, line) in lines {
            let code = line.trim_start();
            if code.starts_with("//") || unshown.iter().any(|u| line.contains(u)) {
                continue;
            }
            let shown = line.split('"').skip(1).step_by(2).filter(|text| {
                let key = text.chars().all(|c| c.is_ascii_lowercase() || "._-".contains(c));
                !key && says_worker(text)
            });
            said.extend(shown.map(|text| format!("{file}:{line_no}: {text:?}")));
        }
        assert!(said.is_empty(), "say machine, not worker:\n{}", said.join("\n"));
    }

    /// Chrome text is drawn in sentence case, as the constants above are written.
    #[test]
    fn a_drawn_label_is_sentence_case() {
        let wrong: Vec<String> = ["slopty-ui/src", "slopty-app/src"]
            .into_iter()
            .flat_map(chrome_lines)
            .filter_map(|(file, line_no, line)| {
                lowercase_label(&line).map(|text| format!("{file}:{line_no}: lowercase: {text:?}"))
            })
            .collect();
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    #[test]
    fn the_label_check_knows_a_label_from_an_id() {
        assert!(lowercase_label(r#"pill("take", id, "take", tone, theme, chrome)"#).is_some());
        assert!(lowercase_label(r#"pill("take", id, TAKE, tone, theme, chrome)"#).is_none());
        assert!(lowercase_label(r#"muted_line("attaching…".into())"#).is_some());
        assert!(lowercase_label(r#".child("Nothing new")"#).is_none());
        assert!(lowercase_label(r#"button(copy_id, "Copy code", "copy")"#).is_some());
        assert!(lowercase_label(r#"button(copy_id, "Copy code", "Copy")"#).is_none());
        assert!(lowercase_label(r#"kit::button(theme, "new-shell", "New shell", kind)"#).is_none());
        assert!(lowercase_label(r#".aria_label("mute")"#).is_some());
        assert!(lowercase_label(r#"kit::label(theme, "workers")"#).is_some());
        assert!(lowercase_label(r#"kit::label(theme, "Workers")"#).is_none());
        assert!(lowercase_label(r#".key_cap("key-find".to_owned(), "find", on, small)"#).is_some());
        assert!(
            lowercase_label(r#"self.bar_key(format!("skey-{label}"), label, lit, f)"#).is_none()
        );
        assert!(lowercase_label(r#".id("file-bar").child(text)"#).is_none(), "an id");
        assert!(lowercase_label(r#"// .child("a comment")"#).is_none());
        assert!(lowercase_label(r#".aria_label("settings.toml")"#).is_none(), "a file name");
        assert!(lowercase_label(r#".placeholder("mac-studio or 100.64.0.3")"#).is_none(), "a host");
    }

    /// The ruling in `docs/decisions/ui.md`: keys live in the palette, the menus and the
    /// hints, all of which read them from the binding tables through `palette::keys_for`.
    /// A chord typed into a button, an empty state or a menu row by hand drifts from the
    /// binding (the "+" menu said `⌘⇧T` while the palette said `⇧⌘T`) and puts keys where
    /// the palette should be, so none compiles.
    #[test]
    fn a_chord_is_spelled_only_by_the_key_tables() {
        let mut wrong = Vec::new();
        for dir in ["slopty-ui/src", "slopty-app/src"] {
            for (file, line_no, line) in chrome_lines(dir) {
                if let Some(text) = literal_chord(&line) {
                    wrong.push(format!("{file}:{line_no}: a chord written by hand: {text:?}"));
                }
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    #[test]
    fn the_chord_check_knows_a_chord_from_a_key_cap() {
        assert!(literal_chord(r#"button("Save", "⌘↩", true)"#).is_some());
        assert!(literal_chord(r#".child("⌘T opens a shell")"#).is_some());
        assert!(literal_chord(r#"("⌘", "cmd", None),"#).is_none(), "a key cap");
        assert!(literal_chord("/// ⌘⌥← walks the columns").is_none(), "a comment");
        assert!(literal_chord(r#"out.push('⌘'); label("a")"#).is_none(), "a char, not text");
        assert!(literal_chord(r#"f("a\"b", "⇧x")"#).is_some(), "past an escaped quote");
    }

    /// The spacing check catches what it is for and leaves alone what it is not.
    ///
    /// A lint that cannot fail is worse than no lint, because it reads like cover.
    #[test]
    fn the_spacing_check_knows_a_pad_from_a_measurement() {
        assert!(literal_spacing(".p(px(7.0))").is_some());
        assert!(literal_spacing("div().gap(px(10.0)).child(x)").is_some());
        assert!(literal_spacing(".py(px(3.))").is_some());
        // Taken from the scale: the whole point.
        assert!(literal_spacing(".p(px(theme.spacing.md))").is_none());
        assert!(literal_spacing(".gap(px(s.xs))").is_none());
        // A measurement of something real, not a rhythm.
        assert!(literal_spacing(".w(px(300.0))").is_none());
        assert!(literal_spacing(".border_b(px(1.0))").is_none());
        assert!(literal_spacing(".size(px(16.0))").is_none());
        // `.pr(` must not be found inside `.appear(` or any other word ending in those letters.
        assert!(literal_spacing("something.expr(px(4.0))").is_none());
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

    /// A block that slides 8 pt into place as it fades in.
    struct Sliding;

    impl Render for Sliding {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let block = div().debug_selector(|| "sliding".to_owned()).size(px(10.0));
            div().child(slide_fade(block, "sliding", 8.0, Pace::Fade, cx))
        }
    }

    /// Where the sliding block's top is on the first frame drawn.
    fn first_top(cx: &mut TestAppContext) -> f32 {
        let (_view, cx) = cx.add_window_view(|_, _| Sliding);
        cx.run_until_parked();
        f32::from(cx.debug_bounds("sliding").expect("drawn").top())
    }

    /// Something that slides in starts below its place and lands there; under Reduce Motion it
    /// is in its place on the first frame.
    #[gpui::test]
    fn a_slide_starts_off_its_place_and_holds_still_under_reduce_motion(cx: &mut TestAppContext) {
        assert!(first_top(cx) > 4.0, "the first frame is below its place");
        cx.update(|cx| cx.set_reduce_motion(true));
        assert!(first_top(cx).abs() < 0.5, "in place at once");
    }

    /// Each pace is the theme's: a fade, an exit and a settle ease out, a sheet takes the
    /// drawer's curve, an exit is the quickest and only a sheet takes longer than a settle.
    #[test]
    fn the_paces_are_the_motion_tokens() {
        let m = Motion::DEFAULT;
        assert_eq!(Pace::Fade.duration(), m.fade);
        assert_eq!(Pace::Settle.duration(), m.settle);
        assert_eq!(Pace::Sheet.duration(), m.sheet);
        assert_eq!(Pace::Fade.curve(), m.ease_out);
        assert_eq!(Pace::Settle.curve(), m.ease_out);
        assert_eq!(Pace::Sheet.curve(), m.drawer);
        assert_eq!(Pace::Exit.duration(), m.exit);
        assert_eq!(Pace::Exit.curve(), m.ease_out);
        assert!(Pace::Exit.duration() < Pace::Fade.duration(), "out quicker than in");
        assert!(Pace::Fade.duration() < Pace::Settle.duration());
        assert!(Pace::Settle.duration() < Pace::Sheet.duration());
    }

    /// A diff's size leaves out the side that is zero, and says nothing for no change.
    #[test]
    fn a_diff_size_drops_its_zero_side() {
        assert_eq!(changes_text(2, 1).as_deref(), Some("+2\u{2009}\u{2012}1"), "a figure dash");
        assert_eq!(changes_text(2, 0).as_deref(), Some("+2"), "no red zero");
        assert_eq!(changes_text(0, 3).as_deref(), Some("\u{2012}3"));
        assert_eq!(changes_text(0, 0), None);
        let theme = Theme::default();
        assert!(changes(&theme, 0, 0).is_none(), "nothing drawn for no change");
        assert!(changes(&theme, 1, 0).is_some());
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

    /// Every icon the chrome names is one Hugeicons draws: one gpui-kit's Lucide bundle backs
    /// would be the one glyph on screen at another hand and weight.
    #[test]
    fn every_icon_the_chrome_names_is_drawn_by_one_set() {
        let by_name: std::collections::HashMap<String, SharedString> =
            crate::icons::IconName::ALL.iter().map(|i| (format!("{i:?}"), i.path())).collect();
        let mut stray = Vec::new();
        for dir in ["slopty-ui/src", "slopty-app/src"] {
            for (file, line, text) in chrome_lines(dir) {
                for rest in text.split("IconName::").skip(1) {
                    let name: String =
                        rest.chars().take_while(char::is_ascii_alphanumeric).collect();
                    let Some(path) = by_name.get(&name) else { continue };
                    if !crate::icons::drawn(path) {
                        stray.push(format!("{file}:{line}: {name}"));
                    }
                }
            }
        }
        assert!(stray.is_empty(), "drawn by the Lucide fallback:\n{}", stray.join("\n"));
    }
}

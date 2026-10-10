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
use slopty_theme::{Motion, Rgb, Theme, Typography, Variant, alpha, stroke};

use crate::colors::{hsla, hsla_alpha};

mod change;
mod disclosure;
mod facts;
pub mod find;
mod fit;
mod identity;
pub mod menu;
pub mod message;
mod pane;
mod press;
mod priority;
pub mod progress;
mod room;
mod shimmer;
mod spark;
pub use change::{Gliding, Rolling, on_change};
pub use disclosure::Disclosure;
pub use facts::{FactAt, FactsRow, MeasuredFacts, facts_row, wrap_facts};
pub use find::FindBar;
pub use fit::{FitLabel, fit_label};
pub use identity::{identity_ink, machine_ink, wears_identity};
pub use menu::{Menu, MenuItem, MenuPanel};
pub use pane::{pane_surface, sash};
pub use press::menu_press;
pub use priority::{Dropped, Measured, Priority, PriorityRow, TitleFit, fit_row, priority_row};
pub use room::{Room, room_query};
pub use shimmer::{Shimmer, shimmer};
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

/// A sheet's curve: [`Motion::drawer`], a phone's palette sheet, the iPad's drawer.
pub fn drawer() -> impl Fn(f32) -> f32 {
    move |t| Motion::DEFAULT.drawer.at(t)
}

/// How the words an agent streams lift in.
///
/// Paced to the stream between [`Motion::fade`] and [`Motion::stream`], word after word
/// [`Motion::stream_stagger`] apart, on the ease-out curve. The first words of an answer, before
/// there is a pace to follow, lift over the longest. gpui-kit holds it still under Reduce Motion.
#[must_use]
pub fn stream_motion() -> gpui_kit::component::text::TextViewMotion {
    let m = Motion::DEFAULT;
    let ((x1, y1), (x2, y2)) = (m.ease_out.p1, m.ease_out.p2);
    gpui_kit::component::text::TextViewMotion::default()
        .with_stream_fade(m.stream)
        .with_stream_fade_pacing(m.fade, m.stream)
        .with_stream_fade_stagger(m.stream_stagger)
        .with_stream_fade_easing(gpui_kit::base::motion::Easing::CubicBezier { x1, y1, x2, y2 })
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
    /// A toast, a notice or a card arriving: [`Motion::toast`], eased out. Menus, popovers and
    /// the palette have no pace: they open on their first frame.
    Toast,
    /// A new pane sliding in from the edge it opened at: [`Motion::pane`], eased out.
    Pane,
    /// A prompt the person sent rising into its place: [`Motion::reveal`], eased out.
    Reveal,
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
            Self::Toast => m.toast,
            Self::Pane => m.pane,
            Self::Reveal => m.reveal,
        }
    }

    /// The curve it follows.
    #[must_use]
    pub const fn curve(self) -> slopty_theme::Curve {
        let m = Motion::DEFAULT;
        match self {
            Self::Fade | Self::Settle | Self::Exit | Self::Toast | Self::Pane | Self::Reveal => {
                m.ease_out
            }
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
/// Into a hover or a press and back to rest over [`Motion::feedback`], eased out. Only colours
/// ease; the state itself, and layout, change at once, and under Reduce Motion GPUI applies every
/// change at once. Every kit control that answers the pointer wears it, so the durations live here
/// and nowhere else.
///
/// Not on the rows of a list GPUI composites as a scroll layer (the navigator's): a fill easing
/// out under rows that scroll past the pointer repaints the layer, and measured headless it
/// composited 2 of 30 scroll frames where the list composites all 30
/// (`nav_list::a_scroll_of_the_navigator_composites_its_layer`).
pub fn eased<E: gpui::StatefulInteractiveElement>(el: E) -> E {
    let m = Motion::DEFAULT;
    el.transition(gpui::StateTransition::new(m.feedback).enter(m.feedback).with_easing(ease_out()))
}

/// A surface that comes and goes (a menu, a popover, a dialog), drawn as present as it is.
///
/// One value per surface, eased toward whole over [`Pace::Fade`] while it is open and toward
/// clear over [`Pace::Exit`] once it is closed.
///
/// The value is gpui-fast's `ValueTransition`, kept under `id` while the surface is drawn, so a
/// surface opened again while it leaves turns back from where it stands, in the time the way
/// back takes. Its owner keeps drawing it for [`exit_time`] after closing it, under the same
/// `id` and parents it was open under. While it leaves it takes no pointer: a blocker over it
/// holds the clicks within its own bounds. Under Reduce Motion it comes and goes at once.
pub struct Presence<E> {
    id: gpui::ElementId,
    open: bool,
    enters: bool,
    travel: f32,
    el: E,
}

impl<E> std::fmt::Debug for Presence<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Presence")
            .field("id", &self.id)
            .field("open", &self.open)
            .field("enters", &self.enters)
            .field("travel", &self.travel)
            .finish_non_exhaustive()
    }
}

/// `el` as present as its owner says: open, or leaving ([`Presence`]).
pub fn presence<E>(el: E, id: impl Into<gpui::ElementId>, open: bool) -> Presence<E> {
    Presence { id: id.into(), open, enters: true, travel: 0.0, el }
}

impl<E> Presence<E> {
    /// Arriving whole in its first frame, as what a key summons does; it still leaves, and
    /// turns back, eased.
    #[must_use]
    pub const fn arrives_whole(mut self, whole: bool) -> Self {
        self.enters = !whole;
        self
    }

    /// Travelling `from` points below its place (above, when negative) on its way in, and back
    /// on its way out, beside its opacity. It is moved as a `relative` element by its `top`, so
    /// nothing round it shifts.
    #[must_use]
    pub const fn travel(mut self, from: f32) -> Self {
        self.travel = from;
        self
    }
}

impl<E: IntoElement + Styled + gpui::ParentElement + 'static> IntoElement for Presence<E> {
    type Element = gpui::ViewElement<Self>;

    #[track_caller]
    fn into_element(self) -> Self::Element {
        gpui::ViewElement::new(self)
    }
}

impl<E: IntoElement + Styled + gpui::ParentElement + 'static> gpui::RenderOnce for Presence<E> {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let pace = if self.open { Pace::Fade } else { Pace::Exit };
        let curve = pace.curve();
        let start = if self.enters { 0.0_f32 } else { 1.0 };
        let value = window
            .use_keyed_transition(self.id, cx, pace.duration(), move |_, _| start)
            .with_easing(move |t| curve.at(t));
        value.set_goal(if self.open { 1.0 } else { 0.0 }, cx);
        let shown = value.evaluate(window, cx);
        let travel = self.travel;
        // Whole, the surface is drawn as it is, with no layer at full opacity over it.
        let moving = shown < 1.0;
        self.el
            .when(moving, |el| el.opacity(shown))
            .when(moving && travel != 0.0, |el| el.relative().top(px(travel * (1.0 - shown))))
            .when(!self.open, |el| el.child(div().absolute().inset_0().occlude()))
    }
}

/// How long a dismissed overlay stays drawn while it leaves.
///
/// [`Pace::Exit`], or `None` under Reduce Motion, where it goes on the frame it is dismissed.
/// Its owner keeps drawing it
/// through [`Presence`] for this long and then drops it; the keyboard has already gone back.
#[must_use]
pub fn exit_time(cx: &App) -> Option<std::time::Duration> {
    motion(cx).then(|| Pace::Exit.duration())
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
/// navigator row and the breadcrumb. A figure all in red read as an error, and a red "‒0" as
/// an error about nothing. The caller sets the size and adds an identity and a spoken label.
///
/// Its parts are text, the thin space between the sides included, so they take the size the
/// caller sets.
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
/// words that act (Take, Mute) wear it bare, so they stand as tall as the state's [`pill`]
/// beside them and their hover takes the same chip. Key caps keep their 4 pt corners: they
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

/// A state's pill: [`pill_frame`] filled with `tone` at
/// [`pill_fill`], its words in `tone` at the medium weight.
///
/// A header holds one of these at most, the state's (an agent waiting, working), so it is the
/// one shape there that stands out. The caller adds the identity, the role and the words.
#[must_use]
pub fn pill(theme: &Theme, tone: Rgb) -> Div {
    pill_frame(theme)
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

/// `n` and a noun, plural past one: "1 file", "3 files".
#[must_use]
pub fn count(n: u64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
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

/// A dialog's shadow: Zed's modal surface's four layers ([`slopty_theme::Elevation::dialog`]),
/// so the one thing holding the window stands above every float.
#[must_use]
pub fn dialog_elevation(theme: &Theme) -> Vec<BoxShadow> {
    theme
        .elevation
        .dialog
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

/// How far a sunk thing's shade reaches in past its border: one point.
const SUNK_DEPTH: f32 = 1.0;

/// `el` raised off its plane at rest: what stands up from where it lies (a card, a tray's head,
/// a notice's disc, a chip over a body).
///
/// The card's wash (ink at [`alpha::CARD`]) inside the `border` line, with no shadow: on one
/// neutral ground a resting thing is told by its edge, as `MonoCode`'s cards are. A hover or
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
    let el = el.bg(hsla(theme.surfaces.card)).border_l(HAIR).border_r(HAIR);
    let el = if first { el.border_t(HAIR) } else { el };
    let el = if last { el.border_b(HAIR) } else { el };
    el.border_color(hsla(theme.surfaces.border))
}

/// `el` set into its plane as a quiet well: code, a command's output, an attachment's chip, the
/// person's own message, the foot of a sheet. The card's wash, with no edge.
#[must_use]
pub fn inset<E: Styled>(el: E, theme: &Theme) -> E {
    el.bg(hsla(theme.surfaces.card))
}

/// `el` drawn as a field's ground: the card's wash inside the `border` line, sunk ([`sunk`]) so
/// it reads as somewhere to type rather than something to press.
#[must_use]
pub fn field<E: Styled>(el: E, theme: &Theme) -> E {
    let el =
        el.bg(hsla(theme.surfaces.card)).border(HAIR).border_color(hsla(theme.surfaces.border));
    sunk(el, theme, stroke::LINE)
}

/// A search field in chrome: a field a row tall in the hover's wash, at `radii.sm`, a field's
/// radius.
///
/// The magnifier leads at the icon size in the muted ink, then whatever the caller puts in it (a
/// scope's token, the input, a way to clear it).
///
/// It is chrome, not a document's field: no ring and no sunk shade, which made the navigator's
/// filter the hardest-edged thing beside the traffic lights. While it holds the keyboard
/// (`focused`) the focus green's 1 pt line runs just inside its edge, as Zed's focused field's
/// border does.
#[must_use]
pub fn search_field(theme: &Theme, focused: bool) -> Div {
    let (s, spacing) = (theme.surfaces, theme.spacing);
    div()
        .bg(hsla(s.hover))
        .when(focused, |el| el.shadow(vec![ring_inside(s.focus)]))
        .h(px(theme.density.row))
        .px(px(spacing.sm))
        .flex()
        .items_center()
        .gap(px(spacing.xs + spacing.xxs))
        .rounded(px(theme.radii.sm))
        .child(crate::icons::icon(
            theme,
            crate::icons::Symbol::Magnifyingglass,
            crate::icons::IconSize::Inline,
            hsla(s.text_muted),
        ))
}

/// A card: [`raised`] at `radii.md`, Zed's resting card's 6, the caller adding the identity and
/// the padding.
///
/// It is what rests on a plane and holds a thing of its own: a board's task, a settings group,
/// an expanded tool group, a question's option.
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
    let r = px(theme.radii.md);
    raised_part(div(), theme, first, last)
        .when(first, |el| el.rounded_t(r))
        .when(last, |el| el.rounded_b(r))
}

/// `el` sunk into its plane ([`slopty_theme::Sunk`]).
///
/// Shade is held inside its top edge, a point deep past a border `border` wide (0 for none).
/// Fields, the segmented tracks and meters wear it.
pub fn sunk<E: Styled>(el: E, theme: &Theme, border: f32) -> E {
    el.shadow(vec![BoxShadow {
        color: hsla_alpha(theme.elevation.shade, theme.elevation.sunk.shade),
        offset: point(px(0.0), px(border + SUNK_DEPTH)),
        blur_radius: px(0.0),
        spread_radius: px(0.0),
        inset: true,
    }])
}

/// A segmented control's track, `MonoCode`'s: the `border` ring at `radii.sm` on its plane, its
/// options held [`TRACK_PAD`] in. The thumb ([`paint_thumb`]) rides in it.
#[must_use]
pub fn track(theme: &Theme) -> Div {
    div()
        .relative()
        .flex()
        .items_center()
        .p(px(TRACK_PAD))
        .border(HAIR)
        .border_color(hsla(theme.surfaces.border))
        .rounded(px(theme.radii.sm))
}

/// How far a segmented control's track holds its options in from its edge.
pub const TRACK_PAD: f32 = 2.0;

/// A segmented control's thumb at `bounds`: the selected wash, `MonoCode`'s chosen segment,
/// with no ring and nothing raised.
///
/// At [`thumb_radius`]. The selection plate paints it as it slides from option to option.
pub fn paint_thumb(theme: &Theme, bounds: gpui::Bounds<gpui::Pixels>, window: &mut Window) {
    let radius = gpui::Corners::all(px(thumb_radius(theme)));
    window.paint_quad(gpui::fill(bounds, hsla(theme.surfaces.selected)).corner_radii(radius));
}

/// A segmented thumb's radius: the least, 4, inside its track's 6 (`MonoCode`'s 5).
#[must_use]
pub const fn thumb_radius(theme: &Theme) -> f32 {
    theme.radii.xs
}

/// `el` lifted off the chrome: the `elevated` surface, the `border` line and the float's shadow.
///
/// Everything that floats wears it: the palette, a menu, a popover, a hint, a find bar, a chip
/// over a body. The caller keeps its own radius: `radii.lg` for a menu, a popover or a toast,
/// the control's own for a hint or a chip. A dialog takes [`dialog`].
#[must_use]
pub fn elevate<E: Styled>(el: E, theme: &Theme) -> E {
    el.bg(hsla(theme.surfaces.elevated))
        .border(HAIR)
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

/// `el` as a modal's sheet: [`elevate`]d with the dialog's own shadow ([`dialog_elevation`]),
/// at `radii.xl`, a dialog's radius. [`dialog`] wears it, and so does a modal that lays itself
/// out (adding a worker).
#[must_use]
pub fn modal<E: Styled>(el: E, theme: &Theme) -> E {
    elevate(el, theme).shadow(dialog_elevation(theme)).rounded(px(theme.radii.xl))
}

/// The shell every overlay wears: [`elevate`]d with the dialog's own shadow, the UI font, at
/// `radii.lg` for a list typed at (every float's radius) and `radii.xl` for a page.
///
/// `min_w_0` so an unwrapped title cannot hold the box wider than a phone, and `min_h_0` so it
/// gives up height to what is under the [`backdrop`] (a phone's keyboard and key bar) rather
/// than run under it: its list scrolls, and its field and its foot stay in view.
///
/// The caller adds the identity, the accessibility role and label, and the children.
#[must_use]
pub fn dialog(theme: &Theme, size: Overlay) -> Div {
    let (w, h) = size.bounds();
    let radius = match size {
        Overlay::List => theme.radii.lg,
        Overlay::Editor => theme.radii.xl,
    };
    modal(div(), theme)
        .rounded(px(radius))
        .w_full()
        .min_w_0()
        .max_w(px(w))
        .max_h(px(h))
        .min_h_0()
        .mb(px(theme.spacing.xl))
        .flex()
        .flex_col()
        .text_size(px(theme.roles().chrome.size))
        .font_family(theme.typography.ui_family.clone())
        .text_color(hsla(theme.surfaces.text))
        .on_mouse_down(gpui::MouseButton::Left, |_ev, _window, cx| cx.stop_propagation())
}

/// A dialog's or a panel's title: the panel title role (15/20 at the strong weight), in `text`.
///
/// The strong weight is for titles like this one and for headings; a row, a tab or a name that
/// has to stand out takes the medium weight.
#[must_use]
pub fn title(theme: &Theme, text: impl Into<SharedString>) -> Div {
    typed(div(), theme.roles().panel_title).text_color(hsla(theme.surfaces.text)).child(text.into())
}

/// How loud a [`button`] is. One primary per surface; the rest are secondary, or ghost where
/// a fill would crowd a bar.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ButtonKind {
    /// The one action the surface is for: the neutral [`solid`], white on dark and near-black
    /// on light, as `MonoCode`'s stop and Commit buttons are.
    Primary,
    /// The press that removes or ends something for good (Remove a machine): the
    /// [`destructive`] red under the solid's ink, behind a confirm. Never a surface's only way
    /// on, and never beside a [`Self::Primary`].
    Destructive,
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
/// send and stop square, a ticked box, a switch that is on, a key that is armed.
///
/// One per surface at most: two solids side by side leave nothing to say which leads.
pub fn solid<E: Styled>(el: E, theme: &Theme) -> E {
    let s = theme.surfaces;
    el.bg(hsla(s.solid)).text_color(hsla(s.solid_ink))
}

/// [`solid`] that answers the pointer, flat: under the pointer the solid gives a little toward
/// the content and while pressed more, with no shadow and no scale, as `MonoCode`'s are.
pub fn solid_pressable(el: gpui::Stateful<Div>, theme: &Theme) -> gpui::Stateful<Div> {
    let (hovered, pressed) = solid_states(theme);
    eased(solid(el, theme))
        .hover(move |el| el.bg(hsla(hovered)))
        .active(move |el| el.bg(hsla(pressed)))
}

/// The solid under the pointer and pressed: given [`alpha::FAINT`] and [`alpha::DIM`] toward
/// the content. The theme's tests hold the solid's ink to AA on the pressed one.
fn solid_states(theme: &Theme) -> (Rgb, Rgb) {
    let (solid, content) = (theme.surfaces.solid, theme.content());
    (solid.mix(content, alpha::FAINT), solid.mix(content, alpha::DIM))
}

/// `el` tinted as the one press that removes or ends something for good.
///
/// The error's wash at [`alpha::TINTED`] under the error's word, as `MonoCode`'s destructive
/// buttons are. The theme's tests hold the word to AA on the wash over every plane.
pub fn destructive<E: Styled>(el: E, theme: &Theme) -> E {
    let s = theme.surfaces;
    el.bg(hsla_alpha(s.error_fill, alpha::TINTED)).text_color(hsla(s.error))
}

/// [`destructive`] that answers the pointer: the wash deepens to [`alpha::TINTED_HOVER`] under
/// the pointer and while pressed, flat as the solid is.
pub fn destructive_pressable(el: gpui::Stateful<Div>, theme: &Theme) -> gpui::Stateful<Div> {
    let fill = theme.surfaces.error_fill;
    eased(destructive(el, theme))
        .hover(move |el| el.bg(hsla_alpha(fill, alpha::TINTED_HOVER)))
        .active(move |el| el.bg(hsla_alpha(fill, alpha::TINTED_HOVER)))
}

/// `el` drawn as a secondary button ([`ButtonKind::Secondary`]): clear at rest inside the
/// `border` line, the selected wash under the pointer, the pressed one while held, so a press
/// reads apart from a hover.
pub fn secondary(el: gpui::Stateful<Div>, theme: &Theme) -> gpui::Stateful<Div> {
    let s = theme.surfaces;
    eased(el)
        .border(HAIR)
        .border_color(hsla(s.border))
        .text_color(hsla(s.text))
        .hover(move |el| el.bg(hsla(s.selected)))
        .active(move |el| el.bg(hsla(s.pressed)))
}

/// `el` drawn as the selected row of a list, live while that list has the keyboard.
///
/// `focused`: where the keyboard is, as Zed's is, the keyed wash
/// ([`slopty_theme::Surfaces::keyed`], ink 14 % dark, 12 % light) with the focus green's 1 pt line
/// just inside its edge, the keyboard's cursor. A list without the keyboard keeps its selection at
/// the selected wash with no line, so only one list in the window shows where keys go (Apple's key
/// and non-key selection, in Slopty's neutrals). A hover is the hover wash alone.
pub fn selected<E: Styled>(el: E, theme: &Theme, focused: bool) -> E {
    let s = theme.surfaces;
    if focused {
        el.bg(hsla(s.keyed)).shadow(vec![ring_inside(s.focus)])
    } else {
        el.bg(hsla(s.selected))
    }
}

/// Paint a selection into `bounds` at `radius`, as [`selected`] draws it, `keyed` where the
/// keyboard is: for a selection that moves on its own (a list's plate).
pub fn paint_chosen(
    theme: &Theme,
    bounds: gpui::Bounds<gpui::Pixels>,
    radius: gpui::Pixels,
    keyed: bool,
    window: &mut Window,
) {
    let s = theme.surfaces;
    let corners = gpui::Corners::all(radius);
    if keyed {
        window.paint_quad(gpui::quad(
            bounds,
            corners,
            hsla(s.keyed),
            HAIR,
            hsla(s.focus),
            gpui::BorderStyle::Solid,
        ));
    } else {
        window.paint_quad(gpui::fill(bounds, hsla(s.selected)).corner_radii(corners));
    }
}

/// A 1 pt ring just inside an element's edge in `color`, taking no room.
fn ring_inside(color: Rgb) -> BoxShadow {
    BoxShadow {
        color: hsla(color),
        offset: point(px(0.0), px(0.0)),
        blur_radius: px(0.0),
        spread_radius: HAIR,
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
        .font_weight(FontWeight(Typography::MEDIUM_WEIGHT))
        .cursor_pointer()
        .child(label);
    let el = match kind {
        ButtonKind::Primary => solid_pressable(el, theme),
        ButtonKind::Destructive => destructive_pressable(el, theme),
        ButtonKind::Secondary => secondary(el, theme),
        ButtonKind::Ghost => eased(el)
            .text_color(hsla(s.text_secondary))
            .hover(move |el| el.bg(hsla(s.hover)).text_color(hsla(s.text)))
            .active(move |el| el.bg(hsla(s.pressed))),
        ButtonKind::Link => el.text_color(hsla(s.text)).hover(Styled::underline),
    };
    crate::a11y::tab_stop(el, s.focus)
}

/// The side of an [`icon_button`]: the density's button, `MonoCode`'s 26 pt square round a
/// 14 pt glyph.
///
/// It is never under the density's hit target, so a finger gets 44 pt round the same icon.
/// A strip that holds icon buttons in turn with something else sizes itself from it.
#[must_use]
pub const fn icon_button_side(theme: &Theme) -> f32 {
    theme.density.button.max(theme.density.hit)
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

/// A horizontal rule: a hairline in `tint` across its parent, taking its own width in the flow.
///
/// Drawn as a border rather than a box of the hairline's height, since GPUI snaps a border to
/// at least one device pixel where a box half a point tall rounds to nothing on a 1x screen.
#[must_use]
pub fn rule(tint: slopty_theme::Tint) -> Div {
    div().flex_none().w_full().border_t(HAIR).border_color(hsla(tint))
}

/// A rule parting a list's or a menu's groups.
///
/// A [`rule`] in `stroke` that fades out over its last
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
            rule(theme.surfaces.stroke),
            gpui::EdgeFade::x(px(theme.spacing.xl)),
        ))
        .into_any_element()
}

/// A vertical rule: [`rule`] standing up, the height of its parent.
#[must_use]
pub fn rule_v(tint: slopty_theme::Tint) -> Div {
    div().flex_none().h_full().border_l(HAIR).border_color(hsla(tint))
}

/// The pad round the rows of a floating sheet (a menu, the palette's list, the inbox):
/// `MonoCode`'s `p-1`, 4.
#[must_use]
pub const fn sheet_pad(theme: &Theme) -> f32 {
    theme.spacing.xs
}

/// A row inside a floating sheet padded by [`sheet_pad`].
///
/// A [`row`] whose fill (hover, selection) is rounded at [`slopty_theme::Radii::md`], 8 inside
/// the sheet's 12, as `MonoCode`'s menu rows are; its leading words still on the edge grid
/// measured from the sheet's own edge, and its trailing end a row's trailing inset in.
#[must_use]
pub fn sheet_row(theme: &Theme, lines: Row) -> Div {
    let pad = sheet_pad(theme);
    row(theme, lines).pl(px(theme.spacing.inset() - pad)).rounded(px(theme.radii.md))
}

/// `el` padded in to the one edge grid on both sides: [`slopty_theme::Spacing::inset`]. A
/// panel's rows, a header, the palette, the inbox and the foot lines all start there.
#[must_use]
pub fn inset_x<E: Styled>(el: E, theme: &Theme) -> E {
    el.px(px(theme.spacing.inset()))
}

/// `el` set in a type role ([`Theme::roles`]): its size, its line and its weight.
#[must_use]
pub fn typed<E: Styled>(el: E, role: slopty_theme::TypeRole) -> E {
    el.text_size(px(role.size)).line_height(px(role.line)).font_weight(FontWeight(role.weight))
}

/// `el` set as meta text: a row's second line, a bar's readout, a status word. The meta size
/// in `text_muted`; a status word then takes its tone's colour over it.
#[must_use]
pub fn meta<E: Styled>(el: E, theme: &Theme) -> E {
    el.text_size(px(theme.roles().metadata.size)).text_color(hsla(theme.surfaces.text_muted))
}

/// A section's label: a group's head that the rows under it still lead, as Linear's are.
///
/// The metadata role's size (12, a finger's 13) at the medium weight in `text_secondary`, never
/// upper case. Muted and regular, it
/// sank to the level of the facts on the rows and every section read as one run; the strong
/// weight stays for one thing per region, so a head never rivals the names under it.
#[must_use]
pub fn label(theme: &Theme, text: impl Into<SharedString>) -> Div {
    div()
        .flex_none()
        .text_size(px(theme.roles().metadata.size))
        .font_weight(FontWeight(Typography::MEDIUM_WEIGHT))
        .text_color(hsla(theme.surfaces.text_secondary))
        .child(text.into())
}

/// A square button around one icon: a bar's actions, a tile's close and split.
///
/// `MonoCode`'s icon button: a 26 pt square at [`slopty_theme::Radii::sm`] round a 14 pt glyph
/// in `text_secondary`, lifting to `text` over the strong hover. `label` is its accessible
/// name and its hint, since the icon alone names nothing to a screen reader.
#[must_use]
pub fn icon_button(
    theme: &Theme,
    id: impl Into<SharedString>,
    icon: crate::icons::Symbol,
    label: &'static str,
) -> gpui::Stateful<Div> {
    let s = theme.surfaces;
    eased(square_icon(theme, id.into(), icon, label, s.text_secondary))
        .hover(move |el| el.bg(hsla(s.hover_strong)).text_color(hsla(s.text)))
}

/// A tab's close: `MonoCode`'s 20 pt box at [`slopty_theme::Radii::xs`], its cross drawn on a
/// 12 pt grid, so it sits inside a 30 pt tab with room round it. A finger's is its hit square.
#[must_use]
pub fn close_box(
    theme: &Theme,
    id: impl Into<SharedString>,
    label: &'static str,
) -> gpui::Stateful<Div> {
    small_box(theme, id, crate::icons::Symbol::Xmark, label)
}

/// [`close_box`]'s square with another glyph: an action at the end of a line of text, such as
/// a comment's removal or the way to a thread's page, which a full [`icon_button`] would
/// make taller than its line.
#[must_use]
pub fn small_box(
    theme: &Theme,
    id: impl Into<SharedString>,
    glyph: crate::icons::Symbol,
    label: &'static str,
) -> gpui::Stateful<Div> {
    let s = theme.surfaces;
    let id: SharedString = id.into();
    let selector = id.to_string();
    let side =
        if theme.density == slopty_theme::Density::TOUCH { theme.density.hit } else { CLOSE_BOX };
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
        .rounded(px(theme.radii.xs))
        .cursor_pointer()
        .text_color(hsla(s.text_secondary))
        .active(move |el| el.bg(hsla(s.pressed)))
        .child(
            crate::icons::Drawn::disclosure(theme, glyph)
                .slot(px(crate::icons::IconSize::Inline.slot(theme)), hsla(s.text_secondary)),
        );
    eased(crate::a11y::tab_stop(el, s.focus))
        .hover(move |el| el.bg(hsla(s.hover_strong)).text_color(hsla(s.text)))
}

/// The side of [`close_box`] at a pointer's density: `MonoCode`'s `size-5`.
pub const CLOSE_BOX: f32 = 20.0;

/// [`icon_button`] with its icon in `ink`, not its words' tier: the bell while something needs
/// the person, in the warn fill.
#[must_use]
pub fn icon_button_inked(
    theme: &Theme,
    id: impl Into<SharedString>,
    icon: crate::icons::Symbol,
    label: &'static str,
    ink: Rgb,
) -> gpui::Stateful<Div> {
    let s = theme.surfaces;
    eased(square_icon(theme, id.into(), icon, label, ink))
        .hover(move |el| el.bg(hsla(s.hover_strong)))
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
        .child(
            crate::icons::Drawn::new(theme, icon, crate::icons::IconSize::Inline)
                .slot(px(crate::icons::IconSize::Inline.slot(theme)), hsla(ink)),
        );
    crate::a11y::tab_stop(el, s.focus)
}

/// [`icon_button`] that stays on until pressed again: a tile's trackpad mode.
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
    icon: crate::icons::Symbol,
    label: &'static str,
    on: bool,
) -> gpui::Stateful<Div> {
    if !on {
        return icon_button(theme, id, icon, label).aria_toggled(gpui::accesskit::Toggled::False);
    }
    let s = theme.surfaces;
    square_icon(theme, id.into(), icon, label, s.text)
        .aria_toggled(gpui::accesskit::Toggled::True)
        .bg(hsla(s.selected))
}

/// A toggle that is its own short words, `face` ("Aa", "W", ".*"), in an icon button's square:
/// a find's match case, whole word and pattern, the way Xcode and the terminals label them.
///
/// A glyph for "case sensitive" or "regex" has to be learnt; the letters say it. The face is at
/// the caption size and the medium weight, in the secondary ink until on, when it takes the text
/// and the selected wash as [`icon_toggle`] does. Its name, said to a screen reader and in its
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
        .font_weight(FontWeight(Typography::MEDIUM_WEIGHT))
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

/// A tick box the size of an inline icon.
///
/// A sunk hairline square while off, the neutral [`solid`] with its tick while on, as macOS
/// draws one. The caller makes it, or the row it leads, the thing pressed, with
/// `Role::CheckBox` and `aria_toggled`.
#[must_use]
pub fn tick_box(theme: &Theme, on: bool) -> Div {
    let s = theme.surfaces;
    let side = px(theme.typography.icon());
    let el = div()
        .flex_none()
        .size(side)
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(theme.radii.xs))
        .border(HAIR);
    if on {
        solid(el, theme).border_color(hsla(s.solid)).child(
            crate::icons::icon(
                theme,
                crate::icons::Symbol::Checkmark,
                crate::icons::IconSize::Inline,
                hsla(s.solid_ink),
            )
            .size(px(theme.typography.small())),
        )
    } else {
        sunk(el.border_color(hsla(s.border)), theme, stroke::LINE)
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
/// next step the caller adds it under them ([`notice_action`]); its identity too.
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

/// A [`notice`]'s one next step, under its words: a secondary button, a step of space apart.
#[must_use]
pub fn notice_action(theme: &Theme, id: &'static str, label: &'static str) -> gpui::Stateful<Div> {
    button(theme, id, label, ButtonKind::Secondary).mt(px(theme.spacing.sm))
}

/// A [`notice`]'s mark: a kind's glyph across [`NOTICE_MARK`], in `text_muted`.
#[must_use]
pub fn notice_mark(theme: &Theme, icon: impl Into<crate::icons::Mark>) -> Div {
    let ink = hsla(theme.surfaces.text_muted);
    crate::icons::Drawn::notice(theme, icon).slot(px(NOTICE_MARK), ink)
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
            .debug_selector(|| "hint".to_owned())
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

    mod overflow;

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
            find::PLACEHOLDER,
            find::REPLACE_PLACEHOLDER,
            find::MATCH_CASE,
            find::WHOLE_WORD,
            find::REGEX,
            find::REPLACE_ALL,
            find::NO_MATCHES,
            find::BAD_PATTERN,
            crate::workspace::TAKE_OVER,
            crate::workspace::TAKE,
            crate::workspace::MUTE,
            crate::workspace::ATTACHING,
            crate::workspace::PAUSED,
            crate::workspace::OPENING,
            crate::workspace::READING,
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
            crate::settings_form::ADD_KEYS,
            crate::settings_form::RESET_KEYS,
            crate::workspace::COPY_COMMAND,
            crate::palette::NO_COMMAND_MATCHES,
            crate::palette::COMMAND_HINT,
            crate::picker::FILTER_PLACEHOLDER,
            crate::picker::NOTHING_MATCHES,
            crate::picker::NOTHING_TO_JUMP_TO,
            crate::picker::LOADING_WINDOWS,
            crate::terminal::BACK_TO_LIVE,
            crate::terminal::COPY_MODE,
            crate::terminal::COPY_MODE_DONE,
            crate::workspace::RECONNECTING,
            crate::workspace::SESSION_ENDED,
            crate::workspace::CLOSE_TILE,
            crate::workspace::NEW_AGENT,
            crate::workspace::NO_WORKERS,
            crate::workspace::NO_WORKERS_NEXT,
            crate::workspace::ADD_WORKER,
            crate::workspace::CHROME_WORDS[0],
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
                assert_eq!(kit.colors.background, hsla(theme.surfaces.ground));
                assert_eq!(kit.colors.foreground, hsla(theme.surfaces.text));
                assert_eq!(kit.colors.primary, hsla(theme.surfaces.solid), "the neutral solid");
                assert_eq!(kit.colors.primary_foreground, hsla(theme.surfaces.solid_ink));
                assert_eq!(kit.colors.caret, hsla(theme.surfaces.text), "a neutral caret");
                assert_eq!(kit.colors.border, hsla(theme.surfaces.border));
                assert_eq!(kit.colors.ring, hsla(theme.surfaces.border), "no accent on a field");
                for bar in [kit.colors.title_bar, kit.colors.tab_bar, kit.colors.status_bar] {
                    assert_eq!(bar, hsla(theme.surfaces.ground), "bars on the ground");
                }
                assert_eq!(kit.colors.sidebar, hsla(theme.surfaces.sidebar), "the side panel");
                assert_eq!(kit.colors.title_bar_border, hsla(theme.surfaces.stroke), "one line");
                let shown = theme.surfaces.selected.over(theme.surfaces.ground);
                assert_eq!(kit.colors.tab_active, hsla(shown), "the shown tab a selected pill");
                assert_eq!(kit.colors.table_row_border, hsla(theme.surfaces.stroke));
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
            return Some("a shadow of its own, not `kit::elevate` or `kit::dialog`");
        }
        let dim = ["alpha::SCRIM", "elevation.scrim", "elevation.shade"];
        dim.iter()
            .any(|token| line.contains(token))
            .then_some("a scrim of its own, not `kit::scrim`")
    }

    #[test]
    fn the_elevation_check_knows_a_lift_from_a_token() {
        assert!(own_elevation(".bg(hsla(s.ground)).shadow_sm()").is_some());
        assert!(own_elevation(".shadow(vec![shadow])").is_some());
        assert!(own_elevation(".bg(hsla_alpha(s.ground, alpha::SCRIM))").is_some());
        assert!(own_elevation("kit::elevate(div(), theme).rounded(px(r))").is_none());
        assert!(own_elevation(".bg(kit::scrim(theme))").is_none());
        assert!(own_elevation(".when(floats, gpui::Styled::shadow_sm)").is_some());
        assert!(own_elevation(".shadow_none()").is_none(), "taking a shadow off is fine");
        assert!(own_elevation("/// no `.shadow_sm()` here").is_none(), "a comment");
        let dimmed = ".bg(hsla_alpha(theme.elevation.shade, alpha::DIM))";
        assert!(own_elevation(dimmed).is_some(), "the shade dimmed by hand");
        assert!(own_elevation(".bg(hsla_alpha(s.ground, t.elevation.scrim))").is_some());
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
                // The kit's own files draw the elevations and finishes the rest call.
                let waived = file.ends_with("slopty-ui/src/kit.rs")
                    || file.ends_with("slopty-ui/src/kit/go.rs");
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
        assert!(hand_rolled_pill("kit::pill(theme, s.warn)").is_none());
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

    /// A pill is its fixed height and a chip at `radii.sm`, and the state's pill is the frame
    /// filled at the faint step, a notch fainter on white.
    #[test]
    fn a_pill_is_a_twenty_point_chip() {
        for (variant, step) in
            [(Variant::Dark, alpha::FAINT), (Variant::Light, alpha::FAINT_ON_PAPER)]
        {
            let theme = Theme::new(variant);
            let mut frame = pill_frame(&theme);
            let height = frame.style().size.height;
            assert_eq!(height, Some(px(PILL_HEIGHT).into()));
            let corner = frame.style().corner_radii.top_left;
            assert_eq!(corner, Some(px(theme.radii.sm).into()), "a chip");
            let mut filled = pill(&theme, theme.surfaces.warn);
            assert_eq!(filled.style().size.height, Some(px(PILL_HEIGHT).into()));
            let fill = filled.style().background.clone();
            let faint = gpui::Fill::from(hsla_alpha(theme.surfaces.warn, step));
            assert_eq!(fill, Some(faint), "{variant:?}: the tone at the faint step");
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
        let icon = crate::icons::Symbol::Cursorarrow;
        let mut off = icon_toggle(&theme, "t", icon, "Trackpad mode", false);
        let mut on = icon_toggle(&theme, "t", icon, "Trackpad mode", true);
        assert_eq!(off.style().background, None, "off is bare");
        let selected = Some(gpui::Fill::from(hsla(theme.surfaces.selected)));
        assert_eq!(on.style().background, selected, "on rests on the selected wash");
        assert_eq!(on.style().size.width, off.style().size.width, "one size either way");
    }

    /// The primary is the neutral solid, the destructive the error's wash, the secondary clear
    /// inside the border line and the rest bare, all one control's height, in both variants: no
    /// button wears the accent. A selection where the keyboard is is the keyed wash with the
    /// focus green's line inside it, and elsewhere the selected wash alone.
    #[test]
    fn a_button_is_neutral_and_one_height() {
        for variant in [Variant::Dark, Variant::Light] {
            let theme = Theme::new(variant);
            let s = theme.surfaces;
            let fill = |kind| button(&theme, "b", "Go", kind).style().background.clone();
            assert_eq!(fill(ButtonKind::Primary), Some(gpui::Fill::from(hsla(s.solid))));
            let red = Some(gpui::Fill::from(hsla_alpha(s.error_fill, alpha::TINTED)));
            assert_eq!(fill(ButtonKind::Destructive), red);
            assert_eq!(fill(ButtonKind::Secondary), None, "{variant:?}: clear at rest");
            let mut second = button(&theme, "b", "Go", ButtonKind::Secondary);
            assert_eq!(second.style().border_color, Some(hsla(s.border)), "{variant:?}");
            assert_eq!(fill(ButtonKind::Ghost), None);
            assert_eq!(fill(ButtonKind::Link), None);
            for kind in [
                ButtonKind::Primary,
                ButtonKind::Destructive,
                ButtonKind::Secondary,
                ButtonKind::Ghost,
            ] {
                let mut b = button(&theme, "b", "Go", kind);
                assert_eq!(
                    b.style().size.height,
                    Some(px(theme.density.control).into()),
                    "{kind:?}"
                );
                assert!(b.style().box_shadow.is_none(), "{variant:?} {kind:?}: flat");
            }
            let mut picked = selected(div(), &theme, true);
            assert_eq!(picked.style().background, Some(gpui::Fill::from(hsla(s.keyed))));
            let ring = picked.style().box_shadow.clone().unwrap_or_default();
            let green =
                |l: &BoxShadow| l.inset && l.spread_radius == HAIR && l.color == hsla(s.focus);
            assert!(!ring.is_empty() && ring.iter().all(green), "{ring:?}");
            let mut away = selected(div(), &theme, false);
            let wash = Some(gpui::Fill::from(hsla(s.selected)));
            assert_eq!(away.style().background, wash, "not key");
            assert!(away.style().box_shadow.is_none(), "no ring off the keyboard");
        }
    }

    /// The destructive button's word reads AA on its wash at rest and under the pointer, over
    /// the ground and over a float, in both variants.
    #[test]
    fn the_destructive_button_reads_in_every_state() {
        for variant in [Variant::Light, Variant::Dark] {
            let theme = Theme::new(variant);
            let s = theme.surfaces;
            for plane in [theme.content(), s.elevated] {
                for (state, share) in [("rest", alpha::TINTED), ("hover", alpha::TINTED_HOVER)] {
                    let wash = slopty_theme::Tint::of(s.error_fill, share).over(plane);
                    let ratio = s.error.contrast(wash);
                    assert!(ratio >= 4.5, "{variant:?} {state}: the word reads {ratio:.2}");
                }
            }
        }
    }

    /// The destructive wash is a control's fill only through [`ButtonKind::Destructive`]: drawn
    /// here, and where the thread's answer row draws the kinds itself.
    #[test]
    fn the_destructive_red_is_the_kits() {
        const RULED: [&str; 1] = ["slopty-ui/src/conversation/thread/view.rs"];
        let red = |line: &str| {
            let code = !line.trim_start().starts_with("//");
            (code && line.contains("alpha::TINTED"))
                .then_some("the destructive wash outside `ButtonKind::Destructive`")
        };
        let wrong = flagged(&RULED, red);
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
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

    /// Chrome's sources as whole files of lines, test modules and comments left out.
    fn chrome_files() -> Vec<(String, Vec<(usize, String)>)> {
        let mut files: Vec<(String, Vec<(usize, String)>)> = Vec::new();
        let lines = ["slopty-ui/src", "slopty-app/src"].into_iter().flat_map(chrome_lines);
        for (file, line_no, line) in lines {
            let line = if line.trim_start().starts_with("//") { String::new() } else { line };
            match files.last_mut() {
                Some((last, lines)) if *last == file => lines.push((line_no, line)),
                _ => files.push((file, vec![(line_no, line)])),
            }
        }
        files
    }

    /// The methods a `div()` chain at the start of `code` calls on itself, its arguments left
    /// out: `div().size(px(6.0)).bg(hsla(x))` is `div().size().bg()`. It ends where the
    /// chain does, at a `,`, `;` or the parenthesis that closes what holds it.
    fn own_chain(code: &str) -> String {
        let mut depth = 0_u32;
        let mut own = String::new();
        for c in code.chars() {
            match c {
                ')' | ',' | ';' if depth == 0 => break,
                '(' => {
                    if depth == 0 {
                        own.push(c);
                    }
                    depth = depth.saturating_add(1);
                }
                ')' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        own.push(c);
                    }
                }
                _ if depth == 0 => own.push(c),
                _ => {}
            }
        }
        own
    }

    /// A dot: a `div()` filled, rounded to a circle and holding nothing, its own chain read
    /// from `code`.
    fn is_dot(code: &str) -> bool {
        let own = own_chain(code);
        own.contains(".rounded_full()")
            && own.contains(".bg(")
            && !own.contains(".child(")
            && !own.contains(".children(")
    }

    #[test]
    fn the_dot_check_knows_a_dot_from_a_glyph() {
        assert!(is_dot("div().size(px(6.0)).rounded_full().bg(hsla(s.accent_fill))"));
        assert!(is_dot("div().id(\"d\").size(px(d)).rounded_full().bg(hsla(fill)))"));
        assert!(is_dot("div().size(px(6.0)).bg(hsla(dot)).rounded_full(),.child(word)"));
        assert!(!is_dot("div().rounded_full().bg(hsla(s.warn_fill)).child(count)"), "a badge");
        assert!(!is_dot("div().rounded_full().border(HAIR).child(div().size(px(6.0)))"));
        assert!(!is_dot("div().size(px(6.0)).rounded_full().overflow_hidden()"), "no fill");
    }

    /// A state is a glyph, never a dot: what needs the person, what finished and what failed
    /// each draw their own mark in its fill (`docs/decisions/ui.md`, "State is a glyph"). A dot
    /// read as decoration, and a list of them as confetti. The ruled dots are not states.
    #[test]
    fn a_state_is_a_glyph_not_a_dot() {
        const RULED: [(&str, &str); 3] = [
            ("slopty-ui/src/settings_form.rs", "a switch's knob"),
            ("slopty-ui/src/project/view.rs", "a switch's knob"),
            ("slopty-ui/src/workspace/about.rs", "the brand mark's dots"),
        ];
        let mut wrong = Vec::new();
        for (file, lines) in chrome_files() {
            if RULED.iter().any(|(ruled, _)| file.ends_with(ruled)) {
                continue;
            }
            let code: Vec<String> =
                lines.iter().map(|(_, l)| l.split_whitespace().collect()).collect();
            let joined = code.concat();
            let mut at = 0_usize;
            for ((line_no, line), squeezed) in lines.iter().zip(&code) {
                let here = at;
                at = at.saturating_add(squeezed.len());
                let Some(found) = squeezed.find(".rounded_full()") else { continue };
                let before = joined.get(..here.saturating_add(found)).unwrap_or_default();
                let Some(div) = before.rfind("div()") else { continue };
                if is_dot(joined.get(div..).unwrap_or_default()) {
                    wrong
                        .push(format!("{file}:{line_no}: a state drawn as a dot: {}", line.trim()));
                }
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// A text tone named as a whole token: `s.warn` but not `s.warn_fill`.
    fn names_text_tone(call: &str) -> bool {
        ["warn", "error", "success", "accent"].iter().any(|tone| {
            call.split(&format!(".{tone}"))
                .skip(1)
                .any(|after| !after.starts_with(|c: char| c == '_' || c.is_alphanumeric()))
        })
    }

    /// The call opened on `lines[at]` at `needle`, through its closing parenthesis, squeezed.
    fn call_at(lines: &[(usize, String)], at: usize, needle: &str) -> String {
        let text: String = lines.iter().skip(at).take(12).map(|(_, l)| l.as_str()).collect();
        let Some((_, from)) = text.split_once(needle) else { return String::new() };
        let mut depth = 1_u32;
        let mut call = needle.to_owned();
        for c in from.chars() {
            call.push(c);
            match c {
                '(' => depth = depth.saturating_add(1),
                ')' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
        }
        call.split_whitespace().collect()
    }

    /// A status glyph in a text tone: `status_icon` or a filled circle or triangle symbol
    /// handed `warn`, `error`, `success` or `accent` rather than their fill step.
    fn glyph_in_text_tone(call: &str) -> Option<&'static str> {
        let glyph = call.starts_with("status_icon(")
            || (call.starts_with("icon(")
                && (call.contains("CircleFill") || call.contains("TriangleFill")));
        (glyph && names_text_tone(call))
            .then_some("a status glyph in a text tone; it wears the `_fill` step (`Status::ink`)")
    }

    #[test]
    fn the_glyph_tone_check_knows_a_fill_from_a_text_tone() {
        assert!(glyph_in_text_tone("status_icon(theme,status,side,hsla(s.warn))").is_some());
        assert!(
            glyph_in_text_tone("icon(theme,Symbol::XmarkCircleFill,size,hsla(s.error))").is_some()
        );
        assert!(glyph_in_text_tone("status_icon(theme,status,side,hsla(s.warn_fill))").is_none());
        assert!(glyph_in_text_tone("icon(theme,Symbol::Bell,size,hsla(s.warn))").is_none());
        assert!(glyph_in_text_tone("status_icon(theme,status,side,hsla(ink))").is_none());
    }

    /// A status glyph wears its hue's fill step, never the text tone: the text steps are for
    /// words, and a glyph in one read dimmer than its neighbours' (`docs/decisions/ui.md`,
    /// "State is a glyph").
    #[test]
    fn a_status_glyph_wears_its_fill() {
        let mut wrong = Vec::new();
        for (file, lines) in chrome_files() {
            for (ix, (line_no, line)) in lines.iter().enumerate() {
                if line.trim_start().starts_with("//") {
                    continue;
                }
                for needle in ["status_icon(", "icon("] {
                    let whole = line.split_once(needle).is_some_and(|(before, _)| {
                        !before.ends_with(|c: char| c == '_' || c.is_alphanumeric())
                    });
                    if !whole {
                        continue;
                    }
                    if let Some(why) = glyph_in_text_tone(&call_at(&lines, ix, needle)) {
                        wrong.push(format!("{file}:{line_no}: {why}: {}", line.trim()));
                    }
                }
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// A machine's or a project's own colour goes on the navigator's head and nowhere else: the
    /// identity hues are read only through [`identity_ink`] and [`machine_ink`], and those are
    /// called only there.
    #[test]
    fn identity_colour_stays_on_its_glyph() {
        // The navigator's machine and project heads, and nowhere else (`docs/decisions/ui.md`,
        // "Premium foundations"): a palette row, a tile's header, the breadcrumb and the empty
        // workspace draw a machine in its words' tier.
        const GLYPHS: [&str; 3] = [
            "slopty-ui/src/kit/identity.rs",
            "slopty-ui/src/workspace/navigator.rs",
            // Until the board's task meta goes to its words' tier (lane A, the same ruling).
            "slopty-ui/src/project/view.rs",
        ];
        let mut wrong = Vec::new();
        for (file, line_no, line) in
            ["slopty-ui/src", "slopty-app/src"].into_iter().flat_map(chrome_lines)
        {
            let code = line.split("//").next().unwrap_or_default();
            let hues = code.contains("surfaces.identity") || code.contains("s.identity");
            let ink = code.contains("identity_ink(") || code.contains("machine_ink(");
            let mine = file.ends_with("slopty-ui/src/kit/identity.rs");
            if (hues && !mine) || (ink && !GLYPHS.iter().any(|f| file.ends_with(f))) {
                wrong.push(format!("{file}:{line_no}: {}", line.trim()));
            }
        }
        assert!(wrong.is_empty(), "an identity colour off its glyph:\n{}", wrong.join("\n"));
    }

    /// A row's lead handed the muted tone: two tiers under its title.
    fn muted_lead(call: &str) -> Option<&'static str> {
        call.contains(".text_muted").then_some(
            "a lead in `text_muted`; it is a tier under its title (`text_secondary`), muted \
             only when away or disabled",
        )
    }

    /// A lead sits one tier under its title, never two: `text_secondary` at rest and `text`
    /// when chosen, so the navigator stops reading as grey specks beside black words
    /// (`docs/decisions/ui.md`, "An icon takes its words' size, weight and tier"). Away and
    /// disabled are muted through their own words (`Status::Away`), not at the call.
    #[test]
    fn a_lead_is_never_muted_at_rest() {
        const AWAITING: [&str; 0] = [];
        let mut wrong = Vec::new();
        for (file, lines) in chrome_files() {
            if AWAITING.iter().any(|f| file.ends_with(f)) {
                continue;
            }
            for (ix, (line_no, line)) in lines.iter().enumerate() {
                for needle in ["lead_slot(", "icon_slot("] {
                    let whole = line.split_once(needle).is_some_and(|(before, _)| {
                        !before.ends_with(|c: char| c == '_' || c.is_alphanumeric())
                            && !before.trim_end().ends_with("fn")
                    });
                    if !whole {
                        continue;
                    }
                    // A fold chevron sits in a lead's slot but leads nothing.
                    let call = call_at(&lines, ix, needle);
                    if call.contains("disclosure(") {
                        continue;
                    }
                    if let Some(why) = muted_lead(&call) {
                        wrong.push(format!("{file}:{line_no}: {why}: {}", line.trim()));
                    }
                }
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// A colour picked at a call site rather than taken from the theme: a hex or channel
    /// literal handed to GPUI. The tokens derive every colour from the content, so a literal
    /// is the one colour a custom background cannot move.
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

    /// Whether `line` fills something with a state wash: the pointer's, the selection's or the
    /// press's.
    fn washes(line: &str) -> bool {
        let code = line.split("//").next().unwrap_or_default();
        code.contains(".bg(hsla(")
            && [".hover))", ".selected))", ".pressed))"].iter().any(|w| code.contains(w))
    }

    /// Whether a wash on `line`, after `before`, is laid in a state: inside a `hover` or `active`
    /// closure, or under a condition (a `when`, an `if`) that names the state.
    fn in_a_state(line: &str, before: &[&str]) -> bool {
        const STATES: [&str; 4] = [".hover(", ".active(", ".when(", "group_hover("];
        STATES.iter().any(|s| line.contains(s))
            || before.iter().any(|l| l.contains(".when(") || l.trim_start().starts_with("if "))
    }

    #[test]
    fn the_wash_check_knows_a_state_from_a_surface() {
        assert!(washes("            .bg(hsla(s.hover))"));
        assert!(washes(".bg(hsla(theme.surfaces.selected))"));
        assert!(!washes(".bg(hsla(s.card))"), "a well");
        assert!(!washes("// .bg(hsla(s.hover)) once filled it"), "a comment");
        assert!(in_a_state(".hover(move |el| el.bg(hsla(s.hover)))", &[]));
        assert!(in_a_state("el.bg(hsla(s.hover))", &["        if shown {"]));
        assert!(in_a_state(
            "el.bg(hsla(s.hover)).aria_selected(true)",
            &[".when(ix == at, |el| {"]
        ));
        assert!(!in_a_state(".bg(hsla(s.hover))", &[".rounded(px(theme.radii.sm))"]));
    }

    /// A resting fill is raised or sunk, never a wash. The `hover`, `selected` and `pressed`
    /// washes are for the pointer, the selection and the press: laid on something at rest they
    /// lift it in dark and sink it in light, so a tray, an option or a field went grey on paper.
    /// What rests is [`super::raised`], a well is [`super::inset`] and a field
    /// [`super::field`]; this file, which draws those, is not checked.
    #[test]
    fn a_resting_fill_is_raised_or_sunk() {
        let mut wrong = Vec::new();
        for dir in ["slopty-ui/src", "slopty-app/src"] {
            let lines = chrome_lines(dir);
            for (ix, (file, no, line)) in lines.iter().enumerate() {
                if file.ends_with("slopty-ui/src/kit.rs") || !washes(line) {
                    continue;
                }
                let before: Vec<&str> = lines[ix.saturating_sub(2)..ix]
                    .iter()
                    .filter(|(f, ..)| f == file)
                    .map(|(.., l)| l.as_str())
                    .collect();
                if !in_a_state(line, &before) {
                    wrong.push(format!("{file}:{no}: a wash at rest: {}", line.trim()));
                }
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// The thread's stream is type on one plane: no call and no plan in it is a card or rests
    /// raised. A card there boxed the calls that changed something while their neighbours that
    /// only looked were lines, and a stream of type then wore four kinds of edge; how a call
    /// stands is its mark and a word. An opened call's body is a well ([`super::inset`]).
    #[test]
    fn the_stream_wears_no_card() {
        const STREAM: [&str; 2] =
            ["conversation/thread/view/tools.rs", "conversation/thread/view/plan.rs"];
        const RAISED: [&str; 4] = ["kit::card(", "kit::card_part(", "kit::raised(", "raised_part("];
        let mut wrong = Vec::new();
        for (file, no, line) in chrome_lines("slopty-ui/src") {
            let code = !line.trim_start().starts_with("//");
            if !code || !STREAM.iter().any(|s| file.ends_with(s)) {
                continue;
            }
            if let Some(raised) = RAISED.iter().find(|r| line.contains(*r)) {
                wrong.push(format!("{file}:{no}: `{raised}` in the stream: {}", line.trim()));
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// A command in the palette is its words: no line an action runs leads with an icon. Only a
    /// step that lists things (an agent, a machine, a folder, a checkout, a repository to clone,
    /// a past session) marks its lines with what they are, and only there may `with_icon` follow
    /// a command's line.
    #[test]
    fn a_command_is_its_words() {
        const THINGS: [&str; 2] = ["workspace/agent_start.rs", "workspace/clone_here.rs"];
        let bindings: Vec<gpui::KeyBinding> = Vec::new();
        let lines = crate::workspace::palette_items()
            .into_iter()
            .chain(crate::conversation::palette_items(&bindings))
            .chain(crate::project::palette_items(&bindings))
            .chain(crate::review::palette_items(&bindings))
            .chain(crate::folder::folder_palette_items(&bindings))
            .chain(crate::file::editor_palette_items(&bindings));
        let marked: Vec<String> =
            lines.filter(|line| line.icon.is_some()).map(|line| line.label).collect();
        assert!(marked.is_empty(), "commands with an icon: {marked:?}");

        let mut wrong = Vec::new();
        let source = chrome_lines("slopty-ui/src");
        for (ix, (file, no, line)) in source.iter().enumerate() {
            if !line.contains(".with_icon(")
                || THINGS.iter().any(|things| file.ends_with(things))
                || file.ends_with("palette.rs")
            {
                continue;
            }
            let chain = source[ix.saturating_sub(4)..=ix].iter().filter(|(f, ..)| f == file);
            if chain.clone().any(|(.., l)| l.contains("PaletteItem::new(")) {
                wrong.push(format!("{file}:{no}: a command given an icon: {}", line.trim()));
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// A width set to a share of its parent (`.w(relative(…))`) next to a share, a fraction, a
    /// progress or the accent's fill: a progress bar drawn by hand.
    fn hand_drawn_progress(lines: &[(String, usize, String)], ix: usize) -> bool {
        const NEAR: [&str; 4] = ["accent_fill", "progress", "fraction", "share"];
        let (file, _, line) = &lines[ix];
        let squeezed: String = line.split_whitespace().collect();
        let code = !line.trim_start().starts_with("//");
        if !code || !(squeezed.contains(".w(relative(") || squeezed.contains(".w(gpui::relative("))
        {
            return false;
        }
        lines[ix.saturating_sub(6)..lines.len().min(ix.saturating_add(7))]
            .iter()
            .filter(|(f, ..)| f == file)
            .any(|(.., l)| NEAR.iter().any(|n| l.contains(n)))
    }

    /// Progress is drawn one way, by [`super::progress`]: a capsule on a quiet track that glides,
    /// breathes when its length is unknown, waits before it shows and carries its value. Five
    /// had been drawn by hand, three of them square lines along an edge that read as stray rules.
    /// The files below draw theirs by hand until lane D's patches move them; each goes from the
    /// list as its patch lands.
    #[test]
    fn a_progress_is_kit_progress() {
        const UNTIL_MOVED: [&str; 3] =
            ["workspace/tile.rs", "workspace/navigator.rs", "project/view.rs"];
        let mut wrong = Vec::new();
        for dir in ["slopty-ui/src", "slopty-app/src"] {
            let lines = chrome_lines(dir);
            for (ix, (file, no, line)) in lines.iter().enumerate() {
                let own = file.ends_with("kit/progress.rs")
                    || UNTIL_MOVED.iter().any(|f| file.ends_with(f));
                if !own && hand_drawn_progress(&lines, ix) {
                    wrong.push(format!("{file}:{no}: progress by hand: {}", line.trim()));
                }
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    #[test]
    fn the_progress_check_knows_a_bar_from_a_column() {
        let line = |l: &str| ("f.rs".to_owned(), 1, l.to_owned());
        let bar = [line("let share = done / total;"), line("div().w(relative(share))")];
        assert!(hand_drawn_progress(&bar, 1));
        let column = [line("let wide = true;"), line("div().w(relative(0.5))")];
        assert!(!hand_drawn_progress(&column, 1), "a half-width column is no progress");
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

    /// What floats as a sheet (a menu, a popover, a toast) is rounded at `radii.lg`; a modal (a
    /// dialog, the add-worker panel, through [`modal`]) is larger and rounds at `radii.xl`. A hint
    /// and a chip keep their control's radius: at 12 a 20 pt hint is a lozenge.
    #[test]
    fn a_floating_surface_is_rounded_lg() {
        const SHEETS: [(&str, &str); 3] = [
            ("slopty-ui/src/kit/find.rs", "super::elevate(div(), &theme)"),
            ("slopty-ui/src/conversation/thread/view/aside.rs", ".id(\"thread-aside\")"),
            ("slopty-ui/src/kit/menu.rs", "super::elevate(div(), &theme)"),
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
                    "panel_title",
                    "page_heading",
                    "Role::Heading",
                    "APP_NAME",
                    "pub fn title(",
                    "fn phone_title_role(",
                ];
                if !near.into_iter().any(|l| title.iter().any(|t| l.contains(t))) {
                    wrong.push(format!("{file}:{line_no}: the strong weight off a title"));
                }
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// One type scale: a size is a role's ([`slopty_theme::TypeRoles`]) or the chrome's own
    /// steps, never the parallel `title`, `heading`, `display` and `task_title` sizes, which
    /// drew 15 pt titles where the roles said 16 and let sizes drift a point apart.
    #[test]
    fn one_type_scale() {
        let gone = ["title()", "heading()", "display()", "task_title()"];
        let mut wrong = Vec::new();
        for dir in ["slopty-ui/src", "slopty-app/src"] {
            for (file, line_no, line) in chrome_lines(dir) {
                let code = line.split("//").next().unwrap_or_default();
                // The receiver is the typography itself: `typography`, or its usual `ty` and `t`.
                let parallel = gone.iter().any(|f| {
                    code.match_indices(&format!(".{f}")).any(|(at, _)| {
                        let receiver = code
                            .get(..at)
                            .unwrap_or_default()
                            .rsplit(|c: char| !(c.is_alphanumeric() || c == '_'))
                            .next()
                            .unwrap_or_default();
                        matches!(receiver, "typography" | "ty" | "t")
                    })
                });
                if parallel {
                    wrong.push(format!("{file}:{line_no}: {}", line.trim()));
                }
            }
        }
        assert!(wrong.is_empty(), "a size off the one scale:\n{}", wrong.join("\n"));
    }

    /// Every tile stands on a panel ([`super::panel`]): in the files that draw tiles (the
    /// strip, a tile, a tile starting, a tile whose machine is away) nothing paints the
    /// content's surface as a ground of its own, square and flush, where the panel rounds,
    /// rings and stands it on the canvas. A fill of it that is not a ground (a backing that
    /// hides text under the header's controls) says so on the line before.
    #[test]
    fn a_tile_stands_on_a_panel() {
        let tiles = ["strip.rs", "tile.rs", "starting.rs", "kept_items.rs"]
            .map(|name| std::path::Path::new("workspace").join(name));
        let mut wrong = Vec::new();
        let mut before = String::new();
        for (file, line_no, line) in chrome_lines("slopty-ui/src") {
            let drawn = tiles.iter().any(|t| std::path::Path::new(&file).ends_with(t));
            let code = line.split("//").next().unwrap_or_default();
            // A fill whose colour is the content's surface, however it is reached.
            let ground = code.match_indices(".bg(").any(|(at, _)| {
                let fill = code.get(at..).unwrap_or_default();
                let fill = fill.split(".bg(").nth(1).unwrap_or_default();
                let fill = fill.split(").").next().unwrap_or(fill);
                fill.contains("content()") || fill.contains("terminal.bg")
            });
            if drawn && ground && !before.contains("Not a ground") {
                wrong.push(format!("{file}:{line_no}: {}", line.trim()));
            }
            before = line;
        }
        assert!(wrong.is_empty(), "a tile's ground not on a panel:\n{}", wrong.join("\n"));
    }

    /// Chrome context (a header's directory, the breadcrumb's path, the palette's column, a
    /// row's second line) is in the UI face. The mono face is for the settings file and an
    /// address read to judge it (a held-back page's hint), and nothing else calls for it.
    #[test]
    fn the_mono_face_is_for_the_settings_file_and_addresses() {
        let uses: Vec<String> = ["slopty-ui/src", "slopty-app/src"]
            .into_iter()
            .flat_map(chrome_lines)
            .filter(|(_, _, line)| {
                line.contains("mono_family(") && !line.contains("fn mono_family")
            })
            .map(|(file, line_no, _)| format!("{file}:{line_no}"))
            .collect();
        let allowed = |at: &String| {
            ["slopty-ui/src/settings_editor.rs", "slopty-ui/src/kit.rs"]
                .iter()
                .any(|file| at.contains(file))
        };
        assert!(uses.iter().all(allowed), "mono outside the settings and addresses: {uses:#?}");
        assert_eq!(uses.len(), 2, "the settings field and the address: {uses:#?}");
    }

    /// A list typed at (the palette, a picker) floats on the bare [`anchor`]; the modals that
    /// hold the window until answered (adding a worker) dim it with the [`backdrop`]. Both open
    /// at one place. The settings are a page in the panes' place, and dim nothing.
    #[test]
    fn a_list_floats_and_a_modal_dims() {
        let calls = |file: &str, call: &str| {
            let (ui, app) = (chrome_lines("slopty-ui/src"), chrome_lines("slopty-app/src"));
            ui.into_iter().chain(app).any(|(f, _, l)| f.ends_with(file) && l.contains(call))
        };
        for list in ["slopty-ui/src/palette.rs", "slopty-ui/src/picker.rs"] {
            let anchored = calls(list, "kit::anchor(") || calls(list, "palette::list_anchor(");
            assert!(anchored, "{list} lays out on the anchor");
            assert!(!calls(list, "kit::backdrop("), "{list} dims nothing");
        }
        assert!(calls("slopty-app/src/lib.rs", "kit::backdrop("), "adding a worker is a modal");
        assert!(!calls("slopty-ui/src/settings_editor.rs", "kit::backdrop("), "a page");
    }

    /// Under the touch density an icon button is a finger's 44 pt round the same icon, and the
    /// rows grow with it; under the compact one nothing moved from the sizes it replaced.
    #[test]
    fn density_sizes_the_targets_not_the_icons() {
        let mut theme = Theme::default();
        assert!((icon_button_side(&theme) - 26.0).abs() < f32::EPSILON, "compact: MonoCode's 26");
        assert!((Row::One.height(&theme) - 28.0).abs() < f32::EPSILON);
        assert!((Row::Two.height(&theme) - 40.0).abs() < f32::EPSILON);
        theme.density = slopty_theme::Density::TOUCH;
        assert!(icon_button_side(&theme) >= 44.0, "touch: a finger's target");
        assert!(Row::One.height(&theme) >= 44.0 && Row::Two.height(&theme) > 44.0);
        assert!(theme.typography.icon() < icon_button_side(&theme), "the icon stays its size");
    }

    /// The elevation reaches GPUI as the theme says: a float's two crisp layers of the shade
    /// falling down, tightest first and unspread, and a dialog's four reaching further. The scrim
    /// is the same shade.
    #[test]
    fn the_elevation_is_layers_of_the_shade() {
        for variant in [Variant::Dark, Variant::Light] {
            let theme = Theme::new(variant);
            let layers = elevation(&theme);
            assert_eq!(layers.len(), 2, "{variant:?}: Zed's two layers");
            let ink = hsla(theme.elevation.shade);
            let tinted = |c: Hsla| (c.h, c.s, c.l) == (ink.h, ink.s, ink.l);
            assert!(layers.iter().all(|l| !l.inset && tinted(l.color)), "{variant:?}");
            assert!(tinted(scrim(&theme)), "{variant:?}: the scrim is the shade");
            assert!(layers.iter().all(|l| l.offset.y > px(0.0)));
            assert!(layers.first().map(|l| l.blur_radius) < layers.last().map(|l| l.blur_radius));
            assert!(layers.iter().all(|l| l.spread_radius == px(0.0)), "{variant:?}: unspread");
            let dialog = dialog_elevation(&theme);
            assert_eq!(dialog.len(), 4, "{variant:?}: Zed's modal layers");
            let deepest = dialog.iter().map(|l| l.blur_radius).fold(px(0.0), gpui::Pixels::max);
            assert!(deepest > layers[1].blur_radius, "{variant:?}: a dialog stands above");
            assert!((scrim(&theme).a - theme.elevation.scrim).abs() < f32::EPSILON);
        }
    }

    /// A card rests on its edge: the card's wash inside the `border` line, with no shadow, in
    /// both variants. A card cut into parts takes its top line on the first part and its bottom
    /// one on the last.
    #[test]
    fn a_card_rests_on_its_edge() {
        for variant in [Variant::Dark, Variant::Light] {
            let theme = Theme::new(variant);
            let s = theme.surfaces;
            let mut whole = card(&theme);
            let style = whole.style();
            assert_eq!(style.background, Some(gpui::Fill::from(hsla(s.card))), "{variant:?}");
            assert_eq!(style.border_color, Some(hsla(s.border)), "{variant:?}: the border line");
            assert!(style.box_shadow.is_none(), "{variant:?}: no shadow at rest");
            let edges = |first, last| {
                let mut part = card_part(&theme, first, last);
                let w = &part.style().border_widths;
                (w.top.is_some(), w.bottom.is_some())
            };
            assert_eq!(edges(true, false), (true, false), "{variant:?}");
            assert_eq!(edges(false, true), (false, true), "{variant:?}");
            assert_eq!(edges(false, false), (false, false), "{variant:?}: a middle part");
        }
    }

    /// What is sunk holds shade inside its top edge, a point past any border; the segmented
    /// track is not sunk but ringed, `MonoCode`'s, on its plane.
    #[test]
    fn a_field_sinks_where_a_card_rises() {
        for variant in [Variant::Dark, Variant::Light] {
            let theme = Theme::new(variant);
            let layers = sunk(div(), &theme, 1.0).style().box_shadow.clone().unwrap_or_default();
            let [shade] = layers.as_slice() else { panic!("{variant:?}: one shade, {layers:?}") };
            assert!(shade.inset, "{variant:?}: inside");
            assert_eq!(shade.offset.y, px(1.0 + SUNK_DEPTH), "{variant:?}: past the border");
            assert!((shade.color.a - theme.elevation.sunk.shade).abs() < 1e-3, "{variant:?}");
            let tracked = track(&theme).style().clone();
            assert!(tracked.box_shadow.is_none(), "{variant:?}: the track is not sunk");
            assert!(tracked.background.is_none(), "{variant:?}: nor washed");
            assert_eq!(tracked.border_color, Some(hsla(theme.surfaces.border)), "{variant:?}");
        }
    }

    /// A field stands off the page in both modes by the `border` line round the card's wash.
    #[test]
    fn a_field_is_seen_on_its_page() {
        for variant in [Variant::Dark, Variant::Light] {
            let theme = Theme::new(variant);
            let style = field(div(), &theme).style().clone();
            let edged = style.border_widths.top.is_some_and(|w| w != px(0.0).into());
            assert!(edged, "{variant:?}: edged");
            assert_eq!(style.border_color, Some(hsla(theme.surfaces.border)), "{variant:?}");
            assert_eq!(style.background, Some(gpui::Fill::from(hsla(theme.surfaces.card))));
            assert!(style.box_shadow.is_some_and(|l| l.iter().all(|l| l.inset)), "{variant:?}");
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

    /// Streamed words lift on the theme's tokens: paced between the fade and the stream's
    /// longest, word after word, on the chrome's ease-out curve.
    #[test]
    fn streamed_words_lift_on_the_motion_tokens() {
        let (m, lift) = (Motion::DEFAULT, stream_motion());
        assert_eq!(lift.stream_fade_pacing(), Some((m.fade, m.stream)));
        assert_eq!(lift.stream_fade(), m.stream, "the first words, with no pace yet");
        assert_eq!(lift.stream_fade_stagger(), m.stream_stagger);
        let ((x1, y1), (x2, y2)) = (m.ease_out.p1, m.ease_out.p2);
        let curve = format!("{:?}", gpui_kit::base::motion::Easing::CubicBezier { x1, y1, x2, y2 });
        assert_eq!(format!("{:?}", lift.stream_fade_easing()), curve);
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
            && (squeezed.contains("border") || squeezed.contains("stroke"));
        (preset || boxed).then_some("a hairline a point wide, not `kit::HAIR` or `kit::rule`")
    }

    #[test]
    fn the_hairline_check_knows_a_hairline_from_a_point() {
        assert!(thick_hairline(".border_b_1()").is_some());
        assert!(thick_hairline(".border_1().border_color(hsla(s.border))").is_some());
        assert!(thick_hairline(".when(first, gpui::Styled::border_t_1)").is_some());
        assert!(thick_hairline("div().h(px(1.0)).bg(hsla(s.stroke))").is_some());
        assert!(thick_hairline(".border_b(kit::HAIR)").is_none());
        assert!(thick_hairline(".border(px(slopty_theme::stroke::LINE))").is_none(), "an edge");
        assert!(thick_hairline(".border_x_0()").is_none(), "no border");
        assert!(thick_hairline(".when(x, gpui::Styled::border_dashed)").is_none(), "a style");
        assert!(thick_hairline("// .border_1()").is_none(), "a comment");
    }

    /// Every border chrome draws is a hairline, one device pixel ([`HAIR`]), or a named stroke that
    /// marks something (`stroke::LINE`, `stroke::MARK`). At a full point each rule was two
    /// pixels on a Retina screen.
    #[test]
    fn a_chrome_border_is_kit_hair() {
        // Call sites whose owners move them onto `kit::HAIR` in their next change.
        const AWAITING: [&str; 0] = [];
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

    /// A control on the keyboard ring shows its hint once Tab brings the focus to it, on the
    /// warm group's wait as a pointer's does, and Esc hides the hint first, the focus staying.
    #[gpui::test]
    fn a_hint_shows_for_the_keyboard_and_esc_hides_it_first(cx: &mut TestAppContext) {
        struct Control;
        impl Render for Control {
            fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
                let theme = Theme::default();
                let s = theme.surfaces;
                let hinted = Rc::new(theme);
                div().size_full().p(px(40.0)).child(crate::a11y::tab_stop(
                    div()
                        .id("control")
                        .debug_selector(|| "control".to_owned())
                        .size(px(24.0))
                        .map(hint_timing)
                        .tooltip(move |_w, cx| {
                            gpui::AppContext::new(cx, |_| {
                                Hint::new("Fork from here", "", Rc::clone(&hinted))
                            })
                            .into()
                        }),
                    s.focus,
                ))
            }
        }
        cx.update(gpui_kit::init);
        let (_view, cx) = cx.add_window_view(|_, _| Control);
        cx.simulate_resize(gpui::size(px(200.0), px(200.0)));
        cx.run_until_parked();
        cx.simulate_keystrokes("tab");
        cx.update(|window, cx| {
            if window.focused(cx).is_none() {
                window.focus_next(cx);
            }
        });
        cx.run_until_parked();
        cx.executor().advance_clock(HINT_DELAY.saturating_mul(2));
        cx.run_until_parked();
        assert!(cx.debug_bounds("hint").is_some(), "Tab to it names it");
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(cx.debug_bounds("hint").is_none(), "Esc hides the hint");
        assert!(cx.update(|window, cx| window.focused(cx).is_some()), "and the focus stays");
    }

    /// A surface closed half way out and opened again turns back from where it stands rather
    /// than starting over from clear, and one closed leaves over the exit's time.
    #[gpui::test]
    fn a_surface_opened_on_its_way_out_turns_back(cx: &mut TestAppContext) {
        struct Surface {
            open: bool,
        }
        impl Render for Surface {
            fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
                let fill = gpui::rgb(0x0000_0000);
                div().size_full().child(presence(
                    div().id("surface").size(px(40.0)).bg(fill),
                    "surface",
                    self.open,
                ))
            }
        }
        /// The surface's opacity as the last frame painted it.
        fn shown(cx: &mut gpui::VisualTestContext) -> f32 {
            let lines = cx.update(|window, _| crate::retained::painted(window));
            lines
                .iter()
                .find_map(|line| {
                    let at = line.find("Quad")?;
                    let rest = line.get(at..)?;
                    let a = rest.find(" a: ")?;
                    let digits: String = rest
                        .get(a.saturating_add(4)..)?
                        .chars()
                        .take_while(|c| c.is_ascii_digit() || *c == '.')
                        .collect();
                    digits.parse().ok()
                })
                .unwrap_or(0.0)
        }
        let (view, cx) = cx.add_window_view(|_, _| Surface { open: true });
        cx.simulate_resize(gpui::size(px(100.0), px(100.0)));
        cx.executor().advance_clock(Pace::Fade.duration().saturating_mul(2));
        cx.update(Window::simulate_next_frame);
        cx.run_until_parked();
        assert!((shown(cx) - 1.0).abs() < 1e-3, "whole once in: {}", shown(cx));

        view.update(cx, |v, cx| {
            v.open = false;
            cx.notify();
        });
        cx.run_until_parked();
        cx.executor().advance_clock(Pace::Exit.duration().div_f32(2.0));
        cx.update(Window::simulate_next_frame);
        cx.run_until_parked();
        let halfway = shown(cx);
        assert!(halfway > 0.0 && halfway < 1.0, "on its way out: {halfway}");

        view.update(cx, |v, cx| {
            v.open = true;
            cx.notify();
        });
        cx.run_until_parked();
        cx.update(Window::simulate_next_frame);
        cx.run_until_parked();
        assert!(
            shown(cx) >= halfway - 1e-3,
            "turned back from {halfway}, not from clear: {}",
            shown(cx)
        );
    }

    /// Every hint keeps the warm group's timing: GPUI builds it at once ([`hint_timing`]) and
    /// the [`Hint`] holds the wait, so a tooltip built on GPUI's own half-second delay would
    /// wait twice and never join the group.
    #[test]
    fn a_hint_keeps_the_warm_timing() {
        // Call sites whose owners move them onto `kit::hint_timing` in their next change.
        const AWAITING: [&str; 0] = [];
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

    /// A painted line is whole device pixels, never under one, as GPUI snaps a border: one point
    /// at 1x, 2x and 3x.
    #[test]
    fn a_painted_line_is_one_point_in_whole_device_pixels() {
        for scale in [1.0_f32, 2.0, 3.0] {
            let device = f32::from(hair_painted(scale)) * scale;
            assert!((device - scale).abs() < 1e-4, "{scale}x: {device} device pixels");
        }
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
        assert_eq!((Pace::Toast.duration(), Pace::Toast.curve()), (m.toast, m.ease_out));
        assert_eq!(Pace::Exit.curve(), m.ease_out);
        assert_eq!((Pace::Pane.duration(), Pace::Pane.curve()), (m.pane, m.ease_out));
        assert_eq!((Pace::Reveal.duration(), Pace::Reveal.curve()), (m.reveal, m.ease_out));
        assert!(Pace::Sheet.duration() < Pace::Pane.duration(), "a pane travels further");
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

    /// The chrome draws its icons one way (`docs/decisions/ui.md`, "The chrome's icons are
    /// Tabler's, and a file's are Material's"): no SVG is drawn in the app's own code but the
    /// app's mark and the icon modules, and no icon name of another set is left.
    #[test]
    fn the_chrome_draws_its_own_icons() {
        let mut stray = Vec::new();
        for dir in ["slopty-ui/src", "slopty-app/src"] {
            for (file, line, text) in chrome_lines(dir) {
                let code = text.split("//").next().unwrap_or_default();
                // The app's mark is a drawing of ours (`brand`), `icons.rs` names the kit's
                // drawings it stands glyphs in for and draws the files' icons, `icons/glyphs.rs`
                // and `icons/marks.rs` read Tabler's and the agents' owners' outlines into masks,
                // and `file_types.rs` keeps Material's files. A git glyph (`GitGlyph::`) is one
                // of the glyphs.
                let mark = code.contains("assets/icon.svg")
                    || file.ends_with("icons.rs")
                    || file.ends_with("icons/glyphs.rs")
                    || file.ends_with("icons/marks.rs")
                    || file.ends_with("file_types.rs");
                let code = code.replace("GitGlyph::", "");
                if !mark
                    && ["svg()", ".svg\"", "IconName", "Glyph::"].iter().any(|b| code.contains(b))
                {
                    stray.push(format!("{file}:{line}: {}", text.trim()));
                }
            }
        }
        assert!(stray.is_empty(), "an icon drawn another way:\n{}", stray.join("\n"));
    }

    /// An agent's mark wears no colour of its own (`docs/decisions/brand.md`, "Each agent
    /// wears its owner's mark"): it is drawn as coverage alone and painted in the ink of the
    /// words beside it, never in a brand's clay, green or coral, so colour keeps meaning state.
    #[test]
    fn an_agents_mark_wears_no_colour_of_its_own() {
        let marks = include_str!("icons/marks.rs");
        let icons = include_str!("icons.rs");
        let between = |from: &str, to: &str| {
            icons.split(from).nth(1).and_then(|rest| rest.split(to).next()).unwrap_or_default()
        };
        let paint = between("fn paint_agent(", "fn paint_file(");
        let centred = between("fn paint_centred(", "\n}\n");
        assert!(paint.contains("paint_centred("), "painted as a mask is");
        assert!(centred.contains("window.text_style().color"), "painted in the words' ink");
        let painters = [("icons.rs paint_agent", paint), ("icons.rs paint_centred", centred)];
        for (name, code) in std::iter::once(("icons/marks.rs", marks)).chain(painters) {
            for (n, line) in code.lines().enumerate() {
                let code = line.split("//").next().unwrap_or_default();
                let tones = ["surfaces", "Rgb", "rgb(", "hsla(", "Hsla {", "0x", "\"#"];
                assert!(
                    !tones.iter().any(|t| code.contains(t)),
                    "{name}:{}: a colour in an agent's mark: {}",
                    n.saturating_add(1),
                    line.trim()
                );
            }
        }
        for svg in [
            include_str!("../assets/agents/claude.svg"),
            include_str!("../assets/agents/openai.svg"),
        ] {
            assert!(svg.contains("fill=\"currentColor\""), "the outline carries no colour");
        }
    }

    /// An icon takes its words' size: a symbol is sized only in `icons.rs`, from the type
    /// scale ([`crate::icons::IconSize`], [`crate::icons::Drawn`]), and painted only there, so
    /// no call site picks a point size of its own.
    #[test]
    fn an_icon_takes_its_words_size() {
        let mut stray = Vec::new();
        for (file, line, text) in chrome_lines("slopty-ui/src") {
            if file.ends_with("icons.rs") {
                continue;
            }
            let code = text.split("//").next().unwrap_or_default();
            if ["SymbolSize", "paint_mask(", "rasterize("].iter().any(|bad| code.contains(bad)) {
                stray.push(format!("{file}:{line}: {}", text.trim()));
            }
        }
        assert!(stray.is_empty(), "a symbol sized apart from its words:\n{}", stray.join("\n"));
    }
}

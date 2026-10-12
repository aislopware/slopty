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
use slopty_theme::{Motion, Rgb, Theme, Variant, alpha, stroke};

use crate::colors::{hsla, hsla_alpha};

mod change;
mod disclosure;
mod facts;
pub mod find;
mod fit;
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
/// Into a hover or a press and back to rest over [`Motion::feedback`], eased out; Zed's land at
/// once, so with no time to take nothing is eased. Only colours ease; the state itself, and
/// layout, change at once, and under Reduce Motion GPUI applies every change at once. Every kit
/// control that answers the pointer wears it, so the durations live here and nowhere else.
///
/// Not on the rows of a list GPUI composites as a scroll layer (the navigator's): a fill easing
/// out under rows that scroll past the pointer repaints the layer, and measured headless it
/// composited 2 of 30 scroll frames where the list composites all 30
/// (`nav_list::a_scroll_of_the_navigator_composites_its_layer`).
pub fn eased<E: gpui::StatefulInteractiveElement>(el: E) -> E {
    let m = Motion::DEFAULT;
    if m.feedback.is_zero() {
        return el;
    }
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

/// A diff's size, `+12 ‒3`, Delta's: in the monospace face, each side in its tone. `None` for
/// no change.
///
/// What was added is in `success` and what was taken in `error`; the side that is zero is left
/// out, and the two sides are a thin space apart.
///
/// The one way a count of changed lines is drawn: in a tile's header, a diff's head, a fold, a
/// navigator row and the breadcrumb. A zero side is never drawn, so no red "‒0" says an error
/// about nothing. The caller sets the size and adds an identity and a spoken label.
///
/// Its parts are text, the thin space between the sides included, so they take the size the
/// caller sets.
#[must_use]
pub fn changes(theme: &Theme, added: u32, removed: u32) -> Option<Div> {
    let s = &theme.surfaces;
    let side = |sign: &'static str, tone: Rgb, n: u32| {
        (n > 0)
            .then(|| div().text_color(hsla(tone)).child(SharedString::from(format!("{sign}{n}"))))
    };
    (added > 0 || removed > 0).then(|| {
        div()
            .flex_none()
            .flex()
            .items_center()
            .whitespace_nowrap()
            .font_family(mono_family(theme))
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
    div().flex_none().text_color(separator_ink(theme)).child("\u{b7}")
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

/// A state's pill: [`pill_frame`] filled with `tone` at [`alpha::FAINT`], its words in `tone`.
///
/// A header holds one of these at most, the state's (an agent waiting, working), so it is the
/// one shape there that stands out. The caller adds the identity, the role and the words.
#[must_use]
pub fn pill(theme: &Theme, tone: Rgb) -> Div {
    pill_frame(theme).bg(hsla_alpha(tone, alpha::FAINT)).text_color(hsla(tone))
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
/// The work's ground inside the `border` line, with no shadow: a resting thing is told by its
/// edge, as Zed's cards rest on the editor's background. A hover or
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
    el.border_color(hsla(theme.surfaces.border))
}

/// `el` set into its plane as a quiet well: code, a command's output, an attachment's chip, the
/// person's own message, the foot of a sheet. Delta's well, with no edge of its own.
#[must_use]
pub fn inset<E: Styled>(el: E, theme: &Theme) -> E {
    el.bg(hsla(theme.surfaces.well))
}

/// `el` as the one card a thread's column keeps, Zed's agent panel's tool card.
///
/// It holds a call put to the person (a permission, a question, a plan to approve): the
/// work's ground inside the `border` line at `radii.md`, clipped to its corners, its head on
/// [`card_head`].
#[must_use]
pub fn call_card<E: Styled>(el: E, theme: &Theme) -> E {
    raised(el, theme).rounded(px(theme.radii.md)).overflow_hidden()
}

/// `el` as a [`call_card`]'s head.
///
/// The element's ground a shade toward the text (Zed's 2.5 %), the lighter line under it.
#[must_use]
pub fn card_head<E: Styled>(el: E, theme: &Theme) -> E {
    el.bg(hsla(theme.surfaces.card_head)).border_b(HAIR).border_color(hsla(theme.surfaces.stroke))
}

/// `el` on Zed's activity bar's ground, over a thread's composer.
///
/// The work's ground moved [`alpha::QUIET`] toward the selection, so the plan and the queue
/// above the composer read as the composer's own shelf and not as a well of code.
#[must_use]
pub fn activity<E: Styled>(el: E, theme: &Theme) -> E {
    let s = theme.surfaces;
    el.bg(hsla(s.ground.mix(s.selected.over(s.ground), alpha::QUIET)))
}

/// `el` as Delta's code well: a diff, a command and its output, a code block.
///
/// The [`inset`] ground at `radii.sm` inside the lighter `stroke` line, with the soft shade of
/// the theme's `sunk.well` held inside it, so code reads as set into the page rather than laid
/// on it.
#[must_use]
pub fn well<E: Styled>(el: E, theme: &Theme) -> E {
    let layer = theme.elevation.sunk.well;
    inset(el, theme)
        .rounded(px(theme.radii.sm))
        .border(HAIR)
        .border_color(hsla(theme.surfaces.stroke))
        .shadow(vec![BoxShadow { inset: true, ..drop_shadow(theme, layer) }])
}

/// `el` drawn as a field's ground: the work's ground inside the `border` line, sunk ([`sunk`])
/// so it reads as somewhere to type rather than something to press.
#[must_use]
pub fn field<E: Styled>(el: E, theme: &Theme) -> E {
    let el =
        el.bg(hsla(theme.surfaces.ground)).border(HAIR).border_color(hsla(theme.surfaces.border));
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
        .child(crate::icons::icon(theme, crate::icons::Symbol::Magnifyingglass, hsla(s.text_muted)))
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

/// A segmented control's track, Zed's toggle button group: the `border` ring at `radii.sm` on
/// its plane.
///
/// Its options are joined edge to edge inside it ([`TRACK_PAD`] is none). The thumb
/// ([`paint_thumb`]) rides in it.
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

/// How far a segmented control's track holds its options in from its edge: none, as Zed's
/// group joins its buttons inside its ring.
pub const TRACK_PAD: f32 = 0.0;

/// A segmented control's thumb at `bounds`: the selected wash, Zed's chosen button in a group,
/// with no ring and nothing raised.
///
/// At [`thumb_radius`]. The selection plate paints it as it slides from option to option.
pub fn paint_thumb(theme: &Theme, bounds: gpui::Bounds<gpui::Pixels>, window: &mut Window) {
    let radius = gpui::Corners::all(px(thumb_radius(theme)));
    window.paint_quad(gpui::fill(bounds, hsla(theme.surfaces.selected)).corner_radii(radius));
}

/// A segmented thumb's radius: the track's `radii.sm` less its line, so a thumb at either end
/// fills the track's corner, and one between reads as joined.
#[must_use]
pub fn thumb_radius(theme: &Theme) -> f32 {
    slopty_theme::Radii::nested(theme.radii.sm, stroke::LINE, TRACK_PAD)
}

/// `el` lifted off the chrome, Zed's elevated surface: the `elevated` plane inside the lighter
/// `stroke` line (Zed's `border.variant`) under the float's shadow.
///
/// Everything that floats wears it: the palette, a menu, a popover, a hint, a find bar, a chip
/// over a body. The caller keeps its own radius: `radii.lg` for a menu, a popover, a hint or a
/// toast, the control's own for a chip. A dialog takes [`dialog`].
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

/// A dialog's head, Zed's modal header: its title and what it says, `px 12 pt 8 pb 4`.
#[must_use]
pub fn dialog_head(theme: &Theme) -> Div {
    let sp = theme.spacing;
    div().flex().flex_col().gap(px(sp.xs)).px(px(sp.md)).pt(px(sp.sm)).pb(px(sp.xs))
}

/// A part of a dialog under its head, Zed's modal section: `p 8`, in to the head's edge.
#[must_use]
pub fn dialog_section(theme: &Theme) -> Div {
    let sp = theme.spacing;
    div().flex().flex_col().gap(px(sp.xs)).px(px(sp.md)).py(px(sp.sm))
}

/// A dialog's foot, Zed's modal footer: its buttons at the trailing end, `p 8`, under the
/// lighter line.
#[must_use]
pub fn dialog_foot(theme: &Theme) -> Div {
    let sp = theme.spacing;
    div()
        .flex()
        .justify_end()
        .gap(px(sp.xs))
        .p(px(sp.sm))
        .border_t(HAIR)
        .border_color(hsla(theme.surfaces.stroke))
}

/// A dialog's or a panel's title: the panel title role (16/22 at the strong weight), in `text`.
///
/// The strong weight is for titles like this one and for headings; a row, a tab or a name that
/// has to stand out says so by its tone, as Zed's do.
#[must_use]
pub fn title(theme: &Theme, text: impl Into<SharedString>) -> Div {
    typed(div(), theme.roles().panel_title).text_color(hsla(theme.surfaces.text)).child(text.into())
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

/// A list row, Zed's list item: its density's height, starting on the edge grid ([`inset_x`])
/// and ending 6 in ([`slopty_theme::Spacing::inset_trailing`]), its parts centred.
///
/// Its parts sit [`slopty_theme::Spacing::xs`] apart, Zed's `gap_1`. The navigator, the palette,
/// the inbox and the menus draw their rows from it, so one density switch moves them all. The
/// trailing end is usually an icon button or a count, which brings its own room.
#[must_use]
pub fn row(theme: &Theme, lines: Row) -> Div {
    div()
        .pl(px(theme.spacing.inset()))
        .pr(px(theme.spacing.inset_trailing()))
        .flex_none()
        .flex()
        .items_center()
        .gap(px(theme.spacing.xs))
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

/// A rule parting a list's or a menu's groups, Zed's list separator: a [`rule`] in `stroke`
/// (`border.variant`) across the list, [`slopty_theme::Spacing::xs`] above and below it.
#[must_use]
pub fn list_rule(theme: &Theme) -> gpui::AnyElement {
    div()
        .flex_none()
        .w_full()
        .py(px(theme.spacing.xs))
        .child(rule(theme.surfaces.stroke))
        .into_any_element()
}

/// A vertical rule: [`rule`] standing up, the height of its parent.
#[must_use]
pub fn rule_v(tint: slopty_theme::Tint) -> Div {
    div().flex_none().h_full().border_l(HAIR).border_color(hsla(tint))
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
pub fn sheet_row(theme: &Theme, lines: Row) -> Div {
    let pad = sheet_pad(theme);
    row(theme, lines).pl(px(theme.spacing.inset() - pad)).rounded(px(theme.radii.sm))
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
/// The metadata role's size (12, a finger's 13) in `text_secondary`, never upper case, Zed's
/// list header; the strong weight stays for one thing per region, so a head never rivals the
/// names under it.
#[must_use]
pub fn label(theme: &Theme, text: impl Into<SharedString>) -> Div {
    div()
        .flex_none()
        .text_size(px(theme.roles().metadata.size))
        .text_color(hsla(theme.surfaces.text_secondary))
        .child(text.into())
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
                .slot(px(theme.typography.icon()), hsla(s.text_secondary)),
        );
    eased(crate::a11y::tab_stop(el, s.focus))
        .hover(move |el| el.bg(hsla(s.hover)).text_color(hsla(s.text)))
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
    eased(square_icon(theme, id.into(), icon, label, ink)).hover(move |el| el.bg(hsla(s.hover)))
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
        .child(crate::icons::Drawn::new(theme, icon).slot(px(theme.typography.icon()), hsla(ink)));
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
/// the caption size, in the secondary ink until on, when it takes the text
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

/// A tick box, Zed's checkbox: 16 pt at radius 2.
///
/// The control's ground inside the `border` line while off, [`tinted`] with its check in the
/// text's ink while on. The caller makes it, or the row it leads, the thing pressed, with
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
        .rounded(px(theme.radii.xs));
    if on {
        tinted(el, theme).child(
            crate::icons::icon(theme, crate::icons::Symbol::Checkmark, hsla(s.text))
                .size(px(theme.typography.small())),
        )
    } else {
        el.bg(hsla(s.element)).border(HAIR).border_color(hsla(s.border))
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
        .child(SharedString::from(drawn_keys(&keys.into())))
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

/// How wide a hint's words grow before they wrap: Zed's tooltip's `max_w_72`, 288.
pub const HINT_MEASURE: f32 = 288.0;

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
            .gap(px(theme.spacing.lg))
            .px(px(theme.spacing.sm))
            .py(px(theme.spacing.xs))
            .rounded(px(theme.radii.lg))
            .text_size(px(theme.roles().chrome.size))
            .font_family(theme.typography.ui_family.clone())
            .child(
                div()
                    .max_w(px(HINT_MEASURE))
                    .text_color(hsla(s.text))
                    .when(self.mono, |el| el.font_family(mono_family(theme)))
                    .child(self.what.clone()),
            )
            .when(!self.key.is_empty(), |el| {
                el.child(div().text_color(hsla(s.text_muted)).child(self.key.clone()))
            });
        fade_in(hint, "hint", cx)
    }
}

/// The ink of the dot between the facts of a meta line: the muted tone at the pressed step.
///
/// The dot divides the facts without reading as one of them (monocode's footer sets its dot
/// at a quarter of the text). The hairline's own colour vanished on the light theme's `canvas`.
#[must_use]
pub fn separator_ink(theme: &Theme) -> Hsla {
    hsla_alpha(theme.surfaces.text_muted, alpha::PRESSED)
}

/// The UI's monospace family: a port's number and the settings file are set in it. Paths in
/// chrome are context, in the UI face.
#[must_use]
pub fn mono_family(theme: &Theme) -> SharedString {
    theme.typography.mono_families.first().cloned().unwrap_or_default().into()
}

/// Keys as they are drawn, ↩ asking for its text form.
///
/// ↩ has an emoji form too, and a phone's font fallback picks it: a blue tile among plain
/// glyphs. The text presentation selector after it asks for the glyph. Only the drawing
/// carries the selector; what a screen reader gets keeps the bare key.
#[must_use]
pub fn drawn_keys(keys: &str) -> String {
    const TEXT_PRESENTATION: char = '\u{FE0E}';
    keys.chars()
        .flat_map(|c| [Some(c), (c == '↩').then_some(TEXT_PRESENTATION)])
        .flatten()
        .collect()
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

    /// The first line is the first with something on it, trimmed.
    #[test]
    fn return_is_drawn_as_text_not_as_an_emoji() {
        assert_eq!(drawn_keys("⇧⌘↩"), "⇧⌘↩\u{FE0E}");
        assert_eq!(drawn_keys("↩"), "↩\u{FE0E}");
        assert_eq!(drawn_keys("⌘T"), "⌘T", "nothing else changes");
        assert_eq!(drawn_keys("↑↓"), "↑↓", "the arrows have no emoji form");
    }

    /// A code well is Delta's: the well's ground at `radii.sm` in the lighter line, its shade
    /// soft and held inside it, never cast out.
    #[test]
    fn a_code_well_is_sunk_softly_inside_its_line() {
        for variant in [Variant::Dark, Variant::Light] {
            let theme = Theme::new(variant);
            let mut el = well(div(), &theme);
            let style = el.style();
            assert_eq!(style.background, Some(gpui::Fill::from(hsla(theme.surfaces.well))));
            assert_eq!(style.border_color, Some(hsla(theme.surfaces.stroke)), "{variant:?}");
            assert_eq!(style.corner_radii.top_left, Some(px(theme.radii.sm).into()));
            let shade = style.box_shadow.as_ref().and_then(|s| s.first()).cloned();
            let shade = shade.expect("a shade");
            assert!(shade.inset, "{variant:?}: held inside");
            assert!(shade.blur_radius > px(0.0), "{variant:?}: soft");
        }
    }

    #[test]
    fn the_first_line_skips_blank_ones() {
        assert_eq!(first_line("\n  \n  cargo build  \nnext"), "cargo build");
        assert_eq!(first_line(""), "");
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
                assert_eq!(kit.colors.primary, hsla(theme.surfaces.accent_fill), "the accent");
                assert_eq!(kit.colors.primary_foreground, hsla(theme.surfaces.accent_ink));
                // Its primary button is the tinted accent, and its tokens, what it paints from,
                // say so too: a write to the colours alone left a black solid Next.
                let tinted = hsla(theme.surfaces.info_background);
                assert_eq!(kit.colors.button_primary, tinted, "the tinted accent");
                assert_eq!(kit.tokens.button_primary.color, tinted, "in its tokens too");
                assert_eq!(kit.colors.button_primary_foreground, hsla(theme.surfaces.text));
                assert_eq!(kit.colors.caret, hsla(theme.surfaces.text), "a neutral caret");
                assert_eq!(kit.font_family.as_ref(), crate::fonts::UI_FAMILY, "the bundled faces");
                assert_eq!(kit.mono_font_family.as_ref(), crate::fonts::MONO_FAMILY);
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

    /// A pill is its fixed height and a chip at `radii.sm`, and the state's pill is the frame
    /// filled at the faint step.
    #[test]
    fn a_pill_is_a_twenty_point_chip() {
        for (variant, step) in [(Variant::Dark, alpha::FAINT), (Variant::Light, alpha::FAINT)] {
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

    /// Zed's styles: the primary the accent's quiet ground in its line, the destructive the
    /// error's, the secondary clear inside the lighter line and the rest bare, all one
    /// control's height and flat, in both variants. A selection where the keyboard is is the
    /// keyed wash with the focus line inside it, and elsewhere the selected wash alone.
    #[test]
    fn a_button_is_zeds_and_one_height() {
        for variant in [Variant::Dark, Variant::Light] {
            let theme = Theme::new(variant);
            let s = theme.surfaces;
            let fill = |kind| button(&theme, "b", "Go", kind).style().background.clone();
            let blue = Some(gpui::Fill::from(hsla(s.info_background)));
            assert_eq!(fill(ButtonKind::Primary), blue, "{variant:?}: tinted accent");
            let mut primary = button(&theme, "b", "Go", ButtonKind::Primary);
            assert_eq!(primary.style().border_color, Some(hsla(s.info_border)), "{variant:?}");
            let red = Some(gpui::Fill::from(hsla(s.error_background)));
            assert_eq!(fill(ButtonKind::Destructive), red);
            assert_eq!(fill(ButtonKind::Secondary), None, "{variant:?}: clear at rest");
            let mut second = button(&theme, "b", "Go", ButtonKind::Secondary);
            assert_eq!(second.style().border_color, Some(hsla(s.stroke)), "{variant:?}");
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

    /// Under the touch density an icon button is a finger's 44 pt round the same icon, and the
    /// rows grow with it; under the compact one they are Zed's: a 22 pt button, 24 pt rows.
    #[test]
    fn density_sizes_the_targets_not_the_icons() {
        let mut theme = Theme::default();
        assert!((icon_button_side(&theme) - 22.0).abs() < f32::EPSILON, "compact: Zed's 22");
        assert!((Row::One.height(&theme) - 24.0).abs() < f32::EPSILON);
        assert!((Row::Two.height(&theme) - 42.0).abs() < f32::EPSILON);
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

    /// A card rests on its edge: the work's ground inside the `border` line, with no shadow, in
    /// both variants. A card cut into parts takes its top line on the first part and its bottom
    /// one on the last.
    #[test]
    fn a_card_rests_on_its_edge() {
        for variant in [Variant::Dark, Variant::Light] {
            let theme = Theme::new(variant);
            let s = theme.surfaces;
            let mut whole = card(&theme);
            let style = whole.style();
            assert_eq!(style.background, Some(gpui::Fill::from(hsla(s.ground))), "{variant:?}");
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
    /// track is not sunk but ringed, Zed's toggle group, on its plane, its thumb filling its
    /// corner.
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
            let corner = theme.radii.sm - stroke::LINE;
            assert!((thumb_radius(&theme) - corner).abs() < f32::EPSILON, "{variant:?}");
        }
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
            let mut sheet = sheet_row(&theme, Row::One);
            let row = sheet.style();
            let radius = px(theme.radii.sm);
            assert_eq!(row.corner_radii.top_left, Some(radius.into()), "{variant:?}");
            assert_eq!(row.gap.width, Some(px(theme.spacing.xs).into()), "{variant:?}");
            assert_eq!(row.size.height, Some(px(24.0).into()), "{variant:?}");
        }
    }

    /// A field stands off the page in both modes by the `border` line round the work's ground.
    #[test]
    fn a_field_is_seen_on_its_page() {
        for variant in [Variant::Dark, Variant::Light] {
            let theme = Theme::new(variant);
            let style = field(div(), &theme).style().clone();
            let edged = style.border_widths.top.is_some_and(|w| w != px(0.0).into());
            assert!(edged, "{variant:?}: edged");
            assert_eq!(style.border_color, Some(hsla(theme.surfaces.border)), "{variant:?}");
            assert_eq!(style.background, Some(gpui::Fill::from(hsla(theme.surfaces.ground))));
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
        // Zed's two paces: fast for a fade or a settle, slow for what travels.
        assert!(Pace::Sheet.duration() <= Pace::Pane.duration(), "a pane travels as far");
        assert!(Pace::Exit.duration() < Pace::Fade.duration(), "out quicker than in");
        assert_eq!(Pace::Fade.duration(), Pace::Settle.duration(), "Zed's fast");
        assert!(Pace::Settle.duration() < Pace::Sheet.duration(), "Zed's slow");
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
        let mut drawn = changes(&theme, 1, 0).unwrap_or_else(|| panic!("a change is drawn"));
        let family = drawn.style().text.font_family.clone();
        assert_eq!(family, Some(mono_family(&theme)), "Delta's stat is in Lilex");
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

//! The icons the chrome draws: GitHub's Octicons ([`Symbol`]), each drawn on whole device
//! pixels in one ink, the words' beside them, as Zed draws its icons.
//!
//! A [`Symbol`] is one of a closed list. Nothing else is drawn by us but the marks of a state
//! ([`Ring`]) (`docs/decisions/ui.md`, "The chrome's icons are Octicons at 14" and "State is a
//! glyph").
//!
//! Every icon stands in one slot, [`slopty_theme::Typography::icon`] (16 pt), beside a row's
//! words, in bars and in buttons alike. A glyph drawn on Octicons' 16 grid spans
//! [`slopty_theme::Typography::glyph`] (14) of it, where its 1.5 line is the words' stem; one
//! drawn on the 12 grid (a chevron, a tab's close) spans 12, its art's own size. A slot sized again
//! draws its icon larger or smaller with it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, Bounds, DevicePixels, Div, Element, ElementId, EntityId, Global,
    GlobalElementId, Hsla, InspectorElementId, IntoElement, LayoutId, ParentElement as _, Pixels,
    SharedString, Styled as _, TransformationMatrix, Window, canvas, div, point, px,
};
use slopty_platform::outline::Mask;
use slopty_theme::Theme;

mod glyphs;
mod ring;

pub use glyphs::Symbol;
pub use ring::{Knock, Ring};

/// The glyph for an icon gpui-kit's components name by its asset path (`icons/check.svg`),
/// where the chrome has one.
#[must_use]
pub fn kit_symbol(path: &str) -> Option<Symbol> {
    let name = path.strip_prefix("icons/")?.strip_suffix(".svg")?;
    Some(match name {
        "check" => Symbol::Checkmark,
        "x" | "close" => Symbol::Xmark,
        "chevron-down" => Symbol::ChevronDown,
        "chevron-up" => Symbol::ChevronUp,
        "chevron-left" => Symbol::ChevronLeft,
        "chevron-right" => Symbol::ChevronRight,
        "minus" => Symbol::Minus,
        "plus" => Symbol::Plus,
        "search" => Symbol::Magnifyingglass,
        "eye" => Symbol::Eye,
        "eye-off" => Symbol::EyeSlash,
        "copy" => Symbol::DocOnDoc,
        "info" => Symbol::InfoCircle,
        "circle-x" => Symbol::XmarkCircle,
        "circle-check" => Symbol::CheckmarkCircle,
        "triangle-alert" => Symbol::ExclamationmarkTriangle,
        "ellipsis" => Symbol::Ellipsis,
        _ => return None,
    })
}

/// How a glyph is drawn in a slot: its grid spans `share` of the slot's side.
#[derive(Clone, Copy, Debug)]
pub struct Drawn {
    symbol: Symbol,
    share: f32,
}

impl Drawn {
    /// `symbol` in the icon slot: a glyph on Octicons' 16 grid across [`Theme::glyph`] (14 of
    /// 16; a finger's 18 of 20), one on the 12 grid across its art's own size
    /// ([`Theme::glyph_small`]).
    #[must_use]
    pub fn new(theme: &Theme, symbol: Symbol) -> Self {
        let drawn = if symbol.small_art() { theme.glyph_small() } else { theme.glyph() };
        Self { symbol, share: drawn / theme.icon() }
    }

    /// The side in points of the grid a glyph is drawn on in a slot `side` points square.
    #[must_use]
    pub fn grid(self, side: f32) -> f32 {
        side * self.share
    }

    /// The slot, `side` square, its mark in `color`. The slot can be sized again
    /// (`.size(..)`), and the mark is drawn larger or smaller by as much; a glyph's ink is the
    /// slot's text colour, so `.text_color(..)` recolours it, a file's icon too.
    #[must_use]
    pub fn slot(self, side: Pixels, color: Hsla) -> Div {
        div().flex_none().size(side).text_color(color).child(
            canvas(|_, _, _| {}, move |bounds, (), window, _cx| self.paint(bounds, window))
                .size_full(),
        )
    }

    /// Paints the glyph centred in `bounds`.
    fn paint(self, bounds: Bounds<Pixels>, window: &mut Window) {
        self.paint_glyph(self.symbol, bounds, window);
    }

    /// `symbol`'s mask as it is painted in a slot `side` points square on a display of
    /// `scale` device pixels to the point: its grid a whole number of device pixels.
    fn glyph_mask(self, symbol: Symbol, side: f32, scale: f32) -> Kept {
        glyphs::mask(symbol, whole_pixels(self.grid(side) * scale))
    }

    /// Paints a glyph with its grid centred in `bounds`, on the device's pixel grid.
    fn paint_glyph(self, symbol: Symbol, bounds: Bounds<Pixels>, window: &mut Window) {
        let side = f32::from(bounds.size.width.min(bounds.size.height));
        let Some((mask, key)) = self.glyph_mask(symbol, side, window.scale_factor()) else {
            return;
        };
        paint_centred(&mask, key, bounds, window, || symbol.name().to_owned());
    }
}

/// `points` (already in device pixels) rounded to a whole count of them; none for an empty
/// or unreadable size.
fn whole_pixels(points: f32) -> u32 {
    let pixels = points.round();
    #[expect(clippy::cast_possible_truncation, reason = "an icon is a few dozen pixels")]
    #[expect(clippy::cast_sign_loss, reason = "a negative size is caught as no size")]
    let pixels = if pixels.is_finite() && pixels > 0.0 { pixels as u32 } else { 0 };
    pixels
}

/// The origin, in points, of a box `wide` × `high` device pixels centred in `bounds` with its
/// origin on a whole device pixel.
fn place(bounds: Bounds<Pixels>, wide: f32, high: f32, device: f32) -> gpui::Point<Pixels> {
    let centre = bounds.center().scale(device);
    let snap = |middle: f32, extent: f32| px((middle - extent / 2.0).round() / device);
    point(snap(centre.x.0, wide), snap(centre.y.0, high))
}

/// Paints `mask` in the text's ink with its box centred in `bounds` and its origin on a whole
/// device pixel; `what` names it if it fails.
fn paint_centred(
    mask: &Arc<Mask>,
    key: SharedString,
    bounds: Bounds<Pixels>,
    window: &mut Window,
    what: impl FnOnce() -> String,
) {
    let (Ok(width), Ok(height)) = (i32::try_from(mask.width), i32::try_from(mask.height)) else {
        return;
    };
    let device = window.scale_factor();
    #[expect(clippy::cast_precision_loss, reason = "a mask is a few dozen pixels")]
    let origin = place(bounds, mask.width as f32, mask.height as f32, device);
    let devices = gpui::size(DevicePixels(width), DevicePixels(height));
    let ink = window.text_style().color;
    let painted =
        window.paint_mask(origin, devices, key, TransformationMatrix::unit(), ink, || {
            Ok(Some(mask.alpha.clone()))
        });
    if let Err(error) = painted {
        tracing::warn!(%error, mark = what(), "a mark was not painted");
    }
}

/// A drawn mask with the key the atlas keeps it under.
type Kept = Option<(Arc<Mask>, SharedString)>;

/// `symbol` in the icon slot, in `color`; a slot sized again draws the mark at its size.
#[must_use]
pub fn icon(theme: &Theme, symbol: Symbol, color: Hsla) -> Div {
    Drawn::new(theme, symbol).slot(px(theme.icon()), color)
}

/// `symbol` in a slot `side` square, in `ink`: a row's lead at whatever size.
#[must_use]
pub fn symbol(theme: &Theme, symbol: Symbol, side: Pixels, ink: Hsla) -> AnyElement {
    Drawn::new(theme, symbol).slot(side, ink).into_any_element()
}

/// The one vocabulary for how a thing is doing, wherever it is shown: a header, a row, a
/// toast. Each state has one glyph and one hue, so a glance
/// reads the same everywhere.
///
/// Every state is a glyph, never a dot: one family of circles, filled where it carries colour
/// (`docs/decisions/ui.md`, "State is a glyph"). The glyph carries the hue and the words beside
/// it stay neutral: working blue, needs you amber, failed red and a finish not yet seen green,
/// each in its mark's fill step. Waiting, idle and a worker out of reach are
/// grey, so amber means "needs you" and nothing else.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    /// Nothing to say: a shell at its prompt, an agent at rest.
    Idle,
    /// Busy on its own: an agent thinking or running a tool, a remote picture on its way.
    Working,
    /// *Waiting*: a shell's command running for a while, or an agent's turn paused on work in
    /// the background. Busy, but nothing to watch for: the muted tone and a still dashed ring.
    Running,
    /// Waiting on the human: a permission, a question, an elicitation.
    NeedsYou,
    /// Finished and not yet looked at.
    Done,
    /// Failed: a command's non-zero exit, a session that ended in error.
    Failed,
    /// Out of reach: a worker away, a session reconnecting.
    Away,
}

impl Status {
    /// The mark drawn by us for it ([`status_icon`]): a ring for idle, a dashed ring for
    /// waiting, the turning arc for working, and a disc with its figure knocked out for the
    /// three that carry colour, so the colour has a body at 1x. `None` for away, which is
    /// Octicons' glyph ([`Self::symbol`]).
    #[must_use]
    pub const fn ring(self) -> Option<Ring> {
        match self {
            Self::Idle => Some(Ring::Empty),
            Self::Running => Some(Ring::Dashed),
            Self::Working => Some(Ring::Turning(0)),
            Self::NeedsYou => Some(Ring::Disc(Knock::Bang)),
            Self::Done => Some(Ring::Disc(Knock::Check)),
            Self::Failed => Some(Ring::Disc(Knock::Cross)),
            Self::Away => None,
        }
    }

    /// The glyph that marks it where no mark of ours does: a worker out of reach.
    #[must_use]
    pub const fn symbol(self) -> Option<Symbol> {
        match self {
            Self::Away => Some(Symbol::WifiSlash),
            _ => None,
        }
    }
}

/// `status`'s icon, `side` square, in `color`.
///
/// [`Status::Working`]'s arc goes round ([`spin_step`]), the only mark that moves;
/// [`Status::Running`] (waiting on its own background work) is a still dashed ring, since a mark
/// that moves says work is in progress; [`Status::Idle`] is an empty ring, for the places a mark
/// is required. The rest are their marks, and away its glyph.
#[must_use]
pub fn status_icon(theme: &Theme, status: Status, side: Pixels, color: Hsla) -> AnyElement {
    match (status, status.ring(), status.symbol()) {
        (Status::Working, ..) => Spinner { side, color, inner: None }.into_any_element(),
        (_, Some(ring), _) => ring.draw(side, color),
        (_, None, Some(symbol)) => icon(theme, symbol, color).size(side).into_any_element(),
        (_, None, None) => div().flex_none().size(side).into_any_element(),
    }
}

/// The steps in one turn of the working mark, which makes one turn in two seconds, as Zed's
/// turning circle does.
pub const SPIN_STEPS: u32 = 24;

/// How long the working mark holds each step: a twelfth of a second. It steps rather than
/// glides, so a window with an agent at work draws twelve frames a second for it, not one on
/// every display refresh.
pub const SPIN_STEP: Duration = Duration::from_nanos(83_333_333);

/// The step of its turn the working mark shows `since` the spin clock started: always the
/// first under Reduce Motion, where it stands still as a quarter pie.
#[must_use]
pub fn spin_step(since: Duration, reduce_motion: bool) -> u32 {
    if reduce_motion {
        return 0;
    }
    let steps = since.as_nanos().checked_div(SPIN_STEP.as_nanos()).unwrap_or(0);
    u32::try_from(steps.checked_rem(u128::from(SPIN_STEPS)).unwrap_or(0)).unwrap_or(0)
}

/// How long after `since` the next step `step` long begins.
fn until_next(since: Duration, step: Duration) -> Duration {
    let into = since.as_nanos().checked_rem(step.as_nanos()).unwrap_or(0);
    step.saturating_sub(Duration::from_nanos(u64::try_from(into).unwrap_or(0)))
}

/// The clock every working mark turns by, one per app, so marks on screen together show the
/// same step.
///
/// A mark that paints names its view here, and the first to do so since the last step sets a
/// timer for the next one. When it fires the named views are notified and draw again, and those
/// still showing a mark name themselves anew. A view that stops painting one, because the work
/// ended or it scrolled away, is not woken again, and nothing turns while nothing shows.
struct SpinClock {
    /// The executor's clock when the first mark was drawn; the test executor's is simulated.
    epoch: Instant,
    /// How long after `epoch` the marks were last woken. Every mark draws the step
    /// of that moment, not of the moment it is drawn, so a frame drawn again from scratch draws
    /// what the frame shown drew until the views showing marks are woken together.
    shown: Duration,
    /// The views that painted a turning mark since the last step.
    wake: Vec<EntityId>,
    /// Whether a timer for the next step is out, and its number: a timer whose number is not
    /// the latest was let go and does nothing when it fires.
    armed: bool,
    timers: u64,
    /// The moment every mark shows while the clock is pinned ([`pin_steps`]): wakes leave it.
    pinned: Option<Duration>,
}

impl Global for SpinClock {}

impl SpinClock {
    /// The app's clock, made on first use.
    fn get(cx: &mut App) -> &mut Self {
        if !cx.has_global::<Self>() {
            let clock = Self {
                epoch: cx.background_executor().now(),
                shown: Duration::ZERO,
                wake: Vec::new(),
                armed: false,
                timers: 0,
                pinned: None,
            };
            cx.set_global(clock);
        }
        cx.global_mut::<Self>()
    }

    /// Set a timer for the next step to fire after `wait`.
    fn arm(cx: &mut App, wait: Duration) {
        let clock = Self::get(cx);
        clock.armed = true;
        clock.timers = clock.timers.wrapping_add(1);
        let number = clock.timers;
        let timer = cx.background_executor().timer(wait);
        cx.spawn(async move |cx| {
            timer.await;
            cx.update(|cx| {
                if Self::get(cx).timers == number {
                    Self::get(cx).armed = false;
                    Self::wake(cx);
                }
            });
        })
        .detach();
    }

    /// The step every mark drawn now shows: that of the clock's last wake.
    fn drawn(cx: &mut App) -> u32 {
        let reduce = cx.reduce_motion();
        let shown = Self::get(cx).shown;
        spin_step(shown, reduce)
    }

    /// Wake every view that painted a mark since the last step.
    fn wake(cx: &mut App) {
        let now = cx.background_executor().now();
        let clock = Self::get(cx);
        clock.shown = clock.pinned.unwrap_or_else(|| now.saturating_duration_since(clock.epoch));
        for view in std::mem::take(&mut clock.wake) {
            cx.notify(view);
        }
    }
}

/// A moment the working mark stands on its first step: three turns. The marks are pinned to it
/// while the readouts' clock is ([`crate::clock::pin`]).
pub const PINNED_STEPS: Duration = Duration::from_secs(6);

/// Every mark shows the step of `at` (time since the spin clock's start) from now on, whenever
/// it is drawn, and no step falls due; `None` lets the clock run again.
pub fn pin_steps(at: Option<Duration>, cx: &mut App) {
    SpinClock::get(cx).pinned = at;
    SpinClock::wake(cx);
}

/// GPUI's Reduce Motion flag changed: every view showing a mark draws again.
///
/// A mark that was turning stands still at once, and one that stood still turns. The marks read
/// the flag itself as they are drawn, so nothing here keeps a copy of it.
pub fn motion_setting_changed(cx: &mut App) {
    SpinClock::wake(cx);
}

/// The working mark: a quarter of the ring going round a faint track, two dozen frames a turn
/// on the spin clock, Zed's turning circle (`docs/decisions/ui.md`, "State is a glyph"). Drawn
/// by us on whole device pixels in the working hue. Under Reduce Motion it stands still as a
/// quarter pie and wakes nothing.
struct Spinner {
    side: Pixels,
    color: Hsla,
    inner: Option<AnyElement>,
}

impl IntoElement for Spinner {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Spinner {
    type PrepaintState = ();
    type RequestLayoutState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mark =
            if cx.reduce_motion() { Ring::Pie(0.25) } else { Ring::Turning(SpinClock::drawn(cx)) };
        let mut inner = mark.draw(self.side, self.color);
        let layout = inner.request_layout(window, cx);
        self.inner = Some(inner);
        (layout, ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        if let Some(inner) = &mut self.inner {
            inner.prepaint(window, cx);
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(inner) = &mut self.inner {
            inner.paint(window, cx);
        }
        if !cx.reduce_motion() {
            wake_at_next_step(window, cx);
        }
    }
}

/// The view being painted draws again at the spin clock's next step, as one painting a
/// working mark does. Nothing calls it under Reduce Motion, where nothing steps.
pub(crate) fn wake_at_next_step(window: &Window, cx: &mut App) {
    let now = cx.background_executor().now();
    let view = window.current_view();
    let clock = SpinClock::get(cx);
    if !clock.wake.contains(&view) {
        clock.wake.push(view);
    }
    if clock.armed || clock.pinned.is_some() {
        return;
    }
    let wait = until_next(now.saturating_duration_since(clock.epoch), SPIN_STEP);
    SpinClock::arm(cx, wait);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The kit's icons map onto the chrome's symbols by their asset paths; one the chrome has
    /// no symbol for is left to the kit.
    #[test]
    fn the_kits_icons_are_the_chromes_symbols() {
        assert_eq!(kit_symbol("icons/check.svg"), Some(Symbol::Checkmark));
        assert_eq!(kit_symbol("icons/close.svg"), Some(Symbol::Xmark));
        assert_eq!(kit_symbol("icons/chevron-down.svg"), Some(Symbol::ChevronDown));
        assert_eq!(kit_symbol("icons/asterisk.svg"), None);
        assert_eq!(kit_symbol("elsewhere/check.svg"), None);
    }

    /// A glyph is drawn at 14 in its slot of 16 with a point of air round it, the 12-grid art
    /// (a chevron, the small cross) at its own 12; a finger's are 18 in 20 and 14
    /// (`.research/design-polish-2026-10-12.md` §4.12).
    #[test]
    fn a_glyph_is_fourteen_in_a_slot_of_sixteen() {
        for (touch, (side, drawn, small)) in
            [(false, (16.0, 14.0, 12.0)), (true, (20.0, 18.0, 14.0))]
        {
            let mut theme = Theme::default();
            if touch {
                theme.density = slopty_theme::Density::TOUCH;
            }
            let slot = theme.icon();
            assert!((slot - side).abs() < 1e-3, "touch {touch}: {slot}");
            let grid = Drawn::new(&theme, Symbol::Doc).grid(slot);
            assert!((grid - drawn).abs() < 1e-3, "a glyph's grid: {grid}");
            let chevron = Drawn::new(&theme, Symbol::ChevronRight).grid(slot);
            assert!((chevron - small).abs() < 1e-3, "the chevron's grid: {chevron}");
        }
        assert_eq!(whole_pixels(14.0 * 2.0), 28);
        assert_eq!(whole_pixels(f32::NAN), 0);
    }

    /// The glyphs drawn with their outlines: every one but the few that are a filled shape (a
    /// state's disc, a dot, a square, the alert's triangle).
    fn outlined() -> impl Iterator<Item = Symbol> {
        Symbol::ALL.iter().copied().filter(|s| {
            let name = s.name();
            !name.contains("-fill") && !name.contains("kebab")
        })
    }

    /// Each glyph's mask as it is painted in the default slot at `scale`: the paint path's own
    /// sizing ([`Drawn::glyph_mask`]), so the size measured is the size drawn.
    fn painted(symbol: Symbol, scale: f32) -> Arc<Mask> {
        let theme = Theme::default();
        let slot = theme.icon();
        Drawn::new(&theme, symbol)
            .glyph_mask(symbol, slot, scale)
            .unwrap_or_else(|| panic!("{symbol:?} is drawn"))
            .0
    }

    /// A glyph weighs like its words: drawn as it is painted at 1x and measured eight times
    /// finer, the median line is 1.10 to 1.40 px across, about Delta's 1.1–1.3 pt line beside
    /// the 1.29 px stem of IBM Plex Sans at 14 (`.research/delta-re-2026-10-12.md` §3.1,
    /// `.research/design-polish-2026-10-12.md` §3.1). Octicons' 1.5 line on its 16 grid measures
    /// 1.375 drawn at 14, the nearest it comes; at 16 it would be 1.5 and more, heavier than the
    /// words. Carbon's measured 1.06, a hairline beside its words. The line is the shorter of the
    /// inked run across and down each inked pixel, over every outlined glyph.
    #[test]
    fn a_glyph_weighs_like_its_words() {
        const FINER: f32 = 8.0;
        let mut runs: Vec<f32> = Vec::new();
        for symbol in outlined() {
            let at_one = painted(symbol, 1.0);
            let m = painted(symbol, FINER);
            assert_eq!(m.width, at_one.width.saturating_mul(8), "{symbol:?}: the same grid, finer");
            let side = usize::try_from(m.width).unwrap_or_default();
            let ink = |x: usize, y: usize| {
                m.alpha.get(y.saturating_mul(side).saturating_add(x)).is_some_and(|a| *a >= 128)
            };
            for y in 0..side {
                for x in 0..side {
                    if !ink(x, y) {
                        continue;
                    }
                    let left = (0..x).rev().take_while(|&i| ink(i, y)).count();
                    let right = (x.saturating_add(1)..side).take_while(|&i| ink(i, y)).count();
                    let up = (0..y).rev().take_while(|&j| ink(x, j)).count();
                    let down = (y.saturating_add(1)..side).take_while(|&j| ink(x, j)).count();
                    let across = left.saturating_add(right).saturating_add(1);
                    let run = across.min(up.saturating_add(down).saturating_add(1));
                    #[expect(clippy::cast_precision_loss, reason = "a run of a few dozen")]
                    runs.push(run as f32 / FINER);
                }
            }
        }
        runs.sort_by(f32::total_cmp);
        let median = runs.get(runs.len() / 2).copied().unwrap_or_default();
        eprintln!("the median line of {} inked pixels: {median:.3} px", runs.len());
        assert!((1.10..=1.40).contains(&median), "the median line: {median} px");
    }

    /// A glyph is crisp at 1x, where the person looks: drawn as it is painted at 14 on a 1x
    /// display, its ink is mostly whole pixels, Σα² / Σα at least 0.72 over every glyph (one
    /// is every inked pixel solid). Carbon's art at 14 measured 0.63
    /// (`.research/design-polish-2026-10-12.md` §3.2). None falls under 0.55, a smear.
    #[test]
    fn a_glyph_is_crisp_at_one_x() {
        let (mut squares, mut sum) = (0.0_f64, 0.0_f64);
        let mut each = Vec::new();
        for &symbol in Symbol::ALL {
            let m = painted(symbol, 1.0);
            let (sq, su) = m.alpha.iter().fold((0.0_f64, 0.0_f64), |(sq, su), a| {
                let a = f64::from(*a) / 255.0;
                (a.mul_add(a, sq), su + a)
            });
            assert!(su > 0.0, "{symbol:?} has ink");
            each.push((sq / su, symbol));
            squares += sq;
            sum += su;
        }
        let crisp = squares / sum;
        each.sort_by(|a, b| a.0.total_cmp(&b.0));
        eprintln!(
            "crisp at 1x: {crisp:.3}; the softest {:?}",
            each.iter().take(5).collect::<Vec<_>>()
        );
        assert!(crisp >= 0.72, "crisp at 1x: {crisp:.3}");
        for (c, symbol) in each {
            assert!(c >= 0.55, "{symbol:?} smears at 1x: {c:.3}");
        }
    }

    /// Each status has its own mark, the three that carry colour a disc with their own figure
    /// knocked out; only away is a glyph, Octicons'.
    #[test]
    fn each_status_has_its_own_mark() {
        let all = [
            Status::Idle,
            Status::Working,
            Status::Running,
            Status::NeedsYou,
            Status::Done,
            Status::Failed,
        ];
        let rings: Vec<Ring> = all.iter().filter_map(|s| s.ring()).collect();
        assert_eq!(rings.len(), all.len());
        for (i, a) in rings.iter().enumerate() {
            assert!(rings.iter().skip(i.saturating_add(1)).all(|b| a != b), "{a:?} twice");
        }
        assert_eq!(Status::NeedsYou.ring(), Some(Ring::Disc(Knock::Bang)));
        assert_eq!(Status::Done.ring(), Some(Ring::Disc(Knock::Check)));
        assert_eq!(Status::Failed.ring(), Some(Ring::Disc(Knock::Cross)));
        assert_eq!((Status::Away.ring(), Status::Away.symbol()), (None, Some(Symbol::WifiSlash)));
        assert!(all.iter().all(|s| s.symbol().is_none()), "the rest are drawn by us");
    }
}

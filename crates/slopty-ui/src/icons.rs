//! The icons the chrome draws: SF Symbols, drawn by the OS at the size of the words beside them
//! (`slopty_platform::symbols`), painted at exact device pixels in the ink of those words.
//!
//! A [`Symbol`] is one of a closed list. A file's type is one of nine of them
//! ([`FileType::symbol`]), and every agent's thread is the one neutral [`AGENT`]. Nothing else
//! is drawn by us but the working mark's twelve spokes and the dot of a finish not yet seen
//! (`docs/decisions/ui.md`, "The chrome's icons are SF Symbols").
//!
//! An icon takes its size from the type scale: [`IconSize::Inline`] sits in the slot
//! [`slopty_theme::Typography::icon`] beside the chrome's secondary text and is drawn at that
//! text's point size; [`IconSize::Large`] stands in [`slopty_theme::Typography::icon_large`]
//! at the chrome's own size. A slot sized larger or smaller (the chrome's zoom) draws its
//! symbol larger or smaller by as much, so an icon never parts from its words' size.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use gpui::accesskit::Role;
use gpui::{
    AnyElement, App, Bounds, DevicePixels, Div, Element, ElementId, EntityId, Global,
    GlobalElementId, Hsla, InspectorElementId, InteractiveElement as _, IntoElement, LayoutId,
    ParentElement as _, PathBuilder, Pixels, Point, ScaledPixels, SharedString, Stateful,
    StatefulInteractiveElement as _, Styled as _, TransformationMatrix, Window, canvas, div, point,
    px, radians,
};
use parking_lot::RwLock;
use slopty_platform::symbols::{Masks, SymbolMask};
pub use slopty_platform::symbols::{Scale, Symbol, SymbolSize, Weight};
use slopty_theme::{Rgb, Theme};

use crate::colors::hsla;
pub use crate::file_types::FileType;

/// Every agent's thread, whichever agent it is: one neutral mark in the ink beside it, a
/// conversation. Agents are told apart in words (`docs/decisions/ui.md`, "No agent wears a
/// mark of its own").
pub const AGENT: Symbol = Symbol::TextBubble;

/// The symbol for the file at `path`: its type's, or the plain document.
#[must_use]
pub fn file_symbol(path: &str) -> Symbol {
    FileType::of(path).map_or(Symbol::Doc, FileType::symbol)
}

/// The symbol for an icon gpui-kit's components name by its asset path (`icons/check.svg`),
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
        "copy" => Symbol::DocOnDoc,
        "info" => Symbol::InfoCircle,
        "circle-x" => Symbol::XmarkCircle,
        "circle-check" => Symbol::CheckmarkCircle,
        "triangle-alert" => Symbol::ExclamationmarkTriangle,
        "ellipsis" => Symbol::Ellipsis,
        _ => return None,
    })
}

/// How large an icon is drawn, by the words it sits beside.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IconSize {
    /// Beside secondary chrome text: [`slopty_theme::Typography::small`]'s point size in
    /// [`slopty_theme::Typography::icon`]'s slot.
    Inline,
    /// Standing alone or beside a row's title: the chrome's size in
    /// [`slopty_theme::Typography::icon_large`]'s slot.
    Large,
}

impl IconSize {
    /// The slot's side in points.
    #[must_use]
    pub fn slot(self, theme: &Theme) -> f32 {
        match self {
            Self::Inline => theme.typography.icon(),
            Self::Large => theme.typography.icon_large(),
        }
    }

    /// The point size of the words beside it, which the symbol is drawn at.
    #[must_use]
    pub fn point(self, theme: &Theme) -> f32 {
        match self {
            Self::Inline => theme.typography.small(),
            Self::Large => theme.typography.ui_size,
        }
    }
}

/// How a symbol is drawn in a slot: at `ratio` of the slot's side in points, `weight` and
/// `scale`, turned by `turn` radians.
#[derive(Clone, Copy, Debug)]
pub struct Drawn {
    symbol: Symbol,
    ratio: f32,
    weight: Weight,
    scale: Scale,
    turn: f32,
}

impl Drawn {
    /// `symbol` in a slot of `size`, regular and at the medium scale.
    #[must_use]
    pub fn new(theme: &Theme, symbol: Symbol, size: IconSize) -> Self {
        Self {
            symbol,
            ratio: size.point(theme) / size.slot(theme),
            weight: Weight::Regular,
            scale: Scale::Medium,
            turn: 0.0,
        }
    }

    /// At `weight`, as the words beside it are.
    #[must_use]
    pub const fn weight(mut self, weight: Weight) -> Self {
        self.weight = weight;
        self
    }

    /// At `scale`.
    #[must_use]
    pub const fn scale(mut self, scale: Scale) -> Self {
        self.scale = scale;
        self
    }

    /// A disclosure chevron's drawing: Apple's semibold at the small scale, at the caption's
    /// share of the inline slot.
    #[must_use]
    pub fn disclosure(theme: &Theme, symbol: Symbol) -> Self {
        Self {
            ratio: theme.typography.caption() / IconSize::Inline.slot(theme),
            weight: Weight::Semibold,
            scale: Scale::Small,
            ..Self::new(theme, symbol, IconSize::Inline)
        }
    }

    /// An empty state's mark: the page heading's size in [`crate::kit::NOTICE_MARK`], at the
    /// light weight and the large scale.
    #[must_use]
    pub fn notice(theme: &Theme, symbol: Symbol) -> Self {
        Self {
            ratio: theme.typography.heading() / crate::kit::NOTICE_MARK,
            weight: Weight::Light,
            scale: Scale::Large,
            ..Self::new(theme, symbol, IconSize::Large)
        }
    }

    /// The size it is drawn at in a slot `side` points square.
    #[must_use]
    pub fn size(self, side: f32) -> SymbolSize {
        SymbolSize { point: side * self.ratio, weight: self.weight, scale: self.scale }
    }

    /// Turned by `radians` about the slot's centre, while it moves; at rest it is upright, so
    /// its pixels stay the screen's.
    #[must_use]
    pub const fn turned(mut self, radians: f32) -> Self {
        self.turn = radians;
        self
    }

    /// The slot, `side` square, its symbol in `color`. The slot can be sized again
    /// (`.size(..)`), and the symbol is drawn larger or smaller by as much; its ink is the
    /// slot's text colour, so `.text_color(..)` recolours it.
    #[must_use]
    pub fn slot(self, side: Pixels, color: Hsla) -> Div {
        div().flex_none().size(side).text_color(color).child(
            canvas(|_, _, _| {}, move |bounds, (), window, _cx| self.paint(bounds, window))
                .size_full(),
        )
    }

    /// Paints the symbol centred in `bounds`: its box across, its alignment rectangle (the
    /// baseline to the cap height) down, as the words beside it centre.
    fn paint(self, bounds: Bounds<Pixels>, window: &mut Window) {
        let side = f32::from(bounds.size.width.min(bounds.size.height));
        let device = window.scale_factor();
        let Some((mask, key)) =
            fitted(self.symbol, self.size(side), f32::from(bounds.size.width) * device, device)
        else {
            return;
        };
        let (Ok(width), Ok(height)) = (i32::try_from(mask.width), i32::try_from(mask.height))
        else {
            return;
        };
        let a = mask.alignment;
        let centre = bounds.center();
        #[expect(clippy::cast_precision_loss, reason = "a mask is a few dozen pixels")]
        let across = mask.width as f32 / device / 2.0;
        let origin = point(centre.x - px(across), centre.y - px((a.y + a.height / 2.0) / device));
        let transformation = if self.turn == 0.0 {
            TransformationMatrix::unit()
        } else {
            let at = centre.scale(device);
            TransformationMatrix::unit()
                .translate(at)
                .rotate(radians(self.turn))
                .translate(Point::new(ScaledPixels(-at.x.0), ScaledPixels(-at.y.0)))
        };
        let devices = gpui::size(DevicePixels(width), DevicePixels(height));
        let ink = window.text_style().color;
        let painted = window
            .paint_mask(origin, devices, key, transformation, ink, || Ok(Some(mask.alpha.clone())));
        if let Err(error) = painted {
            tracing::warn!(%error, symbol = self.symbol.name(), "a symbol was not painted");
        }
    }
}

/// A drawn mask with the key the atlas keeps it under.
type Kept = Option<(Arc<SymbolMask>, SharedString)>;

/// The masks every window paints from, shared with the prewarm's thread, and each one's atlas
/// key, made once. A symbol the OS lacks is kept as a miss.
struct Symbols {
    masks: Masks,
    kept: RwLock<HashMap<(Symbol, SymbolSize, u32), Kept>>,
}

static SYMBOLS: LazyLock<Symbols> =
    LazyLock::new(|| Symbols { masks: Masks::new(), kept: RwLock::new(HashMap::new()) });

/// `symbol` at `size` for a display of `device` pixels to the point, and its atlas key.
fn mask(symbol: Symbol, size: SymbolSize, device: f32) -> Kept {
    let at = (symbol, size, device.to_bits());
    if let Some(kept) = SYMBOLS.kept.read().get(&at) {
        return kept.clone();
    }
    let kept = SYMBOLS.masks.get(symbol, size, device).map(|mask| {
        let key = format!(
            "sf:{}:{:08x}:{:?}:{:?}@{:08x}",
            symbol.name(),
            size.point.to_bits(),
            size.weight,
            size.scale,
            device.to_bits()
        );
        (mask, SharedString::from(key))
    });
    SYMBOLS.kept.write().entry(at).or_insert(kept).clone()
}

/// `symbol` at `size`, drawn smaller where it is wider than `room` device pixels: a wide
/// symbol (a server rack, a folder) kept in its slot, so it never reaches the words beside it.
///
/// The point size is scaled by the overflow and stepped down a quarter point at a time while
/// the OS's rounding still leaves it a pixel over, so it shrinks no more than it must.
fn fitted(symbol: Symbol, size: SymbolSize, room: f32, device: f32) -> Kept {
    let room = room.ceil();
    let mut kept = mask(symbol, size, device)?;
    let mut point = size.point;
    for _ in 0..FIT_STEPS {
        #[expect(clippy::cast_precision_loss, reason = "a mask is a few dozen pixels")]
        let wide = kept.0.width as f32;
        if wide <= room {
            break;
        }
        point = (point * room / wide * 4.0).floor().min(point.mul_add(4.0, -1.0)) / 4.0;
        if point < 1.0 {
            break;
        }
        kept = mask(symbol, SymbolSize { point, ..size }, device)?;
    }
    Some(kept)
}

/// How many times [`fitted`] draws a symbol smaller before it keeps the last.
const FIT_STEPS: usize = 4;

/// The display scales a window of this platform is likely shown at, the likeliest first.
const SCALES: &[f32] = if cfg!(target_os = "ios") { &[2.0, 3.0] } else { &[1.0, 2.0] };

/// Draws every symbol at the chrome's sizes on background threads.
///
/// The first frame then finds them drawn. Start it before the first window: the first symbol
/// of a process loads the system's catalogue, 40 to 70 ms (`docs/MEASUREMENTS.md`, "SF
/// Symbols as masks").
pub fn prewarm(theme: &Theme) {
    let inline = IconSize::Inline.slot(theme);
    let sizes = [
        Drawn::new(theme, AGENT, IconSize::Large).size(IconSize::Large.slot(theme)),
        Drawn::new(theme, AGENT, IconSize::Inline).size(inline),
        Drawn::disclosure(theme, Symbol::ChevronRight).size(inline),
        Drawn::notice(theme, AGENT).size(crate::kit::NOTICE_MARK),
    ];
    let wanted: Vec<(Symbol, SymbolSize)> = sizes
        .into_iter()
        .flat_map(|size| Symbol::ALL.iter().map(move |&symbol| (symbol, size)))
        .collect();
    for &scale in SCALES {
        if let Err(error) = SYMBOLS.masks.prewarm(wanted.clone(), scale) {
            tracing::warn!(%error, "the symbols' prewarm did not start");
        }
    }
}

/// `symbol` at `size`, in `color`: a slot of the size's side.
#[must_use]
pub fn icon(theme: &Theme, symbol: Symbol, size: IconSize, color: Hsla) -> Div {
    Drawn::new(theme, symbol, size).slot(px(size.slot(theme)), color)
}

/// `symbol` in a slot `side` square, in `ink`, drawn at the inline icon's share of the slot:
/// a row's lead at whatever zoom.
#[must_use]
pub fn symbol(theme: &Theme, symbol: Symbol, side: Pixels, ink: Hsla) -> AnyElement {
    Drawn::new(theme, symbol, IconSize::Inline).slot(side, ink).into_any_element()
}

/// The one vocabulary for how a thing is doing, wherever it is shown: a tile's header, a
/// navigator row, the palette, a toast. Each state has one icon and one tone, so a glance
/// reads the same everywhere.
///
/// Colour goes only to what needs the person: *Needs you* in `warn`, *Failed* in `error`, and
/// the accent dot of a finish not yet seen. Busy states recede into the muted tone, and so does
/// a worker out of reach, so `warn` means "needs you" and nothing else.
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
    /// The symbol that marks it; `None` for the two marks drawn by us, the working mark's
    /// spokes and the dot of a finish ([`status_icon`]). The two that carry colour are filled,
    /// so the colour has a body at 1x.
    #[must_use]
    pub const fn symbol(self) -> Option<Symbol> {
        match self {
            Self::Idle => Some(Symbol::Circle),
            Self::Working | Self::Done => None,
            Self::Running => Some(Symbol::CircleDashed),
            Self::NeedsYou => Some(Symbol::ExclamationmarkCircleFill),
            Self::Failed => Some(Symbol::XmarkCircleFill),
            Self::Away => Some(Symbol::WifiSlash),
        }
    }

    /// Its tone.
    #[must_use]
    pub const fn tone(self, theme: &Theme) -> Rgb {
        let s = &theme.surfaces;
        match self {
            Self::Idle | Self::Working | Self::Running | Self::Away => s.text_muted,
            Self::NeedsYou => s.warn,
            Self::Done => s.accent,
            Self::Failed => s.error,
        }
    }

    /// Its name for the accessibility tree and tooltips.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Idle => "Idle",
            Self::Working => "Working",
            Self::Running => "Waiting",
            Self::NeedsYou => "Needs you",
            Self::Done => "Done",
            Self::Failed => "Failed",
            Self::Away => "Away",
        }
    }
}

/// `status` in a fixed square slot, the width of a large icon at the chrome's zoom `k`.
///
/// Rows that carry a mark and rows that do not keep their titles on one edge. A mark is an
/// image named by [`Status::label`]; an empty slot is nothing to a screen reader.
#[must_use]
pub fn status_mark(theme: &Theme, status: Option<Status>, k: f32) -> Stateful<Div> {
    let slot = div()
        .id("status")
        .flex_shrink_0()
        .size(px(theme.typography.icon_large() * k))
        .flex()
        .items_center()
        .justify_center();
    match status {
        Some(status) => slot.role(Role::Image).aria_label(status.label()).child(status_icon(
            theme,
            status,
            px(theme.typography.icon() * k),
            hsla(status.tone(theme)),
        )),
        None => slot,
    }
}

/// `status` as an empty state's mark, in `color` at the chrome's zoom `k`.
///
/// Its symbol is drawn as [`Drawn::notice`] in the notice's slot, and the working spokes or
/// the dot of a finish at the heading's size, so a tile's state reads at the size of the
/// notice it heads.
#[must_use]
pub fn notice_status(theme: &Theme, status: Status, color: Hsla, k: f32) -> AnyElement {
    match status.symbol() {
        Some(symbol) => Drawn::notice(theme, symbol)
            .slot(px(crate::kit::NOTICE_MARK * k), color)
            .into_any_element(),
        None => status_icon(theme, status, px(theme.typography.heading() * k), color),
    }
}

/// `status`'s icon, `side` square, in `color`.
///
/// [`Status::Working`]'s spokes step round ([`spin_step`]), the only mark that moves;
/// [`Status::Running`] (waiting on its own background work) is a still dashed ring, since a mark
/// that moves says work is in progress; [`Status::Done`] is a dot, the navigator's unseen one at
/// that size, centred where an icon would be.
#[must_use]
pub fn status_icon(theme: &Theme, status: Status, side: Pixels, color: Hsla) -> AnyElement {
    match status {
        Status::Working => Spinner { side, color, inner: None }.into_any_element(),
        Status::Done => {
            let dot = side * ((theme.spacing.xs + theme.spacing.xxs) / theme.typography.icon());
            div()
                .flex_none()
                .size(side)
                .flex()
                .items_center()
                .justify_center()
                .child(div().size(dot).rounded_full().bg(color))
                .into_any_element()
        }
        _ => match status.symbol() {
            Some(symbol) => {
                icon(theme, symbol, IconSize::Inline, color).size(side).into_any_element()
            }
            None => div().flex_none().size(side).into_any_element(),
        },
    }
}

/// How long after `since` the next whole second begins: when a readout of seconds changes.
#[must_use]
pub fn until_next_second(since: Duration) -> Duration {
    Duration::from_secs(1).saturating_sub(Duration::from_nanos(u64::from(since.subsec_nanos())))
}

/// The steps in one turn of the working mark, which makes one turn a second.
pub const SPIN_STEPS: u32 = 12;

/// How long the working mark holds each step: a twelfth of a second. It steps rather than
/// glides, so a window with an agent at work draws twelve frames a second for it, not one on
/// every display refresh.
pub const SPIN_STEP: Duration = Duration::from_nanos(83_333_333);

/// How long the working mark holds each step of its breath under Reduce Motion.
///
/// A fifth of a second, twelve to a breath, so a breathing mark draws under half the frames a
/// turning one does and every breath ends on a step.
pub const BREATH_STEP: Duration = Duration::from_millis(200);

/// The least opacity of a breath: the mark never fades past it, so it is always found.
const BREATH_LOW: f32 = 0.6;

/// The opacity the working mark shows `since` the spin clock started under Reduce Motion.
///
/// It breathes from `BREATH_LOW` to whole and back over [`slopty_theme::Motion::breath`], in
/// steps of [`BREATH_STEP`], upright. Opacity only: nothing travels, turns or scales.
#[must_use]
pub fn breath(since: Duration) -> f32 {
    let (step, period) = (BREATH_STEP.as_nanos(), slopty_theme::Motion::DEFAULT.breath.as_nanos());
    let stepped = since.as_nanos().checked_div(step).unwrap_or(0).saturating_mul(step);
    #[expect(clippy::cast_precision_loss, reason = "a phase within one breath")]
    let phase = stepped.checked_rem(period).unwrap_or(0) as f32 / period as f32;
    let wave = (1.0 - (phase * std::f32::consts::TAU).cos()) / 2.0;
    (1.0 - BREATH_LOW).mul_add(wave, BREATH_LOW)
}

/// The step of its turn the working mark shows `since` the spin clock started: always the
/// first under Reduce Motion, where it stays upright and breathes instead ([`breath`]).
#[must_use]
pub fn spin_step(since: Duration, reduce_motion: bool) -> u32 {
    if reduce_motion {
        return 0;
    }
    let steps = since.as_nanos().checked_div(SPIN_STEP.as_nanos()).unwrap_or(0);
    u32::try_from(steps.checked_rem(u128::from(SPIN_STEPS)).unwrap_or(0)).unwrap_or(0)
}

/// How long after `since` the next step begins.
#[must_use]
pub fn until_next_step(since: Duration) -> Duration {
    until_next(since, SPIN_STEP)
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
///
/// While a typed key waits for its echo ([`hold_steps`]) a step that falls due wakes nobody: it
/// waits for the next frame drawn for another reason, the echo's ([`release_steps`]), or for the
/// hold's end, whichever comes first. A frame drawn for the step alone just before the echo
/// would hold the echo's frame back a whole refresh.
struct SpinClock {
    /// The executor's clock when the first mark was drawn; the test executor's is simulated.
    epoch: Instant,
    /// How long after `epoch` the marks were last woken. Every mark draws the step and breath
    /// of that moment, not of the moment it is drawn, so a frame drawn again from scratch draws
    /// what the frame shown drew until the views showing marks are woken together.
    shown: Duration,
    /// The views that painted a turning mark since the last step.
    wake: Vec<EntityId>,
    /// The timer that is out, if one is, and its number: a timer whose number is not the
    /// latest was let go and does nothing when it fires.
    armed: Option<Timer>,
    timers: u64,
    /// Steps wake nobody until then: a key waits for its echo.
    hold_until: Option<Instant>,
    /// A step fell due during the hold and waits to be drawn.
    held: bool,
    /// The moment every mark shows while the clock is pinned ([`pin_steps`]): wakes leave it.
    pinned: Option<Duration>,
}

/// What an armed timer is for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Timer {
    /// The next step's start.
    Step,
    /// The end of a hold, with a step waiting.
    HoldEnd,
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
                armed: None,
                timers: 0,
                hold_until: None,
                held: false,
                pinned: None,
            };
            cx.set_global(clock);
        }
        cx.global_mut::<Self>()
    }

    /// Set a timer of `kind` to fire after `wait`.
    fn arm(cx: &mut App, kind: Timer, wait: Duration) {
        let clock = Self::get(cx);
        clock.armed = Some(kind);
        clock.timers = clock.timers.wrapping_add(1);
        let number = clock.timers;
        let timer = cx.background_executor().timer(wait);
        cx.spawn(async move |cx| {
            timer.await;
            cx.update(|cx| {
                if Self::get(cx).timers == number {
                    Self::fire(cx, kind);
                }
            });
        })
        .detach();
    }

    /// A timer fired: a step fell due, or a hold with a step waiting ran out.
    fn fire(cx: &mut App, kind: Timer) {
        let now = cx.background_executor().now();
        let clock = Self::get(cx);
        clock.armed = None;
        match kind {
            Timer::Step => {
                if let Some(until) = clock.hold_until.filter(|until| now < *until) {
                    clock.held = true;
                    Self::arm(cx, Timer::HoldEnd, until.saturating_duration_since(now));
                    return;
                }
                clock.hold_until = None;
                Self::wake(cx);
            }
            Timer::HoldEnd => {
                clock.held = false;
                clock.hold_until = None;
                Self::wake(cx);
            }
        }
    }

    /// The step and the breath every mark drawn now shows: those of the clock's last wake.
    fn drawn(cx: &mut App) -> (u32, f32) {
        let reduce = cx.reduce_motion();
        let shown = Self::get(cx).shown;
        (spin_step(shown, reduce), breath(shown))
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

/// A typed key waits for its echo, at most until `until`: steps of the working mark that fall
/// due meanwhile wait for the echo's frame (or `until`), rather than draw a frame of their own.
pub fn hold_steps(cx: &mut App, until: Instant) {
    let clock = SpinClock::get(cx);
    clock.hold_until = Some(clock.hold_until.map_or(until, |held| held.max(until)));
}

/// Until when the working marks' steps are held for a typed key's echo, if they are.
#[cfg(test)]
pub(crate) fn steps_held_until(cx: &mut App) -> Option<Instant> {
    SpinClock::get(cx).hold_until
}

/// A frame is coming anyway (a terminal changed): a step that waited on the hold rides it.
pub fn release_steps(cx: &mut App) {
    let clock = SpinClock::get(cx);
    if clock.hold_until.take().is_some() && std::mem::take(&mut clock.held) {
        // The hold's timer is let go; the marks that paint in this frame set the next step's.
        clock.armed = None;
        clock.timers = clock.timers.wrapping_add(1);
        SpinClock::wake(cx);
    }
}

/// A moment the working mark stands upright at the top of its breath: six turns, two and a half
/// breaths. The marks are pinned to it while the readouts' clock is ([`crate::clock::pin`]).
pub const PINNED_STEPS: Duration = Duration::from_secs(6);

/// Every mark shows the step and breath of `at` (time since the spin clock's start) from now on,
/// whenever it is drawn, and no step falls due; `None` lets the clock run again.
pub fn pin_steps(at: Option<Duration>, cx: &mut App) {
    SpinClock::get(cx).pinned = at;
    SpinClock::wake(cx);
}

/// GPUI's Reduce Motion flag changed: every view showing a mark draws again.
///
/// A mark that was turning stands upright and breathes at once, and one that breathed turns. The
/// marks read the flag itself as they are drawn, so nothing here keeps a copy of it.
pub fn motion_setting_changed(cx: &mut App) {
    SpinClock::wake(cx);
}

/// The share of the side a spoke runs, and its width in points at the inline slot's side.
const SPOKE_LENGTH: f32 = 0.28;
const SPOKE_WIDTH: f32 = 1.5;

/// The faintest spoke, the one the head has just left the furthest behind.
const SPOKE_FAINTEST: f32 = 0.2;

/// The working mark: twelve spokes round a centre, the head at the spin clock's step and the
/// rest fading behind it, so the eye reads a turn while nothing turns (Apple's activity
/// indicator). Under Reduce Motion it stands on its first step and breathes.
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

/// Paints the twelve spokes in `bounds`, the head at `step`.
fn paint_spokes(bounds: Bounds<Pixels>, step: u32, color: Hsla, window: &mut Window) {
    let side = f32::from(bounds.size.width.min(bounds.size.height));
    let unit = side / 14.0;
    let half = SPOKE_WIDTH * unit / 2.0;
    let outer = side / 2.0 - half;
    let inner = SPOKE_LENGTH.mul_add(-side, outer);
    let centre = bounds.center();
    #[expect(clippy::cast_precision_loss, reason = "twelve spokes")]
    let steps = SPIN_STEPS as f32;
    for spoke in 0..SPIN_STEPS {
        #[expect(clippy::cast_precision_loss, reason = "twelve spokes")]
        let angle = spoke as f32 / steps * std::f32::consts::TAU;
        let behind = step.wrapping_add(SPIN_STEPS).wrapping_sub(spoke) % SPIN_STEPS;
        #[expect(clippy::cast_precision_loss, reason = "twelve spokes")]
        let fade = behind as f32 / (steps - 1.0);
        let opacity = (1.0 - SPOKE_FAINTEST).mul_add(-fade, 1.0);
        let (sin, cos) = angle.sin_cos();
        let at = |r: f32, across: f32| {
            point(
                centre.x + px(cos.mul_add(across, sin * r)),
                centre.y + px(sin.mul_add(across, -cos * r)),
            )
        };
        let mut path = PathBuilder::fill();
        path.move_to(at(inner, half));
        path.line_to(at(outer, half));
        path.arc_to(point(px(half), px(half)), px(0.0), false, false, at(outer, -half));
        path.line_to(at(inner, -half));
        path.arc_to(point(px(half), px(half)), px(0.0), false, false, at(inner, half));
        path.close();
        if let Ok(path) = path.build() {
            window.paint_path(path, color.opacity(opacity));
        }
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
        let reduce = cx.reduce_motion();
        let (step, breath) = SpinClock::drawn(cx);
        let color = if reduce { self.color.opacity(breath) } else { self.color };
        let mut inner = canvas(
            |_, _, _| {},
            move |bounds, (), window, _cx| {
                paint_spokes(bounds, step, color, window);
            },
        )
        .flex_none()
        .size(self.side)
        .into_any_element();
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
        wake_at_next_step(window, cx);
    }
}

/// How long after the spin clock's start every working mark was last woken: the moment each
/// drawing that steps with the marks (a busy progress bar's breath) shows, so they step
/// together.
pub(crate) fn steps_shown(cx: &mut App) -> Duration {
    SpinClock::get(cx).shown
}

/// The view being painted draws again at the spin clock's next step, as one painting a
/// working mark does: a step under Reduce Motion is a breath's.
pub(crate) fn wake_at_next_step(window: &Window, cx: &mut App) {
    let reduce = cx.reduce_motion();
    let now = cx.background_executor().now();
    let view = window.current_view();
    let clock = SpinClock::get(cx);
    if !clock.wake.contains(&view) {
        clock.wake.push(view);
    }
    if clock.armed.is_some() || clock.pinned.is_some() {
        return;
    }
    let step = if reduce { BREATH_STEP } else { SPIN_STEP };
    let wait = until_next(now.saturating_duration_since(clock.epoch), step);
    SpinClock::arm(cx, Timer::Step, wait);
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

    /// A file's type is one of nine symbols, and a type it has none for is the plain
    /// document; every agent's thread is the one neutral glyph.
    #[test]
    fn a_file_leads_with_its_types_symbol() {
        assert_eq!(file_symbol("/w/main.rs"), Symbol::ChevronLeftForwardslashChevronRight);
        assert_eq!(file_symbol("/w/README.md"), Symbol::DocText);
        assert_eq!(file_symbol("/w/notes"), Symbol::Doc);
        assert_eq!(AGENT, Symbol::TextBubble, "a conversation, not the cliched sparkles");
    }

    /// A symbol wider than its slot is drawn smaller until it fits, keeping its weight; one
    /// that fits is drawn at the words' size.
    #[test]
    fn a_wide_symbol_fits_its_slot() {
        let size = IconSize::Inline.point(&Theme::default());
        let size = SymbolSize::new(size, Weight::Regular);
        for device in [1.0, 2.0] {
            let natural = mask(Symbol::ServerRack, size, device).unwrap().0;
            let room = f32::from(u16::try_from(natural.width).unwrap()) - 3.0;
            let fit = fitted(Symbol::ServerRack, size, room, device).unwrap().0;
            assert!(
                f32::from(u16::try_from(fit.width).unwrap()) <= room,
                "{device}x: {}",
                fit.width
            );
            assert!(fit.width + 6 >= natural.width, "{device}x shrank too far: {}", fit.width);
            let roomy = fitted(Symbol::ServerRack, size, room + 3.0, device).unwrap().0;
            assert_eq!(roomy.alpha, natural.alpha, "{device}x: one that fits is left alone");
        }
    }

    /// Each status the OS draws has its own symbol, and the two that carry colour are filled;
    /// working and done are ours.
    #[test]
    fn each_status_has_its_own_mark() {
        let all = [Status::Idle, Status::Running, Status::NeedsYou, Status::Failed, Status::Away];
        let symbols: std::collections::HashSet<_> = all.iter().filter_map(|s| s.symbol()).collect();
        assert_eq!(symbols.len(), all.len());
        assert_eq!(Status::NeedsYou.symbol(), Some(Symbol::ExclamationmarkCircleFill));
        assert_eq!(Status::Failed.symbol(), Some(Symbol::XmarkCircleFill));
        assert_eq!(Status::Working.symbol(), None);
        assert_eq!(Status::Done.symbol(), None);
    }

    /// Twelve steps make one turn a second, each held a twelfth of a second, and the timer
    /// always waits for the next step's start. Under Reduce Motion the mark stands on its
    /// first step whatever the time, and breathes: whole and faint by turns over the breath,
    /// never under its floor.
    #[test]
    fn the_working_mark_steps_twelve_times_a_turn_and_stands_under_reduce_motion() {
        let ms = Duration::from_millis;
        assert_eq!(spin_step(ms(0), false), 0);
        assert_eq!(spin_step(ms(80), false), 0, "still the first step");
        assert_eq!(spin_step(ms(90), false), 1);
        assert_eq!(spin_step(ms(990), false), 11);
        assert_eq!(spin_step(ms(1_000), false), 0, "one turn a second");
        assert_eq!(spin_step(ms(2_500), false), 6);
        for at in [0, 90, 500, 990, 12_345] {
            assert_eq!(spin_step(ms(at), true), 0, "Reduce Motion at {at} ms");
        }
        assert_eq!(until_next_step(ms(0)), SPIN_STEP, "on a step's start, a whole step away");
        assert_eq!(until_next_step(ms(80)), SPIN_STEP.saturating_sub(ms(80)));
        let next = ms(90).saturating_add(until_next_step(ms(90)));
        assert_eq!(spin_step(next, false), 2, "the timer lands on the next step");
        let turn = SPIN_STEP.checked_mul(SPIN_STEPS).unwrap_or_default();
        assert!(ms(1_000).saturating_sub(turn) < Duration::from_micros(1), "{turn:?}");
        assert!((breath(ms(0)) - BREATH_LOW).abs() < 1e-4, "it starts at its faintest");
        assert!((breath(ms(1_200)) - 1.0).abs() < 1e-3, "whole half a breath in");
        assert!((breath(ms(2_400)) - BREATH_LOW).abs() < 1e-4, "one breath a period");
        assert_eq!(breath(ms(150)), breath(ms(0)), "held for a step");
        let breath_ns = slopty_theme::Motion::DEFAULT.breath.as_nanos();
        assert_eq!(breath_ns.checked_rem(BREATH_STEP.as_nanos()), Some(0), "steps fill a breath");
        for at in (0..2_400).step_by(50) {
            let b = breath(ms(at));
            assert!((BREATH_LOW..=1.0).contains(&b), "{at} ms: {b}");
        }
    }

    /// A mark is an image named by its status at any zoom; an empty slot is nothing to a
    /// screen reader, and takes the same room.
    #[gpui::test]
    fn a_status_mark_is_an_image_named_by_its_status(cx: &mut gpui::TestAppContext) {
        struct Marks;
        impl gpui::Render for Marks {
            fn render(
                &mut self,
                _window: &mut Window,
                _cx: &mut gpui::Context<Self>,
            ) -> impl IntoElement {
                let theme = Theme::default();
                div()
                    .id("marks")
                    .child(div().id("a").child(status_mark(&theme, Some(Status::NeedsYou), 1.0)))
                    .child(div().id("b").child(status_mark(&theme, Some(Status::Working), 0.5)))
                    .child(div().id("c").child(status_mark(&theme, None, 1.0)))
            }
        }
        let (view, cx) = cx.add_window_view(|_, _| Marks);
        cx.update(|window, _cx| window.set_a11y_active(true));
        view.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        let tree = cx.update(|window, _cx| crate::a11y::tree(window));
        assert!(tree.iter().any(|n| n.is("Image", Some("Needs you"))), "{tree:#?}");
        assert!(tree.iter().any(|n| n.is("Image", Some("Working"))), "{tree:#?}");
        assert_eq!(tree.iter().filter(|n| n.role == "Image").count(), 2, "{tree:#?}");
    }

    /// A view that shows a working mark or not, and counts its renders.
    struct Turning {
        shown: bool,
        status: Status,
        renders: usize,
    }

    impl gpui::Render for Turning {
        fn render(
            &mut self,
            _window: &mut Window,
            _cx: &mut gpui::Context<Self>,
        ) -> impl IntoElement {
            self.renders = self.renders.saturating_add(1);
            let theme = Theme::default();
            div().children(self.shown.then(|| status_mark(&theme, Some(self.status), 1.0)))
        }
    }

    /// Renders of `view` over one second of the executor's clock.
    fn renders_in_a_second(view: &gpui::Entity<Turning>, cx: &gpui::VisualTestContext) -> usize {
        let before = view.read_with(cx, |v, _| v.renders);
        for _ in 0..120 {
            cx.executor().advance_clock(Duration::from_nanos(8_333_333));
            cx.run_until_parked();
        }
        view.read_with(cx, |v, _| v.renders).saturating_sub(before)
    }

    /// A breathing mark draws the breath of the clock's last wake, not of the moment it is
    /// drawn: with a step held back for a key's echo, a mark drawn anew (a frame from scratch)
    /// shows the breath the frame shown does, and the wake that ends the hold moves both.
    #[gpui::test]
    fn a_breath_held_back_is_drawn_from_scratch_as_it_shows(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (view, cx) =
            cx.add_window_view(|_, _| Turning { shown: true, status: Status::Working, renders: 0 });
        cx.run_until_parked();
        let drawn = cx.update(|_w, cx| SpinClock::drawn(cx));
        let held = cx.executor().now().checked_add(BREATH_STEP.saturating_mul(3));
        cx.update(|_w, cx| hold_steps(cx, held.unwrap_or_else(|| cx.background_executor().now())));
        let before = view.read_with(cx, |v, _| v.renders);
        cx.executor().advance_clock(BREATH_STEP.saturating_add(BREATH_STEP.div_f32(2.0)));
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |v, _| v.renders), before, "the step waits on the echo");
        assert_eq!(cx.update(|_w, cx| SpinClock::drawn(cx)), drawn, "drawn anew as it shows");
        cx.update(|_w, cx| release_steps(cx));
        cx.run_until_parked();
        assert!(view.read_with(cx, |v, _| v.renders) > before, "the echo's frame moves it");
        assert_ne!(cx.update(|_w, cx| SpinClock::drawn(cx)), drawn, "a breath further on");
    }

    /// Pinned, every mark stands upright at the top of its breath whenever it is drawn, and
    /// wakes nothing; let go, it breathes again.
    #[gpui::test]
    fn a_pinned_mark_holds_still_until_let_go(cx: &mut gpui::TestAppContext) {
        assert_eq!(spin_step(PINNED_STEPS, false), 0, "upright");
        assert!((breath(PINNED_STEPS) - 1.0).abs() < f32::EPSILON, "whole");
        cx.update(|cx| cx.set_reduce_motion(true));
        let (view, cx) =
            cx.add_window_view(|_, _| Turning { shown: true, status: Status::Working, renders: 0 });
        cx.run_until_parked();
        cx.update(|_w, cx| pin_steps(Some(PINNED_STEPS), cx));
        cx.run_until_parked();
        assert_eq!(cx.update(|_w, cx| SpinClock::drawn(cx)), (0, 1.0), "the pinned moment");
        assert!(renders_over(&view, cx, Duration::from_secs(1)) <= 1, "the step already due");
        assert_eq!(renders_over(&view, cx, Duration::from_secs(1)), 0, "pinned, nothing wakes");
        cx.update(|_w, cx| pin_steps(None, cx));
        cx.run_until_parked();
        assert!(renders_over(&view, cx, Duration::from_secs(1)) >= 4, "let go, it breathes");
    }

    /// The mark wakes the view it was painted in once a step while it shows, and not once it
    /// is gone; under Reduce Motion once a breath's step, under half as often.
    #[gpui::test]
    fn a_working_mark_wakes_its_view_only_while_it_shows(cx: &mut gpui::TestAppContext) {
        let (view, cx) =
            cx.add_window_view(|_, _| Turning { shown: true, status: Status::Working, renders: 0 });
        cx.run_until_parked();
        let shown = renders_in_a_second(&view, cx);
        assert!((11..=13).contains(&shown), "{shown} renders in a second");
        view.update(cx, |v, cx| {
            v.shown = false;
            cx.notify();
        });
        cx.run_until_parked();
        assert!(renders_in_a_second(&view, cx) <= 1, "the step already due at most");
        assert_eq!(renders_in_a_second(&view, cx), 0, "gone, it wakes nothing");
        cx.update(|_w, cx| cx.set_reduce_motion(true));
        view.update(cx, |v, cx| {
            v.shown = true;
            cx.notify();
        });
        cx.run_until_parked();
        let breathing = renders_in_a_second(&view, cx);
        assert!((4..=6).contains(&breathing), "Reduce Motion: it breathes, {breathing} a second");
    }

    /// Waiting is a still dashed ring: drawn once, it wakes its view for nothing, so two frames
    /// a second apart are the same frame. Only the working mark moves.
    #[gpui::test]
    fn waiting_holds_still(cx: &mut gpui::TestAppContext) {
        let (view, cx) =
            cx.add_window_view(|_, _| Turning { shown: true, status: Status::Running, renders: 0 });
        cx.run_until_parked();
        assert_eq!(renders_over(&view, cx, Duration::from_secs(3)), 0, "it never wakes");
        assert_eq!(until_next_second(Duration::from_millis(2_300)), Duration::from_millis(700));
    }

    /// Renders of `view` while the executor's clock runs `for`, a refresh at a time.
    fn renders_over(
        view: &gpui::Entity<Turning>,
        cx: &gpui::VisualTestContext,
        span: Duration,
    ) -> usize {
        let before = view.read_with(cx, |v, _| v.renders);
        let refresh = Duration::from_nanos(8_333_333);
        let mut ran = Duration::ZERO;
        while ran < span {
            cx.executor().advance_clock(refresh);
            cx.run_until_parked();
            ran = ran.saturating_add(refresh);
        }
        view.read_with(cx, |v, _| v.renders).saturating_sub(before)
    }

    /// While a key waits for its echo, a step that falls due wakes nobody: it waits for the
    /// echo's frame, or for the hold's end, and then the steps go on as before.
    #[gpui::test]
    fn a_held_step_waits_for_the_echo_or_the_end_of_the_hold(cx: &mut gpui::TestAppContext) {
        let (view, cx) =
            cx.add_window_view(|_, _| Turning { shown: true, status: Status::Working, renders: 0 });
        cx.run_until_parked();
        let now =
            |cx: &mut gpui::VisualTestContext| cx.update(|_w, cx| cx.background_executor().now());
        let hold = SPIN_STEP.saturating_mul(4);
        let until = now(cx).checked_add(hold).expect("a hold in range");
        cx.update(|_w, cx| hold_steps(cx, until));
        assert_eq!(renders_over(&view, cx, SPIN_STEP.saturating_mul(2)), 0, "held");
        cx.update(|_w, cx| release_steps(cx));
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |v, _| v.renders), 2, "the echo's frame carries it");
        let turning = renders_over(&view, cx, Duration::from_secs(1));
        assert!((11..=13).contains(&turning), "{turning} steps after the release");

        let until = now(cx).checked_add(hold).expect("a hold in range");
        cx.update(|_w, cx| hold_steps(cx, until));
        let held = renders_over(&view, cx, hold.saturating_sub(Duration::from_millis(10)));
        assert_eq!(held, 0, "no echo came: nothing before the hold ends");
        let after = renders_over(&view, cx, SPIN_STEP);
        assert!((1..=2).contains(&after), "the hold's end draws the step: {after}");
        cx.update(|_w, cx| release_steps(cx));
        cx.run_until_parked();
        let turning = renders_over(&view, cx, Duration::from_secs(1));
        assert!((11..=13).contains(&turning), "{turning} steps once it ran out");
    }
}

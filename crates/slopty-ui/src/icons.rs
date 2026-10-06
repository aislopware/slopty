//! The icons the chrome draws: SF Symbols, drawn by the OS at the size of the words beside them
//! (`slopty_platform::symbols`), painted at exact device pixels in the ink of those words.
//!
//! A [`Symbol`] is one of a closed list. A file's type is one of nine of them
//! ([`FileType::symbol`]). An agent wears its owner's mark ([`AgentMark`]), drawn by us from
//! the owner's outline into the same kind of mask, and an agent with none the neutral
//! [`AGENT`]. Git is GitHub's Octicons ([`GitGlyph`]), drawn the same way, since SF has no git
//! vocabulary. Nothing else is drawn by us but the working mark's cell of dots and the rings
//! of a state ([`Ring`]), which SF cannot draw crisp at 1x (`docs/decisions/ui.md`, "The
//! chrome's icons are SF Symbols", "State is a glyph" and "An icon takes its words' size,
//! weight and tier"; `docs/decisions/brand.md`, "Each agent wears its owner's mark").
//!
//! An icon takes its words' type role ([`beside`]): [`IconSize::Lead`] leads a row or stands
//! alone in [`slopty_theme::Typography::icon_large`] at the chrome's size; [`IconSize::Inline`]
//! sits in [`slopty_theme::Typography::icon`] beside a row's facts, at their size but never
//! under [`SYMBOL_FLOOR`], where SF draws a smaller design. Its weight is its words'. A slot
//! sized larger or smaller (the chrome's zoom) draws its symbol larger or smaller by as much,
//! so an icon never parts from its words' size.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use gpui::accesskit::Role;
use gpui::{
    AnimationExt as _, AnyElement, App, Bounds, DevicePixels, Div, Element, ElementId, EntityId,
    Global, GlobalElementId, Hsla, InspectorElementId, InteractiveElement as _, IntoElement,
    LayoutId, ParentElement as _, Pixels, Point, ScaledPixels, SharedString, Stateful,
    StatefulInteractiveElement as _, Styled as _, TransformationMatrix, Window, canvas, div, point,
    px, radians,
};
use parking_lot::RwLock;
use slopty_platform::symbols::{Masks, SymbolMask};
pub use slopty_platform::symbols::{Scale, Symbol, SymbolSize, Weight};
use slopty_theme::{Rgb, Theme, TypeRole, Typography};

use crate::colors::hsla;
pub use crate::file_types::FileType;

mod git;
mod marks;
mod ring;

pub use git::GitGlyph;
pub use marks::AgentMark;
pub use ring::Ring;

/// The thread of an agent with no mark of its own ([`AgentMark::Neutral`]), an ACP agent's:
/// one neutral mark in the ink beside it, a conversation, its agent named in words.
pub const AGENT: Symbol = Symbol::TextBubble;

/// What leads a row: a symbol for what a thing is, or the mark of the agent it runs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mark {
    /// An SF Symbol.
    Symbol(Symbol),
    /// An agent's own mark; [`AgentMark::Neutral`] is drawn as [`AGENT`].
    Agent(AgentMark),
    /// A git glyph, an Octicon.
    Git(GitGlyph),
}

impl From<GitGlyph> for Mark {
    fn from(glyph: GitGlyph) -> Self {
        Self::Git(glyph)
    }
}

impl From<Symbol> for Mark {
    fn from(symbol: Symbol) -> Self {
        Self::Symbol(symbol)
    }
}

impl From<AgentMark> for Mark {
    /// An agent with no mark of its own leads with [`AGENT`], so the two compare equal.
    fn from(mark: AgentMark) -> Self {
        match mark {
            AgentMark::Neutral => Self::Symbol(AGENT),
            mark => Self::Agent(mark),
        }
    }
}

impl Mark {
    /// The mark of the agent named `agent` (an `AgentId`'s name).
    #[must_use]
    pub fn agent(agent: &str) -> Self {
        AgentMark::of(agent).into()
    }

    /// What a screen reader calls it: the agent an agent's mark names, else nothing, as a
    /// symbol beside its words says nothing they do not.
    #[must_use]
    pub const fn label(self) -> Option<&'static str> {
        match self {
            Self::Agent(mark) => mark.label(),
            Self::Symbol(_) | Self::Git(_) => None,
        }
    }
}

/// The symbol for the file at `path`: its type's, or the plain document.
#[must_use]
pub fn file_symbol(path: &str) -> Symbol {
    FileType::of(path).map_or(Symbol::Doc, FileType::symbol)
}

/// The symbol a machine wears, by its form.
///
/// As Finder and Find My show a device: a laptop, a desktop's display, a server's rack. A
/// machine that has not said yet wears the rack, the form of one that does not say.
#[must_use]
pub const fn machine(form: Option<slopty_proto::server::Form>) -> Symbol {
    use slopty_proto::server::Form;
    match form {
        Some(Form::Laptop) => Symbol::Laptopcomputer,
        Some(Form::Desktop) => Symbol::Display,
        Some(Form::Server) | None => Symbol::ServerRack,
    }
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

/// The least point size a symbol is drawn at, the disclosure chevrons aside.
///
/// Under about 12.25 pt SF Symbols draws a smaller design, a fifth narrower for the same stroke,
/// which cost an icon a quarter of its ink beside 13 pt words (`docs/MEASUREMENTS.md`, "SF Symbols'
/// smaller design").
pub const SYMBOL_FLOOR: f32 = 12.5;

/// How large an icon is drawn, by the words it sits beside.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IconSize {
    /// Beside a row's facts, the metadata role: their point size, never under
    /// [`SYMBOL_FLOOR`], in [`slopty_theme::Typography::icon`]'s slot.
    Inline,
    /// A row's lead, an icon button, or standing beside a title: the chrome's size in
    /// [`slopty_theme::Typography::icon_large`]'s slot.
    Lead,
}

impl IconSize {
    /// The size for an icon beside words of `role`: [`Self::Lead`] beside the chrome's size or
    /// larger, [`Self::Inline`] beside smaller.
    #[must_use]
    pub fn beside(theme: &Theme, role: TypeRole) -> Self {
        if role.size >= theme.typography.ui_size { Self::Lead } else { Self::Inline }
    }

    /// The side of the slot an icon beside words of `role` stands in: the words' size and the
    /// room a lead or an inline icon keeps round it, so a finger's larger words get a larger
    /// slot as a pointer's do.
    #[must_use]
    pub fn beside_slot(theme: &Theme, role: TypeRole) -> f32 {
        let ty = &theme.typography;
        let room = match Self::beside(theme, role) {
            Self::Lead => ty.icon_large() - ty.ui_size,
            Self::Inline => ty.icon() - ty.small(),
        };
        role.size + room
    }

    /// The slot's side in points.
    #[must_use]
    pub fn slot(self, theme: &Theme) -> f32 {
        match self {
            Self::Inline => theme.typography.icon(),
            Self::Lead => theme.typography.icon_large(),
        }
    }

    /// The point size the symbol is drawn at: its words', and never under [`SYMBOL_FLOOR`].
    #[must_use]
    pub fn point(self, theme: &Theme) -> f32 {
        match self {
            Self::Inline => theme.typography.small().max(SYMBOL_FLOOR),
            Self::Lead => theme.typography.ui_size.max(SYMBOL_FLOOR),
        }
    }
}

/// The symbol weight beside words of `weight` (400, 500 or 600): regular, medium or semibold,
/// as the HIG matches a symbol's weight to its text's.
#[must_use]
pub fn weight_beside(weight: f32) -> Weight {
    if weight > Typography::MEDIUM_WEIGHT {
        Weight::Semibold
    } else if weight >= Typography::MEDIUM_WEIGHT {
        Weight::Medium
    } else {
        Weight::Regular
    }
}

/// How a mark is drawn in a slot: a symbol at `ratio` of the slot's side in points, `weight`
/// and `scale`, turned by `turn` radians; an agent's mark with its ink box `ink` of the side.
#[derive(Clone, Copy, Debug)]
pub struct Drawn {
    mark: Mark,
    ratio: f32,
    ink: f32,
    weight: Weight,
    scale: Scale,
    turn: f32,
}

impl Drawn {
    /// `mark` in a slot of `size`: a symbol regular and at the medium scale, an agent's mark
    /// with its ink box [`slopty_theme::Spacing::xxs`] short of the slot.
    #[must_use]
    pub fn new(theme: &Theme, mark: impl Into<Mark>, size: IconSize) -> Self {
        let slot = size.slot(theme);
        Self {
            mark: mark.into(),
            ratio: size.point(theme) / slot,
            ink: (slot - theme.spacing.xxs) / slot,
            weight: Weight::Regular,
            scale: Scale::Medium,
            turn: 0.0,
        }
    }

    /// `mark` beside words of `role`, in a slot [`IconSize::beside_slot`] square: at their point
    /// size, never under [`SYMBOL_FLOOR`], and their weight ([`weight_beside`]).
    #[must_use]
    pub fn beside(theme: &Theme, mark: impl Into<Mark>, role: TypeRole) -> Self {
        let slot = IconSize::beside_slot(theme, role);
        Self {
            ratio: role.size.max(SYMBOL_FLOOR) / slot,
            ink: (slot - theme.spacing.xxs) / slot,
            ..Self::new(theme, mark, IconSize::beside(theme, role))
        }
        .weight(weight_beside(role.weight))
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

    /// An empty state's mark: a symbol at the page heading's size in
    /// [`crate::kit::NOTICE_MARK`], at the light weight and the large scale; an agent's mark
    /// with its ink box two [`slopty_theme::Spacing::xxs`] short of the slot.
    #[must_use]
    pub fn notice(theme: &Theme, mark: impl Into<Mark>) -> Self {
        let slot = crate::kit::NOTICE_MARK;
        Self {
            ratio: theme.typography.heading() / slot,
            ink: theme.spacing.xxs.mul_add(-2.0, slot) / slot,
            weight: Weight::Light,
            scale: Scale::Large,
            ..Self::new(theme, mark, IconSize::Lead)
        }
    }

    /// The ink box an agent's mark is drawn with in a slot `side` points square, in points.
    #[must_use]
    pub fn ink(self, side: f32) -> f32 {
        side * self.ink
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

    /// Paints the mark centred in `bounds`.
    fn paint(self, bounds: Bounds<Pixels>, window: &mut Window) {
        match self.mark {
            Mark::Symbol(symbol) => self.paint_symbol(symbol, bounds, window),
            Mark::Agent(AgentMark::Neutral) => self.paint_symbol(AGENT, bounds, window),
            Mark::Agent(mark) => self.paint_agent(mark, bounds, window),
            Mark::Git(glyph) => self.paint_git(glyph, bounds, window),
        }
    }

    /// Paints a git glyph centred on its ink in `bounds`, on the device's pixel grid, its
    /// 16-unit grid [`git::EM`] of the symbol's point size. Never weighted: the Octicon's own
    /// strokes, which sit between SF's regular and medium.
    fn paint_git(self, glyph: GitGlyph, bounds: Bounds<Pixels>, window: &mut Window) {
        let side = f32::from(bounds.size.width.min(bounds.size.height));
        let device = window.scale_factor();
        let em = self.size(side).point * git::EM;
        let Some((mask, key)) = git::mask(glyph, em, device) else {
            return;
        };
        paint_centred(&mask, key, bounds, window, || format!("{glyph:?}"));
    }

    /// Paints an agent's mark centred on its ink in `bounds`, on the device's pixel grid: its
    /// mask is its ink box, so the box is centred and its origin rounded to a whole pixel.
    /// Never turned, never weighted: a filled silhouette, as the owner draws it.
    fn paint_agent(self, mark: AgentMark, bounds: Bounds<Pixels>, window: &mut Window) {
        let side = f32::from(bounds.size.width.min(bounds.size.height));
        let device = window.scale_factor();
        let Some((mask, key)) = marks::mask(mark, self.ink(side), device) else {
            return;
        };
        paint_centred(&mask, key, bounds, window, || format!("{mark:?}"));
    }

    /// Paints `symbol` centred in `bounds`: its box across, its alignment rectangle (the
    /// baseline to the cap height) down, as the words beside it centre.
    fn paint_symbol(self, symbol: Symbol, bounds: Bounds<Pixels>, window: &mut Window) {
        let side = f32::from(bounds.size.width.min(bounds.size.height));
        let device = window.scale_factor();
        let room = f32::from(bounds.size.width) * FIT_ROOM * device;
        let Some((mask, key, _)) = fitted(symbol, self.size(side), room, device) else {
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
            tracing::warn!(%error, symbol = symbol.name(), "a symbol was not painted");
        }
    }
}

/// Paints `mask` in the text's ink with its box centred in `bounds` and its origin on a whole
/// device pixel, as an agent's mark and a git glyph are; `what` names it if it fails.
fn paint_centred(
    mask: &Arc<SymbolMask>,
    key: SharedString,
    bounds: Bounds<Pixels>,
    window: &mut Window,
    what: impl FnOnce() -> String,
) {
    let (Ok(width), Ok(height)) = (i32::try_from(mask.width), i32::try_from(mask.height)) else {
        return;
    };
    let device = window.scale_factor();
    let centre = bounds.center().scale(device);
    #[expect(clippy::cast_precision_loss, reason = "a mask is a few dozen pixels")]
    let (wide, high) = (mask.width as f32, mask.height as f32);
    let snap = |middle: ScaledPixels, extent: f32| px((middle.0 - extent / 2.0).round() / device);
    let origin = point(snap(centre.x, wide), snap(centre.y, high));
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
type Kept = Option<(Arc<SymbolMask>, SharedString)>;

/// The masks every window paints from, shared with the prewarm's thread, and each one's atlas
/// key, made once. A symbol the OS lacks is kept as a miss.
struct Symbols {
    masks: Masks,
    kept: RwLock<HashMap<(Symbol, SymbolSize, u32), Held>>,
}

/// A symbol's mask, its atlas key, and how many device pixels across its ink is: the OS pads
/// a symbol's image a pixel or more each side, so a fit is judged on the ink.
type Held = Option<(Arc<SymbolMask>, SharedString, u32)>;

/// How many columns of `mask` hold ink, from the first to the last.
fn ink_width(mask: &SymbolMask) -> u32 {
    let Ok(wide) = usize::try_from(mask.width) else { return mask.width };
    if wide == 0 {
        return 0;
    }
    let inked = |x: &usize| mask.alpha.chunks(wide).any(|row| row.get(*x).is_some_and(|&a| a > 0));
    let first = (0..wide).find(inked);
    let last = (0..wide).rev().find(inked);
    first
        .zip(last)
        .and_then(|(first, last)| last.checked_sub(first))
        .and_then(|span| u32::try_from(span).ok())
        .map_or(0, |span| span.saturating_add(1))
}

static SYMBOLS: LazyLock<Symbols> =
    LazyLock::new(|| Symbols { masks: Masks::new(), kept: RwLock::new(HashMap::new()) });

/// `symbol` at `size` for a display of `device` pixels to the point, its atlas key and its
/// ink's width.
fn mask(symbol: Symbol, size: SymbolSize, device: f32) -> Held {
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
        let ink = ink_width(&mask);
        (mask, SharedString::from(key), ink)
    });
    SYMBOLS.kept.write().entry(at).or_insert(kept).clone()
}

/// `symbol` at `size`, drawn smaller where its ink is wider than `room` device pixels: a wide
/// symbol kept by its slot, so it never reaches the words beside it.
///
/// The point size is scaled by the overflow and stepped down a quarter point at a time while
/// the OS's rounding still leaves it a pixel over, so it shrinks no more than it must.
fn fitted(symbol: Symbol, size: SymbolSize, room: f32, device: f32) -> Held {
    let room = room.ceil();
    let mut kept = mask(symbol, size, device)?;
    let mut point = size.point;
    for _ in 0..FIT_STEPS {
        #[expect(clippy::cast_precision_loss, reason = "a mask is a few dozen pixels")]
        let wide = kept.2 as f32;
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

/// How wide a symbol's ink may be against its slot before [`fitted`] draws it smaller: an
/// eighth over, into the gap beside it, so a wide symbol (a server rack, a folder, 15 pt of
/// ink at 13 pt) keeps its words' size in a lead's 16 pt slot and gives way only past 18.
const FIT_ROOM: f32 = 18.0 / 16.0;

/// How many times [`fitted`] draws a symbol smaller before it keeps the last.
const FIT_STEPS: usize = 4;

/// The display scales a window of this platform is likely shown at, the likeliest first.
const SCALES: &[f32] = if cfg!(target_os = "ios") { &[2.0, 3.0] } else { &[1.0, 2.0] };

/// The scale the catalogue's first mask is drawn at when the display's is not known yet: any
/// scale loads the catalogue.
const CATALOGUE_SCALE: f32 = 2.0;

/// Draws, on background threads, the masks the last launch's first frames drew, as
/// [`remember`] wrote them to `remembered`, at the scale of the display the window opens on.
///
/// The first frame then finds them drawn. Start it before the first window: the first symbol
/// of a process loads the system's catalogue, 40 to 70 ms (`docs/MEASUREMENTS.md`, "SF
/// Symbols as masks"). The list is read on the prewarm's thread. With no list (a first launch,
/// or one that does not read), only the catalogue is loaded, by drawing the navigator's
/// disclosure, and the first frame draws the rest, a quarter to half a millisecond each.
///
/// Only what is drawn is warmed: each SF Symbol drawn holds its share of the OS's symbol data
/// for the life of the process (`docs/MEASUREMENTS.md`, "the footprint at rest").
pub fn prewarm(theme: &Theme, remembered: std::path::PathBuf) {
    let scale = slopty_platform::symbols::main_display_scale();
    let catalogue = (
        Symbol::ChevronRight,
        Drawn::disclosure(theme, Symbol::ChevronRight).size(IconSize::Inline.slot(theme)),
        scale.unwrap_or(CATALOGUE_SCALE),
    );
    let wanted = move || {
        let list = slopty_platform::symbols::remembered(&remembered, scale);
        if list.is_empty() { vec![catalogue] } else { list }
    };
    if let Err(error) = SYMBOLS.masks.prewarm(wanted) {
        tracing::warn!(%error, "the symbols' prewarm did not start");
    }
    // The agents' marks at the row's, the chip's and the empty state's ink: a dozen masks
    // Core Graphics fills from their outlines, with nothing of the OS's held after.
    let inline = IconSize::Inline.slot(theme);
    let inks = [
        Drawn::new(theme, AGENT, IconSize::Lead).ink(IconSize::Lead.slot(theme)),
        Drawn::new(theme, AGENT, IconSize::Inline).ink(inline),
        Drawn::notice(theme, AGENT).ink(crate::kit::NOTICE_MARK),
    ];
    // The git glyphs at a lead's and a fact's size, on the same thread: Core Graphics fills
    // too, a couple of dozen microseconds each.
    let ems = [IconSize::Lead, IconSize::Inline].map(|size| size.point(theme) * git::EM);
    let scales: Vec<f32> = scale.map_or_else(|| SCALES.to_vec(), |scale| vec![scale]);
    let marks = std::thread::Builder::new().name("agent-marks".into()).spawn(move || {
        for mark in AgentMark::OWNED {
            for (&ink, &scale) in inks.iter().flat_map(|ink| scales.iter().map(move |s| (ink, s))) {
                marks::mask(mark, ink, scale);
            }
        }
        for glyph in GitGlyph::ALL {
            for (&em, &scale) in ems.iter().flat_map(|em| scales.iter().map(move |s| (em, s))) {
                git::mask(glyph, em, scale);
            }
        }
    });
    if let Err(error) = marks {
        tracing::warn!(%error, "the agents' marks' prewarm did not start");
    }
}

/// Writes to `path` the masks painters asked for since launch, for the next launch's
/// [`prewarm`], and stops noting them. Call it once, a few seconds after the first window
/// opens, off the main thread.
pub fn remember(path: &std::path::Path) {
    if let Err(error) = SYMBOLS.masks.remember(path) {
        tracing::warn!(%error, path = %path.display(), "the drawn symbols were not written down");
    }
}

/// `symbol` at `size`, in `color`: a slot of the size's side.
#[must_use]
pub fn icon(theme: &Theme, symbol: impl Into<Mark>, size: IconSize, color: Hsla) -> Div {
    Drawn::new(theme, symbol, size).slot(px(size.slot(theme)), color)
}

/// `mark` beside words of `role`, in `ink`: their size and weight ([`Drawn::beside`]) in a
/// slot of [`IconSize::beside_slot`]. Size the slot again only by the chrome's zoom, and the
/// mark follows it.
#[must_use]
pub fn beside(theme: &Theme, mark: impl Into<Mark>, role: TypeRole, ink: Hsla) -> Div {
    Drawn::beside(theme, mark, role).slot(px(IconSize::beside_slot(theme, role)), ink)
}

/// `symbol` in a slot `side` square, in `ink`, drawn at the inline icon's share of the slot:
/// a row's lead at whatever zoom.
#[must_use]
pub fn symbol(theme: &Theme, symbol: impl Into<Mark>, side: Pixels, ink: Hsla) -> AnyElement {
    Drawn::new(theme, symbol, IconSize::Inline).slot(side, ink).into_any_element()
}

/// The one vocabulary for how a thing is doing, wherever it is shown: a tile's header, a
/// navigator row, the palette, a toast. Each state has one glyph and one hue, so a glance
/// reads the same everywhere.
///
/// Every state is a glyph, never a dot: one family of circles, filled where it carries colour
/// (`docs/decisions/ui.md`, "State is a glyph"). The glyph carries the hue and the words beside
/// it stay neutral: working blue, needs you amber, failed red and a finish not yet seen green,
/// each in its mark's fill step ([`Status::ink`]). Waiting, idle and a worker out of reach are
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
    /// The symbol that marks it; `None` for the marks drawn by us, the working mark's dots
    /// and the rings of idle and waiting ([`status_icon`]). The three that carry colour are
    /// filled, so the colour has a body at 1x.
    #[must_use]
    pub const fn symbol(self) -> Option<Symbol> {
        match self {
            Self::Idle | Self::Working | Self::Running => None,
            Self::NeedsYou => Some(Symbol::ExclamationmarkCircleFill),
            Self::Done => Some(Symbol::CheckmarkCircleFill),
            Self::Failed => Some(Symbol::XmarkCircleFill),
            Self::Away => Some(Symbol::WifiSlash),
        }
    }

    /// Its glyph's ink: the hue's mark step, never its text step, so a glyph has the body a
    /// mark needs on both grounds. Working blue, needs you amber, failed red, done green; the
    /// rest grey.
    #[must_use]
    pub const fn ink(self, theme: &Theme) -> Rgb {
        let s = &theme.surfaces;
        match self {
            Self::Idle | Self::Running | Self::Away => s.text_muted,
            Self::Working => s.working_fill,
            Self::NeedsYou => s.warn_fill,
            Self::Done => s.success_fill,
            Self::Failed => s.error_fill,
        }
    }

    /// Its glyph's ink where colour stays out, inside a thread's stream and over a drop: the
    /// working mark grey, every other as [`Self::ink`].
    #[must_use]
    pub const fn quiet_ink(self, theme: &Theme) -> Rgb {
        match self {
            Self::Working => theme.surfaces.text_muted,
            _ => self.ink(theme),
        }
    }

    /// The tone of its word beside the glyph: neutral, but a failure's, which keeps the
    /// failure's text tone.
    #[must_use]
    pub const fn word(self, theme: &Theme) -> Rgb {
        match self {
            Self::Failed => theme.surfaces.error,
            _ => theme.surfaces.text_muted,
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
        Some(Status::Done) => {
            slot.role(Role::Image).aria_label(Status::Done.label()).child(Arrive {
                child: Some(div().flex_none().child(status_icon(
                    theme,
                    Status::Done,
                    px(theme.typography.icon() * k),
                    hsla(Status::Done.ink(theme)),
                ))),
                inner: None,
            })
        }
        Some(status) => slot.role(Role::Image).aria_label(status.label()).child(status_icon(
            theme,
            status,
            px(theme.typography.icon() * k),
            hsla(status.ink(theme)),
        )),
        None => slot,
    }
}

/// A finish's check arriving: it fades in over [`crate::kit::Pace::Settle`] the first frame its
/// slot shows it, and under Reduce Motion it is there at once. It reads the setting as it is
/// laid out, as the working mark does, so no caller hands it the app.
struct Arrive {
    child: Option<Div>,
    inner: Option<AnyElement>,
}

impl IntoElement for Arrive {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Arrive {
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
        let child = self.child.take().unwrap_or_else(div);
        let mut inner = if crate::kit::motion(cx) {
            child
                .with_animation("arrive", crate::kit::Pace::Settle.animation(), |el, t| {
                    el.opacity(t)
                })
                .into_any_element()
        } else {
            child.into_any_element()
        };
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
    }
}

/// `status` as an empty state's mark, in `color` at the chrome's zoom `k`.
///
/// Its symbol is drawn as [`Drawn::notice`] in the notice's slot, and the working cell or a
/// ring at the heading's size, so a tile's state reads at the size of the notice it heads.
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
/// [`Status::Working`]'s dots go round ([`spin_step`]), the only mark that moves;
/// [`Status::Running`] (waiting on its own background work) is a still dashed ring, since a mark
/// that moves says work is in progress; [`Status::Idle`] is an empty ring, for the places a mark
/// is required. The rest are their symbols.
#[must_use]
pub fn status_icon(theme: &Theme, status: Status, side: Pixels, color: Hsla) -> AnyElement {
    match status {
        Status::Working => Spinner { side, color, inner: None }.into_any_element(),
        Status::Idle => Ring::Empty.draw(side, color),
        Status::Running => Ring::Dashed.draw(side, color),
        _ => match status.symbol() {
            Some(symbol) => {
                icon(theme, symbol, IconSize::Inline, color).size(side).into_any_element()
            }
            None => div().flex_none().size(side).into_any_element(),
        },
    }
}

/// Where a task stands in its life on a project's board, as its lane says.
///
/// The same family of circles as [`Status`], from up next's empty ring through verifying's pie
/// to merged's violet check (`docs/decisions/ui.md`, "State is a glyph"). Not a pipeline's
/// stage (`project::model::Stage`), which is a step of a task's way to the merge.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Phase {
    /// An agent waits on the person.
    NeedsYou,
    /// Given up.
    Failed,
    /// An agent is on it.
    Working,
    /// Its agent waits on work of its own in the background.
    Waiting,
    /// Made, and nothing runs for it yet.
    UpNext,
    /// Its verifier runs, this share of the way.
    Verifying(f32),
    /// Its verifier passed; it waits for the merge.
    ReadyToMerge,
    /// On the target branch.
    Merged,
}

impl Phase {
    /// How far a verifier is said to be along when nothing says: half.
    pub const VERIFYING: Self = Self::Verifying(0.5);

    /// A lane's stage, the one its head wears.
    #[must_use]
    pub const fn of(lane: crate::project::model::Lane) -> Self {
        use crate::project::model::Lane;
        match lane {
            Lane::NeedsYou => Self::NeedsYou,
            Lane::Failed => Self::Failed,
            Lane::Working => Self::Working,
            Lane::UpNext => Self::UpNext,
            Lane::Verifying => Self::VERIFYING,
            Lane::ReadyToMerge => Self::ReadyToMerge,
            Lane::Merged => Self::Merged,
        }
    }

    /// The stage a live agent's `status` puts a task it works on in: its own lane's, unless
    /// the agent says more.
    #[must_use]
    pub const fn with_agent(self, status: Status) -> Self {
        match status {
            Status::NeedsYou => Self::NeedsYou,
            Status::Failed => Self::Failed,
            Status::Working => Self::Working,
            Status::Running => Self::Waiting,
            Status::Idle | Status::Done | Status::Away => self,
        }
    }

    /// Its glyph's ink.
    #[must_use]
    pub const fn ink(self, theme: &Theme) -> Rgb {
        let s = &theme.surfaces;
        match self {
            Self::NeedsYou => s.warn_fill,
            Self::Failed => s.error_fill,
            Self::Working | Self::Verifying(_) => s.working_fill,
            Self::Waiting | Self::UpNext => s.text_muted,
            Self::ReadyToMerge => s.success_fill,
            Self::Merged => s.merged_fill,
        }
    }

    /// Its name for the accessibility tree.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::NeedsYou => "Needs you",
            Self::Failed => "Failed",
            Self::Working => "Working",
            Self::Waiting => "Waiting",
            Self::UpNext => "Up next",
            Self::Verifying(_) => "Verifying",
            Self::ReadyToMerge => "Ready to merge",
            Self::Merged => "Merged",
        }
    }

    /// Its glyph, `side` square, in its ink.
    #[must_use]
    pub fn glyph(self, theme: &Theme, side: Pixels) -> AnyElement {
        let ink = hsla(self.ink(theme));
        let symbol = |symbol: Symbol| {
            icon(theme, symbol, IconSize::Inline, ink).size(side).into_any_element()
        };
        match self {
            Self::NeedsYou => symbol(Symbol::ExclamationmarkCircleFill),
            Self::Failed => symbol(Symbol::XmarkCircleFill),
            Self::Working => status_icon(theme, Status::Working, side, ink),
            Self::Waiting => Ring::Dashed.draw(side, ink),
            Self::UpNext => Ring::Empty.draw(side, ink),
            Self::Verifying(share) => Ring::Pie(share).draw(side, ink),
            Self::ReadyToMerge => symbol(Symbol::CheckmarkCircle),
            Self::Merged => symbol(Symbol::CheckmarkCircleFill),
        }
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

/// The working mark's dot and the pitch from one dot's edge to the next, as shares of its
/// slot's side: at a row's 14 pt slot a dot is 2.4 pt and the pitch 3.8, the dots of a
/// braille cell set at 12.5 pt, rounded to whole device pixels (2 and 4 at 1x, 5 and 8 at 2x).
const DOT: f32 = 0.17;
const DOT_PITCH: f32 = 0.27;

/// How much of the mark's ink the dots at rest keep: a faint track, so the cell holds one
/// shape while the lit dots go round it.
const DOT_TRACK: f32 = 0.22;

/// The cell's six dots, two columns of three, in the order the lit ones go round: clockwise
/// from the top left.
const DOT_RING: [(u8, u8); 6] = [(0, 0), (1, 0), (1, 1), (1, 2), (0, 2), (0, 1)];

/// The working mark: a braille cell of six dots round which three lit dots go, a fourth lit on
/// every other step as the head moves on, twelve frames a turn on the spin clock: the spinner a
/// terminal draws, and `MonoCode`'s (`docs/decisions/ui.md`, "The working mark is a braille
/// cell"). Drawn by us on whole device pixels, in the working hue, the dots at rest a faint
/// track. Under Reduce Motion it stands on its first step and breathes.
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

/// Which of [`DOT_RING`]'s dots `step` lights: the three ending at the head, which moves on
/// every second step, and on the step between, the next one too.
fn lit_dots(step: u32) -> [bool; 6] {
    const RING: u32 = 6;
    // The head starts a dot in, so the first frame, the one Reduce Motion stands on, is ⠋ and
    // not a column of three, which would read as a menu's "⋮".
    let head = step.wrapping_div(2).wrapping_add(1) % RING;
    let half = step % 2 == 1;
    let mut lit = [false; 6];
    for (at, dot) in (0..RING).zip(lit.iter_mut()) {
        let behind = head.wrapping_add(RING).wrapping_sub(at) % RING;
        *dot = behind < 3 || (half && behind == RING.wrapping_sub(1));
    }
    lit
}

/// A dot's side and the pitch between dots, in whole device pixels, for a slot `side` points
/// square on a display of `device` pixels to the point; the pitch leaves a pixel at least.
fn dot_cell(side: f32, device: f32) -> (f32, f32) {
    let dot = (DOT * side * device).round().max(1.0);
    (dot, (DOT_PITCH * side * device).round().max(dot + 1.0))
}

/// Paints the working mark's cell centred in `bounds` at `step`: each dot a whole number of
/// device pixels across, on a whole-pixel pitch, so every dot is the same and sharp at 1x.
fn paint_dots(bounds: Bounds<Pixels>, step: u32, color: Hsla, window: &mut Window) {
    let side = f32::from(bounds.size.width.min(bounds.size.height));
    let device = window.scale_factor();
    let (dot, pitch) = dot_cell(side, device);
    let centre = bounds.center().scale(device);
    let left = (centre.x.0 - f32::midpoint(dot, pitch)).round();
    let top = pitch.mul_add(-1.0, centre.y.0 - dot / 2.0).round();
    let lit = lit_dots(step);
    for (&(column, row), on) in DOT_RING.iter().zip(lit) {
        let x = f32::from(column).mul_add(pitch, left) / device;
        let y = f32::from(row).mul_add(pitch, top) / device;
        let size = px(dot / device);
        let ink = if on { color } else { color.opacity(DOT_TRACK) };
        let dot = Bounds { origin: point(px(x), px(y)), size: gpui::size(size, size) };
        window.paint_quad(gpui::fill(dot, ink).corner_radii(size / 2.0));
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
                paint_dots(bounds, step, color, window);
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
            let natural = mask(Symbol::ServerRack, size, device).unwrap();
            let room = f32::from(u16::try_from(natural.2).unwrap()) - 3.0;
            let fit = fitted(Symbol::ServerRack, size, room, device).unwrap();
            assert!(f32::from(u16::try_from(fit.2).unwrap()) <= room, "{device}x: {}", fit.2);
            // SF's smaller design under 12.25 pt is narrower by a step of its own.
            assert!(fit.2 * 4 >= natural.2 * 3, "{device}x shrank too far: {}", fit.2);
            assert!(natural.2 < natural.0.width, "{device}x: the ink is inside the OS's padding");
            let roomy = fitted(Symbol::ServerRack, size, room + 3.0, device).unwrap().0;
            assert_eq!(roomy.alpha, natural.0.alpha, "{device}x: one that fits is left alone");
        }
    }

    /// Why the floor is 12.5 pt: SF draws a smaller design under about 12.25 pt, narrower for
    /// the same stroke, so a symbol at 12 pt carried far less ink than at 12.5. Printed for
    /// `docs/MEASUREMENTS.md` ("SF Symbols' smaller design"): each symbol's ink width at 2x and
    /// its ink, the sum of its coverage.
    #[test]
    fn sf_draws_a_smaller_design_under_the_floor() {
        let ink = |m: &SymbolMask| m.alpha.iter().map(|&a| u32::from(a)).sum::<u32>() / 255;
        let symbols = [Symbol::Terminal, Symbol::Folder, Symbol::ServerRack, Symbol::DocText];
        for symbol in symbols {
            let mut row = Vec::new();
            for point in [12.0, 12.25, 12.5, 13.0] {
                let size = SymbolSize::new(point, Weight::Regular);
                let (mask, _, wide) = mask(symbol, size, 2.0).expect("the OS draws it");
                row.push((point, wide, ink(&mask)));
            }
            let said: Vec<String> =
                row.iter().map(|(p, w, i)| format!("{p} pt {w} px wide, ink {i}")).collect();
            eprintln!("{}: {}", symbol.name(), said.join("; "));
            let (Some(small), Some(floor)) = (row.first(), row.get(2)) else { continue };
            assert!(floor.1 * 10 >= small.1 * 11, "{}: 12.5 pt is a larger design", symbol.name());
            assert!(floor.2 * 10 >= small.2 * 11, "{}: with more ink", symbol.name());
        }
    }

    /// A lead's wide symbols keep their words' size in its slot: the machine, the folder and
    /// the window are not drawn smaller, as they were when the fit was the slot itself.
    #[test]
    fn a_leads_wide_symbol_keeps_its_size() {
        let theme = Theme::default();
        let slot = IconSize::Lead.slot(&theme);
        let size = Drawn::new(&theme, Symbol::ServerRack, IconSize::Lead).size(slot);
        for symbol in [Symbol::ServerRack, Symbol::Folder, Symbol::Macwindow, Symbol::Terminal] {
            for device in [1.0, 2.0] {
                let natural = mask(symbol, size, device).unwrap().0;
                let fit = fitted(symbol, size, slot * FIT_ROOM * device, device).unwrap().0;
                assert_eq!(fit.alpha, natural.alpha, "{symbol:?} at {device}x was shrunk");
            }
        }
    }

    /// A lint as a test: no symbol is drawn under [`SYMBOL_FLOOR`], where SF's smaller design
    /// starts, but the disclosure chevrons, which are Apple's own small drawing. The sizes
    /// every icon is drawn at come from here, so the check is on them.
    #[test]
    fn no_icon_size_draws_a_symbol_under_12_5() {
        let theme = Theme::default();
        let drawn = [
            (
                "inline",
                Drawn::new(&theme, Symbol::Doc, IconSize::Inline),
                IconSize::Inline.slot(&theme),
            ),
            ("lead", Drawn::new(&theme, Symbol::Doc, IconSize::Lead), IconSize::Lead.slot(&theme)),
            ("notice", Drawn::notice(&theme, Symbol::Doc), crate::kit::NOTICE_MARK),
        ];
        for (what, drawn, slot) in drawn {
            let point = drawn.size(slot).point;
            assert!(point >= SYMBOL_FLOOR - 1e-3, "{what} is drawn at {point}");
        }
        let roles = theme.typography.roles(false);
        for role in [roles.caption, roles.metadata, roles.chrome, roles.action, roles.section] {
            let slot = IconSize::beside_slot(&theme, role);
            let point = Drawn::beside(&theme, Symbol::Doc, role).size(slot).point;
            assert!(point >= SYMBOL_FLOOR - 1e-3, "beside {role:?}: {point}");
        }
        let chevron = Drawn::disclosure(&theme, Symbol::ChevronRight);
        assert!(chevron.size(IconSize::Inline.slot(&theme)).point < SYMBOL_FLOOR, "the exception");
    }

    /// An icon takes its words' weight: regular beside 400, medium beside 500, semibold beside
    /// 600; and its words' size, the lead beside the chrome's and the inline beside a fact's.
    #[test]
    fn an_icon_takes_its_words_size_and_weight() {
        let theme = Theme::default();
        let roles = theme.typography.roles(false);
        let weight = |role: TypeRole| Drawn::beside(&theme, Symbol::Doc, role).weight;
        assert_eq!(weight(roles.chrome), Weight::Regular);
        assert_eq!(weight(roles.action), Weight::Medium);
        assert_eq!(weight(roles.section), Weight::Semibold);
        assert_eq!(IconSize::beside(&theme, roles.chrome), IconSize::Lead);
        assert_eq!(IconSize::beside(&theme, roles.metadata), IconSize::Inline);
        let lead = IconSize::Lead;
        assert_eq!(lead.point(&theme), theme.typography.ui_size, "a lead is its title's size");
        let point = |role: TypeRole| {
            Drawn::beside(&theme, Symbol::Doc, role).size(IconSize::beside_slot(&theme, role)).point
        };
        assert!((point(roles.chrome) - lead.point(&theme)).abs() < 1e-3, "the lead, by role");
        assert!((IconSize::beside_slot(&theme, roles.chrome) - lead.slot(&theme)).abs() < 1e-3);
        let finger = theme.typography.roles(true).chrome;
        assert!((point(finger) - finger.size).abs() < 1e-3, "a finger's row: {}", point(finger));
    }

    /// Each status the OS draws has its own symbol, and the three that carry colour are
    /// filled; working's dots and the idle and waiting rings are ours.
    #[test]
    fn each_status_has_its_own_mark() {
        let all = [Status::NeedsYou, Status::Done, Status::Failed, Status::Away];
        let symbols: std::collections::HashSet<_> = all.iter().filter_map(|s| s.symbol()).collect();
        assert_eq!(symbols.len(), all.len());
        assert_eq!(Status::NeedsYou.symbol(), Some(Symbol::ExclamationmarkCircleFill));
        assert_eq!(Status::Done.symbol(), Some(Symbol::CheckmarkCircleFill));
        assert_eq!(Status::Failed.symbol(), Some(Symbol::XmarkCircleFill));
        for ours in [Status::Idle, Status::Working, Status::Running] {
            assert_eq!(ours.symbol(), None, "{ours:?} is drawn by us");
        }
    }

    /// A glyph wears its hue's mark step, and grey where it says nothing needs a look; working
    /// is grey only where colour stays out. A word beside it is neutral but a failure's.
    #[test]
    fn a_status_wears_its_fill_and_its_word_stays_neutral() {
        for theme in
            [Theme::new(slopty_theme::Variant::Dark), Theme::new(slopty_theme::Variant::Light)]
        {
            let s = &theme.surfaces;
            assert_eq!(Status::Working.ink(&theme), s.working_fill);
            assert_eq!(Status::NeedsYou.ink(&theme), s.warn_fill);
            assert_eq!(Status::Done.ink(&theme), s.success_fill);
            assert_eq!(Status::Failed.ink(&theme), s.error_fill);
            assert_eq!(Status::Running.ink(&theme), s.text_muted);
            assert_eq!(Status::Working.quiet_ink(&theme), s.text_muted);
            assert_eq!(Status::NeedsYou.word(&theme), s.text_muted);
            assert_eq!(Status::Failed.word(&theme), s.error);
            assert_eq!(Phase::Merged.ink(&theme), s.merged_fill);
            assert_eq!(Phase::of(crate::project::model::Lane::Verifying), Phase::VERIFYING);
            assert_eq!(Phase::UpNext.with_agent(Status::Running), Phase::Waiting);
        }
    }

    /// Twelve steps make one turn a second, each held a twelfth of a second, and the timer
    /// always waits for the next step's start. Under Reduce Motion the mark stands on its
    /// first step whatever the time, and breathes: whole and faint by turns over the breath,
    /// never under its floor.
    /// The working mark's cell goes round in twelve frames, one a step: three dots lit, then
    /// four as the head moves on, every frame unlike the one before, so a turn reads in a
    /// second with nothing turning.
    #[test]
    fn the_working_cell_goes_round_in_twelve_frames() {
        let count = |lit: [bool; 6]| lit.iter().filter(|on| **on).count();
        let frames: Vec<[bool; 6]> = (0..SPIN_STEPS).map(lit_dots).collect();
        for (step, lit) in frames.iter().enumerate() {
            assert_eq!(count(*lit), if step % 2 == 0 { 3 } else { 4 }, "step {step}");
        }
        for pair in frames.windows(2) {
            assert_ne!(pair.first(), pair.get(1), "each step moves");
        }
        assert_eq!(lit_dots(SPIN_STEPS), lit_dots(0), "a turn is twelve steps");
        assert_eq!(
            lit_dots(0),
            [true, true, false, false, false, true],
            "the first frame is ⠋, never a column"
        );
        assert_eq!(DOT_RING.len(), 6);
    }

    /// The cell's dots are whole device pixels on a whole-pixel pitch with a gap between, at a
    /// row's slot and at a notice's, at 1x and 2x, so each dot is sharp and alike.
    #[test]
    fn the_working_cell_sits_on_whole_pixels() {
        let theme = Theme::default();
        assert_eq!(dot_cell(theme.typography.icon(), 1.0), (2.0, 4.0), "a row at 1x");
        assert_eq!(dot_cell(theme.typography.icon(), 2.0), (5.0, 8.0), "a row at 2x");
        for side in [theme.typography.icon(), theme.typography.icon_large(), 20.0, 8.0] {
            for device in [1.0, 2.0, 3.0] {
                let (dot, pitch) = dot_cell(side, device);
                assert!(dot >= 1.0 && pitch > dot, "{side} pt at {device}x: {dot} {pitch}");
                assert!(dot.fract() == 0.0 && pitch.fract() == 0.0, "{side} at {device}x");
                assert!(2.0_f32.mul_add(pitch, dot) <= (side * device).ceil(), "inside its slot");
            }
        }
    }

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

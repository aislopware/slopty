//! The icons the chrome draws: Tabler's glyphs ([`Symbol`]), drawn by us on whole device pixels
//! in the ink of the words beside them, and a file's type in Material's own colours
//! ([`FileType`]).
//!
//! A [`Symbol`] is one of a closed list. Git is Tabler's git glyphs ([`GitGlyph`]). An agent
//! wears its owner's mark ([`AgentMark`]), drawn by us from the owner's outline into the same
//! kind of mask, and an agent with none the neutral [`AGENT`]. Nothing else is drawn by us but
//! the working mark's cell of dots and the rings of a state ([`Ring`]) (`docs/decisions/ui.md`,
//! "The chrome's icons are Tabler's, and a file's are Material's" and "State is a glyph";
//! `docs/decisions/brand.md`, "Each agent wears its owner's mark").
//!
//! An icon fills its slot: Tabler's 24 grid and Material's view box span the slot's side, as
//! Zed and `MonoCode` set their icons. [`IconSize::Lead`] leads a row or stands alone in
//! [`slopty_theme::Typography::icon_large`] (16 pt); [`IconSize::Inline`] sits in
//! [`slopty_theme::Typography::icon`] (14 pt) beside a row's facts and in bars and buttons. A
//! slot sized again draws its icon larger or smaller with it, so an icon never parts from its
//! words' size.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use gpui::accesskit::Role;
use gpui::{
    AnimationExt as _, AnyElement, App, Bounds, DevicePixels, Div, Element, ElementId, EntityId,
    Global, GlobalElementId, Hsla, InspectorElementId, InteractiveElement as _, IntoElement,
    LayoutId, ParentElement as _, Pixels, RenderImage, ScaledPixels, SharedString, Stateful,
    StatefulInteractiveElement as _, Styled as _, SvgSize, TransformationMatrix, Window, canvas,
    div, point, px, radians,
};
use parking_lot::RwLock;
use slopty_platform::outline::Mask;
use slopty_theme::{Rgb, Theme, TypeRole};

use crate::colors::hsla;
pub use crate::file_types::FileType;

mod git;
mod glyphs;
mod marks;
mod ring;

pub use git::GitGlyph;
pub use glyphs::Symbol;
pub use marks::AgentMark;
pub use ring::Ring;

/// The thread of an agent with no mark of its own ([`AgentMark::Neutral`]), an ACP agent's:
/// one neutral mark in the ink beside it, a conversation, its agent named in words.
pub const AGENT: Symbol = Symbol::TextBubble;

/// What leads a row: a glyph for what a thing is, the mark of the agent it runs, or a file's
/// type.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mark {
    /// A chrome glyph.
    Symbol(Symbol),
    /// An agent's own mark; [`AgentMark::Neutral`] is drawn as [`AGENT`].
    Agent(AgentMark),
    /// A git glyph.
    Git(GitGlyph),
    /// A file's type, in its icon's own colours.
    File(FileType),
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

impl From<FileType> for Mark {
    fn from(file: FileType) -> Self {
        Self::File(file)
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
    /// glyph beside its words says nothing they do not.
    #[must_use]
    pub const fn label(self) -> Option<&'static str> {
        match self {
            Self::Agent(mark) => mark.label(),
            Self::Symbol(_) | Self::Git(_) | Self::File(_) => None,
        }
    }
}

/// The mark for the file at `path`: its type's icon; else the code glyph for code in a
/// language with no icon, or the plain document.
#[must_use]
pub fn file_mark(path: &str) -> Mark {
    FileType::of(path).map_or_else(
        || {
            let glyph = if crate::file_types::code(path) {
                Symbol::ChevronLeftForwardslashChevronRight
            } else {
                Symbol::Doc
            };
            Mark::Symbol(glyph)
        },
        Mark::File,
    )
}

/// The glyph a machine wears, by its form.
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
    /// Beside a row's facts, in bars and in buttons: [`slopty_theme::Typography::icon`]'s slot,
    /// the metadata role's size and the room round it.
    Inline,
    /// A row's lead, or standing alone or beside a title: the chrome role's size in a slot as
    /// much past it as [`slopty_theme::Typography::icon_large`] is past the chrome size. A
    /// finger's chrome is 17 pt, so on touch a lead's slot is 20 pt, as an iOS row's symbol is
    /// its body text's size.
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
        let ty = &theme.typography;
        match self {
            Self::Inline => theme.roles().metadata.size + (ty.icon() - ty.small()),
            Self::Lead => theme.roles().chrome.size + (ty.icon_large() - ty.ui_size),
        }
    }
}

/// How a mark is drawn in a slot.
///
/// A glyph or a file's icon spans `share` of the slot's side, a glyph turned by `turn` radians;
/// an agent's mark has its ink box `ink` of the side; a file's icon is in its variant for a
/// light ground or a dark one.
#[derive(Clone, Copy, Debug)]
pub struct Drawn {
    mark: Mark,
    share: f32,
    ink: f32,
    turn: f32,
    light: bool,
}

impl Drawn {
    /// `mark` in a slot of `size`: a glyph across the whole slot, an agent's mark with its ink
    /// box [`slopty_theme::Spacing::xxs`] short of it.
    #[must_use]
    pub fn new(theme: &Theme, mark: impl Into<Mark>, size: IconSize) -> Self {
        let slot = size.slot(theme);
        Self {
            mark: mark.into(),
            share: 1.0,
            ink: (slot - theme.spacing.xxs) / slot,
            turn: 0.0,
            light: theme.surfaces.ground.is_light(),
        }
    }

    /// `mark` beside words of `role`, in a slot [`IconSize::beside_slot`] square.
    #[must_use]
    pub fn beside(theme: &Theme, mark: impl Into<Mark>, role: TypeRole) -> Self {
        let slot = IconSize::beside_slot(theme, role);
        Self {
            ink: (slot - theme.spacing.xxs) / slot,
            ..Self::new(theme, mark, IconSize::beside(theme, role))
        }
    }

    /// A disclosure chevron's drawing: on the grid of the chrome's smaller size
    /// ([`slopty_theme::Typography::small`], 12 pt) in the inline slot, the size Zed and the
    /// references give a tree's chevron.
    #[must_use]
    pub fn disclosure(theme: &Theme, symbol: Symbol) -> Self {
        Self {
            share: theme.typography.small() / IconSize::Inline.slot(theme),
            ..Self::new(theme, symbol, IconSize::Inline)
        }
    }

    /// An empty state's mark, across [`crate::kit::NOTICE_MARK`]; an agent's mark with its ink
    /// box two [`slopty_theme::Spacing::xxs`] short of the slot.
    #[must_use]
    pub fn notice(theme: &Theme, mark: impl Into<Mark>) -> Self {
        let slot = crate::kit::NOTICE_MARK;
        Self {
            ink: theme.spacing.xxs.mul_add(-2.0, slot) / slot,
            ..Self::new(theme, mark, IconSize::Lead)
        }
    }

    /// The ink box an agent's mark is drawn with in a slot `side` points square, in points.
    #[must_use]
    pub fn ink(self, side: f32) -> f32 {
        side * self.ink
    }

    /// The side in points of the grid a glyph is drawn on in a slot `side` points square.
    #[must_use]
    pub fn grid(self, side: f32) -> f32 {
        side * self.share
    }

    /// Turned by `radians` about the slot's centre, while it moves; at rest it is upright, so
    /// its pixels stay the screen's. Only a glyph turns.
    #[must_use]
    pub const fn turned(mut self, radians: f32) -> Self {
        self.turn = radians;
        self
    }

    /// The slot, `side` square, its mark in `color`. The slot can be sized again
    /// (`.size(..)`), and the mark is drawn larger or smaller by as much; a glyph's ink is the
    /// slot's text colour, so `.text_color(..)` recolours it. A file's icon keeps its colours.
    #[must_use]
    pub fn slot(self, side: Pixels, color: Hsla) -> Div {
        div().flex_none().size(side).text_color(color).child(
            canvas(|_, _, _| {}, move |bounds, (), window, cx| self.paint(bounds, window, cx))
                .size_full(),
        )
    }

    /// Paints the mark centred in `bounds`.
    fn paint(self, bounds: Bounds<Pixels>, window: &mut Window, cx: &App) {
        match self.mark {
            Mark::Symbol(symbol) => self.paint_glyph(symbol, bounds, window),
            Mark::Agent(AgentMark::Neutral) => self.paint_glyph(AGENT, bounds, window),
            Mark::Agent(mark) => self.paint_agent(mark, bounds, window),
            Mark::Git(glyph) => self.paint_glyph(glyph.symbol(), bounds, window),
            Mark::File(file) => self.paint_file(file, bounds, window, cx),
        }
    }

    /// How many device pixels a grid takes in `bounds` on this window's display.
    fn pixels(self, bounds: Bounds<Pixels>, window: &Window) -> u32 {
        let side = f32::from(bounds.size.width.min(bounds.size.height));
        whole_pixels(self.grid(side) * window.scale_factor())
    }

    /// Paints a glyph with its grid centred in `bounds`, on the device's pixel grid, turned
    /// about the centre while it turns.
    fn paint_glyph(self, symbol: Symbol, bounds: Bounds<Pixels>, window: &mut Window) {
        let Some((mask, key)) = glyphs::mask(symbol, self.pixels(bounds, window)) else {
            return;
        };
        let turn = if self.turn == 0.0 {
            TransformationMatrix::unit()
        } else {
            let at = bounds.center().scale(window.scale_factor());
            TransformationMatrix::unit()
                .translate(at)
                .rotate(radians(self.turn))
                .translate(gpui::Point::new(ScaledPixels(-at.x.0), ScaledPixels(-at.y.0)))
        };
        paint_centred(&mask, key, bounds, turn, window, || symbol.name().to_owned());
    }

    /// Paints an agent's mark centred on its ink in `bounds`, on the device's pixel grid: its
    /// mask is its ink box, so the box is centred and its origin rounded to a whole pixel.
    /// Never weighted: a filled silhouette, as the owner draws it.
    fn paint_agent(self, mark: AgentMark, bounds: Bounds<Pixels>, window: &mut Window) {
        let side = f32::from(bounds.size.width.min(bounds.size.height));
        let device = window.scale_factor();
        let Some((mask, key)) = marks::mask(mark, self.ink(side), device) else {
            return;
        };
        let unit = TransformationMatrix::unit();
        paint_centred(&mask, key, bounds, unit, window, || format!("{mark:?}"));
    }

    /// Paints a file's icon in its colours with its view box centred in `bounds`, drawn at the
    /// device's size, its origin on a whole device pixel.
    fn paint_file(self, file: FileType, bounds: Bounds<Pixels>, window: &mut Window, cx: &App) {
        let pixels = self.pixels(bounds, window);
        let Some(image) = file_image(file, self.light, pixels, cx) else {
            return;
        };
        let device = window.scale_factor();
        #[expect(clippy::cast_precision_loss, reason = "an icon is a few dozen pixels")]
        let extent = pixels as f32;
        let at = place(bounds, extent, extent, device);
        let size = px(extent / device);
        let image_bounds = Bounds { origin: at, size: gpui::size(size, size) };
        let painted = window.paint_image(
            image_bounds,
            image_bounds,
            gpui::Corners::default(),
            image,
            0,
            false,
        );
        if let Err(error) = painted {
            tracing::warn!(%error, file = file.name(), "a file's icon was not painted");
        }
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
/// device pixel, transformed by `turn`; `what` names it if it fails.
fn paint_centred(
    mask: &Arc<Mask>,
    key: SharedString,
    bounds: Bounds<Pixels>,
    turn: TransformationMatrix,
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
        window.paint_mask(origin, devices, key, turn, ink, || Ok(Some(mask.alpha.clone())));
    if let Err(error) = painted {
        tracing::warn!(%error, mark = what(), "a mark was not painted");
    }
}

/// A drawn mask with the key the atlas keeps it under.
type Kept = Option<(Arc<Mask>, SharedString)>;

/// A file's icon by type, ground and side in device pixels, drawn or a miss.
type FileImages = HashMap<(FileType, bool, u32), Option<Arc<RenderImage>>>;

/// The file icons drawn so far; one that does not draw is kept as a miss, said once.
static FILE_IMAGES: LazyLock<RwLock<FileImages>> = LazyLock::new(|| RwLock::new(HashMap::new()));

/// `file`'s icon for a `light` ground or a dark one, its view box `pixels` device pixels
/// square: drawn by GPUI's SVG renderer at that size and no other, so nothing is resampled.
fn file_image(file: FileType, light: bool, pixels: u32, cx: &App) -> Option<Arc<RenderImage>> {
    if pixels == 0 {
        return None;
    }
    let at = (file, light, pixels);
    if let Some(kept) = FILE_IMAGES.read().get(&at) {
        return kept.clone();
    }
    let side = DevicePixels(i32::try_from(pixels).ok()?);
    let renderer = cx.svg_renderer();
    let drawn = renderer
        .parse_svg(file.svg(light).as_bytes())
        .and_then(|svg| renderer.render_parsed(&svg, SvgSize::Size(gpui::size(side, side))))
        .inspect_err(
            |error| tracing::warn!(%error, file = file.name(), "a file's icon did not draw"),
        )
        .ok();
    FILE_IMAGES.write().entry(at).or_insert(drawn).clone()
}

/// The display scales a window of this platform is likely shown at.
const SCALES: &[f32] = if cfg!(target_os = "ios") { &[2.0, 3.0] } else { &[1.0, 2.0] };

/// Reads every glyph, and draws the glyphs at a row's and a lead's slot and the agents' marks
/// at theirs, on a background thread while the first window is made. Start it before the first
/// window.
///
/// A glyph costs microseconds to draw, but the first frame would otherwise pay to read every
/// glyph's file and Core Graphics' first context, about 6 ms together (`docs/MEASUREMENTS.md`,
/// "Tabler glyphs drawn on demand"). Whatever it has not drawn when a frame asks is drawn then.
pub fn prewarm(theme: &Theme) {
    let sides: Vec<u32> = [IconSize::Inline, IconSize::Lead]
        .iter()
        .flat_map(|size| SCALES.iter().map(move |scale| (size.slot(theme), scale)))
        .map(|(slot, scale)| whole_pixels(slot * scale))
        .collect();
    let inks = [
        Drawn::new(theme, AGENT, IconSize::Lead).ink(IconSize::Lead.slot(theme)),
        Drawn::new(theme, AGENT, IconSize::Inline).ink(IconSize::Inline.slot(theme)),
        Drawn::notice(theme, AGENT).ink(crate::kit::NOTICE_MARK),
    ];
    let warmed = std::thread::Builder::new().name("icons-prewarm".into()).spawn(move || {
        for &symbol in Symbol::ALL {
            for &side in &sides {
                glyphs::mask(symbol, side);
            }
        }
        for mark in AgentMark::OWNED {
            for (&ink, &scale) in inks.iter().flat_map(|ink| SCALES.iter().map(move |s| (ink, s))) {
                marks::mask(mark, ink, scale);
            }
        }
    });
    if let Err(error) = warmed {
        tracing::warn!(%error, "the icons' prewarm did not start");
    }
}

/// `symbol` at `size`, in `color`: a slot of the size's side.
#[must_use]
pub fn icon(theme: &Theme, symbol: impl Into<Mark>, size: IconSize, color: Hsla) -> Div {
    Drawn::new(theme, symbol, size).slot(px(size.slot(theme)), color)
}

/// `mark` beside words of `role`, in `ink`, in a slot of [`IconSize::beside_slot`]; a slot
/// sized again draws the mark at its size.
#[must_use]
pub fn beside(theme: &Theme, mark: impl Into<Mark>, role: TypeRole, ink: Hsla) -> Div {
    Drawn::beside(theme, mark, role).slot(px(IconSize::beside_slot(theme, role)), ink)
}

/// `symbol` in a slot `side` square, in `ink`: a row's lead at whatever size.
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

/// `status` in a fixed square slot, the width of a large icon.
///
/// Rows that carry a mark and rows that do not keep their titles on one edge. A mark is an
/// image named by [`Status::label`]; an empty slot is nothing to a screen reader.
#[must_use]
pub fn status_mark(theme: &Theme, status: Option<Status>) -> Stateful<Div> {
    let slot = div()
        .id("status")
        .flex_shrink_0()
        .size(px(theme.typography.icon_large()))
        .flex()
        .items_center()
        .justify_center();
    match status {
        Some(Status::Done) => {
            slot.role(Role::Image).aria_label(Status::Done.label()).child(Arrive {
                child: Some(div().flex_none().child(status_icon(
                    theme,
                    Status::Done,
                    px(theme.typography.icon()),
                    hsla(Status::Done.ink(theme)),
                ))),
                inner: None,
            })
        }
        Some(status) => slot.role(Role::Image).aria_label(status.label()).child(status_icon(
            theme,
            status,
            px(theme.typography.icon()),
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

/// `status` as an empty state's mark, in `color`.
///
/// Its symbol is drawn as [`Drawn::notice`] in the notice's slot, and the working cell or a
/// ring at the heading's size, so a tile's state reads at the size of the notice it heads.
#[must_use]
pub fn notice_status(theme: &Theme, status: Status, color: Hsla) -> AnyElement {
    match status.symbol() {
        Some(symbol) => {
            Drawn::notice(theme, symbol).slot(px(crate::kit::NOTICE_MARK), color).into_any_element()
        }
        None => status_icon(theme, status, px(theme.roles().page_heading.size), color),
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

    /// A file leads with its type's icon; code with no icon kept leads with the code glyph,
    /// and a type neither knows with the plain document. Every agent's thread is the one
    /// neutral glyph.
    #[test]
    fn a_file_leads_with_its_types_icon() {
        assert_eq!(file_mark("/w/main.rs"), Mark::File(FileType::Rust));
        assert_eq!(file_mark("/w/README.md"), Mark::File(FileType::Readme));
        assert_eq!(file_mark("/w/q.graphql"), Symbol::ChevronLeftForwardslashChevronRight.into());
        assert_eq!(file_mark("/w/notes"), Mark::Symbol(Symbol::Doc));
        assert_eq!(AGENT, Symbol::TextBubble, "a conversation, not the cliched sparkles");
    }

    /// An icon fills its slot, the ladder's 14 pt inline and 16 pt lead, a lead beside the
    /// chrome's words and an inline one beside a fact's; a disclosure chevron is on the 12 pt
    /// grid in the inline slot.
    #[test]
    fn an_icon_fills_its_slot_on_the_ladder() {
        let theme = Theme::default();
        let roles = theme.typography.roles(false);
        let (inline, lead) = (IconSize::Inline.slot(&theme), IconSize::Lead.slot(&theme));
        assert!((inline - 14.0).abs() < 1e-3 && (lead - 16.0).abs() < 1e-3, "{inline} {lead}");
        let grid = Drawn::new(&theme, Symbol::Doc, IconSize::Lead).grid(lead);
        assert!((grid - lead).abs() < 1e-3, "a glyph's grid is its slot");
        assert_eq!(IconSize::beside(&theme, roles.chrome), IconSize::Lead);
        assert_eq!(IconSize::beside(&theme, roles.metadata), IconSize::Inline);
        let chevron = Drawn::disclosure(&theme, Symbol::ChevronRight).grid(inline);
        assert!((chevron - 12.0).abs() < 1e-3, "the chevron's grid: {chevron}");
        assert_eq!(whole_pixels(14.0 * 2.0), 28);
        assert_eq!(whole_pixels(f32::NAN), 0);
    }

    /// A file's icon draws at the size it is painted at and no other, in its own colours, and
    /// a light ground takes the light variant where there is one.
    #[gpui::test]
    fn a_file_icon_is_drawn_at_its_size_in_colour(cx: &gpui::TestAppContext) {
        cx.update(|cx| {
            for file in FileType::ALL {
                let image = file_image(*file, false, 28, cx);
                let image = image.unwrap_or_else(|| panic!("{file:?} draws"));
                assert_eq!(image.size(0), gpui::size(DevicePixels(28), DevicePixels(28)));
                let bytes = image.as_bytes(0).unwrap_or_default();
                let coloured =
                    bytes.chunks(4).any(|p| p.iter().take(3).any(|c| Some(c) != p.first()));
                assert!(coloured, "{file:?} is drawn in colour");
            }
            let dark = file_image(FileType::Toml, false, 16, cx);
            let light = file_image(FileType::Toml, true, 16, cx);
            let (Some(dark), Some(light)) = (dark, light) else { panic!("toml draws") };
            assert_ne!(dark.as_bytes(0), light.as_bytes(0), "toml's light variant");
        });
    }

    /// Each status a glyph marks has its own, and the three that carry colour are filled;
    /// working's dots and the idle and waiting rings are drawn as marks of their own.
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

    /// A mark is an image named by its status; an empty slot is nothing to a
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
                    .child(div().id("a").child(status_mark(&theme, Some(Status::NeedsYou))))
                    .child(div().id("b").child(status_mark(&theme, Some(Status::Working))))
                    .child(div().id("c").child(status_mark(&theme, None)))
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
            div().children(self.shown.then(|| status_mark(&theme, Some(self.status))))
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

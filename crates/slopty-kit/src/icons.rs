//! The icons the chrome draws: IBM Carbon's glyphs ([`Symbol`]) and a file's type
//! ([`FileType`], Material's shapes), each drawn on whole device pixels in one ink, the words'
//! beside them, as Zed draws its icons.
//!
//! A [`Symbol`] is one of a closed list. Git is Carbon's git glyphs ([`GitGlyph`]). An agent
//! wears its owner's mark ([`AgentMark`]), drawn by us from the owner's outline into the same
//! kind of mask, and an agent with none the neutral [`AGENT`]. Nothing else is drawn by us but
//! the marks of a state ([`Ring`]) (`docs/decisions/ui.md`,
//! "The chrome's icons are IBM Carbon's at 16" and "State is a glyph";
//! `docs/decisions/brand.md`, "Each agent wears its owner's mark").
//!
//! An icon fills its slot: Carbon's 32 grid and Material's view box span the slot's side. Every
//! icon stands in one slot, [`slopty_theme::Typography::icon`] (16 pt), where Carbon's two-unit
//! line is one whole device pixel at 1x and two at 2x, beside a row's words, in bars and in
//! buttons alike, as Zed sets its icons; only a disclosure's chevron is drawn smaller
//! ([`Drawn::disclosure`]). A slot sized again draws its icon larger or smaller with it.

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
use slopty_theme::{Rgb, Theme};

use crate::colors::hsla;
pub use crate::file_types::FileType;

mod git;
mod glyphs;
mod marks;
mod ring;

pub use git::GitGlyph;
pub use glyphs::Symbol;
pub use marks::AgentMark;
pub use ring::{Knock, Ring};

/// The thread of an agent with no mark of its own ([`AgentMark::Neutral`]), an ACP agent's:
/// one neutral mark in the ink beside it, Carbon's agent, its agent named in words.
pub const AGENT: Symbol = Symbol::AiAgent;

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
    /// A file's type, its icon's silhouette in the ink beside it.
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

/// The mark for the file at `path`: the code glyph for code in a language with no icon (asked
/// first, so `CMakeLists.txt` is code and not text); else its type's icon, or the plain
/// document.
#[must_use]
pub fn file_mark(path: &str) -> Mark {
    if crate::file_types::code(path) {
        return Mark::Symbol(Symbol::ChevronLeftForwardslashChevronRight);
    }
    FileType::of(path).map_or(Mark::Symbol(Symbol::Doc), Mark::File)
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

/// How a mark is drawn in a slot.
///
/// A glyph or a file's icon spans `share` of the slot's side, a glyph turned by `turn` radians;
/// an agent's mark has its ink box `ink` of the side.
#[derive(Clone, Copy, Debug)]
pub struct Drawn {
    mark: Mark,
    share: f32,
    ink: f32,
    turn: f32,
}

impl Drawn {
    /// `mark` in the icon slot: a glyph across the whole slot, an agent's mark with its ink
    /// box [`slopty_theme::Spacing::xxs`] short of it.
    #[must_use]
    pub fn new(theme: &Theme, mark: impl Into<Mark>) -> Self {
        let slot = theme.typography.icon();
        Self { mark: mark.into(), share: 1.0, ink: (slot - theme.spacing.xxs) / slot, turn: 0.0 }
    }

    /// A disclosure chevron's drawing: on the grid of the chrome's smaller size
    /// ([`slopty_theme::Typography::small`], 12 pt) in the icon slot, Zed's extra-small icon.
    #[must_use]
    pub fn disclosure(theme: &Theme, symbol: Symbol) -> Self {
        Self {
            share: theme.typography.small() / theme.typography.icon(),
            ..Self::new(theme, symbol)
        }
    }

    /// An empty state's mark, across [`crate::kit::NOTICE_MARK`]; an agent's mark with its ink
    /// box two [`slopty_theme::Spacing::xxs`] short of the slot.
    #[must_use]
    pub fn notice(theme: &Theme, mark: impl Into<Mark>) -> Self {
        let slot = crate::kit::NOTICE_MARK;
        Self { ink: theme.spacing.xxs.mul_add(-2.0, slot) / slot, ..Self::new(theme, mark) }
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
    /// slot's text colour, so `.text_color(..)` recolours it, a file's icon too.
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

    /// Paints a file's icon as one silhouette in the text's ink, its view box centred in
    /// `bounds`, drawn at the device's size, its origin on a whole device pixel.
    fn paint_file(self, file: FileType, bounds: Bounds<Pixels>, window: &mut Window, cx: &App) {
        let Some((mask, key)) = file_mask(file, self.pixels(bounds, window), cx) else {
            return;
        };
        let unit = TransformationMatrix::unit();
        paint_centred(&mask, key, bounds, unit, window, || file.name().to_owned());
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

/// The file icons drawn so far, by type and side in device pixels; one that does not draw is
/// kept as a miss, said once.
static FILE_MASKS: LazyLock<RwLock<HashMap<(FileType, u32), Kept>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// `file`'s icon in its own colours, its view box `pixels` device pixels square: drawn by
/// GPUI's SVG renderer at that size and no other, so nothing is resampled.
fn file_render(file: FileType, pixels: u32, cx: &App) -> Option<Arc<RenderImage>> {
    let side = DevicePixels(i32::try_from(pixels).ok()?);
    let renderer = cx.svg_renderer();
    renderer
        .parse_svg(file.svg().as_bytes())
        .and_then(|svg| renderer.render_parsed(&svg, SvgSize::Size(gpui::size(side, side))))
        .inspect_err(
            |error| tracing::warn!(%error, file = file.name(), "a file's icon did not draw"),
        )
        .ok()
}

/// `file`'s icon as its silhouette, `pixels` device pixels square: the coverage of its drawing,
/// its colours dropped, so it is painted in the ink beside it as a glyph is.
fn file_mask(file: FileType, pixels: u32, cx: &App) -> Kept {
    if pixels == 0 {
        return None;
    }
    let at = (file, pixels);
    if let Some(kept) = FILE_MASKS.read().get(&at) {
        return kept.clone();
    }
    let kept = file_render(file, pixels, cx).and_then(|image| {
        let size = image.size(0);
        let alpha = image.as_bytes(0)?.as_chunks::<4>().0.iter().map(|&[.., a]| a).collect();
        let mask = Mask {
            width: u32::try_from(size.width.0).ok()?,
            height: u32::try_from(size.height.0).ok()?,
            alpha,
        };
        Some((Arc::new(mask), SharedString::from(format!("file:{}:{pixels}", file.name()))))
    });
    FILE_MASKS.write().entry(at).or_insert(kept).clone()
}

/// The display scales a window of this platform is likely shown at.
const SCALES: &[f32] = if cfg!(target_os = "ios") { &[2.0, 3.0] } else { &[1.0, 2.0] };

/// Reads every glyph, and draws the glyphs at the icon slot and the agents' marks at theirs, on
/// a background thread while the first window is made. Start it before the first window.
///
/// A glyph costs microseconds to draw, but the first frame would otherwise pay to read every
/// glyph's file and Core Graphics' first context, about 6 ms together (`docs/MEASUREMENTS.md`,
/// "Carbon glyphs drawn on demand"). Whatever it has not drawn when a frame asks is drawn then.
pub fn prewarm(theme: &Theme) {
    let slot = theme.typography.icon();
    let sides: Vec<u32> = SCALES.iter().map(|scale| whole_pixels(slot * scale)).collect();
    let inks = [
        Drawn::new(theme, AGENT).ink(slot),
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

/// `symbol` in the icon slot, in `color`; a slot sized again draws the mark at its size.
#[must_use]
pub fn icon(theme: &Theme, symbol: impl Into<Mark>, color: Hsla) -> Div {
    Drawn::new(theme, symbol).slot(px(theme.typography.icon()), color)
}

/// `symbol` in a slot `side` square, in `ink`: a row's lead at whatever size.
#[must_use]
pub fn symbol(theme: &Theme, symbol: impl Into<Mark>, side: Pixels, ink: Hsla) -> AnyElement {
    Drawn::new(theme, symbol).slot(side, ink).into_any_element()
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
    /// The mark drawn by us for it ([`status_icon`]): a ring for idle, a dashed ring for
    /// waiting, the turning arc for working, and a disc with its figure knocked out for the
    /// three that carry colour, so the colour has a body at 1x. `None` for away, which is
    /// Carbon's glyph ([`Self::symbol`]).
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

/// `status` in a fixed square slot, the icon slot.
///
/// Rows that carry a mark and rows that do not keep their titles on one edge. A mark is an
/// image named by [`Status::label`]; an empty slot is nothing to a screen reader.
#[must_use]
pub fn status_mark(theme: &Theme, status: Option<Status>) -> Stateful<Div> {
    let slot = div()
        .id("status")
        .flex_shrink_0()
        .size(px(theme.typography.icon()))
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
/// Its symbol is drawn as [`Drawn::notice`] in the notice's slot, and a mark of ours at the
/// heading's size, so a tile's state reads at the size of the notice it heads.
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
        match self {
            Self::NeedsYou => Ring::Disc(Knock::Bang).draw(side, ink),
            Self::Failed => Ring::Disc(Knock::Cross).draw(side, ink),
            Self::Working => status_icon(theme, Status::Working, side, ink),
            Self::Waiting => Ring::Dashed.draw(side, ink),
            Self::UpNext => Ring::Empty.draw(side, ink),
            Self::Verifying(share) => Ring::Pie(share).draw(side, ink),
            Self::ReadyToMerge => Ring::Checked.draw(side, ink),
            Self::Merged => Ring::Disc(Knock::Check).draw(side, ink),
        }
    }
}

/// How long after `since` the next whole second begins: when a readout of seconds changes.
#[must_use]
pub fn until_next_second(since: Duration) -> Duration {
    Duration::from_secs(1).saturating_sub(Duration::from_nanos(u64::from(since.subsec_nanos())))
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
    /// How long after `epoch` the marks were last woken. Every mark draws the step
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

/// A typed key waits for its echo, at most until `until`: steps of the working mark that fall
/// due meanwhile wait for the echo's frame (or `until`), rather than draw a frame of their own.
pub fn hold_steps(cx: &mut App, until: Instant) {
    let clock = SpinClock::get(cx);
    clock.hold_until = Some(clock.hold_until.map_or(until, |held| held.max(until)));
}

/// Until when the working marks' steps are held for a typed key's echo, if they are.
#[cfg(any(test, feature = "e2e"))]
pub fn steps_held_until(cx: &mut App) -> Option<Instant> {
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

/// How long after the spin clock's start every working mark was last woken: the moment each
/// drawing that steps with the marks (a busy progress bar's breath) shows, so they step
/// together.
pub(crate) fn steps_shown(cx: &mut App) -> Duration {
    SpinClock::get(cx).shown
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
    if clock.armed.is_some() || clock.pinned.is_some() {
        return;
    }
    let wait = until_next(now.saturating_duration_since(clock.epoch), SPIN_STEP);
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
        let cmake = file_mark("/w/CMakeLists.txt");
        assert_eq!(cmake, Symbol::ChevronLeftForwardslashChevronRight.into(), "code, not text");
        assert_eq!(AGENT, Symbol::AiAgent, "Carbon's agent, not the cliched sparkles");
    }

    /// An icon fills its slot, 16 pt, where Carbon's 32 grid lands on whole device pixels at
    /// 1x and 2x, on a pointer and a finger alike; a disclosure chevron is on the 12 pt grid
    /// in the same slot.
    #[test]
    fn an_icon_fills_its_slot_of_sixteen() {
        for touch in [false, true] {
            let mut theme = Theme::default();
            if touch {
                theme.density = slopty_theme::Density::TOUCH;
            }
            let slot = theme.typography.icon();
            assert!((slot - 16.0).abs() < 1e-3, "touch {touch}: {slot}");
            let grid = Drawn::new(&theme, Symbol::Doc).grid(slot);
            assert!((grid - slot).abs() < 1e-3, "a glyph's grid is its slot");
            let chevron = Drawn::disclosure(&theme, Symbol::ChevronRight).grid(slot);
            assert!((chevron - 12.0).abs() < 1e-3, "the chevron's grid: {chevron}");
        }
        assert_eq!(whole_pixels(16.0 * 2.0), 32);
        assert_eq!(whole_pixels(f32::NAN), 0);
    }

    /// A file's icon draws at the size it is painted at and no other, as one silhouette.
    /// A lint as a test: no kept icon loses its figure in one ink, so none leans on a second
    /// colour drawn over its first. A colour edge inside the silhouette is a figure the ink
    /// would lose; Bun's face, Dart's, `CMake`'s and Protobuf's lost a third or more of theirs
    /// against their outline and went (`docs/decisions/ui.md`, "The chrome's icons are IBM
    /// Carbon's at 16").
    #[gpui::test]
    fn a_file_icon_is_one_silhouette_at_its_size(cx: &gpui::TestAppContext) {
        cx.update(|cx| {
            for file in FileType::ALL {
                let (mask, _) = file_mask(*file, 28, cx).unwrap_or_else(|| panic!("{file:?}"));
                assert_eq!((mask.width, mask.height), (28, 28), "{file:?}");
                assert!(mask.alpha.iter().any(|a| *a > 0), "{file:?} has ink");
                let image = file_render(*file, 32, cx).unwrap_or_else(|| panic!("{file:?}"));
                let lost = lost_figure(image.as_bytes(0).unwrap_or_default(), 32);
                assert!(lost < 0.2, "{file:?} loses {lost:.2} of its figure in one ink");
            }
        });
    }

    /// The share of a drawing's figure one ink loses: its colour edges between two solid
    /// pixels, against the edges of its silhouette (BGRA, `side` pixels to a row).
    fn lost_figure(bgra: &[u8], side: usize) -> f32 {
        let pixels: Vec<[u8; 4]> = bgra.as_chunks::<4>().0.to_vec();
        let rows: Vec<&[[u8; 4]]> = pixels.chunks(side).collect();
        let across = rows.iter().flat_map(|row| row.iter().zip(row.iter().skip(1)));
        let down = rows.iter().zip(rows.iter().skip(1)).flat_map(|(a, b)| a.iter().zip(b.iter()));
        let pairs: Vec<(&[u8; 4], &[u8; 4])> = across.chain(down).collect();
        let outline = pairs.iter().filter(|(p, n)| p[3].abs_diff(n[3]) > 128).count();
        let inner = pairs
            .iter()
            .filter(|(p, n)| {
                let apart = p.iter().zip(n.iter()).take(3);
                let apart = apart.map(|(c, d)| f32::from(c.abs_diff(*d)).powi(2)).sum::<f32>();
                p[3] > 230 && n[3] > 230 && apart.sqrt() > 80.0
            })
            .count();
        #[expect(clippy::cast_precision_loss, reason = "a few hundred edges")]
        let share = inner as f32 / outline.max(1) as f32;
        share
    }

    /// Each status has its own mark, the three that carry colour a disc with their own figure
    /// knocked out; only away is a glyph, Carbon's.
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
            assert_eq!(Phase::UpNext.with_agent(Status::Running), Phase::Waiting);
        }
    }

    /// Two dozen steps make one turn in two seconds, each held a twelfth of a second, and the
    /// timer always waits for the next step's start. Under Reduce Motion the mark stands on its
    /// first step whatever the time.
    #[test]
    fn the_working_mark_steps_two_dozen_times_a_turn_and_stands_under_reduce_motion() {
        let ms = Duration::from_millis;
        assert_eq!(spin_step(ms(0), false), 0);
        assert_eq!(spin_step(ms(80), false), 0, "still the first step");
        assert_eq!(spin_step(ms(90), false), 1);
        assert_eq!(spin_step(ms(1_990), false), 23);
        assert_eq!(spin_step(ms(2_000), false), 0, "one turn in two seconds");
        assert_eq!(spin_step(ms(2_500), false), 6);
        for at in [0, 90, 500, 990, 12_345] {
            assert_eq!(spin_step(ms(at), true), 0, "Reduce Motion at {at} ms");
        }
        assert_eq!(until_next_step(ms(0)), SPIN_STEP, "on a step's start, a whole step away");
        assert_eq!(until_next_step(ms(80)), SPIN_STEP.saturating_sub(ms(80)));
        let next = ms(90).saturating_add(until_next_step(ms(90)));
        assert_eq!(spin_step(next, false), 2, "the timer lands on the next step");
        let turn = SPIN_STEP.checked_mul(SPIN_STEPS).unwrap_or_default();
        assert!(ms(2_000).saturating_sub(turn) < Duration::from_micros(1), "{turn:?}");
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

    /// A turning mark draws the step of the clock's last wake, not of the moment it is drawn:
    /// with a step held back for a key's echo, a mark drawn anew (a frame from scratch) shows
    /// the step the frame shown does, and the wake that ends the hold moves both.
    #[gpui::test]
    fn a_step_held_back_is_drawn_from_scratch_as_it_shows(cx: &mut gpui::TestAppContext) {
        let (view, cx) =
            cx.add_window_view(|_, _| Turning { shown: true, status: Status::Working, renders: 0 });
        cx.run_until_parked();
        let drawn = cx.update(|_w, cx| SpinClock::drawn(cx));
        let held = cx.executor().now().checked_add(SPIN_STEP.saturating_mul(3));
        cx.update(|_w, cx| hold_steps(cx, held.unwrap_or_else(|| cx.background_executor().now())));
        let before = view.read_with(cx, |v, _| v.renders);
        cx.executor().advance_clock(SPIN_STEP.saturating_add(SPIN_STEP.div_f32(2.0)));
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |v, _| v.renders), before, "the step waits on the echo");
        assert_eq!(cx.update(|_w, cx| SpinClock::drawn(cx)), drawn, "drawn anew as it shows");
        cx.update(|_w, cx| release_steps(cx));
        cx.run_until_parked();
        assert!(view.read_with(cx, |v, _| v.renders) > before, "the echo's frame moves it");
        assert_ne!(cx.update(|_w, cx| SpinClock::drawn(cx)), drawn, "a step further on");
    }

    /// Pinned, every mark stands on its first step whenever it is drawn, and wakes nothing;
    /// let go, it turns again. Under Reduce Motion it stands still and wakes nothing at all.
    #[gpui::test]
    fn a_pinned_or_still_mark_wakes_nothing(cx: &mut gpui::TestAppContext) {
        assert_eq!(spin_step(PINNED_STEPS, false), 0, "on its first step");
        let (view, cx) =
            cx.add_window_view(|_, _| Turning { shown: true, status: Status::Working, renders: 0 });
        cx.run_until_parked();
        cx.update(|_w, cx| pin_steps(Some(PINNED_STEPS), cx));
        cx.run_until_parked();
        assert_eq!(cx.update(|_w, cx| SpinClock::drawn(cx)), 0, "the pinned moment");
        assert!(renders_over(&view, cx, Duration::from_secs(1)) <= 1, "the step already due");
        assert_eq!(renders_over(&view, cx, Duration::from_secs(1)), 0, "pinned, nothing wakes");
        cx.update(|_w, cx| pin_steps(None, cx));
        cx.run_until_parked();
        assert!(renders_over(&view, cx, Duration::from_secs(1)) >= 11, "let go, it turns");
        cx.update(|_w, cx| cx.set_reduce_motion(true));
        view.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        assert!(renders_over(&view, cx, Duration::from_secs(1)) <= 1, "the step already due");
        assert_eq!(renders_over(&view, cx, Duration::from_secs(1)), 0, "still, nothing wakes");
    }

    /// The mark wakes the view it was painted in once a step while it shows, and not once it
    /// is gone.
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

//! `TerminalElement`: paints a `TermState` as cell-aligned text runs and quads.
//!
//! Per row: the words (a row split at its plain spaces), each shaped once at the base font
//! size and painted glyph by glyph at the zoomed size, background quads for non-default
//! backgrounds, underlines and strikethroughs where the font puts them, the cell-drawn sprites
//! from the atlas, and the cursor. Shaping is cached across frames and views by the content
//! hash of each word, least recently used first out under a glyph budget, and the cache is
//! independent of the zoom: a zoom step re-shapes nothing, it repaints the same glyph ids at
//! another size (positions come from the cell grid, and a fixed-pitch font's advances scale
//! with the size).

use std::collections::VecDeque;
use std::collections::hash_map::Entry;
use std::hash::{Hash as _, Hasher as _};
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    App, BorderStyle, BorrowAppContext as _, Bounds, Corners, DispatchPhase, Edges, Element,
    ElementId, ElementInputHandler, Entity, Focusable as _, Font, FontId, GlobalElementId, GlyphId,
    Hitbox, HitboxBehavior, Hsla, InspectorElementId, IntoElement, LayoutId, LongPressEvent,
    MouseExitEvent, MouseMoveEvent, PathBuilder, Pixels, Point, RenderImage, ShapedLine,
    SharedString, Size, Style, TextAlign, TextRun, TouchPhase, TransformationMatrix,
    UnderlineStyle, Window, fill, point, px, quad, relative, size,
};
use rustc_hash::{FxHashMap, FxHasher};
use slopty_grid::{Cell, CellWidth, CursorShape, Style as CellStyle, StyleFlags, Underline};
use slopty_predict::Prediction;
use slopty_proto::terminal::{Placement, TermSize};
use slopty_theme::{Colors, Rgb, Theme, alpha};

use crate::colors::{hsla, hsla_alpha};
use crate::fonts;
use crate::terminal::metrics::{self, Grid};
use crate::terminal::sprite;
use crate::terminal::view::TerminalView;

/// Cell geometry for one layout.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct CellMetrics {
    /// Content origin in window coordinates.
    pub origin: Point<Pixels>,
    /// Advance of one cell.
    pub cell_width: Pixels,
    /// Height of one row.
    pub line_height: Pixels,
    /// Grid size that fits.
    pub cols: u16,
    /// Grid size that fits.
    pub rows: u16,
    /// Device pixels of the *fitted* grid per logical point painted: `scale / zoom`.
    ///
    /// [`Self::pixel_at`] reports in the units the worker measures in — the cell size in
    /// `TermSize::metrics`, which is whole device pixels of the unzoomed grid — so a pixel
    /// mouse report lands on the cell the pointer is actually over.
    pub pixel_scale: f32,
    /// The face the grid was derived from, in device pixels at `face_size` (the unzoomed font
    /// size times the display scale): what the font said, `None` where it said nothing and
    /// ghostty's estimate stood in. For the self-test dump.
    pub face: metrics::Face,
    /// Device pixels per em the face was measured at.
    pub face_size: f32,
}

impl CellMetrics {
    /// The cell under a window position.
    #[must_use]
    pub fn cell_at(&self, pos: Point<Pixels>) -> Option<(u16, u16)> {
        let x = f32::from(pos.x - self.origin.x);
        let y = f32::from(pos.y - self.origin.y);
        if x < 0.0 || y < 0.0 {
            return None;
        }
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "checked ≥ 0")]
        let (col, row) = (
            (x / f32::from(self.cell_width)).floor() as u16,
            (y / f32::from(self.line_height)).floor() as u16,
        );
        (col < self.cols && row < self.rows).then_some((col, row))
    }

    /// The nearest cell to `pos`, for drags that leave the grid.
    #[must_use]
    pub fn cell_at_clamped(&self, pos: Point<Pixels>) -> (u16, u16) {
        let x = f32::from(pos.x - self.origin.x).max(0.0);
        let y = f32::from(pos.y - self.origin.y).max(0.0);
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped ≥ 0")]
        let (col, row) = (
            (x / f32::from(self.cell_width)).floor() as u16,
            (y / f32::from(self.line_height)).floor() as u16,
        );
        (col.min(self.cols.saturating_sub(1)), row.min(self.rows.saturating_sub(1)))
    }

    /// Pixel offset within the content area (clamped at 0), in the device pixels of the fitted
    /// grid — the units the worker divides by the cell size it was told.
    #[must_use]
    pub fn pixel_at(&self, pos: Point<Pixels>) -> (u32, u32) {
        let scale = if self.pixel_scale.is_finite() && self.pixel_scale > 0.0 {
            self.pixel_scale
        } else {
            1.0
        };
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped ≥ 0")]
        let out = (
            (f32::from(pos.x - self.origin.x).max(0.0) * scale) as u32,
            (f32::from(pos.y - self.origin.y).max(0.0) * scale) as u32,
        );
        out
    }
}

/// Prepared paint data.
#[derive(Debug)]
pub struct Prepared {
    metrics: CellMetrics,
    /// Where the decorations and the cursor sit inside a cell, from ghostty's derivation.
    grid: Grid,
    rows: Vec<PreparedRow>,
    cursor: Option<(Bounds<Pixels>, CursorShape, Hsla)>,
    /// The cells a block cursor covers and the colour their text takes there.
    cursor_text: Option<CursorText>,
    background: Hsla,
    /// Colour of the ⌘-hover link underline.
    link: Hsla,
    /// The scrollbar's thumb over the grid's right edge, while the bar shows.
    scrollbar: Option<(Bounds<Pixels>, Hsla)>,
    /// The failed blocks' rows as `(top, height)` bands: washed edge to edge, a bar at the
    /// left edge.
    failed: Vec<(Pixels, Pixels)>,
    /// The failed blocks' wash and bar colours, and the bar's width.
    failed_look: FailedLook,
    /// Text drawn over the grid: the input method's composition and the "took" captions.
    overlay: Vec<(Point<Pixels>, ShapedLine)>,
    /// The keys whose guesses the overlay shows (the predictor stamps each with the key's
    /// sequence number), for the keystroke → paint meter.
    shown: Vec<u64>,
    /// Painted size over the shaped (base) size, and the font size to paint the words at.
    zoom: f32,
    font_size: Pixels,
    /// The size the glyphs are rasterised at: `font_size`, or the nearest rung of the size
    /// ladder while the zoom is in motion (the raster is stretched to `font_size`).
    raster_size: Pixels,
    /// The frame holds a blinking cursor or SGR 5 text: the view's blink clock must run.
    blinking: bool,
    /// Images the program placed (kitty graphics), clipped to the grid.
    images: Vec<PreparedImage>,
    /// The grid's place in the hit test: a touch that lands on an overlay above it (the find
    /// bar, a menu) is not the grid's.
    hitbox: Hitbox,
}

/// One placed image ready to paint.
///
/// The part shown, and where the whole image would sit so GPUI samples the right part of the
/// texture.
#[derive(Debug)]
struct PreparedImage {
    bounds: Bounds<Pixels>,
    image_bounds: Bounds<Pixels>,
    image: Arc<RenderImage>,
    /// Painted over the glyphs (`z ≥ 0`) rather than under them.
    over_text: bool,
}

/// The columns `start..end` of screen row `row` sit under a block cursor: their text is
/// painted in `color`, the theme's text-under-cursor colour.
#[derive(Clone, Copy, PartialEq, Debug)]
struct CursorText {
    row: u16,
    start: u16,
    end: u16,
    color: Hsla,
}

impl CursorText {
    /// The colour of text at column `col` of screen row `row` that is otherwise `color`.
    fn over(this: Option<Self>, row: u16, col: u16, color: Hsla) -> Hsla {
        match this {
            Some(c) if c.row == row && (c.start..c.end).contains(&col) && color.a > 0.0 => c.color,
            _ => color,
        }
    }
}

#[derive(Debug)]
struct PreparedRow {
    /// The screen row (0 = the top of the viewport).
    row: u16,
    y: Pixels,
    quads: Vec<(u16, u16, Hsla)>,
    /// Underlines and strikethroughs, at ghostty's offsets rather than GPUI's.
    decorations: Vec<Decoration>,
    /// The row's words, each shaped on its own at the base size and placed at its start column.
    segments: Vec<(u16, Rc<Word>)>,
    /// Columns of the link under a ⌘-hover, underlined over the text.
    link: Option<(u16, u16)>,
    /// Colour of the command-block separator drawn along the row's top edge.
    separator: Option<Hsla>,
    /// Cells drawn from geometry rather than the font (box drawing, blocks, Braille).
    sprites: Vec<SpriteCell>,
}

/// One cell the element draws itself (see [`sprite`]), in the cell's own colours.
#[derive(Clone, PartialEq, Debug)]
struct SpriteCell {
    col: u16,
    ch: char,
    fg: Hsla,
    /// The atlas tile it paints from: its name and its mask. `None` while the zoom is in
    /// motion, when the geometry is painted directly rather than filling the atlas with a tile
    /// per intermediate size.
    tile: Option<Rc<SpriteTile>>,
}

/// One sprite's atlas tile: the name GPUI's atlas keys it by (with the size) and the SVG mask
/// it rasterises once, on first use.
#[derive(PartialEq, Eq, Debug)]
struct SpriteTile {
    path: SharedString,
    svg: Box<[u8]>,
}

/// What a sprite tile is rasterised for: the character, the cell in device pixels and the light
/// line's thickness in device pixels (ghostty's sprite key: the cell and the box thickness).
type SpriteKey = (char, u16, u16, u32);

/// One decoration stroke over a run of columns, relative to the row's top-left corner.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Decoration {
    start: u16,
    end: u16,
    color: Hsla,
    /// Distance from the top of the row to the top of the stroke.
    y: Pixels,
    thickness: Pixels,
    /// A curly underline: GPUI draws the wave, at this position and thickness.
    wavy: bool,
    /// Painted over the glyphs (a strikethrough) rather than under them (an underline, so
    /// a descender crosses it instead of being cut by it).
    over: bool,
}

/// Where a stroke sits relative to the glyphs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Layer {
    /// Under the text: underlines, which descenders cross.
    Under,
    /// Over the text: strikethroughs.
    Over,
}

/// Add one cell's worth of stroke, joining it to the run to its left when they match.
///
/// A cell contributes at most three strokes (two for a double underline, one strikethrough),
/// so the run this one continues, if any, is within the last few.
fn stroke(
    out: &mut Vec<Decoration>,
    col: u16,
    color: Hsla,
    line: metrics::Line,
    wavy: bool,
    layer: Layer,
) {
    let over = layer == Layer::Over;
    let joins = |d: &&mut Decoration| {
        d.end == col
            && d.color == color
            && d.y == line.y
            && d.thickness == line.thickness
            && d.wavy == wavy
            && d.over == over
    };
    if let Some(run) = out.iter_mut().rev().take(3).find(joins) {
        run.end = col.saturating_add(1);
        return;
    }
    out.push(Decoration {
        start: col,
        end: col.saturating_add(1),
        color,
        y: line.y,
        thickness: line.thickness,
        wavy,
        over,
    });
}

/// How many columns the cursor covers at `col` of `line`: two on a wide character (ghostty's
/// `cursor_wide`), else one — a block over half a CJK glyph reads as a bug.
fn cursor_span(line: Option<&slopty_grid::Line>, col: u16) -> u16 {
    let wide = line
        .and_then(|l| l.cells.get(usize::from(col)))
        .is_some_and(|cell| cell.width == CellWidth::Wide);
    if wide { 2 } else { 1 }
}

/// The command-block separator for a prompt-start row: the terminal foreground, faint. A
/// failed block says so with its own bar and wash ([`FailedLook`]), not with its rule.
#[must_use]
pub fn separator_color(theme: &Theme) -> Hsla {
    hsla_alpha(theme.terminal.fg, alpha::FAINT)
}

/// How a block whose command failed is drawn, as Warp does it: a bar of the error tone down
/// the block's left edge and a faint wash of it over the whole block.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FailedLook {
    /// Over the block's rows, edge to edge.
    pub wash: Hsla,
    /// Down the element's left edge, in the inset beside the text.
    pub bar: Hsla,
    /// The bar's width, at the zoom it is drawn at.
    pub bar_width: Pixels,
}

impl FailedLook {
    /// The look at `zoom`.
    #[must_use]
    pub fn new(theme: &Theme, zoom: f32) -> Self {
        Self {
            wash: hsla_alpha(theme.surfaces.error, alpha::FAINT),
            bar: hsla(theme.surfaces.error),
            bar_width: px(theme.spacing.xxs * zoom),
        }
    }
}

/// How far a row's paint reaches outside its cell box, `(above, below)`, in painted points.
///
/// The cell box is the row's layout, not its ink: with a reduced line height
/// (`mono_line_height` below 1) the glyphs' descenders and the underline and strikethrough
/// strokes derived from the face sit below the box, and an overline or a tall ascent can sit
/// above it. Culling on the box alone drops a still-visible underline the moment the box
/// crosses the clip edge, so the band is padded by this much on either side.
fn row_overhang(grid: &Grid, face: &metrics::Face, pixel_scale: f32) -> (Pixels, Pixels) {
    let pixel_scale = if pixel_scale.is_finite() && pixel_scale > 0.0 { pixel_scale } else { 1.0 };
    #[expect(clippy::cast_possible_truncation, reason = "font units; f32 carries them")]
    let pts = |device: f64| px(device as f32 / pixel_scale);
    let ink_top = grid.baseline - pts(face.ascent.max(0.0));
    let ink_bottom = grid.baseline + pts((-face.descent).max(0.0));
    let stroke_bottom = |line: metrics::Line| line.y + line.thickness;
    let below = ink_bottom
        .max(stroke_bottom(grid.underline))
        .max(stroke_bottom(grid.strikethrough))
        .max(stroke_bottom(grid.overline))
        - grid.line_height;
    let above = -ink_top.min(grid.overline.y).min(px(0.0));
    (above.max(px(0.0)), below.max(px(0.0)))
}

/// Whether a row at `y` can show anything between `clip_top` and `clip_bottom`, its ink and
/// strokes reaching `overhang` beyond the cell box.
fn row_in_band(
    y: Pixels,
    line_height: Pixels,
    overhang: (Pixels, Pixels),
    clip_top: Pixels,
    clip_bottom: Pixels,
) -> bool {
    let (above, below) = overhang;
    y + line_height + below > clip_top && y - above < clip_bottom
}

/// The element.
#[derive(Debug)]
pub struct TerminalElement {
    view: Entity<TerminalView>,
    focused: bool,
    zoom: f32,
    /// The zoom is changing frame to frame (a pinch, a flight): glyphs come from the raster
    /// ladder, stretched, instead of a fresh raster per size.
    zooming: bool,
    /// What a screen reader hears: the program's title and the cursor row's text. Filled by
    /// the view only while the accessibility tree is being built.
    a11y: Option<(SharedString, SharedString)>,
}

impl TerminalElement {
    /// Paint `view`.
    #[must_use]
    pub const fn new(view: Entity<TerminalView>, focused: bool) -> Self {
        Self { view, focused, zoom: 1.0, zooming: false, a11y: None }
    }

    /// The accessible label (the title) and value (the cursor row's text).
    #[must_use]
    pub fn a11y(mut self, label: SharedString, value: SharedString) -> Self {
        self.a11y = Some((label, value));
        self
    }

    /// Scale everything (font, cells, padding) by `zoom` while keeping the grid size that the
    /// unscaled bounds would give. Used by the canvas so a terminal keeps its columns when the
    /// camera zooms.
    #[must_use]
    pub const fn zoom(mut self, zoom: f32) -> Self {
        self.zoom = zoom;
        self
    }

    /// The zoom is in motion this frame: paint from the raster ladder (see
    /// [`fonts::raster_rung`]); the settled frame paints at the exact size again.
    #[must_use]
    pub const fn zooming(mut self, on: bool) -> Self {
        self.zooming = on;
        self
    }
}

impl IntoElement for TerminalElement {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

/// Most glyphs the word cache holds before it forgets the words used least recently: about
/// ten megabytes, and some forty screens of 20 × 200 × 50 cells of distinct text, so what the
/// canvas showed a moment ago is still shaped when a pan or a scroll brings it back.
const WORD_BUDGET: usize = 1 << 18;

/// Shaped words by the hash of their text, styles and look ([`segment_hash`]), each stamped
/// with the pass that last used it.
///
/// A pass is one prepaint of one element. Every word a frame uses is stamped later than
/// anything an earlier frame used, so evicting the oldest stamps first never drops what is on
/// screen, however many terminals share the frame, and needs no frame counter. Nothing is
/// swept per frame: past [`WORD_BUDGET`] glyphs, the oldest quarter of the words goes at once.
#[derive(Default)]
struct Words {
    map: FxHashMap<u64, (Rc<Word>, u64)>,
    glyphs: usize,
    pass: u64,
}

impl Words {
    /// A new pass begins: evict first if the last ones went over the budget.
    fn begin(&mut self, budget: usize) {
        self.pass = self.pass.wrapping_add(1);
        while self.glyphs > budget && !self.map.is_empty() {
            self.evict_oldest_quarter();
        }
    }

    /// The word under `key`, shaped by `shape` if it is not held.
    fn get_or_shape(&mut self, key: u64, shape: impl FnOnce() -> Word) -> Rc<Word> {
        match self.map.entry(key) {
            Entry::Occupied(mut held) => {
                held.get_mut().1 = self.pass;
                Rc::clone(&held.get().0)
            }
            Entry::Vacant(slot) => {
                let word = Rc::new(shape());
                self.glyphs = self.glyphs.saturating_add(word.weight());
                slot.insert((Rc::clone(&word), self.pass));
                word
            }
        }
    }

    /// Drop the quarter of the words stamped longest ago, at least one, never one stamped by
    /// the current pass.
    fn evict_oldest_quarter(&mut self) {
        let mut stamps: Vec<u64> = self.map.values().map(|(_, pass)| *pass).collect();
        let nth = (stamps.len() / 4).min(stamps.len().saturating_sub(1));
        let (_, cutoff, _) = stamps.select_nth_unstable(nth);
        let cutoff = (*cutoff).min(self.pass.wrapping_sub(1));
        let mut glyphs = 0_usize;
        self.map.retain(|_, (word, pass)| {
            let keep = *pass > cutoff;
            if keep {
                glyphs = glyphs.saturating_add(word.weight());
            }
            keep
        });
        self.glyphs = glyphs;
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.map.len()
    }
}

/// Text colours held to the theme's minimum contrast, remembered per (text, background): the
/// check is six `powf`s, and a frame asks it of every decorated cell.
#[derive(Default)]
struct Contrast {
    least: u16,
    memo: FxHashMap<(Rgb, Rgb), Rgb>,
}

/// Pairs remembered before the memo starts over (a program cycling true colours).
const CONTRAST_MEMO: usize = 4096;

impl Contrast {
    /// [`Colors::text_over`], remembered.
    fn text_over(&mut self, palette: &Colors, fg: Rgb, bg: Rgb) -> Rgb {
        let least = palette.theme.minimum_contrast;
        if least <= 100 {
            return fg;
        }
        if self.least != least || self.memo.len() >= CONTRAST_MEMO {
            self.memo.clear();
            self.least = least;
        }
        *self.memo.entry((fg, bg)).or_insert_with(|| palette.text_over(fg, bg))
    }
}

/// The cell geometry derived for one family, size and scale: the grid in points, the whole-pixel
/// metrics and the face they came from.
type Derived = (Grid, metrics::Metrics, metrics::Face);

/// What the element keeps across frames and views, on the App as a global: the shaped words
/// (at the base size only), the sprite tiles, the derived grids and the resolved families.
#[derive(Default)]
struct ShapeCache {
    words: Words,
    contrast: Contrast,
    /// The sprite tiles by (character, cell, thickness); `None` for a character that turned
    /// out to have no drawing.
    sprites: FxHashMap<SpriteKey, Option<Rc<SpriteTile>>>,
    /// The derived grid per (family, font size, scale, line-height multiplier): deriving it
    /// walked the font system every frame for nothing.
    grids: FxHashMap<(SharedString, u32, u32, u32), Derived>,
    /// The monospace family resolved per theme list: the first installed candidate. Listing
    /// the installed fonts is a trip to the font server (tens of milliseconds), so it happens
    /// once per list for the whole app, not once per view.
    families: FxHashMap<Vec<String>, SharedString>,
}

impl gpui::Global for ShapeCache {}

/// What the tests read back about the element's work; nothing in a release build.
#[cfg(test)]
#[derive(Default)]
struct Probe {
    /// How many times the installed fonts were listed.
    picks: usize,
    /// How many rows the last prepaint built (the rows the clip shows, not the grid's).
    rows_prepared: usize,
    /// The "took" captions the last prepaint drew, top row first.
    captions: Vec<String>,
    /// Words shaped since the app started.
    shaped: usize,
    /// Sprite masks written since the app started.
    sprite_masks: usize,
}

#[cfg(test)]
impl gpui::Global for Probe {}

/// Sprite tiles remembered before the table starts over (every settled zoom adds a cell size).
const SPRITE_TILES: usize = 4096;

impl ShapeCache {
    /// The cell geometry for `family` at `font_size` on this window (see [`measure`]).
    fn grid(
        &mut self,
        window: &Window,
        family: &SharedString,
        ligatures: bool,
        font_size: Pixels,
        height_mult: f32,
    ) -> Derived {
        let key = (
            family.clone(),
            f32::from(font_size).to_bits(),
            window.scale_factor().to_bits(),
            height_mult.to_bits(),
        );
        if let Some(grid) = self.grids.get(&key) {
            return *grid;
        }
        let font = fonts::terminal_font(family, false, false, ligatures);
        let (grid, derived, _font_id, face) = measure(window, &font, font_size, height_mult);
        self.grids.insert(key, (grid, derived, face));
        (grid, derived, face)
    }

    /// The first of `candidates` that is installed, resolved once per list (see
    /// [`pick_family`]).
    fn family(&mut self, window: &Window, candidates: &[String]) -> SharedString {
        if let Some(family) = self.families.get(candidates) {
            return family.clone();
        }
        let picked = SharedString::from(pick_family(window, candidates));
        self.families.insert(candidates.to_vec(), picked.clone());
        picked
    }

    /// The atlas tile of `ch` in a cell `w`×`h` device pixels with a light line `thickness`
    /// device pixels thick, its mask written once.
    fn sprite(&mut self, ch: char, w: u16, h: u16, thickness: f32) -> Option<Rc<SpriteTile>> {
        let key = (ch, w, h, thickness.to_bits());
        if let Some(tile) = self.sprites.get(&key) {
            return tile.clone();
        }
        if self.sprites.len() >= SPRITE_TILES {
            self.sprites.clear();
        }
        let tile = sprite::svg(ch, w, h, thickness).map(|svg| {
            Rc::new(SpriteTile {
                path: SharedString::from(format!(
                    "slopty-sprite/{:x}/{w}x{h}/{thickness}",
                    u32::from(ch)
                )),
                svg: svg.into_bytes().into_boxed_slice(),
            })
        });
        self.sprites.insert(key, tile.clone());
        tile
    }
}

/// The part of a shaped word's key that is the same for every word of a view this frame: size,
/// family, the palette the styles were resolved through (so a theme swap never replays old
/// colours) and the cell width the glyphs were placed on (it is whole device pixels, so it
/// differs between a 1× and a 2× display and a word must be placed again after a move).
fn hash_base(
    font_size: Pixels,
    cell_width: Pixels,
    family: &str,
    ligatures: bool,
    palette: &Colors,
) -> u64 {
    let mut h = FxHasher::default();
    f32::from(font_size).to_bits().hash(&mut h);
    f32::from(cell_width).to_bits().hash(&mut h);
    family.hash(&mut h);
    ligatures.hash(&mut h);
    palette.hash(&mut h);
    h.finish()
}

/// Key of a shaped word: the base plus everything the shaped runs bake in (text, styles).
fn segment_hash(base: u64, cells: &[Cell], blink_off: bool) -> u64 {
    let mut h = FxHasher::default();
    base.hash(&mut h);
    (blink_off && blinks(cells)).hash(&mut h);
    for cell in cells {
        cell.text.as_str().hash(&mut h);
        cell.style.hash(&mut h);
        (cell.width as u8).hash(&mut h);
    }
    h.finish()
}

/// What a word's colours depend on besides its cells: the font family (and whether its
/// ligatures shape), the palette and the blink clock's phase.
#[derive(Clone, Copy)]
struct Look<'a> {
    family: &'a str,
    ligatures: bool,
    palette: &'a Colors,
    blink_off: bool,
}

/// Whether any of `cells` carries SGR 5: such a word is shaped once per blink phase.
fn blinks(cells: &[Cell]) -> bool {
    cells.iter().any(|cell| cell.style.flags.contains(StyleFlags::BLINK))
}

/// A cell whose glyphs paint nothing: a narrow blank (its background is a quad and any
/// underline or strikethrough is a [`Decoration`], both drawn from the cells, not from the
/// shaped word). Rows are split into words at these.
fn plain_space(cell: &Cell) -> bool {
    cell.width == CellWidth::Narrow && matches!(cell.text.as_str(), "" | " ")
}

/// A cell drawn from geometry rather than shaped: box drawing, blocks, Braille, Powerline.
/// It ends a word like a space does, so the font never sees it.
fn drawn_here(cell: &Cell) -> Option<char> {
    (cell.width == CellWidth::Narrow && sprite::is_sprite(cell.text.as_str()))
        .then(|| cell.text.as_str().chars().next())
        .flatten()
}

/// A cell shaped on its own: a digit. Counters, timestamps and sizes make most of a streaming
/// row's unique text, and no coding font ligates digits, so each digit is one cached glyph
/// instead of a fresh word every frame.
fn stands_alone(cell: &Cell) -> bool {
    cell.width == CellWidth::Narrow && cell.text.as_ascii().is_some_and(|b| b.is_ascii_digit())
}

/// The words of a row: maximal runs of cells that are not plain spaces, digits on their own,
/// with the column each starts at. Positions inside a word come from its cluster index, so a
/// word shapes the same wherever it sits. An iterator, so a frame allocates nothing for it.
fn segments(cells: &[Cell]) -> impl Iterator<Item = (u16, &[Cell])> {
    let col = |i: usize| u16::try_from(i).unwrap_or(u16::MAX);
    let mut i = 0_usize;
    let mut start: Option<usize> = None;
    // A digit met right after a word: it comes out next.
    let mut queued: Option<(u16, &[Cell])> = None;
    std::iter::from_fn(move || {
        if let Some(digit) = queued.take() {
            return Some(digit);
        }
        while let Some(cell) = cells.get(i) {
            let at = i;
            i = i.saturating_add(1);
            let space = plain_space(cell) || drawn_here(cell).is_some();
            let digit =
                stands_alone(cell).then(|| cells.get(at..=at).map(|d| (col(at), d))).flatten();
            if space || digit.is_some() {
                if let Some(s) = start.take()
                    && let Some(word) = cells.get(s..at)
                {
                    queued = digit;
                    return Some((col(s), word));
                }
                if digit.is_some() {
                    return digit;
                }
            } else if start.is_none() {
                start = Some(at);
            }
        }
        let s = start.take()?;
        cells.get(s..).map(|word| (col(s), word))
    })
}

/// How a local-echo guess is drawn: faint and underlined, so a guess never reads as the worker's.
const PREDICTED: CellStyle =
    CellStyle { flags: StyleFlags::FAINT, underline: Underline::Single, ..CellStyle::DEFAULT };

/// Screen row `row`'s cells with the predictor's guesses for it written in, or `None` when it
/// has none: the guesses are then shaped, cached and painted as the worker's text is.
fn predicted_cells(cells: &[Cell], guesses: &VecDeque<Prediction>, row: u16) -> Option<Vec<Cell>> {
    let mut on_row = guesses.iter().filter(|p| p.row == row).peekable();
    on_row.peek()?;
    let mut cells = cells.to_vec();
    for guess in on_row {
        if let (Some(cell), Some(ch)) =
            (cells.get_mut(usize::from(guess.col)), guess.text.chars().next())
        {
            *cell = Cell::narrow(ch, PREDICTED);
        }
    }
    Some(cells)
}

/// The whole device pixels `start..start + len` (points) covers when GPUI snaps it: each edge
/// rounded half toward zero on its own, as `Window::paint_svg` snaps a bounds. The size a
/// sprite's atlas tile is rasterised at.
fn device_span(start: Pixels, len: Pixels, scale: f32) -> u16 {
    let edge = |v: Pixels| {
        let v = f32::from(v) * scale;
        (v.abs() - 0.5).ceil().copysign(v)
    };
    let span = (edge(start + len) - edge(start)).clamp(0.0, f32::from(u16::MAX));
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped to u16")]
    let span = span as u16;
    span
}

/// A word shaped once, at the base font size, and what painting it at any size needs: every
/// glyph with its font, its colour and its position — placed by the column of the cell its
/// byte came from, so a wide cluster spans two cells whatever it shaped to.
#[derive(Debug)]
struct Word {
    glyphs: Vec<Glyph>,
}

impl Word {
    /// What the word counts against [`WORD_BUDGET`]: its glyphs, and at least one.
    fn weight(&self) -> usize {
        self.glyphs.len().max(1)
    }
}

/// One glyph of a [`Word`], at the base size, relative to the word's origin on the baseline.
#[derive(Debug, Clone, Copy)]
struct Glyph {
    font: FontId,
    id: GlyphId,
    /// The column of the cell it belongs to, counted from the word's first.
    col: u16,
    position: Point<Pixels>,
    emoji: bool,
    color: Hsla,
}

/// Colour of the byte at `index` of a word's text, from its `(end byte, colour)` style runs.
fn color_at(colors: &[(usize, Hsla)], index: usize) -> Hsla {
    colors
        .iter()
        .find(|(end, _)| index < *end)
        .or_else(|| colors.last())
        .map_or(gpui::black(), |(_, color)| *color)
}

/// Where a glyph shaped at the base size goes when the word is painted at `zoom` times it:
/// its shaped position scales with the size (a fixed-pitch advance is linear in the size) from
/// the word's origin on the baseline.
fn glyph_origin(origin: Point<Pixels>, shaped: Point<Pixels>, zoom: f32) -> Point<Pixels> {
    point(origin.x + shaped.x * zoom, origin.y + shaped.y * zoom)
}

/// Shape one word of `cells` and place its glyphs on the cell grid: each glyph goes at the
/// column of the cell its byte belongs to, keeping its shaped offset from the cell's first
/// glyph. A wide cluster (CJK, an emoji, a ZWJ sequence, a flag) so spans exactly two cells
/// whether the font shaped it to one glyph or several, a ligature keeps its cells, and a
/// combining mark stays on its base.
fn shape_cells(
    text_system: &gpui::WindowTextSystem,
    cells: &[Cell],
    font_size: Pixels,
    cell_width: Pixels,
    look: Look<'_>,
    contrast: &mut Contrast,
) -> Word {
    let mut text = String::with_capacity(cells.len());
    let mut runs: Vec<TextRun> = Vec::new();
    let mut colors: Vec<(usize, Hsla)> = Vec::new();
    // `(first byte, column)` of every cell that draws text, in text order.
    let mut starts: Vec<(usize, u16)> = Vec::with_capacity(cells.len());
    let mut current: Option<(CellStyle, usize)> = None;
    let mut col: u16 = 0;
    for cell in cells {
        if !cell.width.draws_text() {
            // A wide cell's tail is already counted in its columns; a spacer head is its own.
            if cell.width == CellWidth::SpacerHead {
                col = col.saturating_add(1);
            }
            continue;
        }
        let piece: &str = if cell.text.is_empty() { " " } else { cell.text.as_str() };
        starts.push((text.len(), col));
        col = col.saturating_add(cell.width.columns());
        text.push_str(piece);
        let len = piece.len();
        match &mut current {
            Some((style, acc)) if *style == cell.style => {
                *acc = acc.saturating_add(len);
            }
            _ => {
                if let Some((style, acc)) = current.take() {
                    let run = text_run(acc, &style, look, contrast);
                    colors.push((text.len().saturating_sub(len), run.color));
                    runs.push(run);
                }
                current = Some((cell.style, len));
            }
        }
    }
    if let Some((style, acc)) = current.take() {
        let run = text_run(acc, &style, look, contrast);
        colors.push((text.len(), run.color));
        runs.push(run);
    }
    let line = text_system.shape_line(SharedString::from(text), font_size, &runs, None);
    Word { glyphs: place(&line, &starts, cell_width, &colors) }
}

/// Every glyph of `line` at its cell's column (see [`shape_cells`]).
fn place(
    line: &ShapedLine,
    starts: &[(usize, u16)],
    cell_width: Pixels,
    colors: &[(usize, Hsla)],
) -> Vec<Glyph> {
    let mut glyphs = Vec::new();
    // The cell the last glyph fell in and where its first glyph was shaped.
    let mut anchor: Option<(usize, Pixels)> = None;
    for run in &line.layout().runs {
        for glyph in &run.glyphs {
            let k = starts.partition_point(|(start, _)| *start <= glyph.index).saturating_sub(1);
            let col = starts.get(k).map_or(0, |(_, col)| *col);
            let first = match anchor {
                Some((cell, first)) if cell == k => first,
                _ => {
                    anchor = Some((k, glyph.position.x));
                    glyph.position.x
                }
            };
            glyphs.push(Glyph {
                font: run.font_id,
                id: glyph.id,
                col,
                position: point(
                    cell_width * f32::from(col) + (glyph.position.x - first),
                    glyph.position.y,
                ),
                emoji: glyph.is_emoji,
                color: color_at(colors, glyph.index),
            });
        }
    }
    glyphs
}

fn mono_font(family: &str, ligatures: bool, style: &CellStyle) -> Font {
    fonts::terminal_font(
        family,
        style.flags.contains(StyleFlags::BOLD),
        style.flags.contains(StyleFlags::ITALIC),
        ligatures,
    )
}

/// The shape a focused cursor is drawn in: the program's (DECSCUSR), unless the theme fixes
/// one (ghostty's `cursor-style`).
const fn cursor_shape_for(style: slopty_theme::CursorStyle, program: CursorShape) -> CursorShape {
    match style {
        slopty_theme::CursorStyle::Program => program,
        slopty_theme::CursorStyle::Block => CursorShape::Block,
        slopty_theme::CursorStyle::Bar => CursorShape::Bar,
        slopty_theme::CursorStyle::Underline => CursorShape::Underline,
    }
}

/// The colour a cell's glyphs take, with inverse, faint and invisible applied, held to the
/// theme's minimum contrast against the cell's background. In the off phase of the blink
/// clock (`blink_off`) an SGR 5 cell's glyphs are hidden the same way.
fn cell_color(
    style: &CellStyle,
    palette: &Colors,
    blink_off: bool,
    contrast: &mut Contrast,
) -> Hsla {
    let inverse = style.flags.contains(StyleFlags::INVERSE);
    let fg = palette.bold_slot(style.fg, style.flags.contains(StyleFlags::BOLD));
    let (fg_slot, bg_slot) = if inverse { (style.bg, fg) } else { (fg, style.bg) };
    let fg = palette.resolve(fg_slot, inverse);
    let bg = palette.resolve(bg_slot, !inverse);
    let mut color = hsla(contrast.text_over(palette, fg, bg));
    if style.flags.contains(StyleFlags::FAINT) {
        color.a = 0.6;
    }
    if style.flags.contains(StyleFlags::INVISIBLE)
        || (blink_off && style.flags.contains(StyleFlags::BLINK))
    {
        color.a = 0.0;
    }
    color
}

/// The colour of a cell's underline: SGR 58 when the cell sets one, else the text colour.
fn underline_color(style: &CellStyle, palette: &Colors, text: Hsla) -> Hsla {
    match style.underline_color {
        slopty_grid::Color::Default => text,
        other => hsla(palette.resolve(other, false)),
    }
}

/// A run of `len` bytes in `style`. Underlines of every kind and strikethroughs are
/// [`Decoration`]s drawn from the cells, so the run carries only the font and the colour.
fn text_run(len: usize, style: &CellStyle, look: Look<'_>, contrast: &mut Contrast) -> TextRun {
    TextRun {
        len,
        font: mono_font(look.family, look.ligatures, style),
        color: cell_color(style, look.palette, look.blink_off, contrast),
        background_color: None,
        underline: None,
        strikethrough: None,
    }
}

/// The first installed family from the theme's list, else the bundled one.
fn pick_family(window: &Window, candidates: &[String]) -> String {
    let installed = window.text_system().all_font_names();
    candidates
        .iter()
        .find(|family| installed.iter().any(|name| name == *family))
        .cloned()
        .unwrap_or_else(|| fonts::MONO_FAMILY.to_owned())
}

/// A cell length for the wire, with a fallback for nonsense.
fn whole_u16(v: u32, fallback: u16) -> u16 {
    match u16::try_from(v) {
        Ok(0) | Err(_) => fallback,
        Ok(v) => v,
    }
}

/// The theme's line-height multiplier as ghostty's `adjust-cell-height`: a percentage of the
/// derived cell, not a replacement for it. `1.0` leaves the font's own line height alone.
fn adjusted_height(height: u32, mult: f32) -> u32 {
    if !mult.is_finite() || mult <= 0.0 || (mult - 1.0).abs() < f32::EPSILON {
        return height;
    }
    let scaled = f32::from(u16::try_from(height).unwrap_or(u16::MAX)) * mult;
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped first")]
    let out = scaled.round().clamp(1.0, f32::from(u16::MAX)) as u32;
    out
}

/// The cell geometry for `font` at `font_size`, from ghostty's derivation.
///
/// The face is measured in device pixels — `font_size` times the display scale — so the cell
/// is a whole number of them, and [`Grid`] divides back to points. The line gap and the
/// underline come from the font's own tables (`hhea` leading, `post` underline position and
/// thickness, through the fork's `TextSystem::font_metrics`); a font that leaves them at zero
/// gets ghostty's estimates, which is ghostty's own rule.
fn measure(
    window: &Window,
    font: &Font,
    font_size: Pixels,
    height_mult: f32,
) -> (Grid, metrics::Metrics, FontId, metrics::Face) {
    let text_system = window.text_system();
    let font_id = text_system.resolve_font(font);
    let scale = window.scale_factor().max(1.0);
    let size = font_size * scale;
    let device = |v: Pixels| f64::from(f32::from(v));
    let advance = text_system.advance(font_id, size, 'M').map_or(size * 0.6, |s| s.width);
    let face = metrics::Face {
        cell_width: device(advance),
        ascent: device(text_system.ascent(font_id, size)),
        descent: device(text_system.descent(font_id, size)),
        line_gap: device(text_system.line_gap(font_id, size)).max(0.0),
        underline_position: Some(device(text_system.underline_position(font_id, size)))
            .filter(|v| *v != 0.0),
        underline_thickness: Some(device(text_system.underline_thickness(font_id, size)))
            .filter(|v| *v > 0.0),
        cap_height: Some(device(text_system.cap_height(font_id, size))),
        ex_height: Some(device(text_system.x_height(font_id, size))),
        ..metrics::Face::default()
    };
    let mut derived = metrics::calc(&face);
    derived.set_cell_height(adjusted_height(derived.cell_height, height_mult));
    (Grid::new(&derived, scale), derived, font_id, face)
}

impl Element for TerminalElement {
    type PrepaintState = Prepared;
    type RequestLayoutState = ();

    fn id(&self) -> Option<ElementId> {
        Some(ElementId::Name("terminal-grid".into()))
    }

    fn a11y_role(&self) -> Option<gpui::accesskit::Role> {
        Some(gpui::accesskit::Role::Terminal)
    }

    fn write_a11y_info(&self, node: &mut gpui::accesskit::Node) {
        if let Some((label, value)) = &self.a11y {
            node.set_label(label.to_string());
            node.set_value(value.to_string());
        }
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let style = Style {
            size: Size { width: relative(1.0).into(), height: relative(1.0).into() },
            ..Style::default()
        };
        (window.request_layout(style, None, cx), ())
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Prepared {
        if !cx.has_global::<ShapeCache>() {
            cx.set_global(ShapeCache::default());
        }
        #[cfg(test)]
        if !cx.has_global::<Probe>() {
            cx.set_global(Probe::default());
        }
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        // Resolving the family walks every installed font: once per app for a theme's list
        // (the cache), remembered per view so a frame costs neither the walk nor the lookup.
        let known_family = self.view.read(cx).font_family();
        let family = known_family.unwrap_or_else(|| {
            let candidates = self.view.read(cx).theme().typography.mono_families.clone();
            #[cfg(test)]
            let listed = cx.global::<ShapeCache>().families.len();
            let picked =
                cx.update_global::<ShapeCache, _>(|cache, _| cache.family(window, &candidates));
            #[cfg(test)]
            if cx.global::<ShapeCache>().families.len() > listed {
                cx.global_mut::<Probe>().picks = cx.global::<Probe>().picks.saturating_add(1);
            }
            self.view.update(cx, |view, _cx| view.set_font_family(picked.clone()));
            picked
        });
        let zoom = if self.zoom.is_finite() && self.zoom > 0.0 { self.zoom } else { 1.0 };
        let (base_size, height_mult, base_pad, cursor_blink, cursor_style, ligatures) = {
            let theme = self.view.read(cx).theme();
            (
                px(theme.typography.mono_size),
                theme.typography.mono_line_height,
                px(theme.spacing.inset()),
                theme.behaviour.cursor_blink,
                theme.behaviour.cursor_style,
                theme.typography.ligatures,
            )
        };
        // Grid size comes from the unscaled geometry so zooming never resizes the PTY. The
        // cell is derived once per family, size and scale, not once per frame.
        let (base_grid, base_derived, face) = cx.update_global::<ShapeCache, _>(|cache, _| {
            cache.grid(window, &family, ligatures, base_size, height_mult)
        });
        let (base_cell_width, base_line_height) = (base_grid.cell_width, base_grid.line_height);
        let unscaled = size(bounds.size.width / zoom, bounds.size.height / zoom);
        let inner = size(unscaled.width - base_pad * 2.0, unscaled.height - base_pad * 2.0);
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "≥ 1 clamped")]
        let (cols, rows) = (
            (f32::from(inner.width) / f32::from(base_cell_width)).floor().max(1.0) as u16,
            (f32::from(inner.height) / f32::from(base_line_height)).floor().max(1.0) as u16,
        );
        // Paint geometry is the unzoomed one scaled: `cols` and `rows` were counted with the
        // unzoomed cell, so scaling is what keeps `cols × cell_width` inside the item's content
        // width. Re-deriving at the zoomed size would round the cell up and clip the last column.
        let font_size = base_size * zoom;
        let raster_size = if self.zooming { fonts::raster_rung(font_size) } else { font_size };
        let grid = base_grid.scaled(zoom);
        let (cell_width, line_height) = (grid.cell_width, grid.line_height);
        let pad = base_pad * zoom;
        let origin = bounds.origin + point(pad, pad);
        let scale = window.scale_factor().max(1.0);
        let metrics = CellMetrics {
            origin,
            cell_width,
            line_height,
            cols,
            rows,
            pixel_scale: scale / zoom,
            face,
            face_size: f32::from(base_size) * scale,
        };
        let fitted = TermSize {
            cols,
            rows,
            metrics: slopty_proto::input::CellMetrics {
                cell_width: whole_u16(base_derived.cell_width, 8),
                cell_height: whole_u16(base_derived.cell_height, 16),
            },
        };
        self.view.update(cx, |view, cx| view.fitted(fitted, metrics, cx));
        // The pointer can leave the right edge without a move this element hears: to another
        // app (the window going inactive), or by the tile moving under a still pointer. Here the
        // bar is only ever let go; a real move is what brings it up.
        let active = window.is_window_active();
        let deactivated = id.is_some_and(|id| {
            window.with_element_state(id, |was: Option<bool>, _| {
                (was.unwrap_or(active) && !active, active)
            })
        });
        let pointer = window.mouse_position();
        if deactivated || !(bounds.contains(&pointer) && near_scrollbar(&metrics, pointer)) {
            self.view.update(cx, |view, cx| view.pointer_near_scrollbar(false, cx));
        }

        // Shaping reads the view's rows in place (no copy of the grid per frame) while the
        // cache is out of the app: put it back before anything else touches `cx`.
        let mut cache = std::mem::take(cx.global_mut::<ShapeCache>());
        #[cfg(test)]
        let sprite_masks = cache.sprites.len();
        #[cfg(test)]
        let mut shaped = 0_usize;
        let text_system = Arc::clone(window.text_system());
        let focused = self.focused;
        // A sprite is painted from its atlas tile once the zoom settles; in motion, from its
        // geometry, so the atlas does not fill with a tile per intermediate size.
        let sprite_tiles = !self.zooming;
        let sprite_thickness = (f32::from(grid.underline.thickness) * scale).round().max(1.0);
        // Textures for the placed images are made (and stale ones dropped) before the read.
        let placed = self.view.update(cx, |view, _cx| view.placed_images(window));
        #[cfg(test)]
        let mut caption_texts: Vec<String> = Vec::new();
        // A viewport that moved since the last frame brings the scrollbar up in this one.
        let now = cx.background_executor().now();
        self.view.update(cx, |view, cx| view.viewport_drawn(now, cx));
        let fading;
        let prepared = {
            let view = self.view.read(cx);
            let theme = view.theme();
            let state = view.state();
            let colors = Colors::new(&theme.terminal, state.colors());
            let palette = &colors;
            let rows_view = state.view();
            let predicted = view.predictions();
            let cursor = predicted.map_or_else(|| state.cursor(), |(_, c)| c);
            let view_offset = state.view_offset();
            let modes = state.modes();
            let marked = view.marked();
            let selection = view.selection();
            let blink_off = !view.blink_on();
            let mut blinking = false;
            let grid_cols = state.size().cols;
            let (matches, current) = view.search_highlights().unwrap_or((&[], None));
            let link = view.link_highlight();
            let shown = view.scrollbar_opacity(now);
            fading = shown > 0.0 && shown < 1.0;
            let scrollbar = (shown > 0.0).then(|| {
                let held = view.thumb_held();
                let alpha = if held { alpha::PRESSED } else { alpha::TINT };
                (hsla_alpha(palette.theme.fg, alpha * shown), state.history_len(), view_offset)
            });
            let look = |blink_off| Look { family: &family, ligatures, palette, blink_off };

            cache.words.begin(WORD_BUDGET);
            let base = hash_base(base_size, base_cell_width, &family, ligatures, palette);
            // Rows the clip cannot show (a grid half off the viewport) are not built at all:
            // no quads, no words, no hashing. Paint walks only the rows prepared here.
            let clip = window.content_mask().bounds.intersect(&bounds);
            let (clip_top, clip_bottom) = (clip.top(), clip.bottom());
            let overhang = row_overhang(&grid, &face, metrics.pixel_scale);
            // Rows above the oldest line the worker still has: a `~` filler, shaped once.
            let mut filler: Option<Rc<Word>> = None;
            let mut prepared_rows = Vec::with_capacity(rows_view.len());
            // "took 3.2 s" at the right end of a prompt row whose command took a while.
            let mut captions: Vec<(Point<Pixels>, ShapedLine)> = Vec::new();
            // Nothing but blank rows from the terminal's first line down to here: a prompt
            // in that stretch opens the terminal rather than following a command, and a rule
            // over it would only double the tile header's hairline.
            let mut blank_from_start = rows_view.first().is_some_and(|r| r.index.0 == 0);
            let hovered = view.hovered_block();
            let failed = state
                .failed_runs(&rows_view)
                .into_iter()
                .map(|run| {
                    let top = origin.y + line_height * f32::from(run.start);
                    (top, line_height * f32::from(run.end.saturating_sub(run.start)))
                })
                .collect();
            let failed_look = FailedLook::new(theme, zoom);
            for (i, row) in rows_view.iter().enumerate() {
                let opens_terminal = blank_from_start;
                if row.line.is_some_and(|l| l.cells.iter().any(|c| !plain_space(c))) {
                    blank_from_start = false;
                }
                let screen_row = u16::try_from(i).unwrap_or(u16::MAX);
                let y = origin.y + line_height * f32::from(screen_row);
                if !row_in_band(y, line_height, overhang, clip_top, clip_bottom) {
                    continue;
                }
                let Some(line) = row.line else {
                    let filler = filler.get_or_insert_with(|| {
                        let cells = [Cell::narrow('~', CellStyle::DEFAULT)];
                        let (size, width) = (base_size, base_cell_width);
                        let word = shape_cells(
                            &text_system,
                            &cells,
                            size,
                            width,
                            look(false),
                            &mut cache.contrast,
                        );
                        Rc::new(word)
                    });
                    prepared_rows.push(PreparedRow {
                        row: screen_row,
                        y,
                        quads: Vec::new(),
                        decorations: Vec::new(),
                        segments: vec![(0, Rc::clone(filler))],
                        link: None,
                        separator: None,
                        sprites: Vec::new(),
                    });
                    continue;
                };
                // The local-echo guesses on this row are cells like the worker's.
                let guessed = predicted
                    .and_then(|(guesses, _)| predicted_cells(&line.cells, guesses, screen_row));
                let cells: &[Cell] = guessed.as_deref().unwrap_or(&line.cells);
                let mut quads: Vec<(u16, u16, Hsla)> = Vec::new();
                let mut decorations: Vec<Decoration> = Vec::new();
                let mut sprites: Vec<SpriteCell> = Vec::new();
                for (col, cell) in cells.iter().enumerate() {
                    let col = u16::try_from(col).unwrap_or(u16::MAX);
                    let inverse = cell.style.flags.contains(StyleFlags::INVERSE);
                    let bg = if inverse { cell.style.fg } else { cell.style.bg };
                    let is_default_bg = !inverse && matches!(bg, slopty_grid::Color::Default);
                    if !is_default_bg {
                        let color = hsla(palette.resolve(bg, !inverse));
                        if let Some((_start, end, c)) = quads.last_mut()
                            && *end == col
                            && *c == color
                        {
                            *end = col.saturating_add(1);
                        } else {
                            quads.push((col, col.saturating_add(1), color));
                        }
                    }
                    let drawn = drawn_here(cell);
                    let struck = cell.style.flags.contains(StyleFlags::STRIKETHROUGH);
                    if drawn.is_none() && cell.style.underline == Underline::None && !struck {
                        // Its glyphs' colour is the shaped word's business.
                        continue;
                    }
                    let text = cell_color(&cell.style, palette, blink_off, &mut cache.contrast);
                    if let Some(ch) = drawn {
                        let tile = if sprite_tiles {
                            let x = origin.x + cell_width * f32::from(col);
                            let (w, h) = (
                                device_span(x, cell_width, scale),
                                device_span(y, line_height, scale),
                            );
                            cache.sprite(ch, w, h, sprite_thickness)
                        } else {
                            None
                        };
                        sprites.push(SpriteCell { col, ch, fg: text, tile });
                    }
                    // Underline and strikethrough go where the font says, not where GPUI
                    // would put them; a curly underline is GPUI's wave at that position.
                    if cell.style.underline != Underline::None {
                        let color = underline_color(&cell.style, palette, text);
                        let wavy = cell.style.underline == Underline::Curly;
                        stroke(&mut decorations, col, color, grid.underline, wavy, Layer::Under);
                        if cell.style.underline == Underline::Double {
                            // The second stroke sits one stroke's gap above the first.
                            let above = metrics::Line {
                                y: grid.underline.y - grid.underline.thickness * 2.0,
                                thickness: grid.underline.thickness,
                            };
                            stroke(&mut decorations, col, color, above, false, Layer::Under);
                        }
                    }
                    if struck {
                        stroke(&mut decorations, col, text, grid.strikethrough, false, Layer::Over);
                    }
                }
                // The selection paints over cell backgrounds and under the text.
                let index = row.index;
                if let Some(range) = selection.and_then(|s| s.columns(index, grid_cols)) {
                    quads.push((range.start, range.end, hsla(palette.theme.selection)));
                }
                // Search hits, sorted by line: the slice for this row by binary search.
                let first = matches.partition_point(|m| m.line < index);
                for (k, m) in matches.iter().skip(first).enumerate() {
                    if m.line != index {
                        break;
                    }
                    let color = if current == Some(first.saturating_add(k)) {
                        palette.theme.search_current
                    } else {
                        palette.theme.search_match
                    };
                    quads.push((m.col, m.col.saturating_add(m.len).min(grid_cols), hsla(color)));
                }
                let segments = segments(cells)
                    .map(|(col, word)| {
                        blinking |= blinks(word);
                        let key = segment_hash(base, word, blink_off);
                        let shaped_word = cache.words.get_or_shape(key, || {
                            #[cfg(test)]
                            {
                                shaped = shaped.saturating_add(1);
                            }
                            let (size, width) = (base_size, base_cell_width);
                            let look = look(blink_off);
                            shape_cells(&text_system, word, size, width, look, &mut cache.contrast)
                        });
                        (col, shaped_word)
                    })
                    .collect();
                // A link wrapped over rows is underlined on each, edge to edge in between.
                let link = link
                    .filter(|((first, _), (last, _))| (*first..=*last).contains(&index))
                    .map(|((first, start), (last, end))| {
                        (
                            if first == index { start } else { 0 },
                            if last == index { end.min(grid_cols) } else { grid_cols },
                        )
                    });
                // A prompt starts here: rule off the command above it. On the grid's top edge
                // the rule would lie a padding under the tile header's hairline and read as a
                // double line.
                let separator = (line.mark.starts_prompt() && !opens_terminal && screen_row > 0)
                    .then(|| separator_color(theme));
                // A hovered block says how long it took in its own facts, drawn over this row.
                if line.mark.starts_prompt()
                    && hovered != Some(index)
                    && let Some(elapsed) = view.took(index)
                {
                    let text = super::view::took_label(elapsed);
                    let width = u16::try_from(text.chars().count()).unwrap_or(u16::MAX);
                    // The columns the command's text reaches, trailing blanks aside.
                    let typed =
                        line.cells.iter().rposition(|cell| !plain_space(cell)).map_or(0, |last| {
                            u16::try_from(last).unwrap_or(u16::MAX).saturating_add(1)
                        });
                    // Flush with the right edge, a cell clear of the command's text.
                    if let Some(col) = grid_cols.checked_sub(width)
                        && typed < col
                    {
                        let style = &CellStyle::DEFAULT;
                        let mut run = text_run(text.len(), style, look(false), &mut cache.contrast);
                        run.color = hsla_alpha(palette.theme.fg, alpha::TINT);
                        #[cfg(test)]
                        caption_texts.push(text.clone());
                        let shaped = text_system.shape_line(
                            SharedString::from(text),
                            font_size,
                            &[run],
                            Some(cell_width),
                        );
                        captions.push((point(origin.x + cell_width * f32::from(col), y), shaped));
                    }
                }
                prepared_rows.push(PreparedRow {
                    row: screen_row,
                    y,
                    quads,
                    decorations,
                    segments,
                    link,
                    separator,
                    sprites,
                });
            }

            let cursor_visible = cursor.visible
                && view_offset == 0
                && !modes.contains(slopty_grid::TermModes::CURSOR_HIDDEN);
            // A blinking cursor blinks only while focused; unfocused it is a steady hollow
            // block (what ghostty does), so a background terminal never ticks for it. The
            // theme may override the program's choice either way.
            let cursor_blinks = cursor_visible && cursor_blink.blinks(cursor.blink) && focused;
            blinking |= cursor_blinks;
            let cursor_shown = cursor_visible && !(cursor_blinks && blink_off);
            let cursor_line = rows_view.get(usize::from(cursor.row)).and_then(|row| row.line);
            let span = cursor_span(cursor_line, cursor.col);
            // While an input method composes, its underlined preview stands in for the cursor.
            let cursor_prepared = (cursor_shown && marked.is_none()).then(|| {
                let x = origin.x + cell_width * f32::from(cursor.col);
                let y = origin.y + line_height * f32::from(cursor.row);
                let shape = if focused {
                    cursor_shape_for(cursor_style, cursor.shape)
                } else {
                    CursorShape::BlockHollow
                };
                let width = cell_width * f32::from(span);
                (
                    Bounds::new(point(x, y), size(width, line_height)),
                    shape,
                    hsla(palette.theme.cursor),
                )
            });
            // A block hides its cell's text: that text is drawn over it in the cursor-text
            // colour, as ghostty does.
            let cursor_text = cursor_prepared
                .filter(|(_, shape, _)| *shape == CursorShape::Block)
                .map(|_| CursorText {
                    row: cursor.row,
                    start: cursor.col,
                    end: cursor.col.saturating_add(span),
                    color: hsla(palette.theme.cursor_text),
                });

            // The input method's composition, underlined at the cursor (what Terminal.app does).
            let mut overlay: Vec<(Point<Pixels>, ShapedLine)> = Vec::new();
            if let Some(text) = marked.filter(|_| cursor_visible) {
                let style = &CellStyle::DEFAULT;
                let mut run = text_run(text.len(), style, look(false), &mut cache.contrast);
                run.underline = Some(UnderlineStyle {
                    thickness: px(1.0),
                    color: Some(run.color),
                    wavy: false,
                });
                let shaped = text_system.shape_line(
                    SharedString::from(text.to_owned()),
                    font_size,
                    &[run],
                    Some(cell_width),
                );
                let at = point(
                    origin.x + cell_width * f32::from(cursor.col),
                    origin.y + line_height * f32::from(cursor.row),
                );
                overlay.push((at, shaped));
            }
            overlay.extend(captions);

            let grid_bounds = Bounds::new(
                origin,
                size(cell_width * f32::from(cols), line_height * f32::from(rows)),
            );
            let images = placed
                .iter()
                .filter_map(|p| {
                    let (bounds, image_bounds) =
                        placement_bounds(&metrics, &p.placement, (p.width, p.height), view_offset)?;
                    Some(PreparedImage {
                        bounds: bounds.intersect(&grid_bounds),
                        image_bounds,
                        image: Arc::clone(&p.image),
                        over_text: p.placement.z >= 0,
                    })
                })
                .collect();

            Prepared {
                metrics,
                grid,
                zoom,
                font_size,
                raster_size,
                rows: prepared_rows,
                cursor: cursor_prepared,
                cursor_text,
                background: hsla(palette.theme.bg),
                link: hsla(palette.theme.fg),
                scrollbar: scrollbar.and_then(|(color, history, offset)| {
                    scrollbar_thumb(&metrics, history, offset).map(|thumb| (thumb, color))
                }),
                failed,
                failed_look,
                overlay,
                shown: predicted
                    .map(|(guesses, _)| guesses.iter().map(|p| p.seq).collect())
                    .unwrap_or_default(),
                blinking,
                images,
                hitbox,
            }
        };
        #[cfg(test)]
        {
            let masks = cache.sprites.len().saturating_sub(sprite_masks);
            let probe = cx.global_mut::<Probe>();
            probe.rows_prepared = prepared.rows.len();
            probe.captions = caption_texts;
            probe.shaped = probe.shaped.saturating_add(shaped);
            probe.sprite_masks = probe.sprite_masks.saturating_add(masks);
        }
        *cx.global_mut::<ShapeCache>() = cache;
        // The clock ticks only while a painted frame has something to blink.
        self.view.update(cx, |view, cx| view.blinking(prepared.blinking, cx));
        if fading {
            window.request_animation_frame();
        }
        prepared
    }

    fn paint(
        &mut self,
        _global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request: &mut (),
        prepaint: &mut Prepared,
        window: &mut Window,
        cx: &mut App,
    ) {
        let prepared = prepaint;
        let m = prepared.metrics;
        let grid = prepared.grid;
        // Registers the view as the text-input target while it is focused (soft keyboard on
        // iOS, input-method commits on macOS).
        let focus = self.view.read(cx).focus_handle(cx);
        window.handle_input(&focus, ElementInputHandler::new(bounds, self.view.clone()), cx);
        // Touch: a long press over the text starts a selection (a plain drag pans the canvas).
        // Claiming it at `Started` keeps the rest of the gesture away from the canvas.
        let view = self.view.clone();
        let hitbox = prepared.hitbox.clone();
        window.on_mouse_event(move |event: &LongPressEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble {
                return;
            }
            // Only a press that starts on the grid itself, not on what is drawn over it.
            if event.phase == TouchPhase::Started
                && !hitbox.is_hovered_at(event.start_position, window)
            {
                return;
            }
            let claimed = view.update(cx, |view, cx| view.long_press(event, window, cx));
            if claimed {
                window.prevent_default();
                cx.stop_propagation();
            }
        });
        // A drag is followed wherever the pointer goes (the div's own move listener stops at
        // its edge): the selection keeps growing and scrolls past the top or bottom. So is
        // the pointer's way to the scrollbar and away from it, off the card too.
        let view = self.view.clone();
        let grid_hitbox = prepared.hitbox.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble {
                return;
            }
            if event.pressed_button.is_some() {
                view.update(cx, |view, cx| view.drag_move(event, cx));
            }
            let inside = bounds.contains(&event.position);
            let near = inside && near_scrollbar(&m, event.position);
            view.update(cx, |view, cx| view.pointer_near_scrollbar(near, cx));
            // Over the grid the hovered block follows the pointer. Where something covers the
            // grid it holds, so the block's own facts and the sticky header over it keep it.
            if !inside {
                view.update(cx, |view, cx| view.pointer_over(None, cx));
            } else if grid_hitbox.is_hovered(window) {
                view.update(cx, |view, cx| view.pointer_over(Some(event.position), cx));
            }
        });
        // Out of the window the pointer is not near anything, and no move says so.
        let view = self.view.clone();
        window.on_mouse_event(move |_: &MouseExitEvent, phase, _window, cx| {
            if phase == DispatchPhase::Bubble {
                view.update(cx, |view, cx| {
                    view.pointer_near_scrollbar(false, cx);
                    view.pointer_over(None, cx);
                });
            }
        });
        window.paint_quad(fill(bounds, prepared.background));
        let look = prepared.failed_look;
        for &(top, height) in &prepared.failed {
            let band = Bounds::new(point(bounds.origin.x, top), size(bounds.size.width, height));
            window.paint_quad(fill(band, look.wash));
            window
                .paint_quad(fill(Bounds::new(band.origin, size(look.bar_width, height)), look.bar));
        }
        for row in &prepared.rows {
            if let Some(color) = row.separator {
                let w = m.cell_width * f32::from(m.cols);
                window.paint_quad(fill(
                    Bounds::new(point(m.origin.x, row.y), size(w, px(1.0))),
                    color,
                ));
            }
            for (start, end, color) in &row.quads {
                let x = m.origin.x + m.cell_width * f32::from(*start);
                let w = m.cell_width * f32::from(end.saturating_sub(*start));
                window
                    .paint_quad(fill(Bounds::new(point(x, row.y), size(w, m.line_height)), *color));
            }
        }
        // Images under the text (kitty `z < 0`), over the cell backgrounds.
        for image in prepared.images.iter().filter(|i| !i.over_text) {
            paint_placed(window, image);
        }
        if let Some((cursor_bounds, shape, color)) = prepared.cursor {
            let quad = match shape {
                CursorShape::Block => fill(cursor_bounds, color),
                CursorShape::Bar => fill(
                    Bounds::new(cursor_bounds.origin, size(grid.cursor_thickness, m.line_height)),
                    color,
                ),
                CursorShape::Underline => fill(
                    Bounds::new(
                        point(
                            cursor_bounds.origin.x,
                            cursor_bounds.origin.y + m.line_height - grid.cursor_thickness,
                        ),
                        size(cursor_bounds.size.width, grid.cursor_thickness),
                    ),
                    color,
                ),
                CursorShape::BlockHollow => gpui::outline(cursor_bounds, color, BorderStyle::Solid),
            };
            window.paint_quad(quad);
        }
        // Underlines, under the glyphs so a descender crosses the line rather than being
        // cut by it (ghostty draws them in the same order).
        for row in &prepared.rows {
            paint_decorations(window, &m, row, Layer::Under);
        }
        // Box drawing, blocks, Braille and Powerline: drawn from the cell in its colours, so a
        // border never seams between rows and a heavy line keeps its weight. Settled, each is
        // its atlas tile, rasterised once and tinted here (one sprite, like a glyph); in motion,
        // its geometry.
        let cell = sprite::Cell {
            w: f32::from(m.cell_width),
            h: f32::from(m.line_height),
            thickness: f32::from(grid.underline.thickness),
            scale: m.pixel_scale,
        };
        let cursor_text = prepared.cursor_text;
        // One layer for them, as for the glyphs below.
        if prepared.rows.iter().any(|row| !row.sprites.is_empty()) {
            window.paint_layer(bounds, |window| {
                for row in &prepared.rows {
                    for sprite in &row.sprites {
                        let x = m.origin.x + m.cell_width * f32::from(sprite.col);
                        let origin = point(x, row.y);
                        let color = CursorText::over(cursor_text, row.row, sprite.col, sprite.fg);
                        let Some(tile) = &sprite.tile else {
                            paint_sprite(window, origin, cell, sprite.ch, color);
                            continue;
                        };
                        let bounds = Bounds::new(origin, size(m.cell_width, m.line_height));
                        let (path, svg) = (tile.path.clone(), Some(&*tile.svg));
                        let unit = TransformationMatrix::unit();
                        if let Err(e) = window.paint_svg(bounds, path, svg, unit, color, cx) {
                            tracing::debug!(error = %e, "paint sprite");
                        }
                    }
                }
            });
        }
        // The words: every glyph at the derived baseline, from the base-size shaping, at the
        // zoomed size. Nothing is shaped here and the word cache never sees the zoom. One
        // layer for all of them: a primitive outside a layer costs a bounds-tree insert of its
        // own (GPUI gives each line it paints a layer for the same reason), and the glyphs
        // still land above the quads painted before and below what is painted after.
        let (zoom, font_size, raster) = (prepared.zoom, prepared.font_size, prepared.raster_size);
        window.paint_layer(bounds, |window| {
            for row in &prepared.rows {
                let baseline = row.y + grid.baseline;
                for (col, word) in &row.segments {
                    let origin = point(m.origin.x + m.cell_width * f32::from(*col), baseline);
                    for glyph in &word.glyphs {
                        let at = glyph_origin(origin, glyph.position, zoom);
                        let cell = col.saturating_add(glyph.col);
                        let color = CursorText::over(cursor_text, row.row, cell, glyph.color);
                        let painted = if glyph.emoji {
                            window.paint_emoji(at, glyph.font, glyph.id, font_size)
                        } else if raster == font_size {
                            window.paint_glyph(at, glyph.font, glyph.id, font_size, color)
                        } else {
                            // In motion: the nearest rung's raster, stretched (the fork).
                            let (f, g) = (glyph.font, glyph.id);
                            window.paint_glyph_scaled(at, f, g, raster, font_size, color)
                        };
                        if let Err(e) = painted {
                            tracing::debug!(error = %e, "paint glyph");
                        }
                    }
                }
            }
        });
        // Strikethroughs, over the glyphs, where the font's metrics put them.
        for row in &prepared.rows {
            paint_decorations(window, &m, row, Layer::Over);
        }
        // Images over the text (kitty `z ≥ 0`, the default: a picture covers what it sits on).
        for image in prepared.images.iter().filter(|i| i.over_text) {
            paint_placed(window, image);
        }
        // The ⌘-hover link underline joins them, in the text colour.
        for row in &prepared.rows {
            if let Some((start, end)) = row.link {
                let x = m.origin.x + m.cell_width * f32::from(start);
                let w = m.cell_width * f32::from(end.saturating_sub(start));
                let y = row.y + grid.underline.y;
                window.paint_quad(fill(
                    Bounds::new(point(x, y), size(w, grid.underline.thickness)),
                    prepared.link,
                ));
            }
        }
        for (at, line) in &prepared.overlay {
            // Cover whatever the worker currently shows there, then draw the preview or caption.
            window.paint_quad(fill(
                Bounds::new(*at, size(line.width.max(m.cell_width), m.line_height)),
                prepared.background,
            ));
            if let Err(e) = line.paint(*at, m.line_height, TextAlign::Left, None, window, cx) {
                tracing::debug!(error = %e, "paint overlay text");
            }
        }
        if let Some((thumb, color)) = prepared.scrollbar {
            let radius = thumb.size.width / 2.0;
            window.paint_quad(quad(
                thumb,
                radius,
                color,
                Edges::default(),
                Hsla::transparent_black(),
                BorderStyle::default(),
            ));
        }
        if self.view.read(cx).bell_flashing() {
            // The visual bell: the text colour laid thinly over the whole grid.
            window.paint_quad(fill(bounds, Hsla { a: alpha::FAINT, ..prepared.link }));
        }
        // Keystroke → paint is timed when this frame reaches the glass, not now.
        if self.view.read(cx).latency_waiting() {
            let shown = std::mem::take(&mut prepared.shown);
            let ack = self.view.read(cx).state().input_ack();
            let view = self.view.clone();
            crate::shown::after_paint(window, cx, move |frame, cx| {
                view.update(cx, |view, _cx| view.presented(frame, &shown, ack));
            });
        }
    }
}

fn paint_placed(window: &mut Window, image: &PreparedImage) {
    let texture = Arc::clone(&image.image);
    if let Err(e) =
        window.paint_image(image.bounds, image.image_bounds, Corners::default(), texture, 0, false)
    {
        tracing::debug!(error = %e, "paint placed image");
    }
}

/// Where a placement is painted.
///
/// The rectangle its shown part fills, and the rectangle the whole image (`image` pixels
/// wide and high) would fill at that scale, so the renderer samples the placement's source
/// rectangle. The worker lays placements out in its cell pixels (device pixels of the unzoomed
/// grid, `pixel_scale` of them per point) and in viewport rows; a view scrolled
/// `view_offset` rows into its history shows them that far down. `None` when nothing would
/// show (an empty source or size).
#[expect(clippy::cast_precision_loss, reason = "pixel counts and cell positions, far below 2^24")]
#[must_use]
pub fn placement_bounds(
    metrics: &CellMetrics,
    placement: &Placement,
    image: (u32, u32),
    view_offset: u64,
) -> Option<(Bounds<Pixels>, Bounds<Pixels>)> {
    let (source, painted) = (placement.source, (placement.width, placement.height));
    if source.width == 0 || source.height == 0 || painted.0 == 0 || painted.1 == 0 {
        return None;
    }
    let scale = metrics.pixel_scale;
    let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    let pt = |v: u32| px(v as f32 / scale);
    let row = i64::from(placement.row).checked_add(i64::try_from(view_offset).ok()?)?;
    let left =
        metrics.origin.x + metrics.cell_width * (placement.col as f32) + pt(placement.x_offset);
    let top = metrics.origin.y + metrics.line_height * (row as f32) + pt(placement.y_offset);
    let shown = size(pt(painted.0), pt(painted.1));
    let (sx, sy) = (
        f32::from(shown.width) / source.width as f32,
        f32::from(shown.height) / source.height as f32,
    );
    let whole = Bounds::new(
        point(left - px(source.x as f32 * sx), top - px(source.y as f32 * sy)),
        size(px(image.0 as f32 * sx), px(image.1 as f32 * sy)),
    );
    Some((Bounds::new(point(left, top), shown), whole))
}

/// The scrollbar thumb's width, in cells of the grid's advance, and its least height in rows.
const THUMB_CELLS: f32 = 0.6;
const THUMB_MIN_ROWS: f32 = 1.5;
/// How far left of the grid's right edge the pointer brings the scrollbar up, in cells: about
/// the width of a macOS overlay scroller's track.
const SCROLLBAR_REACH_CELLS: f32 = 2.0;

/// Paint one cell-drawn glyph's geometry at `origin` in `fg` (see [`sprite`]).
fn paint_sprite(
    window: &mut Window,
    origin: Point<Pixels>,
    cell: sprite::Cell,
    ch: char,
    fg: Hsla,
) {
    let Some(shapes) = sprite::shapes(ch, cell) else { return };
    let at = |(x, y): (f32, f32)| point(origin.x + px(x), origin.y + px(y));
    for shape in shapes {
        match shape {
            sprite::Shape::Rect { x, y, w, h, ink, round } => {
                let color = match ink {
                    sprite::Ink::Fg => fg,
                    sprite::Ink::Shade(a) => Hsla { a: fg.a * a, ..fg },
                };
                let mut quad = fill(Bounds::new(at((x, y)), size(px(w), px(h))), color);
                if round {
                    quad.corner_radii = Corners::all(px(w / 2.0));
                }
                window.paint_quad(quad);
            }
            sprite::Shape::Stroke { points, thickness } => {
                let mut path = PathBuilder::stroke(px(thickness));
                let mut points = points.into_iter();
                if let Some(first) = points.next() {
                    path.move_to(at(first));
                }
                for p in points {
                    path.line_to(at(p));
                }
                if let Ok(path) = path.build() {
                    window.paint_path(path, fg);
                }
            }
            sprite::Shape::Arc { from, to, r, sweep, thickness } => {
                let mut path = PathBuilder::stroke(px(thickness));
                path.move_to(at(from));
                path.arc_to(point(px(r), px(r)), px(0.0), false, sweep, at(to));
                if let Ok(path) = path.build() {
                    window.paint_path(path, fg);
                }
            }
            sprite::Shape::Poly { points, ink } => {
                let color = match ink {
                    sprite::Ink::Fg => fg,
                    sprite::Ink::Shade(a) => Hsla { a: fg.a * a, ..fg },
                };
                let mut path = PathBuilder::fill();
                let mut points = points.into_iter();
                if let Some(first) = points.next() {
                    path.move_to(at(first));
                }
                for p in points {
                    path.line_to(at(p));
                }
                path.close();
                if let Ok(path) = path.build() {
                    window.paint_path(path, color);
                }
            }
        }
    }
}

/// Paint one row's decorations on `layer`, where the font's metrics put them.
fn paint_decorations(window: &mut Window, m: &CellMetrics, row: &PreparedRow, layer: Layer) {
    let over = layer == Layer::Over;
    for deco in row.decorations.iter().filter(|d| d.over == over) {
        let x = m.origin.x + m.cell_width * f32::from(deco.start);
        let w = m.cell_width * f32::from(deco.end.saturating_sub(deco.start));
        if deco.wavy {
            let style =
                UnderlineStyle { thickness: deco.thickness, color: Some(deco.color), wavy: true };
            window.paint_underline(point(x, row.y + deco.y), w, &style);
        } else {
            let bounds = Bounds::new(point(x, row.y + deco.y), size(w, deco.thickness));
            window.paint_quad(fill(bounds, deco.color));
        }
    }
}

/// The scrollbar's thumb over the grid's right edge: none without history. The track is the
/// grid's height; the thumb's share of it is the screen's share of the whole (screen plus
/// history), at least `THUMB_MIN_ROWS` tall; its top sits where the viewport is in the whole.
#[must_use]
pub fn scrollbar_thumb(m: &CellMetrics, history: u64, offset: u64) -> Option<Bounds<Pixels>> {
    if history == 0 || m.rows == 0 {
        return None;
    }
    #[expect(clippy::cast_precision_loss, reason = "line counts, far below 2^52")]
    let (rows, history_f) = (f64::from(m.rows), history as f64);
    let line = f64::from(f32::from(m.line_height));
    let track = line * rows;
    let height =
        (track * rows / (rows + history_f)).max(line * f64::from(THUMB_MIN_ROWS)).min(track);
    #[expect(clippy::cast_precision_loss, reason = "line counts, far below 2^52")]
    let above = (history.saturating_sub(offset)) as f64;
    let top = (track - height) * above / history_f;
    let width = m.cell_width * THUMB_CELLS;
    let x = m.origin.x + m.cell_width * f32::from(m.cols) - width;
    #[expect(clippy::cast_possible_truncation, reason = "points, well inside f32")]
    let (y, h) = (px(top as f32), px(height as f32));
    Some(Bounds::new(point(x, m.origin.y + y), size(width, h)))
}

/// Whether `at` is where the pointer brings the scrollbar up: level with the grid, within
/// `SCROLLBAR_REACH_CELLS` of its right edge or anywhere right of it (the card's inset).
#[must_use]
pub fn near_scrollbar(m: &CellMetrics, at: Point<Pixels>) -> bool {
    let right = m.origin.x + m.cell_width * f32::from(m.cols);
    let bottom = m.origin.y + m.line_height * f32::from(m.rows);
    at.x >= right - m.cell_width * SCROLLBAR_REACH_CELLS && at.y >= m.origin.y && at.y < bottom
}

/// The viewport offset (lines from the bottom) that puts the thumb's top at `y`: the inverse
/// of [`scrollbar_thumb`], clamped to the track.
#[must_use]
pub fn offset_for_thumb(m: &CellMetrics, history: u64, y: Pixels) -> u64 {
    let Some(thumb) = scrollbar_thumb(m, history, 0) else { return 0 };
    let travel = f64::from(f32::from(m.line_height))
        .mul_add(f64::from(m.rows), -f64::from(f32::from(thumb.size.height)));
    if travel <= 0.0 {
        return 0;
    }
    let along = f64::from(f32::from(y - m.origin.y)).clamp(0.0, travel) / travel;
    #[expect(clippy::cast_precision_loss, reason = "line counts, far below 2^52")]
    let above = (along * history as f64).round();
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "0 ≤ above ≤ history"
    )]
    let above = above as u64;
    history.saturating_sub(above)
}

/// How many rows past the grid's top (positive) or bottom (negative) a pointer at `y` is;
/// zero inside the grid.
#[must_use]
pub fn rows_past_edge(m: &CellMetrics, y: Pixels) -> i64 {
    let line = f32::from(m.line_height).max(1.0);
    let top = f32::from(m.origin.y);
    let bottom = top + line * f32::from(m.rows);
    let y = f32::from(y);
    #[expect(clippy::cast_possible_truncation, reason = "a ceiling of a small quotient")]
    if y < top {
        ((top - y) / line).ceil() as i64
    } else if y >= bottom {
        (((y - bottom) / line).floor() as i64).saturating_add(1).saturating_neg()
    } else {
        0
    }
}

/// How many shaped words the cache holds (tests: a word shaped once serves every row).
#[cfg(test)]
pub fn cached_words(cx: &App) -> usize {
    cx.try_global::<ShapeCache>().map_or(0, |cache| cache.words.len())
}

/// How many words were shaped since the app started (tests: a word seen before shapes nothing).
#[cfg(test)]
pub fn shaped_words(cx: &App) -> usize {
    cx.try_global::<Probe>().map_or(0, |probe| probe.shaped)
}

/// How many sprite masks were written since the app started (tests: once per character and
/// cell, not per frame).
#[cfg(test)]
pub fn sprite_masks(cx: &App) -> usize {
    cx.try_global::<Probe>().map_or(0, |probe| probe.sprite_masks)
}

/// How many times the installed fonts were listed (tests: once for every view of the app).
#[cfg(test)]
pub fn family_picks(cx: &App) -> usize {
    cx.try_global::<Probe>().map_or(0, |probe| probe.picks)
}

/// How many rows the last prepaint built (tests: only the rows inside the clip).
#[cfg(test)]
pub fn rows_prepared(cx: &App) -> usize {
    cx.try_global::<Probe>().map_or(0, |probe| probe.rows_prepared)
}

/// The "took" captions the last prepaint drew, top row first (tests).
#[cfg(test)]
pub fn captions_drawn(cx: &App) -> Vec<String> {
    cx.try_global::<Probe>().map_or_else(Vec::new, |probe| probe.captions.clone())
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use slopty_grid::Line;

    use super::*;

    /// The metrics of a grid laid out at `scale` and painted at `zoom`, `JetBrains Mono` 13 pt:
    /// an 8 × 17 device-pixel cell, so 8/scale × 17/scale points, times the zoom.
    fn metrics(scale: f32, zoom: f32) -> CellMetrics {
        CellMetrics {
            origin: point(px(10.0), px(20.0)),
            cell_width: px(8.0 / scale * zoom),
            line_height: px(17.0 / scale * zoom),
            cols: 80,
            rows: 24,
            pixel_scale: scale / zoom,
            face: metrics::Face::default(),
            face_size: 13.0 * scale,
        }
    }

    /// A placement's pixels are the worker's cell pixels: at display scale 2 a 16 × 32 image
    /// paints 8 × 16 points at its cell plus its offset; a source rectangle shifts the whole
    /// image so that part lands in the placement; a scrolled view moves it down its rows.
    #[test]
    fn a_placement_is_painted_at_its_cell_in_the_workers_pixels() {
        use slopty_proto::terminal::PixelRect;
        let m = metrics(2.0, 1.0);
        let p = Placement {
            image: 1,
            generation: 1,
            col: 2,
            row: 1,
            cols: 1,
            rows: 1,
            x_offset: 4,
            y_offset: 0,
            width: 16,
            height: 32,
            source: PixelRect { x: 0, y: 0, width: 16, height: 32 },
            z: 0,
        };
        let (bounds, whole) = placement_bounds(&m, &p, (16, 32), 0).expect("shown");
        // Cell (2, 1) is at 10 + 2 × 4 = 18, 20 + 8.5; the offset adds 4 px = 2 pt.
        assert_eq!(bounds, Bounds::new(point(px(20.0), px(28.5)), size(px(8.0), px(16.0))));
        assert_eq!(whole, bounds, "the whole image is shown");
        // The right half of the image: the whole image starts one half-width to the left.
        let half = Placement { source: PixelRect { x: 8, y: 0, width: 8, height: 32 }, ..p };
        let (bounds, whole) = placement_bounds(&m, &half, (16, 32), 0).expect("shown");
        assert_eq!(bounds.size, size(px(8.0), px(16.0)));
        assert_eq!(whole, Bounds::new(point(px(12.0), px(28.5)), size(px(16.0), px(16.0))));
        // Scrolled three rows into history: three rows further down.
        let (scrolled, _) = placement_bounds(&m, &p, (16, 32), 3).expect("shown");
        assert_eq!(scrolled.origin.y, px(54.0));
        // An empty source shows nothing.
        let none = Placement { source: PixelRect::default(), ..p };
        assert_eq!(placement_bounds(&m, &none, (16, 32), 0), None);
    }

    /// The thumb is the screen's share of the whole, never thinner than a row and a half,
    /// riding the track from the oldest line (top) to the newest (bottom); the drag maps back
    /// to the offset that put it there, clamped to the track; a pointer past the grid's edge
    /// counts rows past it.
    #[test]
    fn the_scrollbar_thumb_tracks_the_viewport_and_maps_back() {
        let m = metrics(1.0, 1.0);
        assert_eq!(scrollbar_thumb(&m, 0, 0), None, "no history, no thumb");
        let track = f32::from(m.line_height) * 24.0;
        // 24 rows over 24 + 24: half the track, at its end while following output.
        let thumb = scrollbar_thumb(&m, 24, 0).expect("a thumb");
        assert!((f32::from(thumb.size.height) - track / 2.0).abs() < 0.01);
        assert!((f32::from(thumb.origin.y - m.origin.y) - track / 2.0).abs() < 0.01);
        assert!((f32::from(thumb.origin.x + thumb.size.width - m.origin.x) - 640.0).abs() < 0.01);
        // Scrolled all the way up: at the top.
        let top = scrollbar_thumb(&m, 24, 24).expect("a thumb");
        assert_eq!(top.origin.y, m.origin.y);
        // A huge history: the least height, and the round trip holds within a line.
        let tiny = scrollbar_thumb(&m, 100_000, 40_000).expect("a thumb");
        assert!(
            (f32::from(tiny.size.height) - f32::from(m.line_height) * THUMB_MIN_ROWS).abs() < 0.01
        );
        let back = offset_for_thumb(&m, 100_000, tiny.origin.y);
        assert!(back.abs_diff(40_000) <= 100_000 / 400, "{back}");
        assert_eq!(offset_for_thumb(&m, 24, m.origin.y - px(500.0)), 24, "above the track: oldest");
        assert_eq!(offset_for_thumb(&m, 24, m.origin.y + px(9_000.0)), 0, "below: newest");
        assert_eq!(offset_for_thumb(&m, 0, m.origin.y), 0, "no history: nowhere to go");

        assert_eq!(rows_past_edge(&m, m.origin.y + px(5.0)), 0);
        assert_eq!(rows_past_edge(&m, m.origin.y - px(1.0)), 1);
        assert_eq!(rows_past_edge(&m, m.origin.y - px(17.5)), 2);
        assert_eq!(rows_past_edge(&m, m.origin.y + px(17.0 * 24.0)), -1, "the first row below");
        assert_eq!(rows_past_edge(&m, m.origin.y + px(17.0 * 25.5)), -2);
    }

    /// A pixel mouse report is in the units the worker measures in: device pixels of the *fitted*
    /// grid, whatever the display's scale and however far the canvas has zoomed. The worker
    /// divides by the cell size it was told (8 × 17 here), so the column it reads back is the
    /// column the pointer is over.
    #[test]
    fn a_pixel_mouse_report_is_in_the_cell_size_the_worker_was_told() {
        for scale in [1.0, 2.0] {
            for zoom in [0.5, 1.0, 2.0] {
                let m = metrics(scale, zoom);
                // The left edge of column 10, row 3.
                let at = point(m.origin.x + m.cell_width * 10.0, m.origin.y + m.line_height * 3.0);
                let (x, y) = m.pixel_at(at);
                assert_eq!((x / 8, y / 17), (10, 3), "at scale {scale} zoom {zoom}");
                assert_eq!(m.cell_at(at), Some((10, 3)), "at scale {scale} zoom {zoom}");
                assert_eq!(m.pixel_at(m.origin), (0, 0));
            }
        }
    }

    /// A 17 pt cell squeezed to 10 pt (`mono_line_height` 0.6): the descender and the underline
    /// end below the box, so a row whose box is just above the clip but whose underline is
    /// inside is still prepared; one whose whole ink is above is not.
    #[test]
    fn a_row_is_culled_on_its_ink_not_its_cell_box() {
        let grid = Grid {
            cell_width: px(8.0),
            line_height: px(10.0),
            baseline: px(9.0),
            underline: metrics::Line { y: px(11.0), thickness: px(1.0) },
            strikethrough: metrics::Line { y: px(5.0), thickness: px(1.0) },
            overline: metrics::Line { y: px(-1.0), thickness: px(1.0) },
            cursor_thickness: px(1.0),
        };
        let face = metrics::Face { ascent: 10.0, descent: -3.0, ..metrics::Face::default() };
        let (above, below) = row_overhang(&grid, &face, 1.0);
        assert_eq!((above, below), (px(1.0), px(2.0)), "descender to 12, underline to 12");
        // Clip from 100: the box of a row at 88..98 is above it, its underline 99..100 is not.
        let (top, bottom) = (px(100.0), px(300.0));
        assert!(row_in_band(px(89.0), px(10.0), (above, below), top, bottom), "underline shows");
        assert!(!row_in_band(px(88.0), px(10.0), (above, below), top, bottom), "nothing shows");
        assert!(px(89.0) + px(10.0) <= top, "the cell box alone would have culled it");
        // The bottom edge: a row whose box starts at the clip's end still shows its overline.
        assert!(row_in_band(px(300.5), px(10.0), (above, below), top, bottom));
        assert!(!row_in_band(px(301.0), px(10.0), (above, below), top, bottom));
        // Overhang scales with the zoom (device pixels over `pixel_scale`), never negative.
        let (a2, b2) = row_overhang(&grid, &face, 2.0);
        assert_eq!((a2, b2), (px(1.0), px(2.0)), "the strokes, not the face, reach furthest");
        let tight = metrics::Face { ascent: 5.0, descent: -0.5, ..metrics::Face::default() };
        let roomy = Grid { underline: metrics::Line { y: px(8.0), thickness: px(1.0) }, ..grid };
        assert_eq!(row_overhang(&roomy, &tight, 1.0), (px(1.0), px(0.0)));
    }

    /// A word shaped at 13 pt on an 8 pt cell paints at 26 pt with the glyphs 16 pt apart,
    /// from the same shaping: the position scales, the origin is the cell grid's.
    #[test]
    fn a_glyph_shaped_at_the_base_size_lands_on_the_zoomed_cell() {
        let origin = point(px(100.0), px(50.0));
        assert_eq!(glyph_origin(origin, point(px(0.0), px(0.0)), 2.0), origin);
        assert_eq!(glyph_origin(origin, point(px(8.0), px(0.0)), 2.0), point(px(116.0), px(50.0)));
        assert_eq!(glyph_origin(origin, point(px(24.0), px(0.0)), 0.5), point(px(112.0), px(50.0)));
        assert_eq!(glyph_origin(origin, point(px(8.0), px(0.0)), 1.0), point(px(108.0), px(50.0)));
    }

    /// The colour of a glyph is the colour of the style run its byte falls in.
    #[test]
    fn a_word_remembers_the_colour_of_every_byte() {
        let red = gpui::red();
        let blue = gpui::blue();
        let colors = [(2, red), (5, blue)];
        assert_eq!(color_at(&colors, 0), red);
        assert_eq!(color_at(&colors, 1), red);
        assert_eq!(color_at(&colors, 2), blue);
        assert_eq!(color_at(&colors, 4), blue);
        assert_eq!(color_at(&colors, 9), blue, "past the end: the last run");
        assert_eq!(color_at(&[], 0), gpui::black());
    }

    fn cells(text: &str) -> Vec<Cell> {
        Line::from_text(text, 12, CellStyle::DEFAULT).cells
    }

    fn cols(text: &str) -> Vec<(u16, usize)> {
        segments(&cells(text)).map(|(col, word)| (col, word.len())).collect()
    }

    #[test]
    fn rows_split_into_words_at_plain_spaces() {
        assert_eq!(cols("foo bar  baz"), vec![(0, 3), (4, 3), (9, 3)]);
        assert_eq!(cols("  x"), vec![(2, 1)]);
        assert_eq!(cols(""), vec![], "a blank row has nothing to shape");
    }

    #[test]
    fn digits_stand_alone_inside_and_beside_words() {
        assert_eq!(cols("ab12 3"), vec![(0, 2), (2, 1), (3, 1), (5, 1)]);
        assert_eq!(cols("0xf"), vec![(0, 1), (1, 2)]);
        let mut row = cells("12");
        row[0].style.underline = Underline::Curly;
        row[1].style.underline = Underline::Curly;
        assert_eq!(
            segments(&row).count(),
            2,
            "a curly underline changes nothing: it is a decoration"
        );
    }

    /// The cursor covers both columns of a wide character and one of anything else, so a
    /// block over a CJK glyph does not stop halfway through it.
    #[test]
    fn the_cursor_covers_a_wide_character_whole() {
        let mut line = Line::from_text("a", 4, CellStyle::DEFAULT);
        line.cells[1] = Cell::wide("字", CellStyle::DEFAULT);
        line.cells[2] = Cell { width: CellWidth::SpacerTail, ..Cell::BLANK };
        assert_eq!(cursor_span(Some(&line), 0), 1, "narrow");
        assert_eq!(cursor_span(Some(&line), 1), 2, "the wide head");
        assert_eq!(cursor_span(Some(&line), 2), 1, "its tail is a cell like any other");
        assert_eq!(cursor_span(Some(&line), 9), 1, "past the line");
        assert_eq!(cursor_span(None, 1), 1, "a row still being fetched");
    }

    /// Underlines go under the glyphs and strikethroughs over them, and a stroke joins only
    /// a run on its own layer.
    #[test]
    fn underlines_lie_under_the_glyphs_and_strikethroughs_over() {
        let line = metrics::Line { y: px(10.0), thickness: px(1.0) };
        let color = Hsla::default();
        let mut out = Vec::new();
        stroke(&mut out, 0, color, line, false, Layer::Under);
        stroke(&mut out, 1, color, line, false, Layer::Over);
        stroke(&mut out, 1, color, line, false, Layer::Under);
        assert_eq!(out.len(), 2, "each layer joins its own run");
        assert_eq!((out[0].start, out[0].end, out[0].over), (0, 2, false));
        assert_eq!((out[1].start, out[1].end, out[1].over), (1, 2, true));
    }

    /// A box-drawing cell is drawn by the element, not shaped: it ends the word before it
    /// like a space and is not a word itself.
    #[test]
    fn a_box_drawing_cell_is_drawn_not_shaped() {
        assert_eq!(cols("a─b"), vec![(0, 1), (2, 1)]);
        assert_eq!(cols("│ab│"), vec![(1, 2)]);
        assert_eq!(cols("▄▄"), vec![]);
        assert_eq!(drawn_here(&Cell::narrow('╭', CellStyle::DEFAULT)), Some('╭'));
        assert_eq!(drawn_here(&Cell::narrow('a', CellStyle::DEFAULT)), None);
    }

    #[test]
    fn no_decoration_keeps_a_space_in_its_word() {
        let mut row = cells("a b");
        row[1].style.underline = Underline::Curly;
        assert_eq!(segments(&row).count(), 2, "the wave is a decoration drawn from the cells");
        let mut row = cells("a b");
        row[1].style.underline = Underline::Single;
        assert_eq!(segments(&row).count(), 2, "a straight underline is a decoration quad");
        let mut row = cells("a b");
        row[1].style.flags |= StyleFlags::STRIKETHROUGH;
        assert_eq!(segments(&row).count(), 2, "so is a strikethrough");
        let mut row = cells("a b");
        row[1].style.bg = slopty_grid::Color::Palette(1);
        assert_eq!(segments(&row).count(), 2, "a background is a quad, not a glyph");
    }

    /// A wide cell — a CJK character, an emoji, one with a variation selector, a ZWJ family,
    /// a flag — spans two cells, so the word's next glyph lands two cells on, whatever the
    /// cluster's code point count and however many glyphs the font shaped it to.
    #[gpui::test]
    fn a_wide_cluster_takes_two_cells_whatever_its_code_points(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| fonts::install(cx).expect("fonts"));
        let (_, cx) = cx.add_window_view(|_, _| gpui::Empty);
        let width = px(8.0);
        for cluster in ["日", "😀", "❤️", "👨\u{200d}👩\u{200d}👧", "🇻🇳"] {
            let cells = [
                Cell::wide(cluster, CellStyle::DEFAULT),
                Cell {
                    text: slopty_grid::CellText::EMPTY,
                    style: CellStyle::DEFAULT,
                    width: CellWidth::SpacerTail,
                },
                Cell::narrow('x', CellStyle::DEFAULT),
            ];
            let word = cx.update(|window, _| {
                let colors = Colors::from(&Theme::default().terminal);
                let look = Look {
                    family: fonts::MONO_FAMILY,
                    ligatures: true,
                    palette: &colors,
                    blink_off: false,
                };
                shape_cells(
                    window.text_system(),
                    &cells,
                    px(13.0),
                    width,
                    look,
                    &mut Contrast::default(),
                )
            });
            let last = word.glyphs.last().expect("the x");
            assert_eq!(last.position.x, width * 2.0, "{cluster:?} then x");
            assert_eq!(word.glyphs.first().expect("the cluster").position.x, px(0.0));
        }
    }

    #[test]
    fn a_wide_cell_and_its_spacer_stay_together() {
        let wide = |c: char| Cell {
            text: slopty_grid::CellText::from_char(c),
            style: CellStyle::DEFAULT,
            width: CellWidth::Wide,
        };
        let tail = Cell {
            text: slopty_grid::CellText::EMPTY,
            style: CellStyle::DEFAULT,
            width: CellWidth::SpacerTail,
        };
        let row = [wide('日'), tail.clone(), wide('本'), tail, Cell::BLANK, cells("x")[0].clone()];
        let words: Vec<_> = segments(&row).collect();
        assert_eq!(words.len(), 2, "{words:?}");
        assert_eq!(words[0].0, 0);
        assert_eq!(words[0].1.len(), 4, "two wide cells with their spacer tails");
        assert_eq!(words[1].0, 5);
    }

    #[test]
    fn a_word_hashes_the_same_wherever_it_sits() {
        let a = cells("foo bar");
        let b = cells("    bar foo");
        let (wa, wb): (Vec<_>, Vec<_>) = (segments(&a).collect(), segments(&b).collect());
        assert_eq!(segment_hash(1, wa[1].1, false), segment_hash(1, wb[0].1, false), "bar");
        assert_eq!(segment_hash(1, wa[0].1, false), segment_hash(1, wb[1].1, false), "foo");
        assert_ne!(segment_hash(1, wa[0].1, false), segment_hash(2, wa[0].1, false), "base");
        assert_eq!(
            segment_hash(1, wa[0].1, false),
            segment_hash(1, wa[0].1, true),
            "a steady word is one entry whatever the blink phase"
        );
        let mut blinking = wa[0].1.to_vec();
        blinking[1].style.flags |= StyleFlags::BLINK;
        assert_ne!(
            segment_hash(1, &blinking, false),
            segment_hash(1, &blinking, true),
            "a blinking word is shaped once per phase"
        );
        assert!(
            cell_color(
                &blinking[1].style,
                &Colors::from(&Theme::default().terminal),
                true,
                &mut Contrast::default()
            )
            .a <= 0.0,
            "off phase: the glyph is hidden"
        );
        assert!(
            cell_color(
                &blinking[1].style,
                &Colors::from(&Theme::default().terminal),
                false,
                &mut Contrast::default()
            )
            .a > 0.5
        );
    }

    /// A cell whose text would not read against its background is painted black or white
    /// once the theme sets a minimum contrast; inverse video is judged the painted way round.
    #[test]
    fn the_cursor_style_fixes_the_shape_or_leaves_it() {
        use slopty_theme::CursorStyle;
        assert_eq!(cursor_shape_for(CursorStyle::Program, CursorShape::Bar), CursorShape::Bar);
        assert_eq!(cursor_shape_for(CursorStyle::Block, CursorShape::Bar), CursorShape::Block);
        assert_eq!(cursor_shape_for(CursorStyle::Bar, CursorShape::Block), CursorShape::Bar);
        assert_eq!(
            cursor_shape_for(CursorStyle::Underline, CursorShape::Block),
            CursorShape::Underline
        );
    }

    #[test]
    fn bold_text_is_painted_bright_when_asked() {
        let mut theme = Theme::default().terminal;
        let bold_red = CellStyle {
            fg: slopty_grid::Color::Palette(1),
            flags: StyleFlags::BOLD,
            ..CellStyle::default()
        };
        assert_eq!(
            cell_color(&bold_red, &Colors::from(&theme), false, &mut Contrast::default()),
            hsla(theme.ansi[1])
        );
        theme.bold_is_bright = true;
        let colors = Colors::from(&theme);
        assert_eq!(
            cell_color(&bold_red, &colors, false, &mut Contrast::default()),
            hsla(theme.ansi[9])
        );
        let inverse = CellStyle { flags: StyleFlags::BOLD | StyleFlags::INVERSE, ..bold_red };
        assert_eq!(
            cell_color(&inverse, &colors, false, &mut Contrast::default()),
            hsla(theme.bg),
            "inverse: the bg slot"
        );
    }

    #[test]
    fn text_is_held_to_the_minimum_contrast() {
        let mut theme = Theme::default().terminal;
        let navy_on_black = CellStyle {
            fg: slopty_grid::Color::Rgb(0, 0, 95),
            bg: slopty_grid::Color::Rgb(0, 0, 0),
            ..CellStyle::default()
        };
        let navy = hsla(Rgb { r: 0, g: 0, b: 95 });
        assert_eq!(
            cell_color(&navy_on_black, &Colors::from(&theme), false, &mut Contrast::default()),
            navy,
            "off"
        );
        theme.minimum_contrast = 300;
        let colors = Colors::from(&theme);
        let lifted = hsla(colors.text_over(Rgb { r: 0, g: 0, b: 95 }, Rgb::hex(0)));
        assert_ne!(lifted, navy, "lifted toward white");
        assert_eq!(cell_color(&navy_on_black, &colors, false, &mut Contrast::default()), lifted);
        // Two greys either side of the luminance where black and white swap: the painted
        // background decides, so inverse video flips the direction.
        let (white, black) = (hsla(Rgb::hex(0xff_ffff)), hsla(Rgb::hex(0)));
        let greys = CellStyle {
            fg: slopty_grid::Color::Rgb(118, 118, 118),
            bg: slopty_grid::Color::Rgb(116, 116, 116),
            ..CellStyle::default()
        };
        let over_darker = cell_color(&greys, &colors, false, &mut Contrast::default());
        assert!(over_darker.l > 0.46 && over_darker.l < white.l, "lighter: {over_darker:?}");
        let mut inverse = greys;
        inverse.flags |= StyleFlags::INVERSE;
        let over_lighter = cell_color(&inverse, &colors, false, &mut Contrast::default());
        assert!(over_lighter.l < 0.46 && over_lighter.l > black.l, "darker: {over_lighter:?}");
        let mut faint = navy_on_black;
        faint.flags |= StyleFlags::FAINT;
        let painted = cell_color(&faint, &colors, false, &mut Contrast::default());
        assert!((painted.a - 0.6).abs() < f32::EPSILON && painted.l > navy.l, "{painted:?}");
    }

    fn word(glyphs: usize) -> Word {
        let glyph = Glyph {
            font: FontId(0),
            id: GlyphId(0),
            col: 0,
            position: point(px(0.0), px(0.0)),
            emoji: false,
            color: Hsla::default(),
        };
        Word { glyphs: vec![glyph; glyphs] }
    }

    /// Words stay until the budget is passed, then the least recently used go first: a word
    /// the last pass used survives however many older ones there are, and a word used once
    /// long ago goes before one used every pass.
    #[test]
    fn the_word_cache_forgets_the_least_recently_used_past_its_budget() {
        let mut words = Words::default();
        words.begin(100);
        for key in 0..10 {
            let _w = words.get_or_shape(key, || word(10));
        }
        assert_eq!((words.len(), words.glyphs), (10, 100), "at the budget, nothing goes");
        // Many passes later, keys 5..10 are used again; nothing unused is forgotten yet.
        for _ in 0..50 {
            words.begin(100);
        }
        for key in 5..10 {
            let _w = words.get_or_shape(key, || panic!("{key} is held"));
        }
        assert_eq!(words.len(), 10, "no sweep: a word unused for fifty passes is still there");
        // A new word goes over the budget: the next pass drops the oldest quarter, all unused.
        let _w = words.get_or_shape(10, || word(10));
        words.begin(100);
        assert!(words.glyphs <= 100, "{}", words.glyphs);
        for key in 5..=10 {
            let _w = words.get_or_shape(key, || panic!("{key} was used recently"));
        }
        let mut reshaped = 0;
        for key in 0..5 {
            let _w = words.get_or_shape(key, || {
                reshaped += 1;
                word(10)
            });
        }
        assert!((2..=5).contains(&reshaped), "the oldest went: {reshaped}");
    }

    /// A single pass larger than the budget keeps everything it used until the next pass.
    #[test]
    fn a_pass_never_evicts_its_own_words() {
        let mut words = Words::default();
        words.begin(10);
        for key in 0..40 {
            let _w = words.get_or_shape(key, || word(1));
        }
        assert_eq!(words.len(), 40, "within the pass");
        words.begin(10);
        assert!(words.glyphs <= 10, "{}", words.glyphs);
    }

    /// The word key is the same whether the view is focused or not (focus shapes nothing),
    /// and differs when the cell the glyphs are placed on does (a move to a 2× display).
    #[test]
    fn the_word_key_holds_the_cell_width_and_not_the_focus() {
        let colors = Colors::from(&Theme::default().terminal);
        let at = |width: f32| hash_base(px(13.0), px(width), "Mono", true, &colors);
        assert_eq!(at(8.0), at(8.0), "nothing but the look");
        assert_ne!(at(8.0), at(7.5), "a 1× cell and a 2× cell place glyphs apart");
    }

    /// A span of points is the device pixels GPUI's snap gives it, each edge on its own.
    #[test]
    fn a_sprite_tile_is_the_snapped_cell() {
        assert_eq!(device_span(px(10.0), px(8.0), 2.0), 16);
        assert_eq!(device_span(px(10.25), px(8.0), 2.0), 16, "a fractional origin");
        assert_eq!(device_span(px(0.3), px(7.5), 1.0), 8, "0.3 → 0, 7.8 → 8");
        assert_eq!(device_span(px(0.0), px(-4.0), 1.0), 0, "never negative");
    }

    /// The minimum-contrast check is remembered per pair and forgotten when the setting moves.
    #[test]
    fn the_contrast_check_is_remembered_per_pair() {
        let mut theme = Theme::default().terminal;
        theme.minimum_contrast = 300;
        let colors = Colors::from(&theme);
        let mut memo = Contrast::default();
        let (navy, black) = (Rgb { r: 0, g: 0, b: 95 }, Rgb::hex(0));
        let first = memo.text_over(&colors, navy, black);
        assert_eq!(first, colors.text_over(navy, black));
        assert_eq!(memo.memo.len(), 1);
        assert_eq!(memo.text_over(&colors, navy, black), first);
        assert_eq!(memo.memo.len(), 1, "asked again: remembered");
        theme.minimum_contrast = 100;
        let off = Colors::from(&theme);
        assert_eq!(memo.text_over(&off, navy, black), navy, "off: the colour as it is");
    }

    /// A guess replaces the worker's cell on its row, in the faint, underlined look; other rows
    /// have none.
    #[test]
    fn a_guess_is_a_cell_of_its_row() {
        let guesses: VecDeque<Prediction> = [(2, 'x', 1), (3, 'y', 1)]
            .into_iter()
            .map(|(col, ch, seq)| Prediction {
                seq,
                row: 0,
                col,
                text: ch.to_string(),
                at: Instant::now(),
            })
            .collect();
        let row = cells("ab");
        assert_eq!(predicted_cells(&row, &guesses, 1), None, "not on this row");
        let guessed = predicted_cells(&row, &guesses, 0).expect("guessed");
        assert_eq!(guessed[2].text.as_str(), "x");
        assert_eq!(guessed[3].text.as_str(), "y");
        assert_eq!(guessed[2].style, PREDICTED);
        let words: Vec<_> = segments(&guessed).map(|(col, w)| (col, w.len())).collect();
        assert_eq!(words, vec![(0, 4)], "shaped with the text it continues");
    }

    /// Under a block cursor a glyph takes the cursor-text colour; beside it, or hidden, not.
    #[test]
    fn text_under_a_block_cursor_takes_the_cursor_text_colour() {
        let (red, blue) = (gpui::red(), gpui::blue());
        let under = Some(CursorText { row: 2, start: 4, end: 6, color: blue });
        assert_eq!(CursorText::over(under, 2, 4, red), blue);
        assert_eq!(CursorText::over(under, 2, 5, red), blue, "both halves of a wide cell");
        assert_eq!(CursorText::over(under, 2, 6, red), red, "the next cell");
        assert_eq!(CursorText::over(under, 1, 4, red), red, "another row");
        assert_eq!(CursorText::over(None, 2, 4, red), red, "no block cursor");
        let hidden = Hsla { a: 0.0, ..red };
        assert_eq!(CursorText::over(under, 2, 4, hidden), hidden, "invisible text stays so");
    }

    /// The pointer can leave the grid's right edge without a move over the grid: out of the
    /// window, to another app (⌘-tab), or by the tile moving away under a still pointer. Each
    /// lets the scrollbar go (it lingers and fades as after any leave) instead of holding it up.
    #[gpui::test]
    fn the_scrollbar_lets_go_when_the_pointer_leaves_without_a_move(cx: &mut gpui::TestAppContext) {
        use slopty_grid::{LineIndex, RowUpdate, Style, TermModes};
        use slopty_proto::terminal::{Frame, TermEvent};

        use crate::terminal::scrollbar::{FADE, LINGER};

        cx.update(gpui_kit::init);
        let (tx, _rx) = tokio::sync::mpsc::channel(64);
        let (view, cx) = cx.add_window_view(|window, cx| {
            let grid = TermSize { cols: 10, rows: 3, ..TermSize::default() };
            let view =
                TerminalView::new(slopty_core::SessionId::new(), grid, tx, Theme::default(), cx);
            window.focus(&view.focus_handle(cx), cx);
            view
        });
        cx.simulate_resize(size(px(400.0), px(300.0)));
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        let history = TermEvent::Frame(Frame {
            seq: 1,
            full: true,
            epoch: 0,
            cols: 10,
            rows: 3,
            cursor: slopty_grid::Cursor::default(),
            modes: TermModes::empty(),
            oldest_line: LineIndex(0),
            first_visible_line: LineIndex(30),
            total_lines: 33,
            input_ack: 0,
            images: Vec::new(),
            updates: ["one", "two", "three"]
                .iter()
                .zip(0_u16..)
                .map(|(text, row)| RowUpdate {
                    row,
                    line: Line::from_text(text, 10, Style::DEFAULT),
                })
                .collect(),
        });
        view.update(cx, |view, cx| view.apply(history, cx));
        cx.run_until_parked();
        let shown = |cx: &mut gpui::VisualTestContext| {
            view.read_with(cx, |v, cx| v.scrollbar_opacity(cx.background_executor().now()))
        };
        let gone = |cx: &mut gpui::VisualTestContext| {
            cx.executor().advance_clock(LINGER.saturating_add(FADE));
            cx.run_until_parked();
            shown(cx) <= 0.0
        };
        let m = view.read_with(cx, |v, _| v.metrics().expect("laid out"));
        let edge = point(
            m.origin.x + m.cell_width * (f32::from(m.cols) - 0.5),
            m.origin.y + m.line_height * 1.5,
        );
        let mods = gpui::Modifiers::default();
        let to_edge = |cx: &mut gpui::VisualTestContext| {
            cx.simulate_mouse_move(edge, None, mods);
            cx.run_until_parked();
            assert!(shown(cx) >= 1.0, "the pointer at the right edge brings it up");
        };

        to_edge(cx);
        cx.simulate_event(MouseExitEvent { position: edge, pressed_button: None, modifiers: mods });
        assert!(gone(cx), "out of the window");

        to_edge(cx);
        cx.deactivate_window();
        assert!(gone(cx), "another app came to the front");
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();

        to_edge(cx);
        cx.simulate_resize(size(px(600.0), px(300.0)));
        cx.run_until_parked();
        assert!(gone(cx), "the edge moved away from a still pointer");
    }
}

//! `TerminalElement`: paints a `TermState` as cell-aligned text runs and quads.
//!
//! Per row: the words (a row split at its plain spaces), each shaped once at the base font
//! size and painted glyph by glyph at the zoomed size, background quads for non-default
//! backgrounds, underlines and strikethroughs where the font puts them, and the cursor. Shaping
//! is cached across frames and views by the content hash of each word, and the cache is
//! independent of the zoom: a zoom step re-shapes nothing, it repaints the same glyph ids at
//! another size (positions come from the cell grid, and a fixed-pitch font's advances scale
//! with the size).

use std::collections::HashMap;
use std::hash::{Hash as _, Hasher as _};
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    App, BorderStyle, BorrowAppContext as _, Bounds, Corners, DispatchPhase, Edges, Element,
    ElementId, ElementInputHandler, Entity, Focusable as _, Font, FontId, GlobalElementId, GlyphId,
    Hsla, InspectorElementId, IntoElement, LayoutId, LongPressEvent, MouseMoveEvent, PathBuilder,
    Pixels, Point, RenderImage, ShapedLine, SharedString, Size, Style, TextAlign, TextRun,
    UnderlineStyle, Window, fill, point, px, quad, relative, size,
};
use slopty_grid::{Cell, CellWidth, CursorShape, Style as CellStyle, StyleFlags, Underline};
use slopty_proto::terminal::{Placement, TermSize};
use slopty_theme::{Colors, Theme, alpha};

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
    /// [`Self::pixel_at`] reports in the units the host measures in — the cell size in
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
    /// grid — the units the host divides by the cell size it was told.
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
    background: Hsla,
    /// Colour of the ⌘-hover link underline.
    link: Hsla,
    /// The scrollbar's thumb over the grid's right edge, while the bar shows.
    scrollbar: Option<(Bounds<Pixels>, Hsla)>,
    /// Glyphs drawn over the grid: local-echo predictions and the input method's composition.
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

#[derive(Debug)]
struct PreparedRow {
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
#[derive(Clone, Copy, PartialEq, Debug)]
struct SpriteCell {
    col: u16,
    ch: char,
    fg: Hsla,
}

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

/// The command-block separator for a prompt-start row: the terminal foreground, faint, or
/// the chrome's error tone when the command before it reported a non-zero status.
#[must_use]
pub fn separator_color(theme: &Theme, exit: Option<u8>) -> Hsla {
    if exit.is_some_and(|code| code != 0) {
        hsla_alpha(theme.surfaces.error, alpha::STRONG)
    } else {
        hsla_alpha(theme.terminal.fg, alpha::FAINT)
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

/// The cell geometry derived for one family, size and scale: the grid in points, the whole-pixel
/// metrics and the face they came from.
type Derived = (Grid, metrics::Metrics, metrics::Face);

/// Shaped-word cache keyed by (text + styles + palette) hash, kept on the App as a global so it
/// survives across frames and views. Words are shaped at the base size only.
#[derive(Default)]
struct ShapeCache {
    lines: HashMap<u64, Rc<Word>>,
    generation: u64,
    touched: HashMap<u64, u64>,
    /// The frame the last sweep ran in, so twenty terminals in one frame sweep once.
    frame: Option<u64>,
    /// The derived grid per (family, font size, scale, line-height multiplier): deriving it
    /// walked the font system every frame for nothing.
    grids: HashMap<(String, u32, u32, u32), Derived>,
    /// The monospace family resolved per theme list: the first installed candidate. Listing
    /// the installed fonts is a trip to the font server (tens of milliseconds), so it happens
    /// once per list for the whole app, not once per view.
    families: HashMap<Vec<String>, String>,
    /// How many times the installed fonts were listed (tests).
    #[cfg(test)]
    picks: usize,
    /// How many rows the last prepaint built (tests: the rows the clip shows, not the grid's).
    #[cfg(test)]
    rows_prepared: usize,
    /// The "took" captions the last prepaint drew, top row first (tests).
    #[cfg(test)]
    captions: Vec<String>,
}

impl gpui::Global for ShapeCache {}

impl ShapeCache {
    /// The cell geometry for `family` at `font_size` on this window (see [`measure`]).
    fn grid(
        &mut self,
        window: &Window,
        family: &str,
        font: &Font,
        font_size: Pixels,
        height_mult: f32,
    ) -> Derived {
        let key = (
            family.to_owned(),
            f32::from(font_size).to_bits(),
            window.scale_factor().to_bits(),
            height_mult.to_bits(),
        );
        if let Some(grid) = self.grids.get(&key) {
            return *grid;
        }
        let (grid, derived, _font_id, face) = measure(window, font, font_size, height_mult);
        self.grids.insert(key, (grid, derived, face));
        (grid, derived, face)
    }

    /// The first of `candidates` that is installed, resolved once per list (see [`pick_family`]).
    fn family(&mut self, window: &Window, candidates: &[String]) -> String {
        if let Some(family) = self.families.get(candidates) {
            return family.clone();
        }
        #[cfg(test)]
        {
            self.picks = self.picks.saturating_add(1);
        }
        let picked = pick_family(window, candidates);
        self.families.insert(candidates.to_vec(), picked.clone());
        picked
    }

    /// Drop entries not used in the last two generations. A generation is a frame when the
    /// frame probe counts them (`frame`), else one call: without the frame index every
    /// element's prepaint would be a generation and four terminals on screen would evict each
    /// other's rows every frame.
    fn sweep(&mut self, frame: Option<u64>) {
        if let Some(frame) = frame {
            if self.frame == Some(frame) {
                return;
            }
            self.frame = Some(frame);
        }
        self.generation = self.generation.wrapping_add(1);
        let keep_after = self.generation.saturating_sub(2);
        self.touched.retain(|_, g| *g >= keep_after);
        let touched = &self.touched;
        self.lines.retain(|k, _| touched.contains_key(k));
    }
}

/// The part of a shaped word's key that is the same for every word of a view this frame:
/// size, family and the palette the styles were resolved through, so a theme swap never
/// replays old colours.
fn hash_base(
    focused: bool,
    font_size: Pixels,
    family: &str,
    ligatures: bool,
    palette: &Colors,
) -> u64 {
    let mut h = std::hash::DefaultHasher::new();
    focused.hash(&mut h);
    f32::from(font_size).to_bits().hash(&mut h);
    family.hash(&mut h);
    ligatures.hash(&mut h);
    palette.hash(&mut h);
    h.finish()
}

/// Key of a shaped word: the base plus everything the shaped runs bake in (text, styles).
fn segment_hash(base: u64, cells: &[Cell], blink_off: bool) -> u64 {
    let mut h = std::hash::DefaultHasher::new();
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
/// word shapes the same wherever it sits.
fn segments(cells: &[Cell]) -> Vec<(u16, &[Cell])> {
    let col = |i: usize| u16::try_from(i).unwrap_or(u16::MAX);
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for (i, cell) in cells.iter().enumerate() {
        let space = plain_space(cell) || drawn_here(cell).is_some();
        let alone = stands_alone(cell);
        if (space || alone)
            && let Some(s) = start.take()
            && let Some(word) = cells.get(s..i)
        {
            out.push((col(s), word));
        }
        if alone {
            if let Some(digit) = cells.get(i..=i) {
                out.push((col(i), digit));
            }
        } else if !space && start.is_none() {
            start = Some(i);
        }
    }
    if let Some(word) = start.and_then(|s| cells.get(s..).map(|w| (s, w))) {
        out.push((col(word.0), word.1));
    }
    out
}

/// A word shaped once, at the base font size, and what painting it at any size needs: every
/// glyph with its font, its colour and its position — placed by the column of the cell its
/// byte came from, so a wide cluster spans two cells whatever it shaped to.
#[derive(Debug)]
struct Word {
    glyphs: Vec<Glyph>,
}

/// One glyph of a [`Word`], at the base size, relative to the word's origin on the baseline.
#[derive(Debug, Clone, Copy)]
struct Glyph {
    font: FontId,
    id: GlyphId,
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
) -> Word {
    let Look { family, ligatures, palette, blink_off } = look;
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
                    let run = text_run(acc, family, ligatures, &style, palette, blink_off);
                    colors.push((text.len().saturating_sub(len), run.color));
                    runs.push(run);
                }
                current = Some((cell.style, len));
            }
        }
    }
    if let Some((style, acc)) = current.take() {
        let run = text_run(acc, family, ligatures, &style, palette, blink_off);
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
fn cell_color(style: &CellStyle, palette: &Colors, blink_off: bool) -> Hsla {
    let inverse = style.flags.contains(StyleFlags::INVERSE);
    let fg = palette.bold_slot(style.fg, style.flags.contains(StyleFlags::BOLD));
    let (fg_slot, bg_slot) = if inverse { (style.bg, fg) } else { (fg, style.bg) };
    let fg = palette.resolve(fg_slot, inverse);
    let bg = palette.resolve(bg_slot, !inverse);
    let mut color = hsla(palette.text_over(fg, bg));
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
fn text_run(
    len: usize,
    family: &str,
    ligatures: bool,
    style: &CellStyle,
    palette: &Colors,
    blink_off: bool,
) -> TextRun {
    TextRun {
        len,
        font: mono_font(family, ligatures, style),
        color: cell_color(style, palette, blink_off),
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
        _global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Prepared {
        if !cx.has_global::<ShapeCache>() {
            cx.set_global(ShapeCache::default());
        }
        // Resolving the family walks every installed font: once per app for a theme's list
        // (the cache), remembered per view so a frame costs neither the walk nor the lookup.
        let known_family = self.view.read(cx).font_family().map(str::to_owned);
        let family = known_family.unwrap_or_else(|| {
            let candidates = self.view.read(cx).theme().typography.mono_families.clone();
            let picked =
                cx.update_global::<ShapeCache, _>(|cache, _| cache.family(window, &candidates));
            self.view.update(cx, |view, _cx| view.set_font_family(picked.clone()));
            picked
        });
        let zoom = if self.zoom.is_finite() && self.zoom > 0.0 { self.zoom } else { 1.0 };
        let (base_size, height_mult, base_pad, cursor_blink, cursor_style, ligatures) = {
            let theme = self.view.read(cx).theme();
            (
                px(theme.typography.mono_size),
                theme.typography.mono_line_height,
                px(theme.spacing.sm),
                theme.behaviour.cursor_blink,
                theme.behaviour.cursor_style,
                theme.typography.ligatures,
            )
        };
        // Grid size comes from the unscaled geometry so zooming never resizes the PTY. The
        // cell is derived once per family, size and scale, not once per frame.
        let base_font = fonts::terminal_font(&family, false, false, ligatures);
        let (base_grid, base_derived, face) = cx.update_global::<ShapeCache, _>(|cache, _| {
            cache.grid(window, &family, &base_font, base_size, height_mult)
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
        let metrics = CellMetrics {
            origin,
            cell_width,
            line_height,
            cols,
            rows,
            pixel_scale: window.scale_factor().max(1.0) / zoom,
            face,
            face_size: f32::from(base_size) * window.scale_factor().max(1.0),
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

        // Shaping reads the view's rows in place (no copy of the grid per frame) while the
        // cache is out of the app: put it back before anything else touches `cx`.
        let mut cache = std::mem::take(cx.global_mut::<ShapeCache>());
        let text_system = Arc::clone(window.text_system());
        let focused = self.focused;
        // Textures for the placed images are made (and stale ones dropped) before the read.
        let placed = self.view.update(cx, |view, _cx| view.placed_images(window));
        let prepared = {
            let view = self.view.read(cx);
            let theme = view.theme();
            let state = view.state();
            let colors = Colors::new(&theme.terminal, state.colors());
            let palette = &colors;
            let rows_view = state.view();
            let predicted = view.predictions();
            let cursor = predicted.as_ref().map_or_else(|| state.cursor(), |(_, c)| *c);
            let view_offset = state.view_offset();
            let modes = state.modes();
            let marked = view.marked();
            let selection = view.selection();
            let blink_off = !view.blink_on();
            let mut blinking = false;
            let top_index = state.index_at_row(0);
            let grid_cols = state.size().cols;
            let (matches, current) = view.search_highlights().unwrap_or((&[], None));
            let link = view.link_highlight();
            let scrollbar = view.scrollbar_shown().then(|| {
                let held = view.thumb_held();
                let alpha = if held { alpha::PRESSED } else { alpha::TINT };
                (hsla_alpha(palette.theme.fg, alpha), state.history_len(), view_offset)
            });

            cache.sweep(crate::frames::index(cx));
            let base = hash_base(focused, base_size, &family, ligatures, palette);
            // Rows the clip cannot show (a grid half off the viewport) are not built at all:
            // no quads, no words, no hashing. Paint walks only the rows prepared here.
            let clip = window.content_mask().bounds.intersect(&bounds);
            let (clip_top, clip_bottom) = (clip.top(), clip.bottom());
            let overhang = row_overhang(&grid, &face, metrics.pixel_scale);
            // Rows above the oldest line the host still has: a `~` filler, shaped once.
            let mut filler: Option<Rc<Word>> = None;
            let mut prepared_rows = Vec::with_capacity(rows_view.len());
            // "took 3.2 s" at the right end of a prompt row whose command took a while.
            let mut captions: Vec<(Point<Pixels>, ShapedLine)> = Vec::new();
            #[cfg(test)]
            let mut caption_texts: Vec<String> = Vec::new();
            for (i, row) in rows_view.iter().enumerate() {
                let y = origin.y + line_height * f32::from(u16::try_from(i).unwrap_or(u16::MAX));
                if !row_in_band(y, line_height, overhang, clip_top, clip_bottom) {
                    continue;
                }
                let Some(line) = row.line else {
                    let filler = filler.get_or_insert_with(|| {
                        let cells = [Cell::narrow('~', CellStyle::DEFAULT)];
                        let look = Look { family: &family, ligatures, palette, blink_off: false };
                        Rc::new(shape_cells(&text_system, &cells, base_size, base_cell_width, look))
                    });
                    prepared_rows.push(PreparedRow {
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
                let mut quads: Vec<(u16, u16, Hsla)> = Vec::new();
                let mut decorations: Vec<Decoration> = Vec::new();
                let mut sprites: Vec<SpriteCell> = Vec::new();
                for (col, cell) in line.cells.iter().enumerate() {
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
                    // Underline and strikethrough go where the font says, not where GPUI
                    // would put them; a curly underline is GPUI's wave at that position.
                    let text = cell_color(&cell.style, palette, blink_off);
                    if let Some(ch) = drawn_here(cell) {
                        sprites.push(SpriteCell { col, ch, fg: text });
                    }
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
                    if cell.style.flags.contains(StyleFlags::STRIKETHROUGH) {
                        stroke(&mut decorations, col, text, grid.strikethrough, false, Layer::Over);
                    }
                }
                // The selection paints over cell backgrounds and under the text.
                let index = slopty_grid::LineIndex(
                    top_index.0.saturating_add(u64::try_from(i).unwrap_or(u64::MAX)),
                );
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
                let segments = segments(&line.cells)
                    .into_iter()
                    .map(|(col, cells)| {
                        blinking |= blinks(cells);
                        let key = segment_hash(base, cells, blink_off);
                        cache.touched.insert(key, cache.generation);
                        let shaped = cache.lines.entry(key).or_insert_with(|| {
                            Rc::new(shape_cells(
                                &text_system,
                                cells,
                                base_size,
                                base_cell_width,
                                Look { family: &family, ligatures, palette, blink_off },
                            ))
                        });
                        (col, Rc::clone(shaped))
                    })
                    .collect();
                let link = link
                    .filter(|&(at, ..)| at == index)
                    .map(|(_, start, end)| (start, end.min(grid_cols)));
                // A prompt starts here: rule off the command above it, red when it failed.
                let separator = (line.mark.starts_prompt() && index.0 > 0)
                    .then(|| separator_color(theme, line.mark.exit()));
                if line.mark.starts_prompt()
                    && let Some(elapsed) = view.took(index)
                {
                    let text = super::view::took_label(elapsed);
                    let width = u16::try_from(text.chars().count()).unwrap_or(u16::MAX);
                    let typed =
                        u16::try_from(line.text().trim_end().chars().count()).unwrap_or(u16::MAX);
                    // Flush with the right edge, a cell clear of the command's text.
                    if let Some(col) = grid_cols.checked_sub(width)
                        && typed < col
                    {
                        let mut run = text_run(
                            text.len(),
                            &family,
                            ligatures,
                            &CellStyle::DEFAULT,
                            palette,
                            false,
                        );
                        run.color = hsla_alpha(palette.theme.fg, alpha::TINT);
                        let shaped = text_system.shape_line(
                            SharedString::from(text.clone()),
                            font_size,
                            &[run],
                            Some(cell_width),
                        );
                        captions.push((point(origin.x + cell_width * f32::from(col), y), shaped));
                        #[cfg(test)]
                        caption_texts.push(text);
                    }
                }
                prepared_rows.push(PreparedRow {
                    y,
                    quads,
                    decorations,
                    segments,
                    link,
                    separator,
                    sprites,
                });
            }

            #[cfg(test)]
            {
                cache.rows_prepared = prepared_rows.len();
                cache.captions = caption_texts;
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
            // While an input method composes, its underlined preview stands in for the cursor.
            let cursor_prepared = (cursor_shown && marked.is_none()).then(|| {
                let x = origin.x + cell_width * f32::from(cursor.col);
                let y = origin.y + line_height * f32::from(cursor.row);
                let shape = if focused {
                    cursor_shape_for(cursor_style, cursor.shape)
                } else {
                    CursorShape::BlockHollow
                };
                let line = rows_view.get(usize::from(cursor.row)).and_then(|row| row.line);
                let width = cell_width * f32::from(cursor_span(line, cursor.col));
                (
                    Bounds::new(point(x, y), size(width, line_height)),
                    shape,
                    hsla(palette.theme.cursor),
                )
            });

            // Local echo: predicted glyphs, slightly dimmed so a wrong guess never looks final.
            let predicted = predicted.map(|(p, _)| p).unwrap_or_default();
            let mut overlay: Vec<(Point<Pixels>, ShapedLine)> = predicted
                .iter()
                .map(|p| {
                    let mut run = text_run(
                        p.text.len(),
                        &family,
                        ligatures,
                        &CellStyle::DEFAULT,
                        palette,
                        false,
                    );
                    run.color.a = 0.75;
                    run.underline = Some(UnderlineStyle {
                        thickness: px(1.0),
                        color: Some(run.color),
                        wavy: false,
                    });
                    let shaped = text_system.shape_line(
                        SharedString::from(p.text.clone()),
                        font_size,
                        &[run],
                        Some(cell_width),
                    );
                    let at = point(
                        origin.x + cell_width * f32::from(p.col),
                        origin.y + line_height * f32::from(p.row),
                    );
                    (at, shaped)
                })
                .collect();
            // The input method's composition, underlined at the cursor (what Terminal.app does).
            if let Some(text) = marked.filter(|_| cursor_visible) {
                let mut run =
                    text_run(text.len(), &family, ligatures, &CellStyle::DEFAULT, palette, false);
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
                background: hsla(palette.theme.bg),
                link: hsla(palette.theme.fg),
                scrollbar: scrollbar.and_then(|(color, history, offset)| {
                    scrollbar_thumb(&metrics, history, offset).map(|thumb| (thumb, color))
                }),
                overlay,
                shown: predicted.iter().map(|p| p.seq).collect(),
                blinking,
                images,
            }
        };
        *cx.global_mut::<ShapeCache>() = cache;
        // The clock ticks only while a painted frame has something to blink.
        self.view.update(cx, |view, cx| view.blinking(prepared.blinking, cx));
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
        window.on_mouse_event(move |event: &LongPressEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble {
                return;
            }
            let claimed = view.update(cx, |view, cx| view.long_press(event, window, cx));
            if claimed {
                window.prevent_default();
                cx.stop_propagation();
            }
        });
        // A drag is followed wherever the pointer goes (the div's own move listener stops at
        // its edge): the selection keeps growing and scrolls past the top or bottom.
        let view = self.view.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _window, cx| {
            if phase == DispatchPhase::Bubble && event.pressed_button.is_some() {
                view.update(cx, |view, cx| view.drag_move(event, cx));
            }
        });
        window.paint_quad(fill(bounds, prepared.background));
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
        // Box drawing, blocks, Braille and Powerline: geometry in the cell's colours, so a
        // border never seams between rows and a heavy line keeps its weight.
        let cell = sprite::Cell {
            w: f32::from(m.cell_width),
            h: f32::from(m.line_height),
            thickness: f32::from(grid.underline.thickness),
            scale: m.pixel_scale,
        };
        for row in &prepared.rows {
            for sprite in &row.sprites {
                let origin = point(m.origin.x + m.cell_width * f32::from(sprite.col), row.y);
                paint_sprite(window, origin, cell, sprite);
            }
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
                        let painted = if glyph.emoji {
                            window.paint_emoji(at, glyph.font, glyph.id, font_size)
                        } else if raster == font_size {
                            window.paint_glyph(at, glyph.font, glyph.id, font_size, glyph.color)
                        } else {
                            // In motion: the nearest rung's raster, stretched (the fork).
                            let (f, g, c) = (glyph.font, glyph.id, glyph.color);
                            window.paint_glyph_scaled(at, f, g, raster, font_size, c)
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
            // Cover whatever the host currently shows there, then draw the guess or preview.
            window.paint_quad(fill(
                Bounds::new(*at, size(line.width.max(m.cell_width), m.line_height)),
                prepared.background,
            ));
            if let Err(e) = line.paint(*at, m.line_height, TextAlign::Left, None, window, cx) {
                tracing::debug!(error = %e, "paint prediction");
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
        let shown = std::mem::take(&mut prepared.shown);
        self.view.update(cx, |view, _cx| view.painted(&shown));
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
/// rectangle. The host lays placements out in its cell pixels (device pixels of the unzoomed
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

/// Paint one cell-drawn glyph at `origin` (see [`sprite`]).
fn paint_sprite(window: &mut Window, origin: Point<Pixels>, cell: sprite::Cell, s: &SpriteCell) {
    let Some(shapes) = sprite::shapes(s.ch, cell) else { return };
    let at = |(x, y): (f32, f32)| point(origin.x + px(x), origin.y + px(y));
    for shape in shapes {
        match shape {
            sprite::Shape::Rect { x, y, w, h, ink, round } => {
                let color = match ink {
                    sprite::Ink::Fg => s.fg,
                    sprite::Ink::Shade(a) => Hsla { a: s.fg.a * a, ..s.fg },
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
                    window.paint_path(path, s.fg);
                }
            }
            sprite::Shape::Arc { from, to, r, sweep, thickness } => {
                let mut path = PathBuilder::stroke(px(thickness));
                path.move_to(at(from));
                path.arc_to(point(px(r), px(r)), px(0.0), false, sweep, at(to));
                if let Ok(path) = path.build() {
                    window.paint_path(path, s.fg);
                }
            }
            sprite::Shape::Poly { points, ink } => {
                let color = match ink {
                    sprite::Ink::Fg => s.fg,
                    sprite::Ink::Shade(a) => Hsla { a: s.fg.a * a, ..s.fg },
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
    cx.try_global::<ShapeCache>().map_or(0, |cache| cache.lines.len())
}

/// How many times the installed fonts were listed (tests: once for every view of the app).
#[cfg(test)]
pub fn family_picks(cx: &App) -> usize {
    cx.try_global::<ShapeCache>().map_or(0, |cache| cache.picks)
}

/// How many rows the last prepaint built (tests: only the rows inside the clip).
#[cfg(test)]
pub fn rows_prepared(cx: &App) -> usize {
    cx.try_global::<ShapeCache>().map_or(0, |cache| cache.rows_prepared)
}

/// The "took" captions the last prepaint drew, top row first (tests).
#[cfg(test)]
pub fn captions_drawn(cx: &App) -> Vec<String> {
    cx.try_global::<ShapeCache>().map_or_else(Vec::new, |cache| cache.captions.clone())
}

#[cfg(test)]
mod tests {
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

    /// A placement's pixels are the host's cell pixels: at display scale 2 a 16 × 32 image
    /// paints 8 × 16 points at its cell plus its offset; a source rectangle shifts the whole
    /// image so that part lands in the placement; a scrolled view moves it down its rows.
    #[test]
    fn a_placement_is_painted_at_its_cell_in_the_hosts_pixels() {
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

    /// A pixel mouse report is in the units the host measures in: device pixels of the *fitted*
    /// grid, whatever the display's scale and however far the canvas has zoomed. The host
    /// divides by the cell size it was told (8 × 17 here), so the column it reads back is the
    /// column the pointer is over.
    #[test]
    fn a_pixel_mouse_report_is_in_the_cell_size_the_host_was_told() {
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
        segments(&cells(text)).into_iter().map(|(col, word)| (col, word.len())).collect()
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
            segments(&row).len(),
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
        assert_eq!(segments(&row).len(), 2, "the wave is a decoration drawn from the cells");
        let mut row = cells("a b");
        row[1].style.underline = Underline::Single;
        assert_eq!(segments(&row).len(), 2, "a straight underline is a decoration quad");
        let mut row = cells("a b");
        row[1].style.flags |= StyleFlags::STRIKETHROUGH;
        assert_eq!(segments(&row).len(), 2, "so is a strikethrough");
        let mut row = cells("a b");
        row[1].style.bg = slopty_grid::Color::Palette(1);
        assert_eq!(segments(&row).len(), 2, "a background is a quad, not a glyph");
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
                shape_cells(window.text_system(), &cells, px(13.0), width, look)
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
        let words = segments(&row);
        assert_eq!(words.len(), 2, "{words:?}");
        assert_eq!(words[0].0, 0);
        assert_eq!(words[0].1.len(), 4, "two wide cells with their spacer tails");
        assert_eq!(words[1].0, 5);
    }

    #[test]
    fn a_word_hashes_the_same_wherever_it_sits() {
        let a = cells("foo bar");
        let b = cells("    bar foo");
        let (wa, wb) = (segments(&a), segments(&b));
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
            cell_color(&blinking[1].style, &Colors::from(&Theme::default().terminal), true).a
                <= 0.0,
            "off phase: the glyph is hidden"
        );
        assert!(
            cell_color(&blinking[1].style, &Colors::from(&Theme::default().terminal), false).a
                > 0.5
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
        assert_eq!(cell_color(&bold_red, &Colors::from(&theme), false), hsla(theme.ansi[1]));
        theme.bold_is_bright = true;
        let colors = Colors::from(&theme);
        assert_eq!(cell_color(&bold_red, &colors, false), hsla(theme.ansi[9]));
        let inverse = CellStyle { flags: StyleFlags::BOLD | StyleFlags::INVERSE, ..bold_red };
        assert_eq!(cell_color(&inverse, &colors, false), hsla(theme.bg), "inverse: the bg slot");
    }

    #[test]
    fn text_is_held_to_the_minimum_contrast() {
        let mut theme = Theme::default().terminal;
        let navy_on_black = CellStyle {
            fg: slopty_grid::Color::Rgb(0, 0, 95),
            bg: slopty_grid::Color::Rgb(0, 0, 0),
            ..CellStyle::default()
        };
        let navy = hsla(slopty_theme::Rgb { r: 0, g: 0, b: 95 });
        assert_eq!(cell_color(&navy_on_black, &Colors::from(&theme), false), navy, "off");
        theme.minimum_contrast = 300;
        let colors = Colors::from(&theme);
        let white = hsla(slopty_theme::Rgb::hex(0xff_ffff));
        assert_eq!(cell_color(&navy_on_black, &colors, false), white);
        // Two greys either side of the luminance where black and white swap: the painted
        // background decides, so inverse video flips the answer.
        let greys = CellStyle {
            fg: slopty_grid::Color::Rgb(118, 118, 118),
            bg: slopty_grid::Color::Rgb(116, 116, 116),
            ..CellStyle::default()
        };
        assert_eq!(cell_color(&greys, &colors, false), white, "over the darker grey");
        let mut inverse = greys;
        inverse.flags |= StyleFlags::INVERSE;
        let black = hsla(slopty_theme::Rgb::hex(0));
        assert_eq!(cell_color(&inverse, &colors, false), black, "over the lighter grey");
        let mut faint = navy_on_black;
        faint.flags |= StyleFlags::FAINT;
        let painted = cell_color(&faint, &colors, false);
        assert!((painted.a - 0.6).abs() < f32::EPSILON && painted.l > 0.9, "{painted:?}");
    }

    #[test]
    fn the_cache_sweeps_once_per_frame_and_keeps_what_the_frame_touched() {
        let mut cache = ShapeCache::default();
        cache.sweep(Some(1));
        let g = cache.generation;
        cache.sweep(Some(1));
        cache.sweep(Some(1));
        assert_eq!(cache.generation, g, "twenty terminals in one frame sweep once");
        cache.sweep(Some(2));
        assert_eq!(cache.generation, g.wrapping_add(1));
        cache.sweep(None);
        assert_eq!(cache.generation, g.wrapping_add(2), "without a frame index every call sweeps");
    }
}

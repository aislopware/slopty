//! `TerminalElement`: paints a `TermState` as cell-aligned text runs and quads.
//!
//! Per row: one shaped line with a run per style change, background quads for non-default
//! backgrounds, and the cursor. Shaping is cached across frames and views by the content hash
//! of each word (a row split at its plain spaces), so a word shaped once serves every row it
//! appears in.

use std::collections::HashMap;
use std::hash::{Hash as _, Hasher as _};
use std::rc::Rc;

use gpui::{
    App, BorrowAppContext as _, Bounds, DispatchPhase, Element, ElementId, ElementInputHandler,
    Entity, Focusable as _, Font, FontId, GlobalElementId, Hsla, InspectorElementId, IntoElement,
    LayoutId, LongPressEvent, Pixels, Point, ShapedLine, SharedString, Size, Style, TextAlign,
    TextRun, UnderlineStyle, Window, fill, point, px, relative, size,
};
use slopty_grid::{Cell, CellWidth, CursorShape, Style as CellStyle, StyleFlags, Underline};
use slopty_proto::terminal::TermSize;
use slopty_theme::{TerminalPalette, Theme, alpha};

use crate::colors::{hsla, hsla_alpha};
use crate::fonts;
use crate::terminal::metrics::{self, Grid};
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
    /// Glyphs drawn over the grid: local-echo predictions and the input method's composition.
    overlay: Vec<(Point<Pixels>, ShapedLine)>,
    /// The overlay carries predictions (not only a composition).
    predicting: bool,
}

#[derive(Debug)]
struct PreparedRow {
    y: Pixels,
    quads: Vec<(u16, u16, Hsla)>,
    /// Underlines and strikethroughs, at ghostty's offsets rather than GPUI's.
    decorations: Vec<Decoration>,
    /// The row's words, each shaped on its own and placed at its start column.
    segments: Vec<(u16, Rc<ShapedLine>)>,
    /// Columns of the link under a ⌘-hover, underlined over the text.
    link: Option<(u16, u16)>,
    /// Colour of the command-block separator drawn along the row's top edge.
    separator: Option<Hsla>,
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
}

/// Add one cell's worth of stroke, joining it to the run to its left when they match.
///
/// A cell contributes at most three strokes (two for a double underline, one strikethrough),
/// so the run this one continues, if any, is within the last few.
fn stroke(out: &mut Vec<Decoration>, col: u16, color: Hsla, line: metrics::Line) {
    let joins = |d: &&mut Decoration| {
        d.end == col && d.color == color && d.y == line.y && d.thickness == line.thickness
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
    });
}

/// The command-block separator for a prompt-start row: the terminal foreground, faint, or
/// the chrome's error tone when the command before it reported a non-zero status.
#[must_use]
pub fn separator_color(theme: &Theme, exit: Option<u8>) -> Hsla {
    if exit.is_some_and(|code| code != 0) {
        hsla_alpha(theme.surfaces.error, alpha::SEPARATOR_ERROR)
    } else {
        hsla_alpha(theme.terminal.fg, alpha::SEPARATOR)
    }
}

/// The element.
#[derive(Debug)]
pub struct TerminalElement {
    view: Entity<TerminalView>,
    focused: bool,
    zoom: f32,
    /// What a screen reader hears: the program's title and the cursor row's text. Filled by
    /// the view only while the accessibility tree is being built.
    a11y: Option<(SharedString, SharedString)>,
}

impl TerminalElement {
    /// Paint `view`.
    #[must_use]
    pub const fn new(view: Entity<TerminalView>, focused: bool) -> Self {
        Self { view, focused, zoom: 1.0, a11y: None }
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
}

impl IntoElement for TerminalElement {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

/// Shaped-word cache keyed by (text + styles + size + palette) hash, kept on the App as a
/// global so it survives across frames and views.
#[derive(Default)]
struct ShapeCache {
    lines: HashMap<u64, Rc<ShapedLine>>,
    generation: u64,
    touched: HashMap<u64, u64>,
    /// The frame the last sweep ran in, so twenty terminals in one frame sweep once.
    frame: Option<u64>,
    /// The derived grid per (family, font size, scale, line-height multiplier): deriving it
    /// walked the font system every frame for nothing.
    grids: HashMap<(String, u32, u32, u32), (Grid, metrics::Metrics)>,
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
    ) -> (Grid, metrics::Metrics) {
        let key = (
            family.to_owned(),
            f32::from(font_size).to_bits(),
            window.scale_factor().to_bits(),
            height_mult.to_bits(),
        );
        if let Some(grid) = self.grids.get(&key) {
            return *grid;
        }
        let (grid, derived, _font_id) = measure(window, font, font_size, height_mult);
        self.grids.insert(key, (grid, derived));
        (grid, derived)
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
fn hash_base(focused: bool, font_size: Pixels, family: &str, palette: &TerminalPalette) -> u64 {
    let mut h = std::hash::DefaultHasher::new();
    focused.hash(&mut h);
    f32::from(font_size).to_bits().hash(&mut h);
    family.hash(&mut h);
    palette.hash(&mut h);
    h.finish()
}

/// Key of a shaped word: the base plus everything the shaped runs bake in (text, styles).
fn segment_hash(base: u64, cells: &[Cell]) -> u64 {
    let mut h = std::hash::DefaultHasher::new();
    base.hash(&mut h);
    for cell in cells {
        cell.text.as_str().hash(&mut h);
        cell.style.hash(&mut h);
        (cell.width as u8).hash(&mut h);
    }
    h.finish()
}

/// A cell whose shaped run paints nothing: a narrow blank without a curly underline (its
/// background is a quad and a straight underline or strikethrough is a [`Decoration`], both
/// drawn from the cells, not from the run). Rows are split into words at these.
fn plain_space(cell: &Cell) -> bool {
    cell.width == CellWidth::Narrow
        && matches!(cell.text.as_str(), "" | " ")
        && cell.style.underline != Underline::Curly
}

/// A cell shaped on its own: a digit. Counters, timestamps and sizes make most of a streaming
/// row's unique text, and no coding font ligates digits, so each digit is one cached glyph
/// instead of a fresh word every frame. A curly-underlined digit stays in its word so GPUI
/// draws the wave in one piece.
fn stands_alone(cell: &Cell) -> bool {
    cell.width == CellWidth::Narrow
        && cell.text.as_ascii().is_some_and(|b| b.is_ascii_digit())
        && cell.style.underline != Underline::Curly
}

/// The words of a row: maximal runs of cells that are not plain spaces, digits on their own,
/// with the column each starts at. Positions inside a word come from its cluster index, so a
/// word shapes the same wherever it sits.
fn segments(cells: &[Cell]) -> Vec<(u16, &[Cell])> {
    let col = |i: usize| u16::try_from(i).unwrap_or(u16::MAX);
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for (i, cell) in cells.iter().enumerate() {
        let space = plain_space(cell);
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

/// Shape one word of `cells` (wide cells followed by a spacer so advances line up under the
/// forced cell width).
fn shape_cells(
    text_system: &gpui::WindowTextSystem,
    cells: &[Cell],
    font_size: Pixels,
    grid: &Grid,
    family: &str,
    palette: &TerminalPalette,
) -> ShapedLine {
    let mut text = String::with_capacity(cells.len());
    let mut runs: Vec<TextRun> = Vec::new();
    let mut current: Option<(CellStyle, usize)> = None;
    for cell in cells {
        if !cell.width.draws_text() {
            continue;
        }
        let piece: &str = if cell.text.is_empty() { " " } else { cell.text.as_str() };
        text.push_str(piece);
        let mut len = piece.len();
        if cell.width.columns() == 2 && piece.chars().count() == 1 {
            // shape_line forces per-glyph width; a wide glyph gets one cell, so add a spacer
            // cell after it.
            text.push(' ');
            len = len.saturating_add(1);
        }
        match &mut current {
            Some((style, acc)) if *style == cell.style => {
                *acc = acc.saturating_add(len);
            }
            _ => {
                if let Some((style, acc)) = current.take() {
                    runs.push(text_run(acc, family, &style, palette, grid.underline));
                }
                current = Some((cell.style, len));
            }
        }
    }
    if let Some((style, acc)) = current.take() {
        runs.push(text_run(acc, family, &style, palette, grid.underline));
    }
    text_system.shape_line(SharedString::from(text), font_size, &runs, Some(grid.cell_width))
}

fn mono_font(family: &str, style: &CellStyle) -> Font {
    fonts::terminal_font(
        family,
        style.flags.contains(StyleFlags::BOLD),
        style.flags.contains(StyleFlags::ITALIC),
    )
}

/// The colour a cell's glyphs take, with inverse, faint and invisible applied.
fn cell_color(style: &CellStyle, palette: &TerminalPalette) -> Hsla {
    let inverse = style.flags.contains(StyleFlags::INVERSE);
    let fg_slot = if inverse { style.bg } else { style.fg };
    let mut color = hsla(palette.resolve(fg_slot, inverse));
    if style.flags.contains(StyleFlags::FAINT) {
        color.a = 0.6;
    }
    if style.flags.contains(StyleFlags::INVISIBLE) {
        color.a = 0.0;
    }
    color
}

/// The colour of a cell's underline: SGR 58 when the cell sets one, else the text colour.
fn underline_color(style: &CellStyle, palette: &TerminalPalette, text: Hsla) -> Hsla {
    match style.underline_color {
        slopty_grid::Color::Default => text,
        other => hsla(palette.resolve(other, false)),
    }
}

/// A run of same-styled cells.
///
/// Straight underlines and strikethroughs are left off the run and painted from ghostty's
/// metrics instead, because GPUI puts them at offsets of its own. A curly underline stays with
/// the run: drawing a wave is GPUI's alone, and it only reads the thickness from here.
fn text_run(
    len: usize,
    family: &str,
    style: &CellStyle,
    palette: &TerminalPalette,
    underline: metrics::Line,
) -> TextRun {
    let color = cell_color(style, palette);
    let curly = (style.underline == Underline::Curly).then(|| UnderlineStyle {
        thickness: underline.thickness,
        color: Some(underline_color(style, palette, color)),
        wavy: true,
    });
    TextRun {
        len,
        font: mono_font(family, style),
        color,
        background_color: None,
        underline: curly,
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

/// Where to hand a shaped line to GPUI so its baseline lands on `baseline`.
///
/// GPUI centres a line in the box it is given — `(line_height - ascent - descent) / 2 + ascent`
/// down from the origin (`text_system/line.rs`) — which is close to, but not, where the font's
/// metrics put the baseline. Offsetting the origin by the difference is exact, and uses the
/// shaped line's own ascent and descent, so a row that fell back to another font still lines up.
fn text_origin_y(
    row_y: Pixels,
    line_height: Pixels,
    baseline: Pixels,
    ascent: Pixels,
    descent: Pixels,
) -> Pixels {
    let centred = (line_height - ascent - descent) / 2.0 + ascent;
    row_y + baseline - centred
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
/// is a whole number of them, and [`Grid`] divides back to points. GPUI's text system reports
/// neither the line gap nor the `post` underline metrics, so those go in empty and ghostty's
/// estimates stand in, exactly as they do for a font whose tables omit them.
fn measure(
    window: &Window,
    font: &Font,
    font_size: Pixels,
    height_mult: f32,
) -> (Grid, metrics::Metrics, FontId) {
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
        line_gap: 0.0,
        cap_height: Some(device(text_system.cap_height(font_id, size))),
        ex_height: Some(device(text_system.x_height(font_id, size))),
        ..metrics::Face::default()
    };
    let mut derived = metrics::calc(&face);
    derived.set_cell_height(adjusted_height(derived.cell_height, height_mult));
    (Grid::new(&derived, scale), derived, font_id)
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
        // Resolving the family walks every installed font; do it once per view.
        let known_family = self.view.read(cx).font_family().map(str::to_owned);
        let family = known_family.unwrap_or_else(|| {
            let candidates = self.view.read(cx).theme().typography.mono_families.clone();
            let picked = pick_family(window, &candidates);
            self.view.update(cx, |view, _cx| view.set_font_family(picked.clone()));
            picked
        });
        if !cx.has_global::<ShapeCache>() {
            cx.set_global(ShapeCache::default());
        }
        let zoom = if self.zoom.is_finite() && self.zoom > 0.0 { self.zoom } else { 1.0 };
        let (base_size, height_mult, base_pad) = {
            let theme = self.view.read(cx).theme();
            (
                px(theme.typography.mono_size),
                theme.typography.mono_line_height,
                px(theme.spacing.sm),
            )
        };
        // Grid size comes from the unscaled geometry so zooming never resizes the PTY. The
        // cell is derived once per family, size and scale, not once per frame.
        let base_font = fonts::terminal_font(&family, false, false);
        let (base_grid, base_derived) = cx.update_global::<ShapeCache, _>(|cache, _| {
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
        let text_system = std::sync::Arc::clone(window.text_system());
        let focused = self.focused;
        let prepared = {
            let view = self.view.read(cx);
            let theme = view.theme();
            let palette = &theme.terminal;
            let state = view.state();
            let rows_view = state.view();
            let predicted = view.predictions();
            let cursor = predicted.as_ref().map_or_else(|| state.cursor(), |(_, c)| *c);
            let view_offset = state.view_offset();
            let modes = state.modes();
            let marked = view.marked();
            let selection = view.selection();
            let top_index = state.index_at_row(0);
            let grid_cols = state.size().cols;
            let (matches, current) = view.search_highlights().unwrap_or((&[], None));
            let link = view.link_highlight();

            cache.sweep(crate::frames::index(cx));
            let base = hash_base(focused, font_size, &family, palette);
            // Rows above the oldest line the host still has: a `~` filler, shaped once.
            let mut filler: Option<Rc<ShapedLine>> = None;
            let mut prepared_rows = Vec::with_capacity(rows_view.len());
            for (i, row) in rows_view.iter().enumerate() {
                let y = origin.y + line_height * f32::from(u16::try_from(i).unwrap_or(u16::MAX));
                let Some(line) = row.line else {
                    let filler = filler.get_or_insert_with(|| {
                        Rc::new(text_system.shape_line(
                            "~".into(),
                            font_size,
                            &[text_run(1, &family, &CellStyle::DEFAULT, palette, grid.underline)],
                            Some(cell_width),
                        ))
                    });
                    prepared_rows.push(PreparedRow {
                        y,
                        quads: Vec::new(),
                        decorations: Vec::new(),
                        segments: vec![(0, Rc::clone(filler))],
                        link: None,
                        separator: None,
                    });
                    continue;
                };
                let mut quads: Vec<(u16, u16, Hsla)> = Vec::new();
                let mut decorations: Vec<Decoration> = Vec::new();
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
                    // would put them; a curly underline is drawn by GPUI with the run.
                    let text = cell_color(&cell.style, palette);
                    if !matches!(cell.style.underline, Underline::None | Underline::Curly) {
                        let color = underline_color(&cell.style, palette, text);
                        stroke(&mut decorations, col, color, grid.underline);
                        if cell.style.underline == Underline::Double {
                            // The second stroke sits one stroke's gap above the first.
                            let above = metrics::Line {
                                y: grid.underline.y - grid.underline.thickness * 2.0,
                                thickness: grid.underline.thickness,
                            };
                            stroke(&mut decorations, col, color, above);
                        }
                    }
                    if cell.style.flags.contains(StyleFlags::STRIKETHROUGH) {
                        stroke(&mut decorations, col, text, grid.strikethrough);
                    }
                }
                // The selection paints over cell backgrounds and under the text.
                let index = slopty_grid::LineIndex(
                    top_index.0.saturating_add(u64::try_from(i).unwrap_or(u64::MAX)),
                );
                if let Some(range) = selection.and_then(|s| s.columns(index, grid_cols)) {
                    quads.push((range.start, range.end, hsla(palette.selection)));
                }
                // Search hits, sorted by line: the slice for this row by binary search.
                let first = matches.partition_point(|m| m.line < index);
                for (k, m) in matches.iter().skip(first).enumerate() {
                    if m.line != index {
                        break;
                    }
                    let color = if current == Some(first.saturating_add(k)) {
                        palette.search_current
                    } else {
                        palette.search_match
                    };
                    quads.push((m.col, m.col.saturating_add(m.len).min(grid_cols), hsla(color)));
                }
                let segments = segments(&line.cells)
                    .into_iter()
                    .map(|(col, cells)| {
                        let key = segment_hash(base, cells);
                        cache.touched.insert(key, cache.generation);
                        let shaped = cache.lines.entry(key).or_insert_with(|| {
                            Rc::new(shape_cells(
                                &text_system,
                                cells,
                                font_size,
                                &grid,
                                &family,
                                palette,
                            ))
                        });
                        (col, Rc::clone(shaped))
                    })
                    .collect();
                let link = link
                    .filter(|&(at, _, _)| at == index)
                    .map(|(_, start, end)| (start, end.min(grid_cols)));
                // A prompt starts here: rule off the command above it, red when it failed.
                let separator = (line.mark.starts_prompt() && index.0 > 0)
                    .then(|| separator_color(theme, line.mark.exit()));
                prepared_rows.push(PreparedRow {
                    y,
                    quads,
                    decorations,
                    segments,
                    link,
                    separator,
                });
            }

            let cursor_visible = cursor.visible
                && view_offset == 0
                && !modes.contains(slopty_grid::TermModes::CURSOR_HIDDEN);
            // While an input method composes, its underlined preview stands in for the cursor.
            let cursor_prepared = (cursor_visible && marked.is_none()).then(|| {
                let x = origin.x + cell_width * f32::from(cursor.col);
                let y = origin.y + line_height * f32::from(cursor.row);
                let shape = if focused { cursor.shape } else { CursorShape::BlockHollow };
                (
                    Bounds::new(point(x, y), size(cell_width, line_height)),
                    shape,
                    hsla(palette.cursor),
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
                        &CellStyle::DEFAULT,
                        palette,
                        grid.underline,
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
                    text_run(text.len(), &family, &CellStyle::DEFAULT, palette, grid.underline);
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

            Prepared {
                metrics,
                grid,
                rows: prepared_rows,
                cursor: cursor_prepared,
                background: hsla(palette.bg),
                link: hsla(palette.fg),
                overlay,
                predicting: !predicted.is_empty(),
            }
        };
        *cx.global_mut::<ShapeCache>() = cache;
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
                        size(m.cell_width, grid.cursor_thickness),
                    ),
                    color,
                ),
                CursorShape::BlockHollow => {
                    gpui::outline(cursor_bounds, color, gpui::BorderStyle::Solid)
                }
            };
            window.paint_quad(quad);
        }
        for row in &prepared.rows {
            for (col, segment) in &row.segments {
                // GPUI centres the line in its own box; nudge it so its baseline is the derived
                // one and the glyphs sit on the same line as the decorations under them.
                let y = text_origin_y(
                    row.y,
                    m.line_height,
                    grid.baseline,
                    segment.ascent,
                    segment.descent,
                );
                let x = m.origin.x + m.cell_width * f32::from(*col);
                if let Err(e) =
                    segment.paint(point(x, y), m.line_height, TextAlign::Left, None, window, cx)
                {
                    tracing::debug!(error = %e, "paint row");
                }
            }
        }
        // Underlines and strikethroughs, over the glyphs, where the font's metrics put them.
        for row in &prepared.rows {
            for deco in &row.decorations {
                let x = m.origin.x + m.cell_width * f32::from(deco.start);
                let w = m.cell_width * f32::from(deco.end.saturating_sub(deco.start));
                let bounds = Bounds::new(point(x, row.y + deco.y), size(w, deco.thickness));
                window.paint_quad(fill(bounds, deco.color));
            }
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
        let predicting = prepared.predicting;
        self.view.update(cx, |view, _cx| view.painted(predicting));
    }
}

/// How many shaped words the cache holds (tests: a word shaped once serves every row).
#[cfg(test)]
pub fn cached_words(cx: &App) -> usize {
    cx.try_global::<ShapeCache>().map_or(0, |cache| cache.lines.len())
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
        }
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

    /// The glyphs go on the baseline the metrics derived, not the one GPUI would centre on.
    /// `JetBrains Mono` 13 pt at DPR 2: a 17 pt row with a 13.5 pt baseline, where GPUI's own
    /// centring (ascent 13.325, descent 3.575) would put it at 13.375.
    #[test]
    fn the_text_sits_on_the_derived_baseline() {
        let (row_y, line_height) = (px(100.0), px(17.0));
        let (ascent, descent) = (px(13.325), px(3.575));
        let centred = (line_height - ascent - descent) / 2.0 + ascent;
        assert!((f32::from(centred) - 13.375).abs() < 1e-4, "{centred:?}");

        // The derived baseline is the lower of the two, so the line is nudged down by 0.125.
        let y = text_origin_y(row_y, line_height, px(13.5), ascent, descent);
        assert!((f32::from(y) - 100.125).abs() < 1e-4, "{y:?}");
        // Which is to say: painting there puts the baseline exactly where it was derived.
        assert!((f32::from(y + centred - row_y) - 13.5).abs() < 1e-4);

        // A row that fell back to a taller face is corrected by its own amount.
        let tall = text_origin_y(row_y, line_height, px(13.5), px(15.0), px(4.0));
        assert!(tall < y, "{tall:?} vs {y:?}");
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
        assert_eq!(segments(&row).len(), 1, "a curly-underlined number is one piece");
    }

    #[test]
    fn only_a_curly_underline_keeps_a_space_in_its_word() {
        let mut row = cells("a b");
        row[1].style.underline = Underline::Curly;
        assert_eq!(segments(&row).len(), 1, "GPUI draws the wave with the run");
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
        assert_eq!(segment_hash(1, wa[1].1), segment_hash(1, wb[0].1), "bar");
        assert_eq!(segment_hash(1, wa[0].1), segment_hash(1, wb[1].1), "foo");
        assert_ne!(segment_hash(1, wa[0].1), segment_hash(2, wa[0].1), "another base");
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

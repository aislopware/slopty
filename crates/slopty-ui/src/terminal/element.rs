//! `TerminalElement`: paints a `TermState` as cell-aligned text runs and quads.
//!
//! Per row: one shaped line with a run per style change, background quads for non-default
//! backgrounds, and the cursor. Shaping is cached by row content hash across frames.

use std::collections::HashMap;
use std::hash::{Hash as _, Hasher as _};

use gpui::{
    App, BorrowAppContext as _, Bounds, DispatchPhase, Element, ElementId, ElementInputHandler,
    Entity, Focusable as _, Font, FontId, GlobalElementId, Hsla, InspectorElementId, IntoElement,
    LayoutId, LongPressEvent, Pixels, Point, ShapedLine, SharedString, Size, Style, TextAlign,
    TextRun, UnderlineStyle, Window, fill, point, px, relative, size,
};
use slopty_grid::{CursorShape, Line, Style as CellStyle, StyleFlags, Underline};
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
}

#[derive(Debug)]
struct PreparedRow {
    y: Pixels,
    quads: Vec<(u16, u16, Hsla)>,
    /// Underlines and strikethroughs, at ghostty's offsets rather than GPUI's.
    decorations: Vec<Decoration>,
    line: ShapedLine,
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

/// Shaped-line cache keyed by (row text + styles) hash, kept on the App as a global so it
/// survives across frames and views.
#[derive(Default)]
struct ShapeCache {
    lines: HashMap<u64, ShapedLine>,
    generation: u64,
    touched: HashMap<u64, u64>,
}

impl gpui::Global for ShapeCache {}

impl ShapeCache {
    /// Drop entries not used in the last two generations.
    fn sweep(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        let keep_after = self.generation.saturating_sub(2);
        self.touched.retain(|_, g| *g >= keep_after);
        let touched = &self.touched;
        self.lines.retain(|k, _| touched.contains_key(k));
    }
}

/// Key of a shaped row: everything the shaped runs bake in (text, styles, size, family and
/// the palette the styles were resolved through), so a theme swap never replays old colours.
fn row_hash(
    line: &Line,
    focused: bool,
    font_size: Pixels,
    family: &str,
    palette: &TerminalPalette,
) -> u64 {
    let mut h = std::hash::DefaultHasher::new();
    focused.hash(&mut h);
    f32::from(font_size).to_bits().hash(&mut h);
    family.hash(&mut h);
    palette.hash(&mut h);
    for cell in &line.cells {
        cell.text.as_str().hash(&mut h);
        cell.style.hash(&mut h);
        (cell.width as u8).hash(&mut h);
    }
    h.finish()
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
        let (theme, rows_view, cursor, view_offset, modes, known_family, predicted, marked) = {
            let view = self.view.read(cx);
            let state = view.state();
            let rows: Vec<Option<Line>> =
                state.view().into_iter().map(|r| r.line.cloned()).collect();
            let predicted = view.predictions();
            (
                view.theme().clone(),
                rows,
                predicted.as_ref().map_or_else(|| state.cursor(), |(_, c)| *c),
                state.view_offset(),
                state.modes(),
                view.font_family().map(str::to_owned),
                predicted.map(|(p, _)| p).unwrap_or_default(),
                view.marked().map(str::to_owned),
            )
        };
        let (selection, top_index, grid_cols, hits, link) = {
            let view = self.view.read(cx);
            let hits = view
                .search_highlights()
                .map(|(matches, current)| (matches.to_vec(), current))
                .unwrap_or_default();
            (
                view.selection(),
                view.state().index_at_row(0),
                view.state().size().cols,
                hits,
                view.link_highlight(),
            )
        };
        let palette = &theme.terminal;
        // Resolving the family walks every installed font; do it once per view.
        let family = known_family.unwrap_or_else(|| {
            let picked = pick_family(window, &theme.typography.mono_families);
            self.view.update(cx, |view, _cx| view.set_font_family(picked.clone()));
            picked
        });
        let zoom = if self.zoom.is_finite() && self.zoom > 0.0 { self.zoom } else { 1.0 };
        let base_font = fonts::terminal_font(&family, false, false);
        // Grid size comes from the unscaled geometry so zooming never resizes the PTY.
        let base_size = px(theme.typography.mono_size);
        let height_mult = theme.typography.mono_line_height;
        let (base_grid, base_derived, _font_id) =
            measure(window, &base_font, base_size, height_mult);
        let (base_cell_width, base_line_height) = (base_grid.cell_width, base_grid.line_height);
        let base_pad = px(theme.spacing.sm);
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

        if !cx.has_global::<ShapeCache>() {
            cx.set_global(ShapeCache::default());
        }
        let focused = self.focused;
        let mut prepared_rows = Vec::with_capacity(rows_view.len());
        let text_system = std::sync::Arc::clone(window.text_system());
        cx.update_global::<ShapeCache, _>(|cache, _cx| {
            cache.sweep();
            for (i, line) in rows_view.iter().enumerate() {
                let y = origin.y + line_height * f32::from(u16::try_from(i).unwrap_or(u16::MAX));
                let Some(line) = line else {
                    prepared_rows.push(PreparedRow {
                        y,
                        quads: Vec::new(),
                        decorations: Vec::new(),
                        line: text_system.shape_line(
                            "~".into(),
                            font_size,
                            &[text_run(1, &family, &CellStyle::DEFAULT, palette, grid.underline)],
                            Some(cell_width),
                        ),
                        link: None,
                        separator: None,
                    });
                    continue;
                };
                let key = row_hash(line, focused, font_size, &family, palette);
                cache.touched.insert(key, cache.generation);
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
                let (matches, current) = &hits;
                let first = matches.partition_point(|m| m.line < index);
                for (k, m) in matches.iter().skip(first).enumerate() {
                    if m.line != index {
                        break;
                    }
                    let color = if *current == Some(first.saturating_add(k)) {
                        palette.search_current
                    } else {
                        palette.search_match
                    };
                    quads.push((m.col, m.col.saturating_add(m.len).min(grid_cols), hsla(color)));
                }
                let shaped = if let Some(s) = cache.lines.get(&key) {
                    s.clone()
                } else {
                    let mut text = String::with_capacity(line.cells.len());
                    let mut runs: Vec<TextRun> = Vec::new();
                    let mut current: Option<(CellStyle, usize)> = None;
                    for cell in &line.cells {
                        if !cell.width.draws_text() {
                            continue;
                        }
                        let piece: &str =
                            if cell.text.is_empty() { " " } else { cell.text.as_str() };
                        // A wide cell occupies two columns; pad with a space so advances line up
                        // under forced width.
                        text.push_str(piece);
                        let mut len = piece.len();
                        if cell.width.columns() == 2 && piece.chars().count() == 1 {
                            // shape_line forces per-glyph width; a wide glyph gets one cell, so
                            // add a spacer cell after it.
                            text.push(' ');
                            len = len.saturating_add(1);
                        }
                        match &mut current {
                            Some((style, acc)) if *style == cell.style => {
                                *acc = acc.saturating_add(len);
                            }
                            _ => {
                                if let Some((style, acc)) = current.take() {
                                    runs.push(text_run(
                                        acc,
                                        &family,
                                        &style,
                                        palette,
                                        grid.underline,
                                    ));
                                }
                                current = Some((cell.style, len));
                            }
                        }
                    }
                    if let Some((style, acc)) = current.take() {
                        runs.push(text_run(acc, &family, &style, palette, grid.underline));
                    }
                    let shaped = text_system.shape_line(
                        SharedString::from(text),
                        font_size,
                        &runs,
                        Some(cell_width),
                    );
                    cache.lines.insert(key, shaped.clone());
                    shaped
                };
                let link = link
                    .filter(|&(at, _, _)| at == index)
                    .map(|(_, start, end)| (start, end.min(grid_cols)));
                // A prompt starts here: rule off the command above it, red when it failed.
                let separator = (line.mark.starts_prompt() && index.0 > 0)
                    .then(|| separator_color(&theme, line.mark.exit()));
                prepared_rows.push(PreparedRow {
                    y,
                    quads,
                    decorations,
                    line: shaped,
                    link,
                    separator,
                });
            }
        });

        let cursor_visible = cursor.visible
            && view_offset == 0
            && !modes.contains(slopty_grid::TermModes::CURSOR_HIDDEN);
        // While an input method composes, its underlined preview stands in for the cursor.
        let cursor_prepared = (cursor_visible && marked.is_none()).then(|| {
            let x = origin.x + cell_width * f32::from(cursor.col);
            let y = origin.y + line_height * f32::from(cursor.row);
            let shape = if focused { cursor.shape } else { CursorShape::BlockHollow };
            (Bounds::new(point(x, y), size(cell_width, line_height)), shape, hsla(palette.cursor))
        });

        // Local echo: predicted glyphs, slightly dimmed so a wrong guess never looks final.
        let mut overlay: Vec<(Point<Pixels>, ShapedLine)> = predicted
            .iter()
            .map(|p| {
                let mut run =
                    text_run(p.text.len(), &family, &CellStyle::DEFAULT, palette, grid.underline);
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
            run.underline =
                Some(UnderlineStyle { thickness: px(1.0), color: Some(run.color), wavy: false });
            let shaped = text_system.shape_line(
                SharedString::from(text),
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
        }
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
            // GPUI centres the line in its own box; nudge it so its baseline is the derived one
            // and the glyphs sit on the same line as the decorations under them.
            let y = text_origin_y(
                row.y,
                m.line_height,
                grid.baseline,
                row.line.ascent,
                row.line.descent,
            );
            if let Err(e) = row.line.paint(
                point(m.origin.x, y),
                m.line_height,
                TextAlign::Left,
                None,
                window,
                cx,
            ) {
                tracing::debug!(error = %e, "paint row");
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
    }
}

#[cfg(test)]
mod tests {
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
}

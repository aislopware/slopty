//! The grid's keyed paint against a grid painted whole.
//!
//! The element names each row's stretches of paint by a key (`Window::paint_keyed`), and GPUI
//! draws a stretch whose key it painted last frame again from that frame, in place or moved.
//! Two windows here take the same random history of output, edits, scrolls, selections, search
//! hits, cursor moves and blinks, input-method text, links, themes (colours, font size, line
//! height, ligatures, minimum contrast, bold as bright), zooms, focus, resizes and scale factors
//! (1.5 among them, where a row's move is often not whole device pixels): one draws with
//! retention on, so keyed stretches are drawn again, and one with it off, so every stretch is
//! painted afresh. Every frame the two paint must match, primitive for primitive and in the
//! same order.
//!
//! The windows shape with a text system of their own ([`Glyphs`]): GPUI's test one
//! rasterises nothing, so a test on it never sees a glyph painted. Here every glyph gets a
//! raster of its own size, so a glyph painted in the wrong place, colour or character shows.

use std::borrow::Cow;
use std::sync::Arc;

use gpui::{
    Bounds, DevicePixels, Font, FontId, FontMetrics, FontRun, FontStyle, FontWeight, GlyphId,
    LineLayout, NoopTextSystem, Pixels, PlatformTextSystem, RenderGlyphParams, ShapedGlyph,
    ShapedRun, Size, TestAppContext, TestDispatcher, TextRenderingMode, VisualTestContext, Window,
    WindowHandle, point, px, size,
};
use slopty_grid::{
    Cell, Color, Cursor, CursorShape, Line, LineIndex, RowUpdate, SemanticMark, Style, StyleFlags,
    TermModes, Underline,
};
use slopty_proto::terminal::{Frame, SearchMatch, TermEvent, TermSize};
use slopty_theme::{Theme, Variant};
use tokio::sync::mpsc;

use super::{Find, Selection, TerminalView};

/// A text system whose glyphs are their characters: each is a glyph of its own, one em in
/// three fifths wide (two for a wide one), rasterised at a size that follows its id and face,
/// so what the scene holds tells glyphs apart.
pub(super) struct Glyphs;

/// An emoji, painted in colour: the one wide character here drawn as one.
const EMOJI: char = '\u{1f600}';

impl Glyphs {
    /// The face a font resolves to: regular, bold, italic or both, told apart.
    const fn face(font: &Font) -> FontId {
        let bold = font.weight.0 >= FontWeight::BOLD.0;
        let italic = matches!(font.style, FontStyle::Italic | FontStyle::Oblique);
        FontId(match (bold, italic) {
            (false, false) => 1,
            (true, false) => 2,
            (false, true) => 3,
            (true, true) => 4,
        })
    }
}

impl PlatformTextSystem for Glyphs {
    fn add_fonts(&self, fonts: Vec<Cow<'static, [u8]>>) -> anyhow::Result<()> {
        NoopTextSystem.add_fonts(fonts)
    }

    fn all_font_names(&self) -> Vec<String> {
        Vec::new()
    }

    fn font_id(&self, descriptor: &Font) -> anyhow::Result<FontId> {
        Ok(Self::face(descriptor))
    }

    fn font_metrics(&self, font_id: FontId) -> FontMetrics {
        NoopTextSystem.font_metrics(font_id)
    }

    fn typographic_bounds(
        &self,
        font_id: FontId,
        glyph_id: GlyphId,
    ) -> anyhow::Result<Bounds<f32>> {
        NoopTextSystem.typographic_bounds(font_id, glyph_id)
    }

    fn advance(&self, _font_id: FontId, _glyph_id: GlyphId) -> anyhow::Result<Size<f32>> {
        Ok(size(600.0, 0.0))
    }

    fn glyph_for_char(&self, _font_id: FontId, ch: char) -> Option<GlyphId> {
        Some(GlyphId(u32::from(ch)))
    }

    fn glyph_raster_bounds(
        &self,
        params: &RenderGlyphParams,
    ) -> anyhow::Result<Bounds<DevicePixels>> {
        // The scene keeps a sprite's bounds and colour but not its tile, so the size alone must
        // tell every glyph and face the histories paint apart: the width is the id modulo a
        // prime above every ASCII character, the height the face and what the modulo lost.
        let id = params.glyph_id.0;
        let low = i32::try_from(id.wrapping_rem(251)).unwrap_or(0);
        let high = i32::try_from((id / 251).wrapping_rem(8)).unwrap_or(0);
        let face = i32::try_from(params.font_id.0).unwrap_or(0);
        Ok(Bounds {
            origin: point(
                DevicePixels(face.wrapping_rem(2)),
                DevicePixels(low.wrapping_rem(5).wrapping_neg().wrapping_sub(9)),
            ),
            size: size(
                DevicePixels(low.wrapping_add(1)),
                DevicePixels(face.wrapping_mul(8).wrapping_add(high).wrapping_add(9)),
            ),
        })
    }

    fn rasterize_glyph(
        &self,
        _params: &RenderGlyphParams,
        raster_bounds: Bounds<DevicePixels>,
    ) -> anyhow::Result<(Size<DevicePixels>, Vec<u8>)> {
        let bytes = usize::try_from(
            raster_bounds
                .size
                .width
                .0
                .saturating_mul(raster_bounds.size.height.0)
                .saturating_mul(4),
        )
        .unwrap_or(0);
        Ok((raster_bounds.size, vec![0; bytes]))
    }

    fn layout_line(&self, text: &str, font_size: Pixels, runs: &[FontRun]) -> LineLayout {
        let em = font_size * 0.6;
        let metrics = self.font_metrics(FontId(0));
        let mut x = px(0.0);
        let mut start = 0_usize;
        let mut shaped = Vec::with_capacity(runs.len());
        for run in runs {
            let end = start.saturating_add(run.len).min(text.len());
            let glyphs = text
                .get(start..end)
                .unwrap_or_default()
                .char_indices()
                .map(|(at, ch)| {
                    let glyph = ShapedGlyph {
                        id: GlyphId(u32::from(ch)),
                        position: point(x, px(0.0)),
                        index: start.saturating_add(at),
                        is_emoji: ch == EMOJI,
                    };
                    x += if ch == EMOJI || ch == '字' { em * 2.0 } else { em };
                    glyph
                })
                .collect();
            shaped.push(ShapedRun { font_id: run.font_id, glyphs });
            start = end;
        }
        #[expect(clippy::cast_precision_loss, reason = "a font's units per em, far below 2^24")]
        let per_em = metrics.units_per_em as f32;
        LineLayout {
            font_size,
            width: x,
            ascent: font_size * (metrics.ascent / per_em),
            descent: font_size * (metrics.descent / per_em),
            runs: shaped,
            len: text.len(),
        }
    }

    fn recommended_rendering_mode(
        &self,
        _font_id: FontId,
        _font_size: Pixels,
    ) -> TextRenderingMode {
        TextRenderingMode::Grayscale
    }
}

/// xorshift64: the history is the seed's.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// A number below `n`.
    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next().checked_rem(u64::try_from(n).unwrap_or(1)).unwrap_or(0))
            .unwrap_or(0)
    }

    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }

    fn u16_below(&mut self, n: u16) -> u16 {
        u16::try_from(self.below(usize::from(n))).unwrap_or(0)
    }

    fn u8(&mut self) -> u8 {
        u8::try_from(self.next() & 0xff).unwrap_or(0)
    }
}

const COLS: u16 = 36;
const ROWS: u16 = 10;

/// A random style: colours from the palette or true colour, the attributes, every underline.
fn style(rng: &mut Rng) -> Style {
    let color = |rng: &mut Rng| match rng.below(4) {
        0 => Color::Palette(rng.u8() % 16),
        1 => Color::Rgb(rng.u8(), rng.u8(), rng.u8()),
        _ => Color::Default,
    };
    let mut flags = StyleFlags::empty();
    for flag in [
        StyleFlags::BOLD,
        StyleFlags::FAINT,
        StyleFlags::ITALIC,
        StyleFlags::BLINK,
        StyleFlags::INVERSE,
        StyleFlags::INVISIBLE,
        StyleFlags::STRIKETHROUGH,
    ] {
        flags.set(flag, rng.chance(8));
    }
    let underline = match rng.below(14) {
        0 => Underline::Single,
        1 => Underline::Double,
        2 => Underline::Curly,
        3 => Underline::Dotted,
        4 => Underline::Dashed,
        _ => Underline::None,
    };
    Style {
        fg: color(rng),
        bg: if rng.chance(30) { color(rng) } else { Color::Default },
        underline_color: if rng.chance(20) { color(rng) } else { Color::Default },
        underline,
        flags,
    }
}

/// A random row: words, digits, box drawing, blocks, Braille, wide characters, an emoji, a
/// link, and now and then a prompt's or a failed command's mark.
fn line(rng: &mut Rng) -> Line {
    let mut cells: Vec<Cell> = Vec::with_capacity(usize::from(COLS));
    let mut current = style(rng);
    while cells.len() < usize::from(COLS) {
        if rng.chance(15) {
            current = style(rng);
        }
        match rng.below(20) {
            0 | 1 => cells.push(Cell::narrow(' ', current)),
            2 => {
                for ch in ['─', '│', '┼', '█', '⣿', '╭'] {
                    if rng.chance(40) {
                        cells.push(Cell::narrow(ch, current));
                    }
                }
            }
            3 if cells.len() < usize::from(COLS).saturating_sub(1) => {
                let wide = if rng.chance(50) { "字".to_owned() } else { EMOJI.to_string() };
                cells.push(Cell::wide(&wide, current));
                cells.push(Cell::spacer_tail(current));
            }
            4 => cells.extend("https://a.b/c".chars().map(|c| Cell::narrow(c, current))),
            5 => cells.push(Cell::narrow(
                char::from(b'0'.wrapping_add(rng.u8().wrapping_rem(10))),
                current,
            )),
            _ => {
                let ch = char::from(b'a'.wrapping_add(rng.u8().wrapping_rem(26)));
                cells.push(Cell::narrow(ch, current));
            }
        }
    }
    cells.truncate(usize::from(COLS));
    let mut line = Line { cells, ..Line::from_text("", COLS, Style::DEFAULT) };
    line.mark = match rng.below(12) {
        0 => SemanticMark::Prompt { exit: Some(rng.u8() % 2), input: Some(2) },
        1 => SemanticMark::Prompt { exit: None, input: None },
        2 => SemanticMark::Input,
        _ => SemanticMark::Output,
    };
    line
}

/// One window's terminal.
struct Side {
    view: gpui::Entity<TerminalView>,
    cx: VisualTestContext,
}

/// What a window painted last, in the order it is drawn: every quad as it is (its draw order's
/// number aside, which two windows may count differently), then every glyph, icon, image and
/// underline.
fn painted(window: &Window) -> Vec<String> {
    window
        .painted_quads()
        .iter()
        .map(|q| {
            format!(
                "quad {:?} {:?} {:?} {:?} {:?} {:?} {:?}",
                q.bounds,
                q.content_mask,
                q.background,
                q.border_color,
                q.corner_radii,
                q.border_widths,
                q.border_style
            )
        })
        .chain(window.painted_sprites())
        .collect()
}

/// The two windows, drawn once.
fn sides(cx: &mut TestAppContext) -> [Side; 2] {
    [true, false].map(|retained| {
        let (tx, _rx) = mpsc::channel(1 << 12);
        let handle: WindowHandle<TerminalView> = cx.add_window(|window, cx| {
            window.set_view_retention(retained);
            let size = TermSize { cols: COLS, rows: ROWS, ..TermSize::default() };
            let view =
                TerminalView::new(slopty_core::SessionId::new(), size, tx, Theme::default(), cx);
            window.focus(&view.focus, cx);
            view
        });
        let view = handle.entity(cx).unwrap_or_else(|e| panic!("the view: {e}"));
        let cx = VisualTestContext::from_window(handle.into(), cx);
        cx.simulate_resize(size(px(520.0), px(260.0)));
        Side { view, cx }
    })
}

/// A frame from the worker: `updates` at the rows they replace, the screen's top at `first`.
fn frame(seq: u64, first: u64, updates: Vec<(u16, Line)>, cursor: Cursor) -> TermEvent {
    TermEvent::Frame(Frame {
        seq,
        full: seq == 1,
        epoch: 0,
        cols: COLS,
        rows: ROWS,
        cursor,
        modes: TermModes::empty(),
        oldest_line: LineIndex(0),
        first_visible_line: LineIndex(first),
        total_lines: first.saturating_add(u64::from(ROWS)),
        input_ack: 0,
        above: None,
        blocks: None,
        images: Vec::new(),
        updates: updates
            .into_iter()
            .map(|(row, line)| RowUpdate { row, line: line.into() })
            .collect(),
    })
}

/// A step of the history, done to both terminals alike.
type Step = Box<dyn Fn(&mut TerminalView, &mut Window, &mut gpui::Context<TerminalView>)>;

/// Runs `steps` random steps from `seed` on both windows, shaping with `text` and comparing
/// every frame, and gives how many stretches the retained window drew again and how many of
/// those moved.
fn run(seed: u64, steps: usize, text: Arc<dyn PlatformTextSystem>) -> (u64, u64) {
    let mut cx = TestAppContext::build_with_text_system(TestDispatcher::new(seed), None, text);
    cx.update(gpui_kit::init);
    let mut sides = sides(&mut cx);
    let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
    let (mut seq, mut first) = (0_u64, 0_u64);
    let mut cursor =
        Cursor { row: 0, col: 0, shape: CursorShape::Block, visible: true, blink: true };
    let (mut replayed, mut moved) = (0_u64, 0_u64);
    let screen: Vec<(u16, Line)> = (0..ROWS).map(|row| (row, line(&mut rng))).collect();
    seq = seq.saturating_add(1);
    let mut steps_done: Vec<String> = Vec::new();
    let mut pending: Option<(String, Step)> =
        Some(("the first screen".to_owned(), apply(frame(seq, first, screen, cursor))));
    for n in 0..=steps {
        let (what, step) = match pending.take() {
            Some(step) => step,
            None => next_step(&mut rng, &mut seq, &mut first, &mut cursor),
        };
        for side in &mut sides {
            let view = side.view.clone();
            if what.starts_with("resize") {
                side.cx.simulate_resize(size_named(&what));
            }
            if what.starts_with("scale") {
                side.cx.simulate_scale_factor_change(scale_named(&what));
            }
            side.cx.update(|window, cx| view.update(cx, |view, cx| step(view, window, cx)));
        }
        for side in &mut sides {
            side.cx.run_until_parked();
        }
        steps_done.push(what);
        let [kept, fresh] = &mut sides;
        let (shown, stats) = kept.cx.update(|window, _| {
            let stats = window.layout_stats();
            window.reset_layout_stats();
            (painted(window), stats)
        });
        let scratch = fresh.cx.update(|window, _| painted(window));
        replayed = replayed.saturating_add(stats.paints_replayed);
        moved = moved.saturating_add(stats.paints_moved);
        if shown != scratch {
            let at = shown.iter().zip(&scratch).position(|(a, b)| a != b);
            let recent = steps_done.iter().rev().take(6).rev().cloned().collect::<Vec<_>>();
            panic!(
                "seed {seed}, step {n}: the keyed frame differs from one painted whole \
                 ({} against {} primitives), first at {at:?}:\n  kept  {:?}\n  whole {:?}\n\
                 after: {recent:#?}",
                shown.len(),
                scratch.len(),
                at.and_then(|i| shown.get(i)),
                at.and_then(|i| scratch.get(i)),
            );
        }
    }
    (replayed, moved)
}

/// The window sizes a resize step picks from: the first, wider and taller, only taller, a
/// fraction of a cell wider (the grid keeps its columns), and narrower than the rows.
const SIZES: [(f32, f32); 5] =
    [(520.0, 260.0), (560.0, 300.0), (520.0, 330.0), (523.5, 260.0), (300.0, 200.0)];

/// The window size a resize step names.
fn size_named(what: &str) -> Size<Pixels> {
    let at = what.rsplit(' ').next().and_then(|n| n.parse::<usize>().ok()).unwrap_or(0);
    let (w, h) = SIZES.get(at).copied().unwrap_or(SIZES[0]);
    size(px(w), px(h))
}

/// The scale factor a scale step names.
fn scale_named(what: &str) -> f32 {
    what.rsplit(' ').next().and_then(|n| n.parse().ok()).unwrap_or(2.0)
}

fn apply(event: TermEvent) -> Step {
    Box::new(move |view, _window, cx| view.apply(event.clone(), cx))
}

/// The next random step, with what it does.
fn next_step(rng: &mut Rng, seq: &mut u64, first: &mut u64, cursor: &mut Cursor) -> (String, Step) {
    let row = rng.u16_below(ROWS);
    match rng.below(21) {
        0..=3 => {
            *seq = seq.saturating_add(1);
            *first = first.saturating_add(1);
            let fresh = line(rng);
            (
                "a line of output".to_owned(),
                apply(frame(*seq, *first, vec![(ROWS - 1, fresh)], *cursor)),
            )
        }
        4 | 5 => {
            *seq = seq.saturating_add(1);
            let count = rng.below(3).saturating_add(1);
            let rows =
                std::iter::repeat_with(|| (rng.u16_below(ROWS), line(rng))).take(count).collect();
            ("rows rewritten".to_owned(), apply(frame(*seq, *first, rows, *cursor)))
        }
        6 => {
            *seq = seq.saturating_add(1);
            cursor.row = row;
            cursor.col = rng.u16_below(COLS);
            cursor.visible = rng.chance(85);
            cursor.shape =
                [CursorShape::Block, CursorShape::Bar, CursorShape::Underline][rng.below(3)];
            ("the cursor moved".to_owned(), apply(frame(*seq, *first, Vec::new(), *cursor)))
        }
        7 => (
            "the blink".to_owned(),
            Box::new(|view, _window, cx| {
                view.blink_on = !view.blink_on;
                cx.notify();
            }),
        ),
        8 => {
            let top = first.saturating_sub(4);
            let anchor =
                (LineIndex(top.saturating_add(rng.next().wrapping_rem(14))), rng.u16_below(COLS));
            let head =
                (LineIndex(top.saturating_add(rng.next().wrapping_rem(14))), rng.u16_below(COLS));
            let selection =
                (!rng.chance(25)).then_some(Selection { anchor, head, block: rng.chance(30) });
            (
                format!("selected {selection:?}"),
                Box::new(move |view, _window, cx| {
                    view.selection = selection;
                    cx.notify();
                }),
            )
        }
        9 => {
            let lines: Vec<SearchMatch> = {
                let count = rng.below(5);
                let mut hits: Vec<SearchMatch> = std::iter::repeat_with(|| SearchMatch {
                    line: LineIndex(
                        first.saturating_sub(3).saturating_add(rng.next().wrapping_rem(12)),
                    ),
                    col: rng.u16_below(COLS),
                    len: rng.u16_below(6).saturating_add(1),
                })
                .take(count)
                .collect();
                hits.sort_by_key(|m| (m.line, m.col));
                hits
            };
            let close = rng.chance(20);
            (
                format!("search hits {lines:?}, closed {close}"),
                Box::new(move |view, window, cx| {
                    if close {
                        view.search = None;
                        cx.notify();
                        return;
                    }
                    if view.search.is_none() {
                        view.find(&Find, window, cx);
                    }
                    if let Some(search) = &mut view.search {
                        search.query.needle = "x".to_owned();
                        search.asked = "x".to_owned();
                    }
                    let total = u32::try_from(lines.len()).unwrap_or(0);
                    view.matches_arrived("x", total, lines.clone(), cx);
                }),
            )
        }
        10 => {
            let lines = i64::try_from(rng.below(7)).unwrap_or(0).saturating_sub(3);
            ("scrolled".to_owned(), Box::new(move |view, _window, cx| view.scroll_lines(lines, cx)))
        }
        11 => {
            let text = ["かな", "a", "字字"][rng.below(3)].to_owned();
            let on = rng.chance(60);
            (
                format!("composing {on}"),
                Box::new(move |view, _window, cx| {
                    view.marked = on.then(|| text.clone());
                    cx.notify();
                }),
            )
        }
        12 => {
            let at = (rng.u16_below(COLS), row);
            let held = rng.chance(70);
            (
                format!("the command key held {held} over {at:?}"),
                Box::new(move |view, _window, cx| {
                    view.cmd_held = held;
                    view.hover = Some(at);
                    cx.notify();
                }),
            )
        }
        13 => {
            let variant = if rng.chance(50) { Variant::Light } else { Variant::Dark };
            let short = rng.chance(40);
            let font = [0.0, 0.0, -1.5, 2.0][rng.below(4)];
            let ligatures = rng.chance(70);
            let contrast = [100, 100, 450][rng.below(3)];
            let bright = rng.chance(50);
            (
                format!(
                    "theme {variant:?}, short lines {short}, font {font:+}, ligatures \
                     {ligatures}, contrast {contrast}, bold bright {bright}"
                ),
                Box::new(move |view, _window, cx| {
                    let mut theme = Theme::new(variant);
                    if short {
                        theme.typography.mono_line_height = 0.8;
                    }
                    theme.typography.mono_size += font;
                    theme.typography.ligatures = ligatures;
                    theme.terminal.minimum_contrast = contrast;
                    theme.terminal.bold_is_bright = bright;
                    view.set_theme(theme, cx);
                }),
            )
        }
        14 => {
            let zoom = [1.0, 1.0, 0.5, 0.75, 1.25][rng.below(5)];
            let zooming = rng.chance(30);
            (
                format!("zoom {zoom}, in motion {zooming}"),
                Box::new(move |view, _window, cx| {
                    view.set_zoom(zoom);
                    view.set_zooming(zooming);
                    cx.notify();
                }),
            )
        }
        15 => {
            let focus = rng.chance(60);
            (
                format!("focused {focus}"),
                Box::new(move |view, window, cx| {
                    if focus {
                        window.focus(&view.focus, cx);
                    } else {
                        window.blur(cx);
                    }
                }),
            )
        }
        16 => (format!("resize {}", rng.below(SIZES.len())), Box::new(|_, _, _| {})),
        17 => {
            let scale = [1.0, 2.0, 1.5][rng.below(3)];
            (format!("scale {scale}"), Box::new(|_, _, _| {}))
        }
        _ => ("redrawn".to_owned(), Box::new(|_view, _window, cx| cx.notify())),
    }
}

/// Runs the histories of seeds 1 to 6 on `text`, and checks that stretches were drawn again,
/// in place and moved.
fn histories(text: &Arc<dyn PlatformTextSystem>) {
    let (mut replayed, mut moved) = (0_u64, 0_u64);
    for seed in 1..=6 {
        let (r, m) = run(seed, 250, Arc::clone(text));
        replayed = replayed.saturating_add(r);
        moved = moved.saturating_add(m);
    }
    assert!(replayed > 1_000, "stretches were drawn again: {replayed}");
    assert!(moved > 100, "stretches were drawn again moved: {moved}");
}

/// Every frame of a random history paints what painting the grid whole paints, every glyph a
/// raster of its own size.
#[test]
fn the_keyed_grid_paints_what_a_grid_painted_whole_paints() {
    let glyphs: Arc<dyn PlatformTextSystem> = Arc::new(Glyphs);
    histories(&glyphs);
}

/// The same on the Mac's own text system: real fonts shaped and rasterised by Core Text, at
/// the quarter-pixel places GPUI puts glyphs, where a row drawn again moved would drift from
/// one painted there if anything it paints fell between two.
#[test]
fn the_keyed_grid_paints_what_a_grid_painted_whole_paints_in_core_text() {
    let text = gpui_platform::text_system();
    // Without `font-kit` the platform's text system is GPUI's no-op one, which draws no glyph.
    let menlo = text.font_id(&gpui::font("Menlo")).unwrap_or_else(|e| panic!("Menlo: {e}"));
    let m = text.glyph_for_char(menlo, 'm').unwrap_or_else(|| panic!("no glyph for m"));
    let params = RenderGlyphParams {
        font_id: menlo,
        glyph_id: m,
        font_size: px(13.0),
        subpixel_variant: point(0, 0),
        scale_factor: 2.0,
        is_emoji: false,
        subpixel_rendering: false,
        dilation: 0,
    };
    let ink = text.glyph_raster_bounds(&params).unwrap_or_else(|e| panic!("raster: {e}"));
    assert!(ink.size.width.0 > 0 && ink.size.height.0 > 0, "Core Text rasterises glyphs");
    histories(&text);
}

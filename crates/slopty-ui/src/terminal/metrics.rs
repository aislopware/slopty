//! Cell geometry, derived the way ghostty derives it.
//!
//! A terminal that looks right is mostly a question of where the pixels land: cells must be
//! whole device pixels or every column drifts, the baseline must sit the same distance from
//! the top of every row, and underlines must not move when the font size changes by a point.
//! ghostty solves this in `vendor/ghostty/src/font/Metrics.zig`; this is that derivation as a
//! pure function, so it can be checked against hand-computed numbers without a window.
//!
//! Everything here is in **device pixels**: [`calc`] takes a [`Face`] already scaled by the
//! display's pixel ratio and returns whole-pixel [`Metrics`]. [`Grid`] converts back to the
//! logical points GPUI lays out in, which is where the guarantee comes from — a cell is a
//! whole number of device pixels, so a column boundary never lands mid-pixel.
//!
//! Not ported: `icon_height`/`icon_height_single` (nerd-font icon constraints) and `ic_width`
//! (CJK size matching), which belong to ghostty's own glyph rasteriser; we shape through GPUI.

use gpui::{Pixels, px};

/// Minimum values, so a modifier can never produce a zero-thickness line or a zero-size cell
/// (ghostty's `Minimums`).
mod min {
    pub(super) const CELL: f64 = 1.0;
    pub(super) const THICKNESS: f64 = 1.0;
}

/// What a font says about itself, in device pixels at the size in use.
///
/// The sign convention is ghostty's, which is also font-kit's (and so GPUI's): distances are
/// relative to the baseline with **+Y up**, so `descent` is normally negative and
/// `underline_position` (the *top* of the stroke) is normally negative too. The optional
/// fields are the ones a font may leave undefined; the estimates for them are ghostty's.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Face {
    /// The widest advance among the printable ASCII glyphs.
    pub cell_width: f64,
    /// Typographic ascent (positive).
    pub ascent: f64,
    /// Typographic descent (negative).
    pub descent: f64,
    /// Line gap ("leading"), positive.
    pub line_gap: f64,
    /// Top of the underline stroke, relative to the baseline.
    pub underline_position: Option<f64>,
    /// Underline stroke thickness.
    pub underline_thickness: Option<f64>,
    /// Top of the strikethrough stroke, relative to the baseline.
    pub strikethrough_position: Option<f64>,
    /// Strikethrough stroke thickness.
    pub strikethrough_thickness: Option<f64>,
    /// Height of a capital letter.
    pub cap_height: Option<f64>,
    /// Height of a lowercase `x`.
    pub ex_height: Option<f64>,
}

impl Face {
    /// The same face measured at `scale` times the size (the display's pixel ratio).
    #[must_use]
    pub fn scaled(&self, scale: f64) -> Self {
        let by = |v: Option<f64>| v.map(|v| v * scale);
        Self {
            cell_width: self.cell_width * scale,
            ascent: self.ascent * scale,
            descent: self.descent * scale,
            line_gap: self.line_gap * scale,
            underline_position: by(self.underline_position),
            underline_thickness: by(self.underline_thickness),
            strikethrough_position: by(self.strikethrough_position),
            strikethrough_thickness: by(self.strikethrough_thickness),
            cap_height: by(self.cap_height),
            ex_height: by(self.ex_height),
        }
    }

    /// `ascent - descent + line_gap`.
    #[must_use]
    pub fn line_height(&self) -> f64 {
        self.ascent - self.descent + self.line_gap
    }

    /// The cap height, estimated as 75% of the ascent when the font does not say.
    #[must_use]
    pub fn cap_height(&self) -> f64 {
        self.cap_height.filter(|v| *v > 0.0).unwrap_or(0.75 * self.ascent)
    }

    /// The ex height, estimated as 75% of the cap height when the font does not say.
    #[must_use]
    pub fn ex_height(&self) -> f64 {
        self.ex_height.filter(|v| *v > 0.0).unwrap_or_else(|| 0.75 * self.cap_height())
    }

    /// The underline thickness, estimated as 15% of the ex height when the font does not say.
    #[must_use]
    pub fn underline_thickness(&self) -> f64 {
        self.underline_thickness.filter(|v| *v > 0.0).unwrap_or_else(|| 0.15 * self.ex_height())
    }

    /// The strikethrough thickness, the underline's when the font does not say.
    #[must_use]
    pub fn strikethrough_thickness(&self) -> f64 {
        self.strikethrough_thickness
            .filter(|v| *v > 0.0)
            .unwrap_or_else(|| self.underline_thickness())
    }

    /// The top of the underline, one thickness below the baseline when the font does not say.
    #[must_use]
    pub fn underline_position(&self) -> f64 {
        self.underline_position.unwrap_or_else(|| -self.underline_thickness())
    }

    /// The top of the strikethrough; centred on lowercase text when the font does not say.
    #[must_use]
    pub fn strikethrough_position(&self) -> f64 {
        self.strikethrough_position
            .unwrap_or_else(|| f64::midpoint(self.ex_height(), self.strikethrough_thickness()))
    }
}

/// Whole-device-pixel cell geometry (ghostty's `Metrics`).
///
/// Positions are distances from the **top** of the cell, except `cell_baseline`, which is from
/// the bottom — the one place ghostty's convention flips, kept as it is so the derivation can
/// be read against the Zig.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Metrics {
    /// Advance of one cell.
    pub cell_width: u32,
    /// Height of one row.
    pub cell_height: u32,
    /// Baseline, measured up from the bottom of the cell.
    pub cell_baseline: u32,
    /// Top of the underline stroke.
    pub underline_position: u32,
    /// Underline thickness.
    pub underline_thickness: u32,
    /// Top of the strikethrough stroke.
    pub strikethrough_position: u32,
    /// Strikethrough thickness.
    pub strikethrough_thickness: u32,
    /// Top of the overline stroke; may sit above the cell.
    pub overline_position: i32,
    /// Overline thickness.
    pub overline_thickness: u32,
    /// Stroke thickness of box-drawing glyphs.
    pub box_thickness: u32,
    /// Thickness of a bar or underline cursor.
    pub cursor_thickness: u32,
    /// Height of the cursor.
    pub cursor_height: u32,
    /// The unrounded advance, kept for the rounding error.
    pub face_width: f64,
    /// The unrounded line height, kept for the rounding error.
    pub face_height: f64,
    /// Offset from the bottom of the cell to the bottom of the face's box.
    pub face_y: f64,
}

/// A rounded, clamped whole number of pixels.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped into range first; there is no checked f64 → u32"
)]
fn whole(v: f64) -> u32 {
    if v.is_finite() { v.round().clamp(0.0, f64::from(u32::MAX)) as u32 } else { 0 }
}

/// A rounded, clamped whole number of pixels that may be negative.
#[expect(
    clippy::cast_possible_truncation,
    reason = "clamped into range first; there is no checked f64 → i32"
)]
fn whole_signed(v: f64) -> i32 {
    if v.is_finite() { v.round().clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32 } else { 0 }
}

/// Derive the cell geometry of a face (ghostty's `Metrics.calc`).
///
/// The cell width and height are **rounded**, not ceiled: that keeps the error under half a
/// pixel and makes the apparent spacing match between a low- and a high-DPI display, at the
/// cost of a glyph with no side bearing overflowing its cell by a pixel. The baseline is then
/// placed so the face is centred in the rounded cell, so text is inset (or sticks out) by the
/// same amount top and bottom whichever way the rounding went.
#[must_use]
pub fn calc(face: &Face) -> Metrics {
    let face_width = face.cell_width;
    let face_height = face.line_height();
    let cell_width = face_width.round().max(min::CELL);
    let cell_height = face_height.round().max(min::CELL);

    // Half the line gap goes above the text and half below, so text never touches either edge.
    let half_line_gap = face.line_gap / 2.0;
    let face_baseline = half_line_gap - face.descent;
    let cell_baseline = (face_baseline - (cell_height - face_height) / 2.0).round();
    let face_y = cell_baseline - face_baseline;
    let top_to_baseline = cell_height - cell_baseline;

    let underline_thickness = face.underline_thickness().ceil().max(min::THICKNESS);
    let strikethrough_thickness = face.strikethrough_thickness().ceil().max(min::THICKNESS);
    let underline_position = (top_to_baseline - face.underline_position()).round();
    let strikethrough_position = (top_to_baseline - face.strikethrough_position()).round();

    Metrics {
        cell_width: whole(cell_width),
        cell_height: whole(cell_height),
        cell_baseline: whole(cell_baseline),
        underline_position: whole(underline_position),
        underline_thickness: whole(underline_thickness),
        strikethrough_position: whole(strikethrough_position),
        strikethrough_thickness: whole(strikethrough_thickness),
        overline_position: 0,
        overline_thickness: whole(underline_thickness),
        box_thickness: whole(underline_thickness),
        // Not a font metric: ghostty's default, one pixel, is the thinnest a bar or underline
        // cursor may be.
        cursor_thickness: whole(min::THICKNESS),
        cursor_height: whole(cell_height),
        face_width,
        face_height,
        face_y,
    }
}

impl Metrics {
    /// Set the cell height, moving everything that hangs off it (ghostty's `apply` for the
    /// `cell_height` key, which is what an `adjust-cell-height` config entry runs).
    ///
    /// The added or removed pixels are split between the top and the bottom of the cell. When
    /// the difference is odd the extra pixel goes to the edge that needs it more: if the face
    /// currently sits higher than centred, the top gets it, otherwise the bottom does — so the
    /// text stays as close to the middle of the cell as whole pixels allow.
    ///
    /// `cursor_height` deliberately does not follow, as in ghostty: it is its own metric.
    pub fn set_cell_height(&mut self, height: u32) {
        let height = height.max(1);
        if height == self.cell_height {
            return;
        }
        let original = f64::from(self.cell_height);
        let diff = f64::from(height) - original;
        let half = diff / 2.0;
        let centred = self.face_y - (original - self.face_height) / 2.0;
        let (diff_top, diff_bottom) =
            if centred > 0.0 { (half.ceil(), half.floor()) } else { (half.floor(), half.ceil()) };

        self.cell_height = height;
        self.cell_baseline = add(self.cell_baseline, diff_bottom);
        self.face_y += diff_bottom;
        self.underline_position = add(self.underline_position, diff_top);
        self.strikethrough_position = add(self.strikethrough_position, diff_top);
        self.overline_position = self.overline_position.saturating_add(whole_signed(diff_top));
    }
}

/// Saturating `u32 + f64`, the float being a whole number of pixels either way.
fn add(value: u32, delta: f64) -> u32 {
    if delta >= 0.0 {
        value.saturating_add(whole(delta))
    } else {
        value.saturating_sub(whole(-delta))
    }
}

/// One decoration line: where it starts, measured down from the top of the cell, and how thick.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Line {
    /// Distance from the top of the cell to the top of the stroke.
    pub y: Pixels,
    /// Stroke thickness.
    pub thickness: Pixels,
}

/// The geometry the element lays out with: [`Metrics`] back in logical points.
///
/// Every field is a whole number of device pixels divided by the scale factor, so the grid is
/// pixel-aligned on the display it was measured for.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Grid {
    /// Advance of one cell.
    pub cell_width: Pixels,
    /// Height of one row.
    pub line_height: Pixels,
    /// Distance from the top of the cell to the baseline.
    pub baseline: Pixels,
    /// The underline.
    pub underline: Line,
    /// The strikethrough.
    pub strikethrough: Line,
    /// The overline.
    pub overline: Line,
    /// Thickness of a bar or underline cursor.
    pub cursor_thickness: Pixels,
}

impl Grid {
    /// Convert whole device pixels back to logical points.
    #[must_use]
    pub fn new(metrics: &Metrics, scale: f32) -> Self {
        let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
        let pt = |v: u32| px(v.min(u32::from(u16::MAX)).pipe_f32() / scale);
        let signed = |v: i32| px(v.clamp(-65535, 65535).pipe_f32() / scale);
        let height = pt(metrics.cell_height);
        Self {
            cell_width: pt(metrics.cell_width),
            line_height: height,
            baseline: height - pt(metrics.cell_baseline),
            underline: Line {
                y: pt(metrics.underline_position),
                thickness: pt(metrics.underline_thickness),
            },
            strikethrough: Line {
                y: pt(metrics.strikethrough_position),
                thickness: pt(metrics.strikethrough_thickness),
            },
            overline: Line {
                y: signed(metrics.overline_position),
                thickness: pt(metrics.overline_thickness),
            },
            cursor_thickness: pt(metrics.cursor_thickness),
        }
    }
}

/// Widening that is exact for the ranges above (`u32` clamped to `u16::MAX`, `i32` to ±65535).
trait PipeF32 {
    fn pipe_f32(self) -> f32;
}

impl PipeF32 for u32 {
    fn pipe_f32(self) -> f32 {
        f32::from(u16::try_from(self).unwrap_or(u16::MAX))
    }
}

impl PipeF32 for i32 {
    fn pipe_f32(self) -> f32 {
        f32::from(i16::try_from(self).unwrap_or(i16::MAX))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A made-up face with round numbers, so every step of the derivation can be checked by
    /// eye: 1000 units per em, advance 600, ascent 1020, descent -300, no line gap, underline
    /// -125 thick 100, cap height 730, ex height 550.
    fn round_numbers(size: f64) -> Face {
        let em = |units: f64| units / 1000.0 * size;
        Face {
            cell_width: em(600.0),
            ascent: em(1020.0),
            descent: em(-300.0),
            line_gap: 0.0,
            underline_position: Some(em(-125.0)),
            underline_thickness: Some(em(100.0)),
            strikethrough_position: None,
            strikethrough_thickness: None,
            cap_height: Some(em(730.0)),
            ex_height: Some(em(550.0)),
        }
    }

    /// `JetBrains Mono` 2.304, the bundled face, exactly as GPUI reports it (Core Text's
    /// adjusted values, not the `hhea` table's): 1000 units per em, advance 600, ascent 1025,
    /// descent -275, cap height 698, ex height 516. GPUI exposes neither the line gap nor the
    /// `post` underline metrics, so those are `None` and ghostty's estimates stand in — which
    /// is what the element passes in production.
    fn jetbrains_mono(size: f64) -> Face {
        let em = |units: f64| units / 1000.0 * size;
        Face {
            cell_width: em(600.0),
            ascent: em(1025.0),
            descent: em(-275.0),
            line_gap: 0.0,
            cap_height: Some(em(698.0)),
            ex_height: Some(em(516.0)),
            ..Face::default()
        }
    }

    /// Menlo, macOS's own terminal face, from its font tables: 2048 units per em, advance
    /// 1233, ascent 1901, descent -483, no line gap, underline -130 thick 90, cap height 1493,
    /// ex height 1120. A face with every metric present, and a different em, so the
    /// derivation is exercised on more than one shape.
    fn menlo(size: f64) -> Face {
        let em = |units: f64| units / 2048.0 * size;
        Face {
            cell_width: em(1233.0),
            ascent: em(1901.0),
            descent: em(-483.0),
            line_gap: 0.0,
            underline_position: Some(em(-130.0)),
            underline_thickness: Some(em(90.0)),
            strikethrough_position: None,
            strikethrough_thickness: None,
            cap_height: Some(em(1493.0)),
            ex_height: Some(em(1120.0)),
        }
    }

    #[test]
    fn a_cell_is_the_rounded_advance_and_the_rounded_line_height() {
        // 13 pt: advance 7.8 → 8, line height (1020 + 300)/1000 * 13 = 17.16 → 17.
        let m = calc(&round_numbers(13.0));
        assert_eq!((m.cell_width, m.cell_height), (8, 17));
        assert!((m.face_width - 7.8).abs() < 1e-9, "{:?}", m.face_width);
        assert!((m.face_height - 17.16).abs() < 1e-9, "{:?}", m.face_height);

        // The baseline: no line gap, so the face wants it 3.9 px above the bottom; the cell was
        // rounded down by 0.16 px, so it moves up half of that: round(3.9 + 0.08) = 4.
        assert_eq!(m.cell_baseline, 4);
        assert!((m.face_y - 0.1).abs() < 1e-9, "{:?}", m.face_y);

        // Underline: top_to_baseline 13, the font puts the stroke 1.625 px below the baseline,
        // round(13 + 1.625) = 15, thickness ceil(1.3) = 2.
        assert_eq!((m.underline_position, m.underline_thickness), (15, 2));
        // No strikethrough metrics: centred on lowercase text, at (7.15 + 1.3) / 2 = 4.225
        // above the baseline — the *unrounded* thickness, as ghostty does it — so the stroke
        // starts round(13 - 4.225) = 9 below the top, and is as thick as the underline.
        assert_eq!((m.strikethrough_position, m.strikethrough_thickness), (9, 2));
        // The overline sits on the top edge; box drawing and the overline take the underline's
        // thickness, and a cursor is one pixel unless the cell is thinner.
        assert_eq!((m.overline_position, m.overline_thickness), (0, 2));
        assert_eq!((m.box_thickness, m.cursor_thickness, m.cursor_height), (2, 1, 17));
    }

    #[test]
    fn the_same_face_at_a_bigger_size_and_on_a_retina_display() {
        // 15 pt: advance 9.0 → 9, line height 19.8 → 20; the cell was rounded *up* this time,
        // so the baseline moves down by half the difference: round(4.5 - 0.1) = 4.
        let m = calc(&round_numbers(15.0));
        assert_eq!((m.cell_width, m.cell_height, m.cell_baseline), (9, 20, 4));
        // top_to_baseline 16: round(16 + 1.875) = 18, thickness ceil(1.5) = 2.
        assert_eq!((m.underline_position, m.underline_thickness), (18, 2));

        // DPR 2 at 13 pt: 15.6 → 16 and 34.32 → 34 device pixels, i.e. 8.0 × 17.0 points, so
        // the cell is the same size as at DPR 1 but the baseline lands half a point higher.
        let m = calc(&round_numbers(13.0).scaled(2.0));
        assert_eq!((m.cell_width, m.cell_height), (16, 34));
        // face_baseline 7.8, cell rounded down by 0.32: round(7.8 + 0.16) = 8.
        assert_eq!(m.cell_baseline, 8);
        assert_eq!((m.underline_position, m.underline_thickness), (29, 3));

        let grid = Grid::new(&m, 2.0);
        assert_eq!((grid.cell_width, grid.line_height), (px(8.0), px(17.0)));
        assert_eq!(grid.baseline, px(13.0));
        assert_eq!(grid.underline, Line { y: px(14.5), thickness: px(1.5) });
        assert_eq!(grid.cursor_thickness, px(0.5), "one device pixel");
    }

    #[test]
    fn a_font_that_says_nothing_is_estimated() {
        // Only the four required metrics: everything else comes from ghostty's estimates.
        let face = Face { cell_width: 10.0, ascent: 16.0, descent: -4.0, ..Face::default() };
        // cap 12, ex 9, underline thickness 0.15 * 9 = 1.35 → ceil 2, position -1.35.
        let m = calc(&face);
        assert_eq!((m.cell_width, m.cell_height, m.cell_baseline), (10, 20, 4));
        assert_eq!((m.underline_position, m.underline_thickness), (17, 2));
        // Strikethrough centred on lowercase: (9 + 1.35) / 2 = 5.175 above the baseline.
        assert_eq!(m.strikethrough_position, 11);

        // A face so small every rounding lands on zero still yields a drawable cell.
        let tiny = Face { cell_width: 0.2, ascent: 0.2, descent: -0.1, ..Face::default() };
        let m = calc(&tiny);
        assert_eq!((m.cell_width, m.cell_height), (1, 1));
        assert!(m.underline_thickness >= 1 && m.cursor_thickness >= 1);
    }

    #[test]
    fn a_taller_cell_keeps_the_text_centred() {
        // ghostty's own case (`Metrics: adjust cell height larger`), with our own numbers: a
        // face 0.33 px higher than centred, grown by an odd number of pixels, puts the extra
        // pixel on the top.
        let mut m = Metrics {
            cell_height: 100,
            cell_baseline: 50,
            underline_position: 55,
            strikethrough_position: 30,
            overline_position: 0,
            cursor_height: 100,
            face_height: 99.67,
            face_y: 0.33,
            ..Metrics::default()
        };
        m.set_cell_height(175);
        assert_eq!((m.cell_height, m.cell_baseline), (175, 87));
        assert_eq!((m.underline_position, m.strikethrough_position), (93, 68));
        assert_eq!(m.overline_position, 38);
        assert!((m.face_y - 37.33).abs() < 1e-9, "{:?}", m.face_y);

        // And smaller, where the extra pixel comes off the bottom.
        let mut m = Metrics {
            cell_height: 100,
            cell_baseline: 50,
            underline_position: 55,
            strikethrough_position: 30,
            cursor_height: 100,
            face_height: 99.67,
            face_y: 0.33,
            ..Metrics::default()
        };
        m.set_cell_height(75);
        assert_eq!((m.cell_height, m.cell_baseline), (75, 37));
        assert_eq!((m.underline_position, m.strikethrough_position), (43, 18));
        assert_eq!(m.overline_position, -12);
        assert!((m.face_y + 12.67).abs() < 1e-9, "{:?}", m.face_y);
    }

    /// Two real faces at two sizes on two displays, against the numbers ghostty's `calc`
    /// produces for the same input. Device pixels throughout: width, height, baseline,
    /// underline y and thickness, strikethrough y and thickness.
    #[test]
    fn two_fonts_two_sizes_two_displays_match_ghostty() {
        type Row = (fn(f64) -> Face, f64, f64, [u32; 7]);
        let table: [Row; 8] = [
            (jetbrains_mono, 13.0, 1.0, [8, 17, 4, 14, 2, 9, 2]),
            (jetbrains_mono, 13.0, 2.0, [16, 34, 7, 29, 3, 19, 3]),
            (jetbrains_mono, 15.0, 1.0, [9, 20, 4, 17, 2, 12, 2]),
            (jetbrains_mono, 15.0, 2.0, [18, 39, 8, 33, 3, 22, 3]),
            (menlo, 13.0, 1.0, [8, 15, 3, 13, 1, 8, 1]),
            (menlo, 13.0, 2.0, [16, 30, 6, 26, 2, 16, 2]),
            (menlo, 15.0, 1.0, [9, 17, 4, 14, 1, 9, 1]),
            (menlo, 15.0, 2.0, [18, 35, 7, 30, 2, 19, 2]),
        ];

        for (face, size, scale, want) in table {
            let m = calc(&face(size).scaled(scale));
            let got = [
                m.cell_width,
                m.cell_height,
                m.cell_baseline,
                m.underline_position,
                m.underline_thickness,
                m.strikethrough_position,
                m.strikethrough_thickness,
            ];
            assert_eq!(got, want, "{size} pt at DPR {scale}");
        }
    }

    /// The row height the element lays out with, in points, for the same faces and sizes: what
    /// ghostty would give a window on the same display.
    #[test]
    fn the_line_height_is_ghosttys_for_the_same_font_and_size() {
        type Row = (fn(f64) -> Face, f64, f64, Pixels);
        let table: [Row; 8] = [
            (jetbrains_mono, 13.0, 1.0, px(17.0)),
            (jetbrains_mono, 13.0, 2.0, px(17.0)),
            (jetbrains_mono, 15.0, 1.0, px(20.0)),
            (jetbrains_mono, 15.0, 2.0, px(19.5)),
            (menlo, 13.0, 1.0, px(15.0)),
            (menlo, 13.0, 2.0, px(15.0)),
            (menlo, 15.0, 1.0, px(17.0)),
            (menlo, 15.0, 2.0, px(17.5)),
        ];

        for (face, size, scale, want) in table {
            #[expect(clippy::cast_possible_truncation, reason = "1.0 and 2.0 are exact in f32")]
            let grid = Grid::new(&calc(&face(size).scaled(scale)), scale as f32);
            assert_eq!(grid.line_height, want, "{size} pt at DPR {scale}");
            assert!(grid.baseline > px(0.0) && grid.baseline < grid.line_height);
        }
    }
}

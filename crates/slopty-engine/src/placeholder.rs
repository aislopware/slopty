//! Kitty graphics Unicode placeholders: images shown through U+10EEEE cells.
//!
//! A virtual placement (`U=1`) is shown wherever the program writes U+10EEEE cells naming the
//! image (foreground colour), the placement (underline colour) and the tile (two combining
//! diacritics: row, column; a third carries the image id's high byte).
//!
//! libghostty stores the virtual placement but places nothing; the host walks the rows,
//! joins consecutive placeholder cells into runs (ghostty's rules) and turns each run into an
//! ordinary [`Placement`](slopty_proto::terminal::Placement) with ghostty's geometry, so the
//! client paints it like any other. The placeholder cells themselves go out blank.

use slopty_proto::terminal::PixelRect;

/// U+10EEEE, the placeholder character.
pub const PLACEHOLDER: u32 = 0x0010_eeee;

/// The 297 combining characters that number rows and columns, in code point order (the
/// kitty protocol's table; the index is the value).
const DIACRITICS: [u32; 297] = [
    0x0305, 0x030d, 0x030e, 0x0310, 0x0312, 0x033d, 0x033e, 0x033f, 0x0346, 0x034a, 0x034b, 0x034c,
    0x0350, 0x0351, 0x0352, 0x0357, 0x035b, 0x0363, 0x0364, 0x0365, 0x0366, 0x0367, 0x0368, 0x0369,
    0x036a, 0x036b, 0x036c, 0x036d, 0x036e, 0x036f, 0x0483, 0x0484, 0x0485, 0x0486, 0x0487, 0x0592,
    0x0593, 0x0594, 0x0595, 0x0597, 0x0598, 0x0599, 0x059c, 0x059d, 0x059e, 0x059f, 0x05a0, 0x05a1,
    0x05a8, 0x05a9, 0x05ab, 0x05ac, 0x05af, 0x05c4, 0x0610, 0x0611, 0x0612, 0x0613, 0x0614, 0x0615,
    0x0616, 0x0617, 0x0657, 0x0658, 0x0659, 0x065a, 0x065b, 0x065d, 0x065e, 0x06d6, 0x06d7, 0x06d8,
    0x06d9, 0x06da, 0x06db, 0x06dc, 0x06df, 0x06e0, 0x06e1, 0x06e2, 0x06e4, 0x06e7, 0x06e8, 0x06eb,
    0x06ec, 0x0730, 0x0732, 0x0733, 0x0735, 0x0736, 0x073a, 0x073d, 0x073f, 0x0740, 0x0741, 0x0743,
    0x0745, 0x0747, 0x0749, 0x074a, 0x07eb, 0x07ec, 0x07ed, 0x07ee, 0x07ef, 0x07f0, 0x07f1, 0x07f3,
    0x0816, 0x0817, 0x0818, 0x0819, 0x081b, 0x081c, 0x081d, 0x081e, 0x081f, 0x0820, 0x0821, 0x0822,
    0x0823, 0x0825, 0x0826, 0x0827, 0x0829, 0x082a, 0x082b, 0x082c, 0x082d, 0x0951, 0x0953, 0x0954,
    0x0f82, 0x0f83, 0x0f86, 0x0f87, 0x135d, 0x135e, 0x135f, 0x17dd, 0x193a, 0x1a17, 0x1a75, 0x1a76,
    0x1a77, 0x1a78, 0x1a79, 0x1a7a, 0x1a7b, 0x1a7c, 0x1b6b, 0x1b6d, 0x1b6e, 0x1b6f, 0x1b70, 0x1b71,
    0x1b72, 0x1b73, 0x1cd0, 0x1cd1, 0x1cd2, 0x1cda, 0x1cdb, 0x1ce0, 0x1dc0, 0x1dc1, 0x1dc3, 0x1dc4,
    0x1dc5, 0x1dc6, 0x1dc7, 0x1dc8, 0x1dc9, 0x1dcb, 0x1dcc, 0x1dd1, 0x1dd2, 0x1dd3, 0x1dd4, 0x1dd5,
    0x1dd6, 0x1dd7, 0x1dd8, 0x1dd9, 0x1dda, 0x1ddb, 0x1ddc, 0x1ddd, 0x1dde, 0x1ddf, 0x1de0, 0x1de1,
    0x1de2, 0x1de3, 0x1de4, 0x1de5, 0x1de6, 0x1dfe, 0x20d0, 0x20d1, 0x20d4, 0x20d5, 0x20d6, 0x20d7,
    0x20db, 0x20dc, 0x20e1, 0x20e7, 0x20e9, 0x20f0, 0x2cef, 0x2cf0, 0x2cf1, 0x2de0, 0x2de1, 0x2de2,
    0x2de3, 0x2de4, 0x2de5, 0x2de6, 0x2de7, 0x2de8, 0x2de9, 0x2dea, 0x2deb, 0x2dec, 0x2ded, 0x2dee,
    0x2def, 0x2df0, 0x2df1, 0x2df2, 0x2df3, 0x2df4, 0x2df5, 0x2df6, 0x2df7, 0x2df8, 0x2df9, 0x2dfa,
    0x2dfb, 0x2dfc, 0x2dfd, 0x2dfe, 0x2dff, 0xa66f, 0xa67c, 0xa67d, 0xa6f0, 0xa6f1, 0xa8e0, 0xa8e1,
    0xa8e2, 0xa8e3, 0xa8e4, 0xa8e5, 0xa8e6, 0xa8e7, 0xa8e8, 0xa8e9, 0xa8ea, 0xa8eb, 0xa8ec, 0xa8ed,
    0xa8ee, 0xa8ef, 0xa8f0, 0xa8f1, 0xaab0, 0xaab2, 0xaab3, 0xaab7, 0xaab8, 0xaabe, 0xaabf, 0xaac1,
    0xfe20, 0xfe21, 0xfe22, 0xfe23, 0xfe24, 0xfe25, 0xfe26, 0x10a0f, 0x10a38, 0x1d185, 0x1d186,
    0x1d187, 0x1d188, 0x1d189, 0x1d1aa, 0x1d1ab, 0x1d1ac, 0x1d1ad, 0x1d242, 0x1d243, 0x1d244,
];

/// The number a diacritic stands for, if it is one of the table's.
#[must_use]
pub fn diacritic_index(cp: u32) -> Option<u32> {
    DIACRITICS.binary_search(&cp).ok().and_then(|i| u32::try_from(i).ok())
}

/// The id a colour encodes: a palette index as is, RGB as `r << 16 | g << 8 | b`, none as 0.
#[must_use]
pub const fn color_id(palette: Option<u8>, rgb: Option<(u8, u8, u8)>) -> u32 {
    match (palette, rgb) {
        (Some(i), _) => i as u32,
        (None, Some((r, g, b))) => ((r as u32) << 16) | ((g as u32) << 8) | (b as u32),
        (None, None) => 0,
    }
}

/// One decoded placeholder cell.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cell {
    /// The image id's low 24 bits, from the foreground colour.
    pub image_low: u32,
    /// The image id's high byte, from the third diacritic.
    pub image_high: Option<u8>,
    /// The placement id from the underline colour; 0 means "any virtual placement".
    pub placement: u32,
    /// The tile row from the first diacritic.
    pub row: Option<u32>,
    /// The tile column from the second diacritic.
    pub col: Option<u32>,
}

impl Cell {
    /// Decode a cell: `fg` and `underline` as ids (see [`color_id`]), `extra` the combining
    /// code points after U+10EEEE. An invalid diacritic reads as absent, as ghostty does.
    #[must_use]
    pub fn decode(fg: u32, underline: u32, extra: impl IntoIterator<Item = u32>) -> Self {
        let mut extra = extra.into_iter().map(diacritic_index);
        let (row, col, high) =
            (extra.next().flatten(), extra.next().flatten(), extra.next().flatten());
        Self {
            image_low: fg & 0x00ff_ffff,
            image_high: high.and_then(|v| u8::try_from(v).ok()),
            placement: underline,
            row,
            col,
        }
    }
}

/// Consecutive placeholder cells on one row that show one strip of one image.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Run {
    /// Viewport column of the first cell.
    pub x: u16,
    /// Viewport row.
    pub y: u16,
    /// The image id.
    pub image: u32,
    /// The placement id, 0 for "any virtual placement of the image".
    pub placement: u32,
    /// The tile row.
    pub row: u32,
    /// The tile column of the first cell.
    pub col: u32,
    /// Cells in the run.
    pub width: u32,
    /// The image id's low 24 bits, as the cells name it.
    pub image_low: u32,
    /// The image id's high byte, when a cell named it.
    pub image_high: Option<u8>,
}

impl Run {
    /// Whether `cell` continues this run (ghostty's rules: same image and placement, the
    /// same row or none, the next column or none).
    fn accepts(&self, cell: &Cell) -> bool {
        self.image_low == cell.image_low
            && self.placement == cell.placement
            && cell.row.is_none_or(|r| r == self.row)
            && cell.col.is_none_or(|c| Some(c) == self.col.checked_add(self.width))
            && cell.image_high.is_none_or(|h| Some(h) == self.image_high)
    }
}

/// The runs of a viewport, built cell by cell.
#[derive(Default, Debug)]
pub struct Runs {
    /// Finished runs.
    pub runs: Vec<Run>,
    /// The run the last cell belongs to.
    pub current: Option<Run>,
}

impl Runs {
    /// A placeholder cell at viewport `(x, y)`.
    pub fn cell(&mut self, x: u16, y: u16, cell: Cell) {
        if let Some(run) = &mut self.current
            && run.y == y
            && run.accepts(&cell)
        {
            run.width = run.width.saturating_add(1);
            return;
        }
        self.finish();
        let (row, col) = (cell.row.unwrap_or(0), cell.col.unwrap_or(0));
        let high = u32::from(cell.image_high.unwrap_or(0));
        self.current = Some(Run {
            x,
            y,
            image: cell.image_low | (high << 24),
            placement: cell.placement,
            row,
            col,
            width: 1,
            image_low: cell.image_low,
            image_high: cell.image_high,
        });
    }

    /// A cell that is not a placeholder: the current run, if any, ends.
    pub fn finish(&mut self) {
        if let Some(run) = self.current.take() {
            self.runs.push(run);
        }
    }

    /// Every run, the last one closed.
    #[must_use]
    pub fn into_runs(mut self) -> Vec<Run> {
        self.finish();
        self.runs
    }
}

/// The cells a virtual placement spans; 0 means "as many as the image needs".
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Grid {
    /// Columns.
    pub cols: u32,
    /// Rows.
    pub rows: u32,
}

impl Grid {
    /// The grid with unspecified sides sized to the image at this cell size.
    #[must_use]
    pub fn resolved(self, image: (u32, u32), cell: (u32, u32)) -> Self {
        let fit = |px: u32, cell: u32| px.div_ceil(cell.max(1)).max(1);
        Self {
            cols: if self.cols == 0 { fit(image.0, cell.0) } else { self.cols },
            rows: if self.rows == 0 { fit(image.1, cell.1) } else { self.rows },
        }
    }
}

/// Where a run's strip of the image lands: pixels within the run's cells and the source
/// rectangle in image pixels.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rendered {
    /// Pixels from the run's left edge.
    pub x_offset: u32,
    /// Pixels from the run's top edge.
    pub y_offset: u32,
    /// Painted width in pixels.
    pub width: u32,
    /// Painted height in pixels.
    pub height: u32,
    /// The image pixels shown.
    pub source: PixelRect,
}

/// Ghostty's geometry: the image is scaled to fit the placement grid keeping its aspect,
/// centred, and the run shows its own cells' strip of that. `None` when the strip falls in
/// the letterbox.
#[must_use]
pub fn render(run: &Run, image: (u32, u32), grid: Grid, cell: (u32, u32)) -> Option<Rendered> {
    let (img_w, img_h) = (f64::from(image.0), f64::from(image.1));
    let (cell_w, cell_h) = (f64::from(cell.0), f64::from(cell.1));
    let (grid_cols, grid_rows) = (f64::from(grid.cols.max(1)), f64::from(grid.rows.max(1)));
    let (cols_px, rows_px) = (grid_cols * cell_w, grid_rows * cell_h);

    // Scale to fit, centred.
    let (x_scale, y_scale, x_off, y_off) = if img_w * rows_px > img_h * cols_px {
        let scale = cols_px / img_w.max(1.0);
        (scale, scale, 0.0, img_h.mul_add(-scale, rows_px) / 2.0)
    } else {
        let scale = rows_px / img_h.max(1.0);
        (scale, scale, img_w.mul_add(-scale, cols_px) / 2.0, 0.0)
    };
    // The letterboxed canvas in image pixels.
    let (sx_off, sy_off) = (x_off / x_scale, y_off / y_scale);
    let (canvas_w, canvas_h) = (sx_off.mul_add(2.0, img_w), sy_off.mul_add(2.0, img_h));

    // The run's strip of the canvas.
    let (width, col, row) = (f64::from(run.width), f64::from(run.col), f64::from(run.row));
    let mut src_w = canvas_w * (width / grid_cols);
    let mut src_h = canvas_h / grid_rows;
    let mut src_x = canvas_w * (col / grid_cols);
    let mut src_y = canvas_h * (row / grid_rows);

    let (mut dx_off, mut dy_off) = (0.0, 0.0);
    let mut dest_w = width * cell_w;
    let mut dest_h = cell_h;

    if src_y < sy_off {
        let off = sy_off - src_y;
        src_h -= off;
        dy_off = off;
        dest_h = off.mul_add(-y_scale, dest_h);
        src_y = 0.0;
        if src_h > img_h {
            src_h = img_h;
            dest_h = img_h * y_scale;
        }
    } else if src_y + src_h > canvas_h - sy_off {
        src_y -= sy_off;
        src_h = canvas_h - sy_off - src_y;
        src_h -= sy_off;
        dest_h = src_h * y_scale;
    } else {
        src_y -= sy_off;
    }

    if src_x < sx_off {
        let off = sx_off - src_x;
        src_w -= off;
        dx_off = off;
        dest_w = off.mul_add(-x_scale, dest_w);
        src_x = 0.0;
        if src_w > img_w {
            src_w = img_w;
            dest_w = img_w * x_scale;
        }
    } else if src_x + src_w > canvas_w - sx_off {
        src_x -= sx_off;
        src_w = canvas_w - sx_off - src_x;
        src_w -= sx_off;
        dest_w = src_w * x_scale;
    } else {
        src_x -= sx_off;
    }

    if src_w <= 0.0 || src_h <= 0.0 {
        return None;
    }
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "rounded ≥ 0")]
    let px = |v: f64| v.round().max(0.0) as u32;
    Some(Rendered {
        x_offset: px(dx_off * x_scale),
        y_offset: px(dy_off * y_scale),
        width: px(dest_w),
        height: px(dest_h),
        source: PixelRect { x: px(src_x), y: px(src_y), width: px(src_w), height: px(src_h) },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diacritics_number_from_zero_and_colours_encode_ids() {
        assert_eq!(diacritic_index(0x0305), Some(0));
        assert_eq!(diacritic_index(0x030d), Some(1));
        assert_eq!(diacritic_index(0x1d244), Some(296));
        assert_eq!(diacritic_index(0x0301), None);
        assert_eq!(color_id(Some(7), None), 7);
        assert_eq!(color_id(None, Some((1, 2, 3))), 0x0001_0203);
        assert_eq!(color_id(None, None), 0);
        let cell = Cell::decode(0x0001_0203, 4, [0x030d, 0x030e, 0x0310]);
        assert_eq!(
            cell,
            Cell {
                image_low: 0x0001_0203,
                image_high: Some(3),
                placement: 4,
                row: Some(1),
                col: Some(2)
            }
        );
        assert_eq!(
            Cell::decode(9, 0, [0x0301]),
            Cell { image_low: 9, image_high: None, placement: 0, row: None, col: None }
        );
    }

    /// Cells continue a run when they name the next column (or nothing); a new row, another
    /// image or a gap starts a new one; the high byte joins the id.
    #[test]
    fn placeholder_cells_join_into_runs_by_ghosttys_rules() {
        let mut runs = Runs::default();
        let at = |row, col| Cell { image_low: 5, image_high: None, placement: 0, row, col };
        runs.cell(2, 0, at(Some(0), Some(0)));
        runs.cell(3, 0, at(None, None));
        runs.cell(4, 0, at(Some(0), Some(2)));
        runs.cell(5, 0, at(Some(0), Some(7)));
        runs.finish();
        runs.cell(0, 1, at(Some(1), None));
        runs.cell(
            1,
            1,
            Cell { image_low: 5, image_high: Some(1), placement: 0, row: Some(1), col: None },
        );
        let runs = runs.into_runs();
        assert_eq!(runs.len(), 4, "{runs:?}");
        assert_eq!(
            (runs[0].x, runs[0].y, runs[0].row, runs[0].col, runs[0].width),
            (2, 0, 0, 0, 3)
        );
        assert_eq!((runs[1].x, runs[1].col, runs[1].width), (5, 7, 1));
        assert_eq!((runs[2].y, runs[2].row, runs[2].col, runs[2].image), (1, 1, 0, 5));
        assert_eq!(runs[3].image, 5 | (1 << 24));
    }

    /// A 4×2 image in a 2×1 grid of 8×16 cells is letterboxed: scaled ×4 to 16×8, centred 4
    /// px down; a run over the whole strip shows all of it.
    #[test]
    fn a_run_shows_its_strip_of_the_image_scaled_to_fit_the_grid() {
        let run = |col, width| Run {
            x: 0,
            y: 0,
            image: 1,
            placement: 0,
            row: 0,
            col,
            width,
            image_low: 1,
            image_high: None,
        };
        let grid = Grid { cols: 2, rows: 1 };
        assert_eq!(
            render(&run(0, 2), (4, 2), grid, (8, 16)),
            Some(Rendered {
                x_offset: 0,
                y_offset: 4,
                width: 16,
                height: 8,
                source: PixelRect { x: 0, y: 0, width: 4, height: 2 },
            })
        );
        // The second cell alone: the right half of the image.
        assert_eq!(
            render(&run(1, 1), (4, 2), grid, (8, 16)).map(|r| r.source),
            Some(PixelRect { x: 2, y: 0, width: 2, height: 2 })
        );
        // An unsized grid takes the cells the image needs.
        assert_eq!(
            Grid { cols: 0, rows: 0 }.resolved((20, 17), (8, 16)),
            Grid { cols: 3, rows: 2 }
        );
        // A tall image in a wide grid: pillarboxed, a run on the far column is empty.
        let wide = Grid { cols: 8, rows: 1 };
        assert_eq!(render(&run(7, 1), (2, 16), wide, (8, 16)), None);
    }
}

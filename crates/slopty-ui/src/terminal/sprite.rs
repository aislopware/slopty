//! Cell-drawn glyphs: box drawing, block elements, Braille and Powerline shapes.
//!
//! A font fits each of these glyphs to its own em box, so two adjacent cells cannot agree on
//! where the ink stops: a border seams at every row when the line height is not the font's,
//! and a heavy line changes weight across a fallback boundary. ghostty draws them itself
//! (`src/font/sprite/draw/`), from the cell's size and the underline thickness, and so does
//! this: [`shapes`] answers the geometry of one cell in points, snapped to device pixels,
//! and the element paints it in the cell's colours. Nothing here shapes text.

/// The paint of one shape.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Ink {
    /// The cell's text colour.
    Fg,
    /// The text colour at this opacity: the shades `░▒▓`.
    Shade(f32),
}

/// One primitive of a cell's sprite, in points from the cell's top-left corner.
#[derive(Clone, PartialEq, Debug)]
pub enum Shape {
    /// An axis-aligned box; `round` makes it a disc (a Braille dot).
    Rect { x: f32, y: f32, w: f32, h: f32, ink: Ink, round: bool },
    /// A polyline stroked at `thickness`: a diagonal.
    Stroke { points: Vec<(f32, f32)>, thickness: f32 },
    /// A quarter circle of radius `r` from `from` to `to`, stroked at `thickness`, bulging
    /// away from the cell's centre; `sweep` is the SVG flag (clockwise on screen).
    Arc { from: (f32, f32), to: (f32, f32), r: f32, sweep: bool, thickness: f32 },
    /// A filled polygon: a Powerline triangle.
    Poly(Vec<(f32, f32)>),
}

/// Whether the cell's text is drawn here rather than by the font.
#[must_use]
pub fn is_sprite(text: &str) -> bool {
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => is_sprite_char(c),
        _ => false,
    }
}

const fn is_sprite_char(c: char) -> bool {
    matches!(c, '\u{2500}'..='\u{259F}' | '\u{2800}'..='\u{28FF}' | '\u{E0B0}'..='\u{E0B3}')
}

/// A line's weight on one side of the cell.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Arm {
    None,
    Light,
    Heavy,
    Double,
}

/// The four arms of a box-drawing junction: up, down, left, right.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Arms {
    up: Arm,
    down: Arm,
    left: Arm,
    right: Arm,
}

/// Which lines U+2500–U+254B and U+2550–U+256C draw, in code point order: one letter per
/// arm (up, down, left, right); `n` none, `l` light, `h` heavy, `d` double.
const SOLID: [&str; 76] = [
    "nnll", "nnhh", "llnn", "hhnn", // ─ ━ │ ┃
    "nnll", "nnhh", "llnn", "hhnn", "nnll", "nnhh", "llnn", "hhnn", // dashes, weights only
    "nlnl", "nlnh", "nhnl", "nhnh", // ┌ ┍ ┎ ┏
    "nlln", "nlhn", "nhln", "nhhn", // ┐ ┑ ┒ ┓
    "lnnl", "lnnh", "hnnl", "hnnh", // └ ┕ ┖ ┗
    "lnln", "lnhn", "hnln", "hnhn", // ┘ ┙ ┚ ┛
    "llnl", "llnh", "hlnl", "lhnl", "hhnl", "hlnh", "lhnh",
    "hhnh", // ├ ┝ ┞ ┟ ┠ ┡ ┢ ┣
    "llln", "llhn", "hlln", "lhln", "hhln", "hlhn", "lhhn",
    "hhhn", // ┤ ┥ ┦ ┧ ┨ ┩ ┪ ┫
    "nlll", "nlhl", "nllh", "nlhh", "nhll", "nhhl", "nhlh",
    "nhhh", // ┬ ┭ ┮ ┯ ┰ ┱ ┲ ┳
    "lnll", "lnhl", "lnlh", "lnhh", "hnll", "hnhl", "hnlh",
    "hnhh", // ┴ ┵ ┶ ┷ ┸ ┹ ┺ ┻
    "llll", "llhl", "lllh", "llhh", "hlll", "lhll", "hhll",
    "hlhl", // ┼ ┽ ┾ ┿ ╀ ╁ ╂ ╃
    "hllh", "lhhl", "lhlh", "hlhh", "lhhh", "hhhl", "hhlh",
    "hhhh", // ╄ ╅ ╆ ╇ ╈ ╉ ╊ ╋
];

/// U+2550–U+256C, the double lines.
const DOUBLE: [&str; 29] = [
    "nndd", "ddnn", // ═ ║
    "nlnd", "ndnl", "ndnd", // ╒ ╓ ╔
    "nldn", "nddn", "nddn", // ╕ ╖ ╗
    "lnnd", "dnnl", "dnnd", // ╘ ╙ ╚
    "lndn", "dnln", "dndn", // ╛ ╜ ╝
    "llnd", "ddnl", "ddnd", // ╞ ╟ ╠
    "lldn", "ddln", "dddn", // ╡ ╢ ╣
    "nldd", "ndll", "nddd", // ╤ ╥ ╦
    "lndd", "dnll", "dndd", // ╧ ╨ ╩
    "lldd", "ddll", "dddd", // ╪ ╫ ╬
];

/// U+2574–U+257F, the half lines.
const HALF: [&str; 12] = [
    "nnln", "lnnn", "nnnl", "nlnn", // ╴ ╵ ╶ ╷
    "nnhn", "hnnn", "nnnh", "nhnn", // ╸ ╹ ╺ ╻
    "nnlh", "lhnn", "nnhl", "hlnn", // ╼ ╽ ╾ ╿
];

const fn arm(letter: u8) -> Arm {
    match letter {
        b'l' => Arm::Light,
        b'h' => Arm::Heavy,
        b'd' => Arm::Double,
        _ => Arm::None,
    }
}

fn arms(code: &str) -> Arms {
    let b = code.as_bytes();
    let at = |i: usize| b.get(i).copied().map_or(Arm::None, arm);
    Arms { up: at(0), down: at(1), left: at(2), right: at(3) }
}

/// The arms of a box-drawing character, if it is one made of straight lines.
fn box_arms(c: char) -> Option<Arms> {
    let n = u32::from(c);
    let code = match n {
        0x2500..=0x254b => SOLID.get(index(n, 0x2500)?)?,
        0x254c | 0x254d => "nnll",
        0x254e | 0x254f => "llnn",
        0x2550..=0x256c => DOUBLE.get(index(n, 0x2550)?)?,
        0x2574..=0x257f => HALF.get(index(n, 0x2574)?)?,
        _ => return None,
    };
    let mut arms = arms(code);
    if matches!(n, 0x254d | 0x254f) {
        // The heavy double dashes share a code with their light twins above.
        let heavy = |a: Arm| if a == Arm::Light { Arm::Heavy } else { a };
        arms = Arms {
            up: heavy(arms.up),
            down: heavy(arms.down),
            left: heavy(arms.left),
            right: heavy(arms.right),
        };
    }
    Some(arms)
}

/// `n`'s place in a table starting at `base`.
fn index(n: u32, base: u32) -> Option<usize> {
    usize::try_from(n.checked_sub(base)?).ok()
}

/// How many dashes a dashed line is drawn with, when it is one.
const fn dashes(c: char) -> Option<u32> {
    match c {
        '\u{2504}'..='\u{2507}' => Some(3),
        '\u{2508}'..='\u{250B}' => Some(4),
        '\u{254C}'..='\u{254F}' => Some(2),
        _ => None,
    }
}

/// The geometry a cell of `w`×`h` points needs, with the light line `thickness` and the
/// device `scale` positions snap to.
#[derive(Clone, Copy, Debug)]
pub struct Cell {
    /// Cell width, points.
    pub w: f32,
    /// Row height, points.
    pub h: f32,
    /// Thickness of a light line, points (the underline's, as ghostty).
    pub thickness: f32,
    /// Device pixels per point; edges snap to it so a one-pixel line is one pixel.
    pub scale: f32,
}

impl Cell {
    fn snap(&self, v: f32) -> f32 {
        let scale = if self.scale.is_finite() && self.scale > 0.0 { self.scale } else { 1.0 };
        (v * scale).round() / scale
    }

    fn t(&self) -> f32 {
        self.snap(self.thickness).max(self.snap(1.0 / self.scale.max(1.0)))
    }

    fn weight(&self, arm: Arm) -> f32 {
        match arm {
            Arm::Heavy => self.t() * 3.0,
            _ => self.t(),
        }
    }

    /// The centre lines, snapped so a light line lands on whole pixels.
    fn centre(&self) -> (f32, f32) {
        let t = self.t();
        (self.snap((self.w - t) / 2.0) + t / 2.0, self.snap((self.h - t) / 2.0) + t / 2.0)
    }

    fn rect(&self, x0: f32, y0: f32, x1: f32, y1: f32, ink: Ink) -> Shape {
        let (x0, y0, x1, y1) = (self.snap(x0), self.snap(y0), self.snap(x1), self.snap(y1));
        Shape::Rect {
            x: x0,
            y: y0,
            w: (x1 - x0).max(0.0),
            h: (y1 - y0).max(0.0),
            ink,
            round: false,
        }
    }
}

/// The shapes of `c` in a cell, or `None` when the font draws it.
#[must_use]
pub fn shapes(c: char, cell: Cell) -> Option<Vec<Shape>> {
    let n = u32::from(c);
    if let Some(arms) = box_arms(c) {
        return Some(lines(arms, dashes(c), cell));
    }
    Some(match n {
        0x256d..=0x2570 => arc(c, cell),
        0x2571..=0x2573 => diagonals(c, cell),
        0x2580..=0x259f => blocks(n, cell),
        0x2800..=0x28ff => braille(n, cell),
        0xe0b0..=0xe0b3 => powerline(n, cell),
        _ => return None,
    })
}

/// The side of the cell an arm leaves by.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Side {
    Up,
    Down,
    Left,
    Right,
}

impl Arms {
    const fn get(self, side: Side) -> Arm {
        match side {
            Side::Up => self.up,
            Side::Down => self.down,
            Side::Left => self.left,
            Side::Right => self.right,
        }
    }
}

const fn opposite(side: Side) -> Side {
    match side {
        Side::Up => Side::Down,
        Side::Down => Side::Up,
        Side::Left => Side::Right,
        Side::Right => Side::Left,
    }
}

/// The two sides perpendicular to `side`, the negative-coordinate one first.
const fn across(side: Side) -> (Side, Side) {
    match side {
        Side::Up | Side::Down => (Side::Left, Side::Right),
        Side::Left | Side::Right => (Side::Up, Side::Down),
    }
}

/// Straight box-drawing lines: each arm is one or two bars from the cell's edge towards the
/// centre; how far past (or short of) the centre it runs depends on what it meets there,
/// which is what makes a double corner an L and a stem hang from the near double line.
fn lines(arms: Arms, dashes: Option<u32>, cell: Cell) -> Vec<Shape> {
    let t = cell.t();
    let (cx, cy) = cell.centre();
    let mut out = Vec::new();
    for side in [Side::Up, Side::Down, Side::Left, Side::Right] {
        let arm = arms.get(side);
        if arm == Arm::None {
            continue;
        }
        let (neg, pos) = across(side);
        let (neg_arm, pos_arm) = (arms.get(neg), arms.get(pos));
        let opp = arms.get(opposite(side));
        // Offsets across the arm's axis and how far the bar starts from the centre
        // (negative: past it), each with its thickness.
        let bars: Vec<(f32, f32, f32)> = match arm {
            Arm::Double => [-t, t]
                .into_iter()
                .map(|o| {
                    let (near, far) = if o < 0.0 { (neg_arm, pos_arm) } else { (pos_arm, neg_arm) };
                    let start = if near == Arm::Double {
                        t / 2.0
                    } else if opp == Arm::Double {
                        0.0
                    } else if far == Arm::Double {
                        -1.5 * t
                    } else if near != Arm::None || far != Arm::None {
                        -cell.weight(near).max(cell.weight(far)) / 2.0
                    } else {
                        0.0
                    };
                    (o, start, t)
                })
                .collect(),
            weight => {
                let thick = cell.weight(weight);
                let doubles = match (neg_arm == Arm::Double, pos_arm == Arm::Double) {
                    (true, true) => 2,
                    (false, false) => 0,
                    _ => 1,
                };
                let start = if neg_arm == Arm::None && pos_arm == Arm::None {
                    // A straight or half line: the halves meet at the centre.
                    0.0
                } else if opp != Arm::None {
                    -perp_weight(cell, neg_arm, pos_arm) / 2.0
                } else if doubles == 2 {
                    t / 2.0
                } else if doubles == 1 {
                    -1.5 * t
                } else {
                    -perp_weight(cell, neg_arm, pos_arm) / 2.0
                };
                vec![(0.0, start, thick)]
            }
        };
        for (offset, start, thick) in bars {
            let (lo, hi) = (offset - thick / 2.0, offset + thick / 2.0);
            let bar = match side {
                Side::Up => (cx + lo, 0.0, cx + hi, cy - start),
                Side::Down => (cx + lo, cy + start, cx + hi, cell.h),
                Side::Left => (0.0, cy + lo, cx - start, cy + hi),
                Side::Right => (cx + start, cy + lo, cell.w, cy + hi),
            };
            match dashes {
                Some(n) => dash(&mut out, cell, side, bar, n),
                None => out.push(cell.rect(bar.0, bar.1, bar.2, bar.3, Ink::Fg)),
            }
        }
    }
    out
}

/// The widest of two perpendicular arms, a double counting as its three-bar band.
fn perp_weight(cell: Cell, a: Arm, b: Arm) -> f32 {
    let one = |arm: Arm| match arm {
        Arm::Double => cell.t() * 3.0,
        other => cell.weight(other),
    };
    one(a).max(one(b))
}

/// A dashed line is a plain one cut into `n` dashes across the whole cell, with a gap of one
/// line thickness between them (the two halves of a dashed line meet at the centre).
fn dash(out: &mut Vec<Shape>, cell: Cell, side: Side, bar: (f32, f32, f32, f32), n: u32) {
    let t = cell.t();
    #[expect(clippy::cast_precision_loss, reason = "at most four dashes")]
    let n_f = n as f32;
    let horizontal = matches!(side, Side::Left | Side::Right);
    let length = if horizontal { cell.w } else { cell.h };
    let dash_len = (n_f - 1.0).mul_add(-t, length) / n_f;
    let (lo, hi) = if horizontal { (bar.0, bar.2) } else { (bar.1, bar.3) };
    for i in 0..n {
        #[expect(clippy::cast_precision_loss, reason = "at most four dashes")]
        let i_f = i as f32;
        let a = i_f * (dash_len + t);
        let b = a + dash_len;
        // Only the part of this dash that lies within this half of the line.
        let (a, b) = (a.max(lo), b.min(hi));
        if b <= a {
            continue;
        }
        out.push(if horizontal {
            cell.rect(a, bar.1, b, bar.3, Ink::Fg)
        } else {
            cell.rect(bar.0, a, bar.2, b, Ink::Fg)
        });
    }
}

/// The rounded corners `╭ ╮ ╯ ╰`: a quarter circle where a corner would be, with the two
/// straight stubs to the edges.
fn arc(c: char, cell: Cell) -> Vec<Shape> {
    let t = cell.t();
    let (cx, cy) = cell.centre();
    let r = (cell.w.min(cell.h) / 2.0).min(cx.min(cy)).max(t);
    let (top, bottom, left, right) = ((cx, cy - r), (cx, cy + r), (cx - r, cy), (cx + r, cy));
    let (from, to, sweep, stubs) = match c {
        '\u{256D}' => (bottom, right, true, [(cx, cy + r, cx, cell.h), (cx + r, cy, cell.w, cy)]),
        '\u{256E}' => (bottom, left, false, [(cx, cy + r, cx, cell.h), (0.0, cy, cx - r, cy)]),
        '\u{256F}' => (top, left, true, [(cx, 0.0, cx, cy - r), (0.0, cy, cx - r, cy)]),
        _ => (top, right, false, [(cx, 0.0, cx, cy - r), (cx + r, cy, cell.w, cy)]),
    };
    let mut out = vec![Shape::Arc { from, to, r, sweep, thickness: t }];
    for (x0, y0, x1, y1) in stubs {
        if (x1 - x0).abs() < f32::EPSILON {
            out.push(cell.rect(x0 - t / 2.0, y0, x0 + t / 2.0, y1, Ink::Fg));
        } else {
            out.push(cell.rect(x0, y0 - t / 2.0, x1, y0 + t / 2.0, Ink::Fg));
        }
    }
    out
}

/// `╱ ╲ ╳`: corner to corner.
fn diagonals(c: char, cell: Cell) -> Vec<Shape> {
    let t = cell.t();
    let rising = vec![(0.0, cell.h), (cell.w, 0.0)];
    let falling = vec![(0.0, 0.0), (cell.w, cell.h)];
    let stroke = |points: Vec<(f32, f32)>| Shape::Stroke { points, thickness: t };
    match c {
        '\u{2571}' => vec![stroke(rising)],
        '\u{2572}' => vec![stroke(falling)],
        _ => vec![stroke(rising), stroke(falling)],
    }
}

/// Block elements U+2580–U+259F: halves, eighths, shades and quadrants.
fn blocks(n: u32, cell: Cell) -> Vec<Shape> {
    let (w, h) = (cell.w, cell.h);
    let eighth = |k: u32| {
        #[expect(clippy::cast_precision_loss, reason = "eighths")]
        let k = k as f32;
        k / 8.0
    };
    let full = |ink: Ink| cell.rect(0.0, 0.0, w, h, ink);
    let quadrants = |mask: u8| {
        // Bits: 1 upper-left, 2 upper-right, 4 lower-left, 8 lower-right.
        let (mx, my) = (cell.snap(w / 2.0), cell.snap(h / 2.0));
        let mut out = Vec::new();
        for (bit, (x0, y0, x1, y1)) in [
            (1, (0.0, 0.0, mx, my)),
            (2, (mx, 0.0, w, my)),
            (4, (0.0, my, mx, h)),
            (8, (mx, my, w, h)),
        ] {
            if mask & bit != 0 {
                out.push(cell.rect(x0, y0, x1, y1, Ink::Fg));
            }
        }
        out
    };
    match n {
        0x2580 => vec![cell.rect(0.0, 0.0, w, h / 2.0, Ink::Fg)],
        0x2581..=0x2588 => {
            vec![cell.rect(0.0, h * (1.0 - eighth(n.saturating_sub(0x2580))), w, h, Ink::Fg)]
        }
        0x2589..=0x258f => {
            vec![cell.rect(0.0, 0.0, w * eighth(0x2590_u32.saturating_sub(n)), h, Ink::Fg)]
        }
        0x2590 => vec![cell.rect(w / 2.0, 0.0, w, h, Ink::Fg)],
        0x2591 => vec![full(Ink::Shade(0.25))],
        0x2592 => vec![full(Ink::Shade(0.5))],
        0x2593 => vec![full(Ink::Shade(0.75))],
        0x2594 => vec![cell.rect(0.0, 0.0, w, h / 8.0, Ink::Fg)],
        0x2595 => vec![cell.rect(w * 7.0 / 8.0, 0.0, w, h, Ink::Fg)],
        0x2596 => quadrants(4),
        0x2597 => quadrants(8),
        0x2598 => quadrants(1),
        0x2599 => quadrants(1 | 4 | 8),
        0x259a => quadrants(1 | 8),
        0x259b => quadrants(1 | 2 | 4),
        0x259c => quadrants(1 | 2 | 8),
        0x259d => quadrants(2),
        0x259e => quadrants(2 | 4),
        _ => quadrants(2 | 4 | 8),
    }
}

/// Braille dot bits in code point order with each dot's (column, row): 1 2 4 are column 1
/// rows 1–3, 8 16 32 column 2 rows 1–3, 64 column 1 row 4, 128 column 2 row 4.
const DOTS: [(u32, u32, u32); 8] =
    [(1, 0, 0), (2, 0, 1), (4, 0, 2), (8, 1, 0), (16, 1, 1), (32, 1, 2), (64, 0, 3), (128, 1, 3)];

/// Braille U+2800–U+28FF: the eight dots of the pattern, two columns by four rows.
fn braille(n: u32, cell: Cell) -> Vec<Shape> {
    let bits = n.saturating_sub(0x2800);
    let (width, height) = (cell.w, cell.h);
    let dot = cell.snap((width / 4.0).min(height / 8.0)).max(cell.t());
    let mut out = Vec::new();
    for (mask, col, row) in DOTS {
        if bits & mask == 0 {
            continue;
        }
        #[expect(clippy::cast_precision_loss, reason = "two columns, four rows")]
        let (col, row) = (col as f32, row as f32);
        // Dot centres at the quarter points of the width and the eighth points of the height.
        let x = cell.snap(col.mul_add(2.0, 1.0).mul_add(width / 4.0, -dot / 2.0));
        let y = cell.snap(row.mul_add(2.0, 1.0).mul_add(height / 8.0, -dot / 2.0));
        out.push(Shape::Rect { x, y, w: dot, h: dot, ink: Ink::Fg, round: true });
    }
    out
}

/// Powerline U+E0B0–U+E0B3: the solid and thin separators.
fn powerline(n: u32, cell: Cell) -> Vec<Shape> {
    let (w, h) = (cell.w, cell.h);
    let t = cell.t();
    match n {
        0xe0b0 => vec![Shape::Poly(vec![(0.0, 0.0), (w, h / 2.0), (0.0, h)])],
        0xe0b1 => {
            vec![Shape::Stroke { points: vec![(0.0, 0.0), (w, h / 2.0), (0.0, h)], thickness: t }]
        }
        0xe0b2 => vec![Shape::Poly(vec![(w, 0.0), (0.0, h / 2.0), (w, h)])],
        _ => vec![Shape::Stroke { points: vec![(w, 0.0), (0.0, h / 2.0), (w, h)], thickness: t }],
    }
}

#[cfg(test)]
#[expect(clippy::float_cmp, reason = "snapped values are exact")]
mod tests {
    use super::*;

    const CELL: Cell = Cell { w: 8.0, h: 16.0, thickness: 1.0, scale: 1.0 };

    fn rects(c: char) -> Vec<(f32, f32, f32, f32)> {
        shapes(c, CELL)
            .expect("a sprite")
            .into_iter()
            .filter_map(|s| match s {
                Shape::Rect { x, y, w, h, ink: Ink::Fg, round: false } => Some((x, y, w, h)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_ranges_are_box_drawing_blocks_braille_and_powerline() {
        for text in ["─", "╬", "▄", "⠿", "\u{E0B0}"] {
            assert!(is_sprite(text), "{text}");
        }
        for text in ["a", "─a", "", "字", "→"] {
            assert!(!is_sprite(text), "{text:?}");
        }
        assert_eq!(shapes('a', CELL), None);
    }

    #[test]
    fn a_light_line_is_one_pixel_through_the_centre_and_a_heavy_one_three() {
        // The halves meet, without a gap or an overlap.
        assert_eq!(rects('─'), vec![(0.0, 8.0, 5.0, 1.0), (5.0, 8.0, 3.0, 1.0)]);
        assert_eq!(rects('│'), vec![(4.0, 0.0, 1.0, 9.0), (4.0, 9.0, 1.0, 7.0)]);
        let heavy = rects('━');
        assert_eq!(heavy[0].3, 3.0);
        assert_eq!(heavy[0].1, 7.0, "centred on the light line");
    }

    #[test]
    fn a_corner_meets_at_the_centre_and_a_junction_overlaps_the_thicker_arm() {
        // ┌: down from the centre band's top, right from the centre band's left.
        assert_eq!(rects('┌'), vec![(4.0, 8.0, 1.0, 8.0), (4.0, 8.0, 4.0, 1.0)]);
        // ┍: the light stem starts at the top of the heavy bar.
        let r = rects('┍');
        assert_eq!(r[0], (4.0, 7.0, 1.0, 9.0));
        assert_eq!(r[1], (4.0, 7.0, 4.0, 3.0));
        // ╴: a half line stops at the centre.
        assert_eq!(rects('╴'), vec![(0.0, 8.0, 5.0, 1.0)]);
    }

    #[test]
    fn double_lines_are_two_bars_and_a_double_corner_is_an_l() {
        let r = rects('═');
        assert_eq!(r.len(), 4);
        assert!(r.iter().any(|b| b.1 == 7.0) && r.iter().any(|b| b.1 == 9.0), "{r:?}");
        // ╔: outer bars reach the outer corner, inner bars stop short of the centre.
        let r = rects('╔');
        let (cx, cy) = CELL.centre();
        let outer_v = r.iter().find(|b| b.0 < cx - 1.0).expect("left bar");
        let inner_v = r.iter().find(|b| b.0 > cx).expect("right bar");
        assert!(outer_v.1 < inner_v.1, "the outer bar starts higher: {r:?}");
        assert_eq!(outer_v.1, 7.0, "to the top of the outer horizontal bar: {r:?}");
        assert_eq!(inner_v.1, 9.0, "to the top of the inner horizontal bar: {r:?}");
        // ╤: the single stem hangs from the lower bar, the doubles run through.
        let r = rects('╤');
        let stem = r.iter().find(|b| b.2 == 1.0).expect("stem");
        assert_eq!(stem.1, cy + 0.5, "{r:?}");
        // ╫: the single line crosses both bars.
        let r = rects('╫');
        let horizontals: Vec<_> = r.iter().filter(|b| b.3 == 1.0).collect();
        assert_eq!(horizontals.len(), 2);
        assert!(horizontals.iter().any(|b| b.0 == 0.0 && b.0 + b.2 >= cx), "{r:?}");
    }

    #[test]
    fn dashes_split_the_line_with_a_gap_of_one_thickness() {
        let r = rects('┈');
        assert_eq!(r.len(), 4, "{r:?}");
        let total: f32 = r.iter().map(|b| b.2).sum();
        assert!((total - (8.0 - 3.0)).abs() < 1.0, "{r:?}");
        assert_eq!(rects('╌').len(), 2);
    }

    #[test]
    fn rounded_corners_are_an_arc_with_two_stubs() {
        let s = shapes('╭', CELL).expect("arc");
        assert!(matches!(s[0], Shape::Arc { sweep: true, .. }), "{s:?}");
        assert_eq!(s.len(), 3);
        let s = shapes('╰', CELL).expect("arc");
        assert!(matches!(s[0], Shape::Arc { sweep: false, .. }), "{s:?}");
    }

    #[test]
    fn blocks_fill_their_fraction_and_shades_are_translucent() {
        assert_eq!(rects('▄'), vec![(0.0, 8.0, 8.0, 8.0)]);
        assert_eq!(rects('█'), vec![(0.0, 0.0, 8.0, 16.0)]);
        assert_eq!(rects('▏'), vec![(0.0, 0.0, 1.0, 16.0)]);
        assert_eq!(rects('▝'), vec![(4.0, 0.0, 4.0, 8.0)]);
        assert_eq!(rects('▚').len(), 2);
        assert!(matches!(
            shapes('▒', CELL).expect("shade")[0],
            Shape::Rect { ink: Ink::Shade(a), .. } if (a - 0.5).abs() < f32::EPSILON
        ));
    }

    #[test]
    fn braille_draws_one_dot_per_bit_and_powerline_a_triangle() {
        assert_eq!(shapes('⠀', CELL).expect("blank").len(), 0);
        let full = shapes('⣿', CELL).expect("all dots");
        assert_eq!(full.len(), 8);
        assert!(full.iter().all(|s| matches!(s, Shape::Rect { round: true, .. })));
        let one = shapes('⠁', CELL).expect("dot 1");
        assert!(matches!(one[0], Shape::Rect { x, y, .. } if x < 4.0 && y < 4.0), "{one:?}");
        assert!(matches!(shapes('\u{E0B0}', CELL).expect("pl")[0], Shape::Poly(_)));
        assert!(matches!(shapes('\u{E0B1}', CELL).expect("pl")[0], Shape::Stroke { .. }));
    }

    #[test]
    fn edges_snap_to_device_pixels() {
        let cell = Cell { w: 7.5, h: 15.5, thickness: 0.5, scale: 2.0 };
        for s in shapes('┼', cell).expect("cross") {
            if let Shape::Rect { x, y, w, h, .. } = s {
                for v in [x, y, w, h] {
                    let doubled = v * 2.0;
                    assert!((doubled - doubled.round()).abs() < 1e-4, "{v}");
                }
            }
        }
    }
}

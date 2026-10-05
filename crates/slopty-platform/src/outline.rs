//! Filled vector outlines drawn by Core Graphics into one alpha byte per device pixel: an
//! agent's mark beside its words, as crisp at 1x as the SF Symbols round it.
//!
//! An [`Outline`] is read from SVG path data (`M L H V C A Z`, absolute and relative, the
//! subset the marks' published files use), with its arcs turned into cubic Béziers and its
//! ink box worked out from the curves themselves rather than their control points.
//! [`rasterize_outline`] scales it so its ink box's longer side is a whole number of device
//! pixels and fills it with the non-zero rule into an alpha-only bitmap the ink box's size, so
//! a caller centres the ink and not a padded box. GPUI's own SVG path draws at twice the size
//! and halves it, which costs a filled mark its edges at 1x
//! (`docs/decisions/brand.md`, "Each agent wears its owner's mark").

use std::ffi::c_void;
use std::fmt;

use objc2_core_foundation::CGFloat;
use objc2_core_graphics::{CGBitmapContextCreate, CGContext, CGImageAlphaInfo};

use crate::symbols::{MaskRect, SymbolMask};

/// A point in the outline's own units, y down as SVG has it.
type Point = (f64, f64);

/// One step of a path.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Seg {
    Move(Point),
    Line(Point),
    Cubic(Point, Point, Point),
    Close,
}

/// A filled shape read from SVG path data, and its ink box.
#[derive(Clone, Debug, PartialEq)]
pub struct Outline {
    segs: Vec<Seg>,
    /// The ink's left, top, right and bottom edges, in the outline's units.
    ink: [f64; 4],
}

/// Why path data could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutlineError(String);

impl fmt::Display for OutlineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "path data: {}", self.0)
    }
}

impl std::error::Error for OutlineError {}

impl Outline {
    /// The shape `paths` draw together, each SVG path data (a `d` attribute), filled with the
    /// non-zero rule as SVG fills them by default.
    ///
    /// # Errors
    ///
    /// When a path holds a command this reader does not know, a number it cannot read, or
    /// draws nothing.
    pub fn parse(paths: &[&str]) -> Result<Self, OutlineError> {
        let mut segs = Vec::new();
        for path in paths {
            read_path(path, &mut segs)?;
        }
        let ink = ink_box(&segs).ok_or_else(|| OutlineError("no ink".to_owned()))?;
        Ok(Self { segs, ink })
    }

    /// The ink's width and height, in the outline's units.
    #[must_use]
    pub fn ink_size(&self) -> (f64, f64) {
        let [left, top, right, bottom] = self.ink;
        (right - left, bottom - top)
    }
}

/// Draws `outline` with its ink box's longer side `ink_px` device pixels, into a mask the ink
/// box's size, rounded to whole pixels; its alignment rectangle is the whole mask.
///
/// `None` for an empty size or when Core Graphics makes no context.
#[must_use]
pub fn rasterize_outline(outline: &Outline, ink_px: u32) -> Option<SymbolMask> {
    let (ink_w, ink_h) = outline.ink_size();
    let longer = ink_w.max(ink_h);
    if ink_px == 0 || longer <= 0.0 {
        return None;
    }
    let scale = f64::from(ink_px) / longer;
    let width = whole(ink_w * scale)?;
    let height = whole(ink_h * scale)?;
    let mut alpha = vec![0_u8; width.checked_mul(height)?];
    // SAFETY: `CGBitmapContextCreate` (CGBitmapContext.h) draws into `data` for the context's
    // life: `alpha` holds `bytes_per_row × height` bytes (one byte a pixel) and outlives the
    // context, which is dropped before `alpha` is read. 8 bits of alpha alone with no colour
    // space is one of its supported formats.
    let context = unsafe {
        CGBitmapContextCreate(
            alpha.as_mut_ptr().cast::<c_void>(),
            width,
            height,
            8,
            width,
            None,
            CGImageAlphaInfo::Only.0,
        )
    }?;
    let ctx = Some(&*context);
    // Core Graphics' user space has its origin at the bottom left; the outline's y runs down.
    #[expect(clippy::cast_precision_loss, reason = "a mask is a few hundred pixels high")]
    CGContext::translate_ctm(ctx, 0.0, height as CGFloat);
    CGContext::scale_ctm(ctx, scale, -scale);
    let [left, top, ..] = outline.ink;
    CGContext::translate_ctm(ctx, -left, -top);
    for seg in &outline.segs {
        match *seg {
            Seg::Move((x, y)) => CGContext::move_to_point(ctx, x, y),
            Seg::Line((x, y)) => CGContext::add_line_to_point(ctx, x, y),
            Seg::Cubic((x1, y1), (x2, y2), (x, y)) => {
                CGContext::add_curve_to_point(ctx, x1, y1, x2, y2, x, y);
            }
            Seg::Close => CGContext::close_path(ctx),
        }
    }
    CGContext::set_gray_fill_color(ctx, 0.0, 1.0);
    CGContext::fill_path(ctx);
    drop(context);
    #[expect(clippy::cast_precision_loss, reason = "a mask is a few hundred pixels")]
    let (w, h) = (width as f32, height as f32);
    Some(SymbolMask {
        width: u32::try_from(width).ok()?,
        height: u32::try_from(height).ok()?,
        alpha,
        alignment: MaskRect { x: 0.0, y: 0.0, width: w, height: h },
        baseline: h,
    })
}

/// `pixels` rounded to a whole count of at least one; `None` when not finite.
fn whole(pixels: f64) -> Option<usize> {
    let pixels = pixels.round().max(1.0);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "checked finite and at least one, and a mark is far below `usize::MAX` pixels"
    )]
    pixels.is_finite().then_some(pixels as usize)
}

/// Reads one path's data onto `segs`.
fn read_path(d: &str, segs: &mut Vec<Seg>) -> Result<(), OutlineError> {
    let mut tokens = Tokens { rest: d };
    let mut command = None;
    let mut at: Point = (0.0, 0.0);
    let mut start: Point = (0.0, 0.0);
    while let Some(next) = tokens.command_or_number()? {
        let cmd = match next {
            Token::Command(c) => {
                command = Some(c);
                c
            }
            // A number after a command repeats it; after a move, the repeats are lines.
            Token::Number => match command {
                Some('M') => 'L',
                Some('m') => 'l',
                Some(c) if !matches!(c, 'Z' | 'z') => c,
                _ => return Err(OutlineError(format!("a number with no command at {d:.40}"))),
            },
        };
        let relative = cmd.is_ascii_lowercase();
        let base = if relative { at } else { (0.0, 0.0) };
        let pair = |tokens: &mut Tokens<'_>| -> Result<Point, OutlineError> {
            Ok((tokens.number()? + base.0, tokens.number()? + base.1))
        };
        match cmd.to_ascii_uppercase() {
            'M' => {
                at = pair(&mut tokens)?;
                start = at;
                segs.push(Seg::Move(at));
                command = Some(if relative { 'l' } else { 'L' });
            }
            'L' => {
                at = pair(&mut tokens)?;
                segs.push(Seg::Line(at));
            }
            'H' => {
                let x = tokens.number()?;
                at = (if relative { at.0 + x } else { x }, at.1);
                segs.push(Seg::Line(at));
            }
            'V' => {
                let y = tokens.number()?;
                at = (at.0, if relative { at.1 + y } else { y });
                segs.push(Seg::Line(at));
            }
            'C' => {
                let (c1, c2, to) = (pair(&mut tokens)?, pair(&mut tokens)?, pair(&mut tokens)?);
                segs.push(Seg::Cubic(c1, c2, to));
                at = to;
            }
            'A' => {
                let radii = (tokens.number()?, tokens.number()?);
                let turn = tokens.number()?;
                let (large, sweep) = (tokens.flag()?, tokens.flag()?);
                let to = pair(&mut tokens)?;
                arc(segs, at, &Arc { radii, turn, large, sweep, to });
                at = to;
            }
            'Z' => {
                segs.push(Seg::Close);
                at = start;
            }
            other => return Err(OutlineError(format!("no command {other:?}"))),
        }
    }
    Ok(())
}

/// What comes next in path data.
enum Token {
    Command(char),
    /// A number, left unread.
    Number,
}

/// Path data being read.
struct Tokens<'a> {
    rest: &'a str,
}

impl Tokens<'_> {
    fn skip(&mut self) {
        self.rest = self.rest.trim_start_matches(|c: char| c.is_ascii_whitespace() || c == ',');
    }

    /// The next command, or `Number` when a number comes first; `None` at the end.
    fn command_or_number(&mut self) -> Result<Option<Token>, OutlineError> {
        self.skip();
        let Some(c) = self.rest.chars().next() else { return Ok(None) };
        if c.is_ascii_alphabetic() {
            self.rest = self.rest.get(c.len_utf8()..).unwrap_or_default();
            return Ok(Some(Token::Command(c)));
        }
        if c.is_ascii_digit() || matches!(c, '-' | '+' | '.') {
            return Ok(Some(Token::Number));
        }
        Err(OutlineError(format!("cannot read {:.20}", self.rest)))
    }

    /// The next number: a sign, digits with at most one point, and an exponent.
    fn number(&mut self) -> Result<f64, OutlineError> {
        self.skip();
        let end = number_end(self.rest.as_bytes());
        let (text, rest) = self.rest.split_at_checked(end).unwrap_or((self.rest, ""));
        let value = text
            .parse()
            .map_err(|e| OutlineError(format!("no number at {:.20}: {e}", self.rest)))?;
        self.rest = rest;
        Ok(value)
    }

    /// An arc's flag: one digit, 0 or 1, which may run straight into what follows.
    fn flag(&mut self) -> Result<bool, OutlineError> {
        self.skip();
        let flag = match self.rest.as_bytes().first() {
            Some(b'0') => false,
            Some(b'1') => true,
            _ => return Err(OutlineError(format!("no arc flag at {:.20}", self.rest))),
        };
        self.rest = self.rest.get(1..).unwrap_or_default();
        Ok(flag)
    }
}

/// Where a number that starts `bytes` ends: an optional sign, digits with at most one point,
/// and an exponent when one follows whole.
fn number_end(bytes: &[u8]) -> usize {
    let sign = usize::from(matches!(bytes.first(), Some(b'-' | b'+')));
    let mut point = false;
    let digits = bytes
        .iter()
        .skip(sign)
        .take_while(|&&b| {
            let first_point = b == b'.' && !point;
            point |= first_point;
            b.is_ascii_digit() || first_point
        })
        .count();
    let end = sign.saturating_add(digits);
    let Some(after) = bytes.get(end..) else { return end };
    if !matches!(after.first(), Some(b'e' | b'E')) {
        return end;
    }
    let exp_sign = usize::from(matches!(after.get(1), Some(b'-' | b'+')));
    let exp_digits =
        after.iter().skip(exp_sign.saturating_add(1)).take_while(|b| b.is_ascii_digit()).count();
    if exp_digits == 0 {
        end
    } else {
        end.saturating_add(1).saturating_add(exp_sign).saturating_add(exp_digits)
    }
}

/// An SVG elliptical arc's parameters, in the outline's units.
struct Arc {
    radii: (f64, f64),
    /// The ellipse's x axis turned this many degrees.
    turn: f64,
    large: bool,
    sweep: bool,
    to: Point,
}

/// The arc from `from` as cubic Béziers of at most a quarter turn each (SVG 1.1, appendix F.6:
/// the endpoint form to the centre form).
fn arc(segs: &mut Vec<Seg>, from: Point, arc: &Arc) {
    let &Arc { radii, turn, large, sweep, to } = arc;
    let (mut rx, mut ry) = (radii.0.abs(), radii.1.abs());
    if from == to {
        return;
    }
    if rx == 0.0 || ry == 0.0 {
        segs.push(Seg::Line(to));
        return;
    }
    let phi = turn.to_radians();
    let (sin, cos) = phi.sin_cos();
    let (dx, dy) = ((from.0 - to.0) / 2.0, (from.1 - to.1) / 2.0);
    let x1 = cos.mul_add(dx, sin * dy);
    let y1 = (-sin).mul_add(dx, cos * dy);
    // Radii too small to reach are scaled up until they just do.
    let reach = (x1 * x1) / (rx * rx) + (y1 * y1) / (ry * ry);
    if reach > 1.0 {
        let grow = reach.sqrt();
        rx *= grow;
        ry *= grow;
    }
    let (rx2, ry2) = (rx * rx, ry * ry);
    let num = ry2.mul_add(-(x1 * x1), rx2.mul_add(ry2, -(rx2 * y1 * y1)));
    let den = rx2.mul_add(y1 * y1, ry2 * x1 * x1);
    let mut k = (num / den).max(0.0).sqrt();
    if large == sweep {
        k = -k;
    }
    let cx1 = k * rx * y1 / ry;
    let cy1 = -k * ry * x1 / rx;
    let cx = cos.mul_add(cx1, -(sin * cy1)) + f64::midpoint(from.0, to.0);
    let cy = sin.mul_add(cx1, cos * cy1) + f64::midpoint(from.1, to.1);
    let angle = |ux: f64, uy: f64| uy.atan2(ux);
    let start = angle((x1 - cx1) / rx, (y1 - cy1) / ry);
    let mut delta = angle((-x1 - cx1) / rx, (-y1 - cy1) / ry) - start;
    if sweep && delta < 0.0 {
        delta += std::f64::consts::TAU;
    } else if !sweep && delta > 0.0 {
        delta -= std::f64::consts::TAU;
    }
    let pieces = (delta.abs() / std::f64::consts::FRAC_PI_2).ceil().max(1.0);
    let step = delta / pieces;
    let handle = 4.0 / 3.0 * (step / 4.0).tan();
    // A point on the ellipse at `t`, and its tangent, in the outline's space.
    let on = |t: f64| {
        let (st, ct) = t.sin_cos();
        let (ex, ey) = (rx * ct, ry * st);
        (cos.mul_add(ex, -(sin * ey)) + cx, sin.mul_add(ex, cos * ey) + cy)
    };
    let tangent = |t: f64| {
        let (st, ct) = t.sin_cos();
        let (ex, ey) = (-rx * st, ry * ct);
        (cos.mul_add(ex, -(sin * ey)), sin.mul_add(ex, cos * ey))
    };
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "at most 4")]
    let count = pieces as u32;
    let mut t = start;
    for i in 0..count {
        let t2 = t + step;
        let (p0, p3) = (on(t), if i.saturating_add(1) == count { to } else { on(t2) });
        let (d0, d3) = (tangent(t), tangent(t2));
        let c1 = (handle.mul_add(d0.0, p0.0), handle.mul_add(d0.1, p0.1));
        let c2 = ((-handle).mul_add(d3.0, p3.0), (-handle).mul_add(d3.1, p3.1));
        segs.push(Seg::Cubic(c1, c2, p3));
        t = t2;
    }
}

/// The ink box of `segs`: the extremes of every line's ends and every curve's own extrema, not
/// its control points.
fn ink_box(segs: &[Seg]) -> Option<[f64; 4]> {
    let mut ink: Option<[f64; 4]> = None;
    let mut grow = |(x, y): Point| {
        let b = ink.get_or_insert([x, y, x, y]);
        *b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
    };
    let mut at = (0.0, 0.0);
    let mut start = (0.0, 0.0);
    for seg in segs {
        match *seg {
            Seg::Move(p) => {
                at = p;
                start = p;
            }
            Seg::Line(p) => {
                grow(at);
                grow(p);
                at = p;
            }
            Seg::Cubic(c1, c2, p) => {
                grow(at);
                grow(p);
                for t in
                    extrema(at.0, c1.0, c2.0, p.0).into_iter().chain(extrema(at.1, c1.1, c2.1, p.1))
                {
                    grow(cubic_at(at, c1, c2, p, t));
                }
                at = p;
            }
            Seg::Close => at = start,
        }
    }
    ink.filter(|[l, t, r, b]| r > l && b > t)
}

/// The `t` in (0, 1) where a cubic's coordinate turns, from its derivative's roots.
fn extrema(p0: f64, p1: f64, p2: f64, p3: f64) -> Vec<f64> {
    let a = 3.0 * (3.0_f64.mul_add(p1 - p2, p3 - p0));
    let b = 6.0 * (2.0_f64.mul_add(-p1, p0) + p2);
    let c = 3.0 * (p1 - p0);
    let roots = if a.abs() < 1e-12 {
        if b.abs() < 1e-12 { Vec::new() } else { vec![-c / b] }
    } else {
        let disc = b.mul_add(b, -4.0 * a * c);
        if disc < 0.0 {
            Vec::new()
        } else {
            let root = disc.sqrt();
            vec![(-b + root) / (2.0 * a), (-b - root) / (2.0 * a)]
        }
    };
    roots.into_iter().filter(|t| *t > 0.0 && *t < 1.0).collect()
}

/// The point at `t` on a cubic.
fn cubic_at(p0: Point, p1: Point, p2: Point, p3: Point, t: f64) -> Point {
    let u = 1.0 - t;
    let weights = [u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t];
    let along = |coord: fn(Point) -> f64| {
        [p0, p1, p2, p3].iter().zip(weights).fold(0.0, |sum, (p, w)| w.mul_add(coord(*p), sum))
    };
    (along(|p| p.0), along(|p| p.1))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Absolute and relative commands, implicit repeats, exponents and arcs read as SVG reads
    /// them, and the ink box is the curves', not their control points'.
    #[test]
    fn path_data_reads_as_svg_reads_it() {
        let square = Outline::parse(&["M1 1H11V11H1Z"]).unwrap();
        assert_eq!(square.ink, [1.0, 1.0, 11.0, 11.0]);
        let relative = Outline::parse(&["m1 1 10 0 0 10-10 0z"]).unwrap();
        assert_eq!(relative.ink, [1.0, 1.0, 11.0, 11.0], "a move's repeats are lines");
        let tiny = Outline::parse(&["M 3.3e-05 0 L 2 0 L 2 2 Z"]).unwrap();
        assert!((tiny.ink[0] - 3.3e-05).abs() < 1e-12);
        // A half circle of radius 5 above the line from (0, 5) to (10, 5).
        let dome = Outline::parse(&["M0 5A5 5 0 0 1 10 5Z"]).unwrap();
        let [l, t, r, b] = dome.ink;
        assert!((l, r, b) == (0.0, 10.0, 5.0) && (t - 0.0).abs() < 1e-3, "{:?}", dome.ink);
        let flags = Outline::parse(&["M0 5a5 5 0 015-5 5 5 0 015 5z"]).unwrap();
        assert!((flags.ink[1]).abs() < 1e-3, "flags run into the numbers: {:?}", flags.ink);
        assert!(Outline::parse(&["M0 0Q1 1 2 2"]).is_err(), "a command it does not know");
        assert!(Outline::parse(&[""]).is_err(), "no ink");
    }

    /// A shape on whole pixels fills them whole and leaves the rest empty; the mask is the ink
    /// box at the size asked, and a non-zero hole stays a hole.
    #[test]
    fn an_outline_lands_on_the_pixels_it_covers() {
        let frame = Outline::parse(&["M0 0H4V4H0Z M1 1V3H3V1Z"]).unwrap();
        let mask = rasterize_outline(&frame, 8).unwrap();
        assert_eq!((mask.width, mask.height), (8, 8));
        let at = |x: usize, y: usize| mask.alpha[y * 8 + x];
        assert_eq!((at(0, 0), at(7, 7), at(1, 1)), (255, 255, 255), "the frame is solid");
        assert_eq!((at(3, 3), at(4, 4)), (0, 0), "the hole wound the other way is empty");
        assert!(mask.alpha.iter().all(|a| *a == 0 || *a == 255), "nothing half covered");
        let wide = Outline::parse(&["M0 0H10V5H0Z"]).unwrap();
        let mask = rasterize_outline(&wide, 14).unwrap();
        assert_eq!((mask.width, mask.height), (14, 7), "the longer side is the size asked");
        assert!(rasterize_outline(&wide, 0).is_none());
    }
}

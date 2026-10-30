// Derived from zeron (MIT, github.com/zeronsh/zeron @ 3b2f780), see THIRD_PARTY_NOTICES.md
//! `ui.canvas` drawing.
//!
//! The draw function gets a `g` object whose methods record vector commands
//! in design pixels, origin top-left. The desktop
//! paints them with GPUI paths and shaped text, so a canvas is as crisp and
//! as theme-bound as the rest of the reply.

use std::f32::consts::{FRAC_PI_2, PI, TAU};

use mlua::{Table, UserData, UserDataFields, UserDataMethods, Value};

use crate::node::{parse_color, value_text};
use crate::num::{count, narrow, whole};
use crate::style::{Align, Color, TextSize, Token, Weight};
use crate::{CanvasDrawing, VisualError};

const MAX_COMMANDS: usize = 40_000;
const MAX_POINTS: usize = 200_000;

/// One thing a canvas draws, in design pixels.
#[derive(Debug, Clone, PartialEq)]
pub enum DrawCmd {
    /// An open or closed stroked path.
    Stroke {
        /// The path's points, in order.
        points: Vec<[f32; 2]>,
        /// The last point joins the first.
        closed: bool,
        /// The line's color.
        color: Color,
        /// The line's width.
        width: f32,
        /// Dash length, with gaps as long; `None` draws it solid.
        dash: Option<f32>,
    },
    /// A filled polygon (any simple polygon, convex or not).
    Fill {
        /// The outline's points, in order.
        points: Vec<[f32; 2]>,
        /// The fill's color.
        color: Color,
    },
    /// A line of text.
    Text(DrawText),
}

/// A line of text a canvas draws, in the interface font.
#[derive(Debug, Clone, PartialEq)]
pub struct DrawText {
    /// Where the text is anchored, across.
    pub x: f32,
    /// Where the text is anchored, down.
    pub y: f32,
    /// The text, on one line.
    pub text: String,
    /// Its step on the type scale.
    pub size: TextSize,
    /// Its weight.
    pub weight: Weight,
    /// Its color.
    pub color: Color,
    /// Set in the monospace font.
    pub mono: bool,
    /// Which side of the text sits at `x`.
    pub align: Align,
    /// Which side of the text sits at `y`: its top, middle, or bottom.
    pub baseline: Align,
}

/// The pointer, in canvas pixels, while it is over the canvas.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Pointer {
    /// Across, from the canvas's left.
    pub x: f32,
    /// Down, from the canvas's top.
    pub y: f32,
    /// A button or finger is down.
    pub down: bool,
}

/// The `g` a canvas's draw function draws with: it records what is drawn.
#[derive(Debug)]
pub(crate) struct Graphics {
    width: f32,
    height: f32,
    time: f64,
    pointer: Option<Pointer>,
    commands: Vec<DrawCmd>,
    /// The draw function looked at `pointer`: a touch host lets such a
    /// canvas take drags instead of scrolling past it.
    read_pointer: std::cell::Cell<bool>,
    points: usize,
}

impl Graphics {
    pub(crate) const fn new(width: f32, height: f32, time: f64, pointer: Option<Pointer>) -> Self {
        Self {
            width,
            height,
            time,
            pointer,
            commands: Vec::new(),
            read_pointer: std::cell::Cell::new(false),
            points: 0,
        }
    }

    /// What the draw function drew, and whether it read the pointer.
    pub(crate) fn into_drawing(self) -> CanvasDrawing {
        CanvasDrawing { reads_pointer: self.read_pointer.get(), commands: self.commands }
    }

    fn push(&mut self, command: DrawCmd) -> mlua::Result<()> {
        if self.commands.len() >= MAX_COMMANDS {
            return Err(lua_error(format!(
                "a canvas draws at most {MAX_COMMANDS} shapes per frame"
            )));
        }
        self.points = self.points.saturating_add(match &command {
            DrawCmd::Stroke { points, .. } | DrawCmd::Fill { points, .. } => points.len(),
            DrawCmd::Text(_) => 1,
        });
        if self.points > MAX_POINTS {
            return Err(lua_error(format!("a canvas draws at most {MAX_POINTS} points per frame")));
        }
        self.commands.push(command);
        Ok(())
    }
}

pub(crate) fn lua_error(message: impl Into<String>) -> mlua::Error {
    mlua::Error::RuntimeError(message.into())
}

fn to_lua(error: VisualError) -> mlua::Error {
    lua_error(error.message)
}

/// Shape options: `{ color = ..., fill = ..., stroke = ..., width = ...,
/// dash = ... }`.
#[derive(Default)]
pub(crate) struct Paint {
    pub fill: Option<Color>,
    pub stroke: Option<Color>,
    pub width: f32,
    pub dash: Option<f32>,
}

impl Paint {
    /// `default_fill` decides what a bare `color` means: a fill for shapes,
    /// a stroke for lines.
    pub(crate) fn parse(options: Option<Table>, default_fill: bool) -> mlua::Result<Self> {
        let mut paint = Self { width: 1.0, ..Default::default() };
        let Some(options) = options else {
            let color = Color::token(Token::Text);
            if default_fill {
                paint.fill = Some(color);
            } else {
                paint.stroke = Some(color);
            }
            return Ok(paint);
        };
        let color = parse_color(options.raw_get::<Value>("color")?).map_err(to_lua)?;
        paint.fill = parse_color(options.raw_get::<Value>("fill")?).map_err(to_lua)?;
        paint.stroke = parse_color(options.raw_get::<Value>("stroke")?).map_err(to_lua)?;
        if let Some(color) = color {
            if default_fill && paint.fill.is_none() {
                paint.fill = Some(color);
            } else if paint.stroke.is_none() {
                paint.stroke = Some(color);
            }
        }
        if paint.fill.is_none() && paint.stroke.is_none() {
            let color = Color::token(Token::Text);
            if default_fill {
                paint.fill = Some(color);
            } else {
                paint.stroke = Some(color);
            }
        }
        if let Some(alpha) = options.raw_get::<Option<f64>>("alpha")? {
            paint.fill = paint.fill.map(|c| c.with_alpha(narrow(alpha)));
            paint.stroke = paint.stroke.map(|c| c.with_alpha(narrow(alpha)));
        }
        paint.width =
            narrow(options.raw_get::<Option<f64>>("width")?.unwrap_or(1.0).clamp(0.25, 32.0));
        paint.dash = options.raw_get::<Option<f64>>("dash")?.map(|d| narrow(d.clamp(1.0, 64.0)));
        Ok(paint)
    }

    /// A line's color: the stroke, else the fill. [`Self::parse`] always
    /// sets one, so the text color never stands in.
    fn line_color(&self) -> Color {
        self.stroke.or(self.fill).unwrap_or(Color::token(Token::Text))
    }
}

/// Points as a flat `{x1, y1, x2, y2, ...}` list or `{{x, y}, ...}` pairs.
pub(crate) fn points(table: &Table) -> mlua::Result<Vec<[f32; 2]>> {
    let len = table.raw_len();
    let mut out = Vec::with_capacity((len / 2).saturating_add(1));
    match table.raw_get::<Value>(1)? {
        Value::Table(_) => {
            for pair in table.sequence_values::<Table>() {
                let pair = pair?;
                out.push([narrow(pair.raw_get::<f64>(1)?), narrow(pair.raw_get::<f64>(2)?)]);
            }
        }
        _ => {
            for ix in (1..len).step_by(2) {
                let (x, y) =
                    (table.raw_get::<f64>(ix)?, table.raw_get::<f64>(ix.saturating_add(1))?);
                out.push([narrow(x), narrow(y)]);
            }
        }
    }
    if out.iter().any(|[x, y]| !x.is_finite() || !y.is_finite()) {
        return Err(lua_error("a point is NaN or infinite"));
    }
    Ok(out)
}

/// `segments` points around an ellipse arc from `a0` to `a1` (radians,
/// clockwise from 3 o'clock, since y points down).
pub(crate) fn arc_points(cx: f32, cy: f32, r: f32, a0: f32, a1: f32) -> Vec<[f32; 2]> {
    let sweep = a1 - a0;
    let segments = whole(((sweep.abs() * r.max(1.0)).sqrt() * 2.5).clamp(6.0, 160.0));
    (0..=segments)
        .map(|i| {
            let a = a0 + sweep * count(i) / count(segments);
            [r.mul_add(a.cos(), cx), r.mul_add(a.sin(), cy)]
        })
        .collect()
}

impl Graphics {
    fn shape(&mut self, points: Vec<[f32; 2]>, paint: &Paint) -> mlua::Result<()> {
        if let Some(fill) = paint.fill {
            self.push(DrawCmd::Fill { points: points.clone(), color: fill })?;
        }
        if let Some(stroke) = paint.stroke {
            self.push(DrawCmd::Stroke {
                points,
                closed: true,
                color: stroke,
                width: paint.width,
                dash: paint.dash,
            })?;
        }
        Ok(())
    }
}

fn rect_points(left: f32, top: f32, width: f32, height: f32, radius: f32) -> Vec<[f32; 2]> {
    let (left, width) = if width < 0.0 { (left + width, -width) } else { (left, width) };
    let (top, height) = if height < 0.0 { (top + height, -height) } else { (top, height) };
    let (right, bottom) = (left + width, top + height);
    let round = radius.clamp(0.0, width.min(height) / 2.0);
    if round < 0.25 {
        return vec![[left, top], [right, top], [right, bottom], [left, bottom]];
    }
    let mut out = Vec::new();
    out.extend(arc_points(right - round, top + round, round, -FRAC_PI_2, 0.0));
    out.extend(arc_points(right - round, bottom - round, round, 0.0, FRAC_PI_2));
    out.extend(arc_points(left + round, bottom - round, round, FRAC_PI_2, PI));
    out.extend(arc_points(left + round, top + round, round, PI, PI + FRAC_PI_2));
    out
}

impl UserData for Graphics {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("width", |_, g| Ok(g.width));
        fields.add_field_method_get("height", |_, g| Ok(g.height));
        fields.add_field_method_get("time", |_, g| Ok(g.time));
        fields.add_field_method_get("pointer", |lua, g| {
            g.read_pointer.set(true);
            match g.pointer {
                Some(p) => {
                    let t = lua.create_table()?;
                    t.set("x", p.x)?;
                    t.set("y", p.y)?;
                    t.set("down", p.down)?;
                    Ok(Value::Table(t))
                }
                None => Ok(Value::Nil),
            }
        });
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method_mut(
            "line",
            |_, g, (x1, y1, x2, y2, options): (f32, f32, f32, f32, Option<Table>)| {
                let paint = Paint::parse(options, false)?;
                g.push(DrawCmd::Stroke {
                    points: vec![[x1, y1], [x2, y2]],
                    closed: false,
                    color: paint.line_color(),
                    width: paint.width,
                    dash: paint.dash,
                })
            },
        );
        methods.add_method_mut("polyline", |_, g, (pts, options): (Table, Option<Table>)| {
            let paint = Paint::parse(options, false)?;
            let points = points(&pts)?;
            if points.len() < 2 {
                return Ok(());
            }
            g.push(DrawCmd::Stroke {
                points,
                closed: false,
                color: paint.line_color(),
                width: paint.width,
                dash: paint.dash,
            })
        });
        methods.add_method_mut("polygon", |_, g, (pts, options): (Table, Option<Table>)| {
            let paint = Paint::parse(options, true)?;
            let points = points(&pts)?;
            if points.len() < 3 {
                return Ok(());
            }
            g.shape(points, &paint)
        });
        methods.add_method_mut(
            "rect",
            |_, g, (x, y, w, h, options): (f32, f32, f32, f32, Option<Table>)| {
                let radius = match &options {
                    Some(o) => o.raw_get::<Option<f32>>("radius")?.unwrap_or(0.0),
                    None => 0.0,
                };
                let paint = Paint::parse(options, true)?;
                g.shape(rect_points(x, y, w, h, radius), &paint)
            },
        );
        methods.add_method_mut(
            "circle",
            |_, g, (x, y, r, options): (f32, f32, f32, Option<Table>)| {
                let paint = Paint::parse(options, true)?;
                let mut pts = arc_points(x, y, r.abs(), 0.0, TAU);
                pts.pop();
                g.shape(pts, &paint)
            },
        );
        // An arc stroke, or with `inner` (a radius) / `fill`, a pie or donut
        // sector.
        methods.add_method_mut(
            "arc",
            |_, g, (x, y, r, a0, a1, options): (f32, f32, f32, f32, f32, Option<Table>)| {
                let inner = match &options {
                    Some(o) => o.raw_get::<Option<f32>>("inner")?,
                    None => None,
                };
                let sector = inner.is_some()
                    || options
                        .as_ref()
                        .is_some_and(|o| o.raw_get::<Value>("fill").is_ok_and(|v| !v.is_nil()));
                let paint = Paint::parse(options, sector)?;
                let outer = arc_points(x, y, r.abs(), a0, a1);
                if !sector {
                    return g.push(DrawCmd::Stroke {
                        points: outer,
                        closed: false,
                        color: paint.line_color(),
                        width: paint.width,
                        dash: paint.dash,
                    });
                }
                let mut pts = outer;
                match inner.filter(|r| *r > 0.0) {
                    Some(inner) => {
                        let mut back = arc_points(x, y, inner, a0, a1);
                        back.reverse();
                        pts.extend(back);
                    }
                    None => pts.push([x, y]),
                }
                g.shape(pts, &paint)
            },
        );
        methods.add_method_mut(
            "text",
            |_, g, (x, y, text, options): (f32, f32, Value, Option<Table>)| {
                let mut draw = DrawText {
                    x,
                    y,
                    text: value_text(&text),
                    size: TextSize::Sm,
                    weight: Weight::Regular,
                    color: Color::token(Token::Text),
                    mono: false,
                    align: Align::Start,
                    baseline: Align::Start,
                };
                if let Some(o) = options {
                    if let Some(size) = o.raw_get::<Option<String>>("size")? {
                        draw.size = TextSize::parse(&size).map_err(to_lua)?;
                    }
                    match o.raw_get::<Value>("weight")? {
                        Value::Integer(n) => draw.weight = Weight::from_integer(n),
                        Value::Number(n) => draw.weight = Weight::from_number(n),
                        Value::String(s) => {
                            draw.weight = Weight::parse(&s.to_string_lossy()).map_err(to_lua)?;
                        }
                        _ => {}
                    }
                    if let Some(color) =
                        parse_color(o.raw_get::<Value>("color")?).map_err(to_lua)?
                    {
                        draw.color = color;
                    }
                    draw.mono = o.raw_get::<Option<bool>>("mono")?.unwrap_or(false);
                    if let Some(align) = o.raw_get::<Option<String>>("align")? {
                        draw.align = Align::parse(&align).map_err(to_lua)?;
                    }
                    if let Some(baseline) = o.raw_get::<Option<String>>("baseline")? {
                        draw.baseline = Align::parse(&baseline).map_err(to_lua)?;
                    }
                }
                if draw.text.is_empty() {
                    return Ok(());
                }
                draw.text = draw.text.replace('\n', " ");
                g.push(DrawCmd::Text(draw))
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounded_rects_and_sectors_are_closed_polygons() {
        let plain = rect_points(0.0, 0.0, 10.0, 4.0, 0.0);
        assert_eq!(plain.len(), 4);
        let rounded = rect_points(0.0, 0.0, 10.0, 4.0, 2.0);
        assert!(rounded.len() > 8);
        assert!(
            rounded.iter().all(|[x, y]| *x >= -0.01 && *x <= 10.01 && *y >= -0.01 && *y <= 4.01)
        );
        let negative = rect_points(10.0, 4.0, -10.0, -4.0, 0.0);
        assert_eq!(negative[0], [0.0, 0.0]);
    }
}

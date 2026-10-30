// Derived from zeron (MIT, github.com/zeronsh/zeron @ 3b2f780), see THIRD_PARTY_NOTICES.md
//! Software rendering for `ui.scene`: a depth-buffered rasterizer that turns a
//! scene into one image the desktop paints.
//!
//! Sorting whole faces back to front (the painter's algorithm) cannot order a
//! wall against the furniture beside it, and painting every face as its own
//! vector path made orbiting slow. Here every sample keeps its depth, so
//! surfaces hide each other exactly; faces crossing the near plane are clipped
//! rather than dropped; curved solids shade smoothly; silhouettes and folds get
//! a fine outline; and edges are supersampled once the camera rests. The image
//! is straight-alpha BGRA, transparent wherever nothing was drawn, so a scene
//! still sits on glass and uses the theme's colors.

use std::ops::RangeInclusive;

use crate::num::{byte, count, whole};
use crate::scene::{Camera, Face, Line3, Scene, V3, View, add, dot, normalize, scale, sub};
use crate::style::{Color, TextSize};

/// How carefully to draw: `Draft` while the camera moves (one sample per
/// pixel), `Final` once it rests (supersampled within a budget).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Quality {
    /// One sample per pixel, for a camera in motion.
    Draft,
    /// Supersampled within a budget, for a camera at rest.
    Final,
}

/// A hover hint: canvas position (logical pixels), radius, text.
type Hint = ([f32; 2], f32, String);

/// A rendered scene.
#[derive(Debug, Clone, Default)]
pub struct Raster {
    /// Width in device pixels.
    pub width: u32,
    /// Height in device pixels.
    pub height: u32,
    /// Straight-alpha BGRA, top row first.
    pub bgra: Vec<u8>,
    /// Hover hints: canvas position (logical pixels), radius, text.
    pub hints: Vec<Hint>,
    /// Labels for the desktop to set as text over the image.
    pub labels: Vec<RasterLabel>,
}

/// A label the desktop sets as text over a rendered scene.
#[derive(Debug, Clone, PartialEq)]
pub struct RasterLabel {
    /// Where it is anchored, in logical canvas pixels.
    pub at: [f32; 2],
    /// Its text.
    pub text: String,
    /// Its color.
    pub color: Color,
    /// Its step on the type scale.
    pub size: TextSize,
}

/// Samples a `Final` render may use; past it, it draws one per pixel.
const SAMPLE_BUDGET: usize = 6_000_000;
/// Where two neighbouring samples' depths differ by more than this fraction,
/// a fold or silhouette is outlined.
const EDGE_DEPTH: f32 = 0.02;
/// How much an outline darkens the surface it crosses.
const EDGE_SHADE: f32 = 0.78;

#[derive(Clone, Copy)]
struct Vertex {
    /// View space.
    p: V3,
    /// Lit color, straight sRGB with alpha.
    c: [f32; 4],
}

/// The sample grid: `k` samples per logical pixel.
struct Grid {
    width: usize,
    height: usize,
    k: f32,
}

/// Rows `y0..y1` of the sample grid, rendered by one thread.
struct Band<'a> {
    y0: usize,
    y1: usize,
    width: usize,
    /// Premultiplied sRGB.
    color: &'a mut [[f32; 4]],
    /// View-space depth of the nearest surface; infinity where empty.
    depth: &'a mut [f32],
}

/// A dot in sample space.
struct Dot {
    x: f32,
    y: f32,
    z: f32,
    r: f32,
    c: [f32; 4],
}

impl Scene {
    /// Draw the scene for a canvas `width` by `height` logical pixels at
    /// `pixel_scale` device pixels each. `rgba` resolves a scene color to
    /// straight sRGB components in 0..1. Rows render in parallel.
    #[expect(
        clippy::too_many_arguments,
        reason = "each is the caller's own choice: the camera, the canvas it lays out and its \
                  scale, how carefully to draw, and the palette"
    )]
    pub fn render(
        &self,
        camera: Camera,
        width: f32,
        height: f32,
        pixel_scale: f32,
        quality: Quality,
        rgba: &dyn Fn(Color) -> [f32; 4],
    ) -> Raster {
        let device_w = whole((width * pixel_scale).ceil().max(0.0));
        let device_h = whole((height * pixel_scale).ceil().max(0.0));
        if device_w == 0 || device_h == 0 {
            return Raster::default();
        }
        let Some(view) = self.view_for(camera, width, height) else {
            return Raster {
                width: side(device_w),
                height: side(device_h),
                bgra: vec![0; device_w.saturating_mul(device_h).saturating_mul(4)],
                ..Raster::default()
            };
        };
        // Two samples a side is four a pixel.
        let ss: usize = if quality == Quality::Final
            && device_w.saturating_mul(device_h).saturating_mul(4) <= SAMPLE_BUDGET
        {
            2
        } else {
            1
        };
        let grid = Grid {
            width: device_w.saturating_mul(ss),
            height: device_h.saturating_mul(ss),
            k: pixel_scale * count(ss),
        };

        // Everything in sample space first (cheap, single-threaded); the
        // per-sample work after splits across cores.
        let mut opaque: Vec<[Sample; 3]> = Vec::new();
        let mut translucent: Vec<(f32, Vec<[Sample; 3]>)> = Vec::new();
        for face in self.faces() {
            let Some(polygon) = shade_face(face, &view, rgba) else {
                continue;
            };
            let samples: Vec<Sample> = polygon.iter().map(|v| to_sample(&grid, &view, v)).collect();
            if polygon.iter().any(|v| v.c[3] < 0.999) {
                let depth = polygon.iter().map(|v| v.p[2]).sum::<f32>() / count(polygon.len());
                translucent.push((depth, fan(&samples).collect()));
            } else {
                opaque.extend(fan(&samples));
            }
        }
        // See-through surfaces blend far to near over what they cover.
        translucent.sort_by(|a, b| b.0.total_cmp(&a.0));
        let translucent: Vec<[Sample; 3]> = translucent.into_iter().flat_map(|(_, t)| t).collect();
        // Lines and points ride just in front of the surface they lie on.
        let bias = view.radius * 2e-3;
        let mut lines: Vec<[Sample; 3]> = Vec::new();
        for line in self.lines() {
            line_tris(&grid, &view, line, rgba(line.color), &mut lines);
        }
        let (hints, labels) = self.annotations(camera, width, height);
        let mut dots = Vec::new();
        for point in self.points() {
            let p = view.view(point.at);
            if p[2] < view.near {
                continue;
            }
            let at = view.screen(p);
            dots.push(Dot {
                x: at[0] * grid.k,
                y: at[1] * grid.k,
                z: p[2] - bias,
                r: point.radius * grid.k,
                c: rgba(point.color),
            });
        }
        let cells = grid.width.saturating_mul(grid.height);
        let mut color = vec![[0.0_f32; 4]; cells];
        let mut depth = vec![f32::INFINITY; cells];
        // Opaque surfaces, writing depth.
        for_bands(&grid, ss, &mut color, &mut depth, |band| {
            for tri in &opaque {
                fill_triangle(band, *tri, true, 0.0);
            }
        });
        outline(&grid, ss, &mut color, &depth);
        // Then everything that blends without hiding what is behind it.
        for_bands(&grid, ss, &mut color, &mut depth, |band| {
            for tri in &translucent {
                fill_triangle(band, *tri, false, 0.0);
            }
            for tri in &lines {
                fill_triangle(band, *tri, false, bias);
            }
            for dot in &dots {
                draw_dot(band, dot);
            }
        });

        Raster {
            width: side(device_w),
            height: side(device_h),
            bgra: resolve(&grid, &color, device_w, device_h, ss),
            hints,
            labels,
        }
    }
}

impl Scene {
    /// Hover hints and labels at their canvas positions, for the desktop to
    /// draw as text over whichever renderer made the image.
    pub fn annotations(
        &self,
        camera: Camera,
        width: f32,
        height: f32,
    ) -> (Vec<Hint>, Vec<RasterLabel>) {
        let Some(view) = self.view_for(camera, width, height) else {
            return (Vec::new(), Vec::new());
        };
        let hints = self
            .points()
            .iter()
            .filter_map(|point| {
                let p = view.view(point.at);
                let hint = point.hint.as_ref()?;
                (p[2] >= view.near).then(|| (view.screen(p), point.radius.max(4.0), hint.clone()))
            })
            .collect();
        let labels = self
            .labels()
            .iter()
            .filter_map(|label| {
                let p = view.view(label.at);
                (p[2] >= view.near).then(|| RasterLabel {
                    at: view.screen(p),
                    text: label.text.clone(),
                    color: label.color,
                    size: label.size,
                })
            })
            .collect();
        (hints, labels)
    }
}

/// A device size as a raster reports it; a canvas is far narrower than `u32`
/// counts.
fn side(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// A convex polygon as a fan of triangles from its first corner.
fn fan(corners: &[Sample]) -> impl Iterator<Item = [Sample; 3]> + '_ {
    corners.split_first().into_iter().flat_map(|(&first, rest)| {
        rest.iter().zip(rest.iter().skip(1)).map(move |(&b, &c)| [first, b, c])
    })
}

/// How many threads share a render.
fn threads() -> usize {
    std::thread::available_parallelism().map_or(1, std::num::NonZero::get).clamp(1, 16)
}

/// Sample rows a thread takes: whole output rows (`ss` sample rows each), so
/// supersampled pixels never straddle two bands.
fn band_rows(height: usize, ss: usize) -> usize {
    let rows = height.checked_div(ss).unwrap_or(0);
    rows.div_ceil(threads()).max(1).saturating_mul(ss).max(1)
}

/// Run `work` on horizontal bands of the grid, one per thread.
fn for_bands(
    grid: &Grid,
    ss: usize,
    color: &mut [[f32; 4]],
    depth: &mut [f32],
    work: impl Fn(&mut Band) + Sync,
) {
    let per = band_rows(grid.height, ss);
    let width = grid.width;
    std::thread::scope(|scope| {
        let mut color = color;
        let mut depth = depth;
        let mut y0 = 0;
        let work = &work;
        while y0 < grid.height {
            let y1 = y0.saturating_add(per).min(grid.height);
            let len = y1.saturating_sub(y0).saturating_mul(width);
            let (Some((band_color, rest_color)), Some((band_depth, rest_depth))) =
                (color.split_at_mut_checked(len), depth.split_at_mut_checked(len))
            else {
                break;
            };
            color = rest_color;
            depth = rest_depth;
            let mut band = Band { y0, y1, width, color: band_color, depth: band_depth };
            scope.spawn(move || work(&mut band));
            y0 = y1;
        }
    });
}

/// A face as lit view-space vertices, clipped to the near plane; `None` when
/// it is behind the camera or turned away.
fn shade_face(face: &Face, view: &View, rgba: &dyn Fn(Color) -> [f32; 4]) -> Option<Vec<Vertex>> {
    let corners = face.corners();
    let centroid =
        scale(corners.iter().fold([0.0; 3], |acc, v| add(acc, *v)), 1.0 / count(corners.len()));
    let toward_eye = dot(face.normal, sub(view.eye, centroid));
    if !face.two_sided && toward_eye < 0.0 {
        return None;
    }
    // The lit side of a two-sided surface is the one the camera sees.
    let flip = if toward_eye < 0.0 { -1.0 } else { 1.0 };
    let mut base = rgba(face.color);
    // Without the GPU's refraction, glass is a see-through tint.
    base[3] *= face.material.glass.mul_add(-0.7, 1.0);
    let glow = face.material.glow;
    let polygon: Vec<Vertex> = corners
        .iter()
        .enumerate()
        .map(|(i, corner)| {
            let normal =
                face.normals.and_then(|normals| normals.get(i).copied()).unwrap_or(face.normal);
            Vertex { p: view.view(*corner), c: lit(base, scale(normal, flip), view.light, glow) }
        })
        .collect();
    let clipped = clip_near(&polygon, view.near);
    (clipped.len() >= 3).then_some(clipped)
}

/// A soft studio light: sky-to-ground ambient, a key light from the upper
/// left, and a dim fill from the opposite side. Lit in linear light.
fn lit(base: [f32; 4], normal: V3, key: V3, glow: f32) -> [f32; 4] {
    let n = normalize(normal);
    let sky = 0.5_f32.mul_add(n[1], 0.5);
    let ambient = 0.2_f32.mul_add(sky, 0.34);
    let key_light = dot(n, key).max(0.0) * 0.6;
    let fill = dot(n, scale(key, -1.0)).max(0.0) * 0.14;
    let light = ambient + key_light + fill + glow;
    let mut out = base;
    for channel in out.iter_mut().take(3) {
        *channel = to_srgb((to_linear(*channel) * light).min(1.0));
    }
    out
}

fn to_linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn to_srgb(c: f32) -> f32 {
    if c <= 0.003_130_8 { c * 12.92 } else { 1.055_f32.mul_add(c.powf(1.0 / 2.4), -0.055) }
}

/// Sutherland–Hodgman against `z = near`: keep what is in front.
fn clip_near(polygon: &[Vertex], near: f32) -> Vec<Vertex> {
    if polygon.iter().all(|v| v.p[2] >= near) {
        return polygon.to_vec();
    }
    let mut out = Vec::with_capacity(polygon.len().saturating_add(2));
    for (&a, &b) in polygon.iter().zip(polygon.iter().cycle().skip(1)) {
        let (a_in, b_in) = (a.p[2] >= near, b.p[2] >= near);
        if a_in {
            out.push(a);
        }
        if a_in != b_in {
            let t = (near - a.p[2]) / (b.p[2] - a.p[2]);
            out.push(Vertex { p: add(a.p, scale(sub(b.p, a.p), t)), c: lerp4(a.c, b.c, t) });
        }
    }
    out
}

fn lerp4(from: [f32; 4], to: [f32; 4], t: f32) -> [f32; 4] {
    let mut out = from;
    for (value, end) in out.iter_mut().zip(to) {
        *value = (end - *value).mul_add(t, *value);
    }
    out
}

/// A vertex in sample space: x, y, view depth, color.
#[derive(Clone, Copy)]
struct Sample {
    x: f32,
    y: f32,
    z: f32,
    c: [f32; 4],
}

fn to_sample(grid: &Grid, view: &View, v: &Vertex) -> Sample {
    let [x, y] = view.screen(v.p);
    Sample { x: x * grid.k, y: y * grid.k, z: v.p[2], c: v.c }
}

/// Twice the signed area of the triangle `origin`, `p`, `q`; its sign says
/// which way the triangle winds.
#[expect(
    clippy::suboptimal_flops,
    reason = "triangles sharing an edge must get exact negatives here, so a sample on the edge \
              lands in one of them; a fused multiply-add rounds the two differently and leaves \
              cracks"
)]
fn edge(origin: [f32; 2], p: [f32; 2], q: [f32; 2]) -> f32 {
    (p[0] - origin[0]) * (q[1] - origin[1]) - (p[1] - origin[1]) * (q[0] - origin[0])
}

/// The whole samples `low..=high` touches along one axis, within
/// `start..end`; `None` when it touches none of them.
fn span(low: f32, high: f32, start: usize, end: usize) -> Option<RangeInclusive<usize>> {
    let first = whole(low.floor().max(count(start)));
    let high = high.ceil();
    if high < 0.0 {
        return None;
    }
    let last = whole(high).min(end.checked_sub(1)?);
    (first <= last).then_some(first..=last)
}

/// Rasterize one triangle into a band with perspective-correct depth and
/// color. `opaque` writes depth and replaces color; otherwise it blends.
fn fill_triangle(band: &mut Band, tri: [Sample; 3], opaque: bool, bias: f32) {
    let [a, b, c] = tri;
    let (at_a, at_b, at_c) = ([a.x, a.y], [b.x, b.y], [c.x, c.y]);
    let area = edge(at_a, at_b, at_c);
    if area.abs() < 1e-8 {
        return;
    }
    let (Some(rows), Some(cols)) = (
        span(a.y.min(b.y).min(c.y), a.y.max(b.y).max(c.y), band.y0, band.y1),
        span(a.x.min(b.x).min(c.x), a.x.max(b.x).max(c.x), 0, band.width),
    ) else {
        return;
    };
    let inv_area = 1.0 / area;
    let (inv_za, inv_zb, inv_zc) = (1.0 / a.z, 1.0 / b.z, 1.0 / c.z);
    for row in rows {
        let py = count(row) + 0.5;
        let start = row.saturating_sub(band.y0).saturating_mul(band.width);
        for col in cols.clone() {
            let here = [count(col) + 0.5, py];
            let w0 = edge(here, at_b, at_c) * inv_area;
            let w1 = edge(here, at_c, at_a) * inv_area;
            let w2 = 1.0 - w0 - w1;
            if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                continue;
            }
            let (p0, p1, p2) = (w0 * inv_za, w1 * inv_zb, w2 * inv_zc);
            let one_over_z = p0 + p1 + p2;
            let depth = 1.0 / one_over_z - bias;
            let index = start.saturating_add(col);
            let (Some(nearest), Some(sample)) =
                (band.depth.get_mut(index), band.color.get_mut(index))
            else {
                continue;
            };
            if depth >= *nearest {
                continue;
            }
            let mut color = [0.0_f32; 4];
            for (((out, from_a), from_b), from_c) in color.iter_mut().zip(a.c).zip(b.c).zip(c.c) {
                *out = p0.mul_add(from_a, p1.mul_add(from_b, p2 * from_c)) / one_over_z;
            }
            if opaque {
                *nearest = depth;
                color[3] = 1.0;
                *sample = color;
            } else {
                blend(sample, color);
            }
        }
    }
}

/// Straight color over a premultiplied sample.
fn blend(under: &mut [f32; 4], over: [f32; 4]) {
    let [red, green, blue, alpha] = over;
    let alpha = alpha.clamp(0.0, 1.0);
    let keep = 1.0 - alpha;
    for (channel, value) in under.iter_mut().zip([red, green, blue]) {
        *channel = value.mul_add(alpha, *channel * keep);
    }
    under[3] = under[3].mul_add(keep, alpha);
}

/// Darken samples on a silhouette or fold, where depth jumps between
/// neighbours, so shapes read clearly whatever the lighting.
fn outline(grid: &Grid, ss: usize, color: &mut [[f32; 4]], depth: &[f32]) {
    let (w, h) = (grid.width, grid.height);
    let per = band_rows(h, ss);
    // The depths of sample row `y`; `None` past the grid.
    let row = move |y: usize| depth.get(y.checked_mul(w)?..)?.get(..w);
    std::thread::scope(|scope| {
        let mut rest = color;
        let mut y0 = 0;
        while y0 < h {
            let y1 = y0.saturating_add(per).min(h);
            let Some((band, tail)) =
                rest.split_at_mut_checked(y1.saturating_sub(y0).saturating_mul(w))
            else {
                break;
            };
            rest = tail;
            scope.spawn(move || {
                for (y, out) in (y0..y1).zip(band.chunks_exact_mut(w.max(1))) {
                    let Some(here) = row(y) else {
                        continue;
                    };
                    let above = y.checked_sub(1).and_then(row);
                    let below = row(y.saturating_add(1));
                    for (x, (sample, &z)) in out.iter_mut().zip(here).enumerate() {
                        if !z.is_finite() {
                            continue;
                        }
                        let jump = |other: Option<&f32>| {
                            other.is_some_and(|&other| {
                                !other.is_finite() || (other - z).abs() > EDGE_DEPTH * z.min(other)
                            })
                        };
                        let edge = jump(here.get(x.saturating_add(1)))
                            || jump(x.checked_sub(1).and_then(|left| here.get(left)))
                            || jump(below.and_then(|line| line.get(x)))
                            || jump(above.and_then(|line| line.get(x)));
                        if edge {
                            for channel in sample.iter_mut().take(3) {
                                *channel *= EDGE_SHADE;
                            }
                        }
                    }
                }
            });
            y0 = y1;
        }
    });
}

/// A scene line as two triangles in sample space, in its resolved `color`.
fn line_tris(grid: &Grid, view: &View, line: &Line3, color: [f32; 4], out: &mut Vec<[Sample; 3]>) {
    let (mut a, mut b) = (view.view(line.a), view.view(line.b));
    if a[2] < view.near && b[2] < view.near {
        return;
    }
    // Clip the part behind the near plane.
    if a[2] < view.near || b[2] < view.near {
        let t = (view.near - a[2]) / (b[2] - a[2]);
        let cut = add(a, scale(sub(b, a), t));
        if a[2] < view.near {
            a = cut;
        } else {
            b = cut;
        }
    }
    let to = |p: V3| {
        let [x, y] = view.screen(p);
        (x * grid.k, y * grid.k, p[2])
    };
    let (ax, ay, az) = to(a);
    let (bx, by, bz) = to(b);
    let (dx, dy) = (bx - ax, by - ay);
    let len = dx.hypot(dy);
    if len < 1e-4 {
        return;
    }
    let half = (line.width * grid.k).max(1.0) / 2.0;
    let (nx, ny) = (-dy / len * half, dx / len * half);
    let corner = |x: f32, y: f32, z: f32| Sample { x, y, z, c: color };
    let quad = [
        corner(ax + nx, ay + ny, az),
        corner(bx + nx, by + ny, bz),
        corner(bx - nx, by - ny, bz),
        corner(ax - nx, ay - ny, az),
    ];
    out.push([quad[0], quad[1], quad[2]]);
    out.push([quad[0], quad[2], quad[3]]);
}

/// A round dot, depth-tested at its center.
fn draw_dot(band: &mut Band, dot: &Dot) {
    let (Some(rows), Some(cols)) = (
        span(dot.y - dot.r, dot.y + dot.r, band.y0, band.y1),
        span(dot.x - dot.r, dot.x + dot.r, 0, band.width),
    ) else {
        return;
    };
    for y in rows {
        let start = y.saturating_sub(band.y0).saturating_mul(band.width);
        for x in cols.clone() {
            let (px, py) = (count(x) + 0.5 - dot.x, count(y) + 0.5 - dot.y);
            if px.mul_add(px, py * py) > dot.r * dot.r {
                continue;
            }
            let index = start.saturating_add(x);
            if let (Some(nearest), Some(sample)) =
                (band.depth.get(index), band.color.get_mut(index))
                && dot.z < *nearest
            {
                blend(sample, dot.c);
            }
        }
    }
}

/// Average each pixel's samples and hand back straight-alpha BGRA.
fn resolve(grid: &Grid, color: &[[f32; 4]], width: usize, height: usize, ss: usize) -> Vec<u8> {
    let mut out = vec![0_u8; width.saturating_mul(height).saturating_mul(4)];
    let per_pixel = count(ss.saturating_mul(ss));
    let per = height.div_ceil(threads()).max(1);
    let row_bytes = width.saturating_mul(4).max(1);
    std::thread::scope(|scope| {
        for (y0, rows) in
            (0_usize..).step_by(per).zip(out.chunks_mut(per.saturating_mul(row_bytes)))
        {
            scope.spawn(move || {
                for (y, row) in (y0..).zip(rows.chunks_mut(row_bytes)) {
                    for (x, pixel) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                        let mut sum = [0.0_f32; 4];
                        for sy in 0..ss {
                            let base = y
                                .saturating_mul(ss)
                                .saturating_add(sy)
                                .saturating_mul(grid.width)
                                .saturating_add(x.saturating_mul(ss));
                            let samples = color.get(base..base.saturating_add(ss));
                            for sample in samples.unwrap_or_default() {
                                for (total, value) in sum.iter_mut().zip(sample) {
                                    *total += value;
                                }
                            }
                        }
                        let [red, green, blue, alpha] = sum.map(|total| total / per_pixel);
                        if alpha <= 0.0 {
                            continue;
                        }
                        let straight = |channel: f32| {
                            byte(((channel / alpha).clamp(0.0, 1.0) * 255.0).round())
                        };
                        *pixel = [
                            straight(blue),
                            straight(green),
                            straight(red),
                            byte((alpha.clamp(0.0, 1.0) * 255.0).round()),
                        ];
                    }
                }
            });
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::Token;

    fn camera() -> Camera {
        Camera { yaw: 0.0, pitch: 0.0, zoom: 1.0 }
    }

    fn colors(color: Color) -> [f32; 4] {
        match color.token {
            Token::Chart(0) => [1.0, 0.0, 0.0, 1.0],
            Token::Chart(1) => [0.0, 0.0, 1.0, 1.0],
            _ => [0.5, 0.5, 0.5, 1.0],
        }
    }

    fn pixel(raster: &Raster, x: u32, y: u32) -> [u8; 4] {
        let index = usize::try_from(y.saturating_mul(raster.width).saturating_add(x)).unwrap();
        let [blue, green, red, alpha] = raster.bgra.as_chunks::<4>().0[index];
        [red, green, blue, alpha]
    }

    /// A small red square in front of a large blue one: whatever order the
    /// faces come in, the near one covers the far one where they overlap.
    #[test]
    fn nearer_surfaces_hide_farther_ones_whatever_their_order() {
        for near_first in [true, false] {
            let mut s = Scene::default();
            let far = [[-2., -2., -1.], [2., -2., -1.], [2., 2., -1.], [-2., 2., -1.]];
            let near = [[-0.5, -0.5, 1.], [0.5, -0.5, 1.], [0.5, 0.5, 1.], [-0.5, 0.5, 1.]];
            let faces = [(near, 0), (far, 1)];
            let order: Vec<_> =
                if near_first { faces.to_vec() } else { faces.iter().rev().copied().collect() };
            for (quad, chart) in order {
                s.face(&quad, Color::token(Token::Chart(chart)), Some([0., 0., 1.]), true).unwrap();
            }
            let raster = s.render(camera(), 200.0, 200.0, 1.0, Quality::Draft, &colors);
            let center = pixel(&raster, 100, 100);
            assert!(center[0] > 100 && center[2] < 60, "near red wins: {center:?}");
            let corner = pixel(&raster, 100, 30);
            assert!(corner[2] > 100 && corner[0] < 60, "far blue shows around it: {corner:?}");
            // Nothing outside the geometry: transparent.
            assert_eq!(pixel(&raster, 1, 1)[3], 0);
        }
    }

    #[test]
    fn faces_crossing_the_camera_are_clipped_not_dropped() {
        let mut s = Scene::default();
        // A long floor that runs from far away to behind the camera.
        let floor = [[-1., -1., -40.], [1., -1., -40.], [1., -1., 40.], [-1., -1., 40.]];
        s.face(&floor, Color::token(Token::Chart(0)), Some([0., 1., 0.]), true).unwrap();
        // A marker the camera frames itself on, near the middle.
        s.face(
            &[[-0.2, 0., 0.], [0.2, 0., 0.], [0., 0.3, 0.]],
            Color::token(Token::Chart(1)),
            Some([0., 0., 1.]),
            true,
        )
        .unwrap();
        let zoomed = Camera { zoom: 6.0, ..camera() };
        let raster = s.render(zoomed, 200.0, 200.0, 1.0, Quality::Draft, &colors);
        let drawn = raster.bgra.as_chunks::<4>().0.iter().filter(|p| p[3] > 0).count();
        assert!(drawn > 0, "the floor still shows");
    }

    #[test]
    fn final_quality_supersamples_within_its_budget() {
        let mut s = Scene::default();
        s.face(
            &[[-1., -1., 0.], [1., -1., 0.], [0., 1., 0.]],
            Color::token(Token::Chart(0)),
            Some([0., 0., 1.]),
            true,
        )
        .unwrap();
        let draft = s.render(camera(), 100.0, 80.0, 2.0, Quality::Draft, &colors);
        let fine = s.render(camera(), 100.0, 80.0, 2.0, Quality::Final, &colors);
        assert_eq!((draft.width, draft.height), (200, 160));
        assert_eq!((fine.width, fine.height), (200, 160));
        // Supersampled edges have partial coverage; one sample cannot.
        let partial = |r: &Raster| r.bgra.as_chunks::<4>().0.iter().any(|p| p[3] > 0 && p[3] < 255);
        assert!(!partial(&draft));
        assert!(partial(&fine));
    }
}

#[cfg(test)]
mod timing {
    use super::*;
    use crate::style::Token;

    /// Not an assertion: prints how long a furnished room takes, so a change
    /// that slows rendering shows up in `cargo test -- --nocapture timing`.
    #[test]
    fn timing_a_furnished_room() {
        let mut s = Scene::default();
        let quad = |s: &mut Scene, q: [V3; 4], c: u8| {
            s.face(&q, Color::token(Token::Chart(c)), None, true).unwrap();
        };
        quad(&mut s, [[-4., 0., -3.], [4., 0., -3.], [4., 0., 3.], [-4., 0., 3.]], 0);
        quad(&mut s, [[-4., 0., -3.], [4., 0., -3.], [4., 3., -3.], [-4., 3., -3.]], 1);
        quad(&mut s, [[-4., 0., -3.], [-4., 0., 3.], [-4., 3., 3.], [-4., 3., -3.]], 2);
        // Spheres stand in for plants and lamps: smooth, many faces.
        for k in 0..8_u8 {
            let at = [f32::from(k).mul_add(0.8, -3.0), 0.6, 1.0];
            let (lon, lat) = (36_u16, 18_u16);
            for j in 0..lat {
                for i in 0..lon {
                    let p = |i: u16, j: u16| {
                        let t = std::f32::consts::TAU * f32::from(i) / f32::from(lon);
                        let f = std::f32::consts::PI * f32::from(j) / f32::from(lat);
                        [
                            (0.4 * f.sin()).mul_add(t.cos(), at[0]),
                            0.4_f32.mul_add(f.cos(), at[1]),
                            (0.4 * f.sin()).mul_add(t.sin(), at[2]),
                        ]
                    };
                    let q = [p(i, j), p(i + 1, j), p(i + 1, j + 1), p(i, j + 1)];
                    let n: Vec<V3> = q.iter().map(|c| sub(*c, at)).collect();
                    s.smooth_face(&q, &n, Color::token(Token::Chart(3))).unwrap();
                }
            }
        }
        let colors = |_: Color| [0.6, 0.6, 0.6, 1.0];
        let camera = Camera { yaw: 0.7, pitch: 0.4, zoom: 1.0 };
        for quality in [Quality::Draft, Quality::Final] {
            let start = std::time::Instant::now();
            let raster = s.render(camera, 736.0, 420.0, 2.0, quality, &colors);
            println!(
                "{quality:?}: {} faces, {}x{} in {:?}",
                s.face_count(),
                raster.width,
                raster.height,
                start.elapsed()
            );
        }
    }
}

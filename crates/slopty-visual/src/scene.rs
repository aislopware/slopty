// Derived from zeron (MIT, github.com/zeronsh/zeron @ 3b2f780), see THIRD_PARTY_NOTICES.md
//! `ui.scene` 3D.
//!
//! The draw function gets a scene object whose methods add world-space
//! geometry (y up). Slopty frames the camera on the geometry,
//! orbits it on drag, shades faces with one soft light, and projects
//! everything to depth-sorted 2D polygons that the desktop paints like any
//! other vector shape, so a 3D view stays transparent over glass and uses the
//! theme's colors.

use std::f32::consts::{PI, TAU};
use std::sync::Arc;

use mlua::{Function, Table, UserData, UserDataMethods, Value};

use crate::draw::lua_error;
use crate::node::{parse_color, value_text};
use crate::num::{count, integer, narrow, whole};
use crate::style::{Color, TextSize, Token};

/// Faces a scene may draw (glTF models bring many).
const MAX_FACES: usize = 500_000;
const MAX_ITEMS: usize = 20_000;

/// A point or a direction in 3D: x, y (up), z.
pub type V3 = [f32; 3];

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Face {
    pub v: [V3; 4],
    /// 3 or 4 corners.
    pub n: u8,
    pub normal: V3,
    /// Per-corner normals of a curved solid (spheres, cylinder sides), so it
    /// shades smoothly instead of faceted.
    pub normals: Option<[V3; 4]>,
    pub color: Color,
    /// Closed solids cull faces turned away from the camera; open surfaces
    /// and meshes light both sides.
    pub two_sided: bool,
    pub material: Material,
    /// Texture coordinates per corner, for a textured face.
    pub uvs: Option<[[f32; 2]; 4]>,
    /// Index into the scene's textures.
    pub texture: Option<u32>,
}

impl Face {
    /// Its 3 or 4 corners.
    pub(crate) fn corners(&self) -> &[V3] {
        first(&self.v, self.n)
    }
}

/// The first `n` of `items`, or all of them if there are fewer.
fn first<T>(items: &[T], n: u8) -> &[T] {
    items.get(..usize::from(n)).unwrap_or(items)
}

/// A texture's index, as faces keep it; a scene's textures are far fewer
/// than `u32` counts.
fn index(i: usize) -> u32 {
    u32::try_from(i).unwrap_or(u32::MAX)
}

/// An image a scene samples: an asset still encoded (PNG, JPEG, HDR), or
/// pixels a model brought already decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextureData {
    /// An image file's bytes: PNG, JPEG, WebP, or Radiance HDR.
    Encoded(Arc<[u8]>),
    /// sRGB RGBA pixels, straight alpha, top row first.
    Rgba {
        /// Width in pixels.
        width: u32,
        /// Height in pixels.
        height: u32,
        /// Four bytes a pixel.
        pixels: Arc<[u8]>,
    },
}

/// One texture a scene uses; `key` names it stably across frames (so a
/// renderer decodes and uploads it once).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextureSource {
    /// `asset:<name>` or `model:<name>:<image>`.
    pub key: String,
    /// The image.
    pub data: TextureData,
}

/// An environment map: an equirectangular image the scene reflects and
/// glass shows when nothing stands behind it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Environment {
    /// Its index in the scene's textures.
    pub texture: u32,
    /// How bright its light is (1 as the image has it).
    pub intensity: f32,
    /// Whether the scene stands in front of the map (else the chat shows
    /// behind it, and the map only lights and reflects).
    pub background: bool,
}

/// A camera-facing image (a flare, a glint, a soft glow), drawn as added
/// light.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sprite {
    /// Where it shines.
    pub at: V3,
    /// Pixels across.
    pub size: f32,
    /// Its image's index in the scene's textures; `None` is a soft round light.
    pub texture: Option<u32>,
    /// Its tint.
    pub color: Color,
    /// How much light it adds.
    pub glow: f32,
}

/// The degrees `{x, y, z}` (or one number, about y) as a rotation, applied
/// about x, then y, then z.
fn rotation_of(value: Value) -> mlua::Result<[V3; 3]> {
    let [x, y, z] = match value {
        Value::Nil => return Ok([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]),
        Value::Integer(n) => [0.0, integer(n), 0.0],
        Value::Number(n) => [0.0, narrow(n), 0.0],
        other => v3(other, "rotate")?,
    }
    .map(f32::to_radians);
    let (sx, cx, sy, cy, sz, cz) = (x.sin(), x.cos(), y.sin(), y.cos(), z.sin(), z.cos());
    // Rows of Rz * Ry * Rx.
    Ok([
        [cz * cy, (cz * sy).mul_add(sx, -(sz * cx)), (cz * sy).mul_add(cx, sz * sx)],
        [sz * cy, (sz * sy).mul_add(sx, cz * cx), (sz * sy).mul_add(cx, -(cz * sx))],
        [-sy, cy * sx, cy * cx],
    ])
}

fn rotate(m: &[V3; 3], v: V3) -> V3 {
    [dot(m[0], v), dot(m[1], v), dot(m[2], v)]
}

/// How a surface takes light. `glass` lets light through (refracting by
/// `ior`, splitting into colors by `dispersion`); `glow` emits light of the
/// surface's own color, which blooms.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Material {
    /// 0 is a mirror finish, 1 fully matte.
    pub roughness: f32,
    /// 0 is a dielectric (plastic, wood), 1 a metal.
    pub metalness: f32,
    /// Light the surface emits, 0 to 8.
    pub glow: f32,
    /// 0 opaque to 1 clear glass.
    pub glass: f32,
    /// Glass's index of refraction, 1 to 3.
    pub ior: f32,
    /// How far glass splits light into colors, 0 to 1.
    pub dispersion: f32,
}

impl Default for Material {
    fn default() -> Self {
        Self { roughness: 0.55, metalness: 0.0, glow: 0.0, glass: 0.0, ior: 1.5, dispersion: 0.0 }
    }
}

/// A face as the GPU renderer reads it.
#[derive(Debug)]
pub struct FaceView<'a> {
    /// Its 3 or 4 corners.
    pub corners: &'a [V3],
    /// Its outward normal.
    pub normal: V3,
    /// Per-corner normals of a curved solid.
    pub normals: Option<&'a [V3]>,
    /// Its color.
    pub color: Color,
    /// Both sides are lit and drawn; else the back is culled.
    pub two_sided: bool,
    /// How it takes light.
    pub material: Material,
    /// Texture coordinates per corner.
    pub uvs: Option<&'a [[f32; 2]]>,
    /// Its texture's index in the scene's textures.
    pub texture: Option<u32>,
}

/// The framed orbit camera, for a renderer that does its own projection.
#[derive(Debug, Clone, Copy)]
pub struct CameraFrame {
    /// Where the camera is.
    pub eye: V3,
    /// The screen's right, in the world.
    pub right: V3,
    /// The screen's up, in the world.
    pub up: V3,
    /// Into the screen.
    pub forward: V3,
    /// Pixels per unit at unit depth, for the canvas the frame was made for.
    pub focal: f32,
    /// The near clipping distance.
    pub near: f32,
    /// The geometry's center.
    pub center: V3,
    /// The geometry's bounding radius.
    pub radius: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Line3 {
    pub a: V3,
    pub b: V3,
    pub color: Color,
    pub width: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Point3 {
    pub at: V3,
    pub radius: f32,
    pub color: Color,
    pub hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Label3 {
    pub at: V3,
    pub text: String,
    pub color: Color,
    pub size: TextSize,
}

/// The geometry one draw call produced.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Scene {
    faces: Vec<Face>,
    lines: Vec<Line3>,
    points: Vec<Point3>,
    labels: Vec<Label3>,
    time: f64,
    /// The material the primitive being added gives its faces.
    material: Material,
    /// The texture the primitive being added gives its faces.
    texture: Option<u32>,
    textures: Vec<TextureSource>,
    environment: Option<Environment>,
    sprites: Vec<Sprite>,
    assets: crate::assets::Assets,
}

/// The camera a scene is seen through: framed on its geometry and orbited.
pub(crate) struct View {
    pub eye: V3,
    pub right: V3,
    pub up: V3,
    pub forward: V3,
    pub focal: f32,
    pub near: f32,
    pub cx: f32,
    pub cy: f32,
    pub center: V3,
    /// The geometry's bounding radius.
    pub radius: f32,
    /// The soft key light the painter shades with, from the upper left.
    pub light: V3,
}

impl View {
    /// World to view space: x right, y up, z into the screen.
    pub(crate) fn view(&self, p: V3) -> V3 {
        let d = sub(p, self.eye);
        [dot(d, self.right), dot(d, self.up), dot(d, self.forward)]
    }

    /// View space to canvas pixels.
    pub(crate) fn screen(&self, v: V3) -> [f32; 2] {
        [self.cx + self.focal * v[0] / v[2], self.cy - self.focal * v[1] / v[2]]
    }
}

/// The orbit camera: angles in radians around the geometry's center.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Camera {
    /// The turn about the vertical axis.
    pub yaw: f32,
    /// The tilt, up from level.
    pub pitch: f32,
    /// 1 frames the geometry; more is closer.
    pub zoom: f32,
}

/// One painted primitive, in canvas pixels.
#[derive(Debug, Clone, PartialEq)]
pub enum Prim {
    /// A filled face.
    Face {
        /// Its outline.
        points: Vec<[f32; 2]>,
        /// Its color.
        color: Color,
        /// Lighting, 0 (unlit) to 1 (facing the light).
        shade: f32,
    },
    /// A line.
    Line {
        /// One end.
        a: [f32; 2],
        /// The other end.
        b: [f32; 2],
        /// Its color.
        color: Color,
        /// Its width in pixels.
        width: f32,
    },
    /// A round dot.
    Dot {
        /// Its center.
        at: [f32; 2],
        /// Its radius in pixels.
        radius: f32,
        /// Its color.
        color: Color,
    },
    /// A text label.
    Label {
        /// Where it is anchored.
        at: [f32; 2],
        /// Its text.
        text: String,
        /// Its color.
        color: Color,
        /// Its step on the type scale.
        size: TextSize,
    },
}

/// A scene drawn as 2D primitives.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Projection {
    /// Back to front; labels last.
    pub prims: Vec<Prim>,
    /// Hover hints: screen position, radius, text.
    pub hints: Vec<([f32; 2], f32, String)>,
}

pub(crate) fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
pub(crate) fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
pub(crate) fn scale(a: V3, s: f32) -> V3 {
    [a[0] * s, a[1] * s, a[2] * s]
}
pub(crate) fn dot(a: V3, b: V3) -> f32 {
    a[0].mul_add(b[0], a[1].mul_add(b[1], a[2] * b[2]))
}
fn cross(a: V3, b: V3) -> V3 {
    [
        a[1].mul_add(b[2], -(a[2] * b[1])),
        a[2].mul_add(b[0], -(a[0] * b[2])),
        a[0].mul_add(b[1], -(a[1] * b[0])),
    ]
}
pub(crate) fn normalize(a: V3) -> V3 {
    let len = dot(a, a).sqrt();
    if len < 1e-9 { [0.0, 1.0, 0.0] } else { scale(a, 1.0 / len) }
}

impl Scene {
    /// An empty scene drawn at `time` seconds.
    pub fn new(time: f64) -> Self {
        Self { time, ..Default::default() }
    }

    /// A scene whose draw may use `assets` (textures, models).
    pub fn with_assets(time: f64, assets: crate::assets::Assets) -> Self {
        Self { time, assets, ..Default::default() }
    }

    /// The texture keyed `key`, registered once; its index.
    fn texture_index(&mut self, key: String, data: impl FnOnce() -> TextureData) -> u32 {
        if let Some(i) = self.textures.iter().position(|t| t.key == key) {
            return index(i);
        }
        let added = index(self.textures.len());
        self.textures.push(TextureSource { key, data: data() });
        added
    }

    /// The asset `name` as a texture.
    fn asset_texture(&mut self, name: &str) -> mlua::Result<u32> {
        let bytes = self.assets.require(name).map_err(lua_error)?;
        Ok(self.texture_index(format!("asset:{name}"), || TextureData::Encoded(bytes)))
    }

    /// The asset `name` parsed as a glTF model, once per visual.
    fn model(&self, name: &str) -> mlua::Result<Arc<crate::model::Model>> {
        self.assets.model(name).map_err(lua_error)
    }

    /// Give the face just added its texture coordinates.
    fn uv_last(&mut self, uvs: &[[f32; 2]]) {
        if let Some(face) = self.faces.last_mut() {
            let mut out = [[0.0; 2]; 4];
            for (slot, uv) in out.iter_mut().zip(uvs) {
                *slot = *uv;
            }
            face.uvs = Some(out);
        }
    }

    /// The textures the scene's faces and sprites use, by index.
    pub fn textures(&self) -> &[TextureSource] {
        &self.textures
    }

    /// The environment map, if the scene set one.
    pub const fn environment(&self) -> Option<Environment> {
        self.environment
    }

    /// The camera-facing glows.
    pub fn sprites(&self) -> &[Sprite] {
        &self.sprites
    }

    /// The faces, in the order added.
    pub(crate) fn faces(&self) -> &[Face] {
        &self.faces
    }

    /// The lines, in the order added.
    pub(crate) fn lines(&self) -> &[Line3] {
        &self.lines
    }

    /// The points, in the order added.
    pub(crate) fn points(&self) -> &[Point3] {
        &self.points
    }

    /// The labels, in the order added.
    pub(crate) fn labels(&self) -> &[Label3] {
        &self.labels
    }

    pub(crate) fn face(
        &mut self,
        corners: &[V3],
        color: Color,
        outward: Option<V3>,
        two_sided: bool,
    ) -> mlua::Result<()> {
        if self.faces.len() >= MAX_FACES {
            return Err(lua_error(format!("a scene draws at most {MAX_FACES} faces")));
        }
        if corners.iter().flatten().any(|c| !c.is_finite()) {
            return Err(lua_error("a scene vertex is NaN or infinite"));
        }
        let (mut normal, v, n) = match *corners {
            [a, b, c] => (normalize(cross(sub(b, a), sub(c, a))), [a, b, c, [0.0; 3]], 3),
            // A non-planar quad: average both diagonals' normals.
            [a, b, c, d] => (normalize(cross(sub(c, a), sub(d, b))), [a, b, c, d], 4),
            _ => return Err(lua_error("a scene face has 3 or 4 corners")),
        };
        if let Some(outward) = outward
            && dot(normal, outward) < 0.0
        {
            normal = scale(normal, -1.0);
        }
        self.faces.push(Face {
            v,
            n,
            normal,
            normals: None,
            color,
            two_sided,
            material: self.material,
            uvs: None,
            texture: self.texture,
        });
        Ok(())
    }

    /// A face of a curved solid, shaded from the surface's own normals.
    pub(crate) fn smooth_face(
        &mut self,
        corners: &[V3],
        normals: &[V3],
        color: Color,
    ) -> mlua::Result<()> {
        let outward = normals.iter().fold([0.0; 3], |acc, n| add(acc, *n));
        self.face(corners, color, Some(outward), false)?;
        let mut n = [[0.0; 3]; 4];
        for (slot, normal) in n.iter_mut().zip(normals) {
            *slot = normalize(*normal);
        }
        if let Some(face) = self.faces.last_mut() {
            face.normals = Some(n);
        }
        Ok(())
    }

    fn polygon(
        &mut self,
        corners: &[V3],
        color: Color,
        outward: Option<V3>,
        two_sided: bool,
    ) -> mlua::Result<()> {
        match corners.len() {
            0..=2 => Ok(()),
            3 | 4 => self.face(corners, color, outward, two_sided),
            _ => {
                // A fan from the first corner.
                if let Some((&first, rest)) = corners.split_first() {
                    for (&b, &c) in rest.iter().zip(rest.iter().skip(1)) {
                        self.face(&[first, b, c], color, outward, two_sided)?;
                    }
                }
                Ok(())
            }
        }
    }

    const fn items(&self) -> usize {
        self.lines
            .len()
            .saturating_add(self.points.len())
            .saturating_add(self.labels.len())
            .saturating_add(self.sprites.len())
    }

    fn check_items(&self) -> mlua::Result<()> {
        if self.items() >= MAX_ITEMS {
            return Err(lua_error(format!(
                "a scene draws at most {MAX_ITEMS} lines, points, and labels"
            )));
        }
        Ok(())
    }

    /// How many faces the scene has.
    pub const fn face_count(&self) -> usize {
        self.faces.len()
    }

    fn bounds(&self) -> Option<(V3, V3)> {
        let mut lo = [f32::INFINITY; 3];
        let mut hi = [f32::NEG_INFINITY; 3];
        let mut grow = |p: V3| {
            for ((low, high), value) in lo.iter_mut().zip(hi.iter_mut()).zip(p) {
                *low = low.min(value);
                *high = high.max(value);
            }
        };
        for face in &self.faces {
            for v in face.corners() {
                grow(*v);
            }
        }
        for line in &self.lines {
            grow(line.a);
            grow(line.b);
        }
        for point in &self.points {
            grow(point.at);
        }
        for label in &self.labels {
            grow(label.at);
        }
        lo[0].is_finite().then_some((lo, hi))
    }

    /// The camera framed on this geometry and orbited to `camera`, for a
    /// canvas `width` by `height`.
    pub(crate) fn view_for(&self, camera: Camera, width: f32, height: f32) -> Option<View> {
        let (lo, hi) = self.bounds()?;
        let target = scale(add(lo, hi), 0.5);
        let half = scale(sub(hi, lo), 0.5);
        let radius = dot(half, half).sqrt().max(1e-3);
        let pitch = camera.pitch.clamp(-1.52, 1.52);
        // Fit the geometry's extent as the turntable sees it: the footprint's
        // radius sideways, and its height plus the tilted footprint
        // vertically. Neither depends on yaw, so a spinning scene keeps its
        // size.
        let footprint = half[0].hypot(half[2]).max(1e-3);
        let tall = half[1].mul_add(pitch.cos().abs(), footprint * pitch.sin().abs()).max(1e-3);
        let distance = radius * 4.0;
        let eye = add(
            target,
            scale(
                [pitch.cos() * camera.yaw.sin(), pitch.sin(), pitch.cos() * camera.yaw.cos()],
                distance,
            ),
        );
        let forward = normalize(sub(target, eye));
        let right = normalize(cross(forward, [0.0, 1.0, 0.0]));
        let up = cross(right, forward);
        let focal = 0.86
            * (width / 2.0 / footprint).min(height / 2.0 / tall)
            * distance
            * camera.zoom.max(0.05);
        // A soft key light from the upper left, in front of the geometry.
        let light = normalize(add(add(scale(right, -0.45), scale(up, 0.8)), scale(forward, -0.55)));
        Some(View {
            eye,
            right,
            up,
            forward,
            focal,
            near: distance * 0.02,
            cx: width / 2.0,
            cy: height / 2.0,
            center: target,
            radius,
            light,
        })
    }

    /// The camera framed on this geometry, for the GPU renderer.
    pub fn camera_frame(&self, camera: Camera, width: f32, height: f32) -> Option<CameraFrame> {
        let view = self.view_for(camera, width, height)?;
        Some(CameraFrame {
            eye: view.eye,
            right: view.right,
            up: view.up,
            forward: view.forward,
            focal: view.focal,
            near: view.near,
            center: view.center,
            radius: view.radius,
        })
    }

    /// The faces, as a renderer reads them.
    pub fn face_views(&self) -> impl Iterator<Item = FaceView<'_>> {
        self.faces.iter().map(|f| FaceView {
            corners: f.corners(),
            normal: f.normal,
            normals: f.normals.as_ref().map(|n| first(n, f.n)),
            color: f.color,
            two_sided: f.two_sided,
            material: f.material,
            uvs: f.uvs.as_ref().map(|uv| first(uv, f.n)),
            texture: f.texture,
        })
    }

    /// Lines: ends, color, width in pixels.
    pub fn line_views(&self) -> impl Iterator<Item = (V3, V3, Color, f32)> + '_ {
        self.lines.iter().map(|l| (l.a, l.b, l.color, l.width))
    }

    /// Points: center, radius in pixels, color.
    pub fn point_views(&self) -> impl Iterator<Item = (V3, f32, Color)> + '_ {
        self.points.iter().map(|p| (p.at, p.radius, p.color))
    }

    /// Frame the geometry, orbit to `camera`, light, cull, and depth-sort.
    pub fn project(&self, camera: Camera, width: f32, height: f32) -> Projection {
        let Some(setup) = self.view_for(camera, width, height) else {
            return Projection::default();
        };
        let (eye, near, radius, light) = (setup.eye, setup.near, setup.radius, setup.light);
        let view = |p: V3| setup.view(p);
        let screen = |v: V3| setup.screen(v);

        let mut sorted: Vec<(f32, Prim)> =
            Vec::with_capacity(self.faces.len().saturating_add(self.items()));
        for face in &self.faces {
            let corners = face.corners();
            let viewed: Vec<V3> = corners.iter().map(|v| view(*v)).collect();
            if viewed.iter().any(|v| v[2] < near) {
                continue;
            }
            let centroid = scale(
                corners.iter().fold([0.0; 3], |acc, v| add(acc, *v)),
                1.0 / count(corners.len()),
            );
            let toward_eye = dot(face.normal, sub(eye, centroid));
            if !face.two_sided && toward_eye < 0.0 {
                continue;
            }
            let mut normal = face.normal;
            if toward_eye < 0.0 {
                normal = scale(normal, -1.0);
            }
            let lambert = dot(normal, light).max(0.0);
            let depth = viewed.iter().map(|v| v[2]).sum::<f32>() / count(viewed.len());
            sorted.push((
                depth,
                Prim::Face {
                    points: viewed.iter().map(|v| screen(*v)).collect(),
                    color: face.color,
                    shade: 0.65_f32.mul_add(lambert, 0.35),
                },
            ));
        }
        for line in &self.lines {
            let (a, b) = (view(line.a), view(line.b));
            if a[2] < near || b[2] < near {
                continue;
            }
            sorted.push((
                radius.mul_add(-1e-3, f32::midpoint(a[2], b[2])),
                Prim::Line { a: screen(a), b: screen(b), color: line.color, width: line.width },
            ));
        }
        let mut hints = Vec::new();
        for point in &self.points {
            let v = view(point.at);
            if v[2] < near {
                continue;
            }
            let at = screen(v);
            if let Some(hint) = &point.hint {
                hints.push((at, point.radius.max(4.0), hint.clone()));
            }
            sorted.push((
                radius.mul_add(-2e-3, v[2]),
                Prim::Dot { at, radius: point.radius, color: point.color },
            ));
        }
        sorted.sort_by(|a, b| b.0.total_cmp(&a.0));
        let mut prims: Vec<Prim> = sorted.into_iter().map(|(_, p)| p).collect();
        for label in &self.labels {
            let v = view(label.at);
            if v[2] < near {
                continue;
            }
            prims.push(Prim::Label {
                at: screen(v),
                text: label.text.clone(),
                color: label.color,
                size: label.size,
            });
        }
        Projection { prims, hints }
    }
}

fn v3(value: Value, what: &str) -> mlua::Result<V3> {
    match value {
        Value::Table(t) => {
            let get = |i: usize, key: &str| -> mlua::Result<f32> {
                match t.raw_get::<Option<f64>>(i)? {
                    Some(n) => Ok(narrow(n)),
                    None => Ok(narrow(t.raw_get::<Option<f64>>(key)?.unwrap_or(0.0))),
                }
            };
            Ok([get(1, "x")?, get(2, "y")?, get(3, "z")?])
        }
        Value::Vector(v) => Ok([v.x(), v.y(), v.z()]),
        _ => Err(lua_error(format!("{what} must be a {{x, y, z}} list"))),
    }
}

/// A primitive's material options: roughness, metal, glow, glass (true or
/// 0-1), ior, dispersion.
fn material_of(options: &Table) -> mlua::Result<Material> {
    material_over(Material::default(), options)
}

/// `base` with whichever material options `options` sets.
fn material_over(mut m: Material, options: &Table) -> mlua::Result<Material> {
    let number = |key: &str| options.raw_get::<Option<f32>>(key);
    if let Some(v) = number("roughness")? {
        m.roughness = v.clamp(0.02, 1.0);
    }
    if let Some(v) = number("metal")? {
        m.metalness = v.clamp(0.0, 1.0);
    }
    if let Some(v) = number("glow")? {
        m.glow = v.clamp(0.0, 8.0);
    }
    match options.raw_get::<Value>("glass")? {
        Value::Boolean(true) => {
            m.glass = 1.0;
            m.roughness = m.roughness.min(0.08);
        }
        Value::Integer(n) => m.glass = integer(n).clamp(0.0, 1.0),
        Value::Number(n) => m.glass = narrow(n).clamp(0.0, 1.0),
        _ => {}
    }
    if let Some(v) = number("ior")? {
        m.ior = v.clamp(1.0, 3.0);
    }
    if let Some(v) = number("dispersion")? {
        m.dispersion = v.clamp(0.0, 1.0);
    }
    Ok(m)
}

/// A primitive's `texture = "asset name"`, registered with the scene.
fn texture_of(scene: &mut Scene, options: &Table) -> mlua::Result<Option<u32>> {
    match options.raw_get::<Option<String>>("texture")? {
        Some(name) => scene.asset_texture(&name).map(Some),
        None => Ok(None),
    }
}

fn color_of(options: &Table, default: Token) -> mlua::Result<Color> {
    Ok(scene_color(options.raw_get::<Value>("color")?)?.unwrap_or_else(|| Color::token(default)))
}

/// A scene color: a theme token like everywhere else, or, for physical
/// materials, `"#rrggbb"`.
pub(crate) fn scene_color(value: Value) -> mlua::Result<Option<Color>> {
    if let Value::String(s) = &value {
        let text = s.to_string_lossy();
        if let Some(hex) = text.strip_prefix('#') {
            let rgb = match hex.len() {
                6 => u32::from_str_radix(hex, 16).ok(),
                3 => u32::from_str_radix(hex, 16).ok().map(|v| {
                    // Each digit doubled: `f` is `ff`.
                    let double = |digit: u32| (digit << 4) | digit;
                    let (r, g, b) = ((v >> 8) & 0xf, (v >> 4) & 0xf, v & 0xf);
                    (double(r) << 16) | (double(g) << 8) | double(b)
                }),
                _ => None,
            }
            .ok_or_else(|| lua_error(format!("'{text}' is not a #rrggbb color")))?;
            return Ok(Some(Color::token(Token::Rgb(rgb))));
        }
    }
    parse_color(value).map_err(|e| lua_error(e.to_string()))
}

/// A ramp through `colors` (a list of tokens) at `t` in 0..1.
fn ramp(colors: &[Color], t: f32) -> Color {
    match colors {
        [] => Color::token(Token::Accent),
        [only] => *only,
        [first, ..] => {
            let gaps = colors.len().saturating_sub(1);
            let scaled = t.clamp(0.0, 1.0) * count(gaps);
            let i = whole(scaled.floor()).min(gaps.saturating_sub(1));
            match colors.get(i..) {
                Some([from, to, ..]) => Color {
                    token: from.token,
                    mix: Some((to.token, scaled - count(i))),
                    alpha: from.alpha,
                },
                _ => *first,
            }
        }
    }
}

fn colors_of(options: &Table) -> mlua::Result<Vec<Color>> {
    let Some(list) = options.raw_get::<Option<Table>>("colors")? else {
        return Ok(vec![color_of(options, Token::Accent)?]);
    };
    let mut out = Vec::new();
    for value in list.sequence_values::<Value>() {
        if let Some(color) = parse_color(value?).map_err(|e| lua_error(e.to_string()))? {
            out.push(color);
        }
    }
    Ok(out)
}

impl UserData for Scene {
    fn add_fields<F: mlua::UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("time", |_, s| Ok(s.time));
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method_mut("box", |_, s, o: Table| {
            s.material = material_of(&o)?;
            s.texture = texture_of(s, &o)?;
            let at = v3(o.raw_get("at")?, "box at")?;
            let size = match o.raw_get::<Value>("size")? {
                Value::Nil => [1.0; 3],
                Value::Integer(n) => [integer(n); 3],
                Value::Number(n) => [narrow(n); 3],
                other => v3(other, "box size")?,
            };
            let color = color_of(&o, Token::Accent)?;
            let h = scale(size, 0.5);
            let c = |sx: f32, sy: f32, sz: f32| {
                [sx.mul_add(h[0], at[0]), sy.mul_add(h[1], at[1]), sz.mul_add(h[2], at[2])]
            };
            let quads = [
                ([c(-1., -1., 1.), c(1., -1., 1.), c(1., 1., 1.), c(-1., 1., 1.)], [0., 0., 1.]),
                (
                    [c(1., -1., -1.), c(-1., -1., -1.), c(-1., 1., -1.), c(1., 1., -1.)],
                    [0., 0., -1.],
                ),
                ([c(1., -1., 1.), c(1., -1., -1.), c(1., 1., -1.), c(1., 1., 1.)], [1., 0., 0.]),
                (
                    [c(-1., -1., -1.), c(-1., -1., 1.), c(-1., 1., 1.), c(-1., 1., -1.)],
                    [-1., 0., 0.],
                ),
                ([c(-1., 1., 1.), c(1., 1., 1.), c(1., 1., -1.), c(-1., 1., -1.)], [0., 1., 0.]),
                (
                    [c(-1., -1., -1.), c(1., -1., -1.), c(1., -1., 1.), c(-1., -1., 1.)],
                    [0., -1., 0.],
                ),
            ];
            for (quad, outward) in quads {
                s.face(&quad, color, Some(outward), false)?;
                s.uv_last(&[[0., 1.], [1., 1.], [1., 0.], [0., 0.]]);
            }
            Ok(())
        });
        methods.add_method_mut("sphere", |_, s, o: Table| {
            s.material = material_of(&o)?;
            s.texture = texture_of(s, &o)?;
            let at = v3(o.raw_get("at")?, "sphere at")?;
            let r = o.raw_get::<Option<f32>>("radius")?.unwrap_or(0.5).abs();
            let color = color_of(&o, Token::Accent)?;
            let detail = o.raw_get::<Option<u32>>("detail")?.unwrap_or(18).clamp(6, 48);
            let lat = u16::try_from(detail).unwrap_or(48);
            let lon = lat.saturating_mul(2);
            let p = |i: u16, j: u16| {
                let theta = TAU * f32::from(i) / f32::from(lon);
                let phi = PI * f32::from(j) / f32::from(lat);
                [
                    (r * phi.sin()).mul_add(theta.cos(), at[0]),
                    r.mul_add(phi.cos(), at[1]),
                    (r * phi.sin()).mul_add(theta.sin(), at[2]),
                ]
            };
            for j in 0..lat {
                let (j1, last) = (j.saturating_add(1), j.saturating_add(1) == lat);
                for i in 0..lon {
                    let i1 = i.saturating_add(1);
                    let quad = [p(i, j), p(i1, j), p(i1, j1), p(i, j1)];
                    let corners: Vec<V3> = if j == 0 {
                        vec![quad[0], quad[2], quad[3]]
                    } else if last {
                        vec![quad[0], quad[1], quad[2]]
                    } else {
                        quad.to_vec()
                    };
                    let normals: Vec<V3> = corners.iter().map(|c| sub(*c, at)).collect();
                    s.smooth_face(&corners, &normals, color)?;
                    let uv = |i: u16, j: u16| {
                        [f32::from(i) / f32::from(lon), f32::from(j) / f32::from(lat)]
                    };
                    let uvq = [uv(i, j), uv(i1, j), uv(i1, j1), uv(i, j1)];
                    if j == 0 {
                        s.uv_last(&[uvq[0], uvq[2], uvq[3]]);
                    } else if last {
                        s.uv_last(&[uvq[0], uvq[1], uvq[2]]);
                    } else {
                        s.uv_last(&uvq);
                    }
                }
            }
            Ok(())
        });
        methods.add_method_mut("cylinder", |_, s, o: Table| {
            s.material = material_of(&o)?;
            s.texture = texture_of(s, &o)?;
            let at = v3(o.raw_get("at")?, "cylinder at")?;
            let r = o.raw_get::<Option<f32>>("radius")?.unwrap_or(0.5).abs();
            let h = o.raw_get::<Option<f32>>("height")?.unwrap_or(1.0);
            let color = color_of(&o, Token::Accent)?;
            let detail = o.raw_get::<Option<u32>>("detail")?.unwrap_or(24).clamp(6, 64);
            let n = u16::try_from(detail).unwrap_or(64);
            let ring = |i: u16, y: f32| {
                let a = TAU * f32::from(i) / f32::from(n);
                [r.mul_add(a.cos(), at[0]), at[1] + y, r.mul_add(a.sin(), at[2])]
            };
            for i in 0..n {
                let i1 = i.saturating_add(1);
                let quad = [ring(i, 0.0), ring(i1, 0.0), ring(i1, h), ring(i, h)];
                let normals: Vec<V3> =
                    quad.iter().map(|c| [c[0] - at[0], 0.0, c[2] - at[2]]).collect();
                s.smooth_face(&quad, &normals, color)?;
                let (u0, u1) = (f32::from(i) / f32::from(n), f32::from(i1) / f32::from(n));
                s.uv_last(&[[u0, 1.0], [u1, 1.0], [u1, 0.0], [u0, 0.0]]);
            }
            s.texture = None;
            let top: Vec<V3> = (0..n).map(|i| ring(i, h)).collect();
            let bottom: Vec<V3> = (0..n).map(|i| ring(i, 0.0)).collect();
            s.polygon(&top, color, Some([0.0, h.signum(), 0.0]), false)?;
            s.polygon(&bottom, color, Some([0.0, -h.signum(), 0.0]), false)
        });
        // A height field: y = f(x, z) over a grid, or `values` rows.
        methods.add_method_mut("surface", |_, s, o: Table| {
            s.material = material_of(&o)?;
            s.texture = texture_of(s, &o)?;
            let range = |key: &str| -> mlua::Result<(f32, f32)> {
                match o.raw_get::<Option<Table>>(key)? {
                    Some(t) => Ok((t.raw_get::<f32>(1)?, t.raw_get::<f32>(2)?)),
                    None => Ok((-1.0, 1.0)),
                }
            };
            let (x0, x1) = range("x")?;
            let (z0, z1) = range("z")?;
            let colors = colors_of(&o)?;
            let wire = o.raw_get::<Option<bool>>("wire")?.unwrap_or(false);
            let mut grid: Vec<Vec<f32>> = Vec::new();
            if let Some(values) = o.raw_get::<Option<Table>>("values")? {
                for row in values.sequence_values::<Table>() {
                    grid.push(row?.sequence_values::<f32>().collect::<mlua::Result<_>>()?);
                }
            } else {
                let f: Function = o
                    .raw_get::<Option<Function>>("f")?
                    .ok_or_else(|| lua_error("scene:surface needs f = function(x, z) or values"))?;
                let detail = o.raw_get::<Option<u32>>("detail")?.unwrap_or(32).clamp(2, 120);
                let n = usize::try_from(detail).unwrap_or(120);
                for j in 0..=n {
                    let z = z0 + (z1 - z0) * count(j) / count(n);
                    let mut row = Vec::with_capacity(n.saturating_add(1));
                    for i in 0..=n {
                        let x = x0 + (x1 - x0) * count(i) / count(n);
                        row.push(f.call::<f32>((x, z))?);
                    }
                    grid.push(row);
                }
            }
            let rows = grid.len();
            let cols = grid.iter().map(Vec::len).min().unwrap_or(0);
            if rows < 2 || cols < 2 {
                return Err(lua_error("scene:surface needs at least a 2x2 grid"));
            }
            let (lo, hi) = grid
                .iter()
                .flatten()
                .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), v| (lo.min(*v), hi.max(*v)));
            let span = (hi - lo).max(1e-6);
            let (col_gaps, row_gaps) =
                (count(cols.saturating_sub(1)), count(rows.saturating_sub(1)));
            let point = |i: usize, j: usize| {
                [
                    x0 + (x1 - x0) * count(i) / col_gaps,
                    grid.get(j).and_then(|row| row.get(i)).copied().unwrap_or_default(),
                    z0 + (z1 - z0) * count(j) / row_gaps,
                ]
            };
            for j in 0..rows.saturating_sub(1) {
                let j1 = j.saturating_add(1);
                for i in 0..cols.saturating_sub(1) {
                    let i1 = i.saturating_add(1);
                    let quad = [point(i, j), point(i1, j), point(i1, j1), point(i, j1)];
                    let mean = quad.iter().map(|v| v[1]).sum::<f32>() / 4.0;
                    let color = ramp(&colors, (mean - lo) / span);
                    if wire {
                        s.check_items()?;
                        for (a, b) in [(quad[0], quad[1]), (quad[0], quad[3])] {
                            s.lines.push(Line3 { a, b, color, width: 1.0 });
                        }
                    } else {
                        s.face(&quad, color, Some([0.0, 1.0, 0.0]), true)?;
                        let (u0, u1) = (count(i) / col_gaps, count(i1) / col_gaps);
                        let (v0, v1) = (count(j) / row_gaps, count(j1) / row_gaps);
                        s.uv_last(&[[u0, v0], [u1, v0], [u1, v1], [u0, v1]]);
                    }
                }
            }
            Ok(())
        });
        methods.add_method_mut("mesh", |_, s, o: Table| {
            s.material = material_of(&o)?;
            s.texture = texture_of(s, &o)?;
            let color = color_of(&o, Token::Accent)?;
            let vertices: Vec<V3> = o
                .raw_get::<Table>("vertices")?
                .sequence_values::<Value>()
                .map(|v| v.and_then(|v| v3(v, "mesh vertex")))
                .collect::<mlua::Result<_>>()?;
            let uvs: Option<Vec<[f32; 2]>> = match o.raw_get::<Option<Table>>("uvs")? {
                Some(list) => Some(
                    list.sequence_values::<Table>()
                        .map(|uv| {
                            let uv = uv?;
                            Ok([uv.raw_get::<f32>(1)?, uv.raw_get::<f32>(2)?])
                        })
                        .collect::<mlua::Result<_>>()?,
                ),
                None => None,
            };
            for face in o.raw_get::<Table>("faces")?.sequence_values::<Table>() {
                let indices: Vec<usize> =
                    face?.sequence_values::<usize>().collect::<mlua::Result<_>>()?;
                let corners: Vec<V3> = indices
                    .iter()
                    .map(|&i| {
                        vertices.get(i.wrapping_sub(1)).copied().ok_or_else(|| {
                            lua_error(format!("mesh face index {i} is out of range"))
                        })
                    })
                    .collect::<mlua::Result<_>>()?;
                match &uvs {
                    Some(uvs) if corners.len() >= 3 => {
                        let uv = |i: usize| {
                            indices
                                .get(i)
                                .and_then(|n| uvs.get(n.wrapping_sub(1)))
                                .copied()
                                .unwrap_or([0.0; 2])
                        };
                        // Fan it, so every triangle keeps its own coordinates.
                        if let Some((&first, rest)) = corners.split_first() {
                            for (k, (&b, &c)) in
                                (1_usize..).zip(rest.iter().zip(rest.iter().skip(1)))
                            {
                                s.face(&[first, b, c], color, None, true)?;
                                s.uv_last(&[uv(0), uv(k), uv(k.saturating_add(1))]);
                            }
                        }
                    }
                    _ => s.polygon(&corners, color, None, true)?,
                }
            }
            Ok(())
        });
        methods.add_method_mut("line", |_, s, (a, b, o): (Value, Value, Option<Table>)| {
            s.check_items()?;
            let (a, b, o) = match (&a, &b) {
                (Value::Table(t), Value::Nil)
                    if t.raw_get::<Value>("from").is_ok_and(|v| !v.is_nil()) =>
                {
                    (t.raw_get("from")?, t.raw_get("to")?, Some(t.clone()))
                }
                _ => (a, b, o),
            };
            let (color, width) = line_style(o)?;
            s.lines.push(Line3 { a: v3(a, "line start")?, b: v3(b, "line end")?, color, width });
            Ok(())
        });
        methods.add_method_mut("path", |_, s, (points, o): (Table, Option<Table>)| {
            let (color, width) = line_style(o)?;
            let pts: Vec<V3> = points
                .sequence_values::<Value>()
                .map(|v| v.and_then(|v| v3(v, "path point")))
                .collect::<mlua::Result<_>>()?;
            for (&a, &b) in pts.iter().zip(pts.iter().skip(1)) {
                s.check_items()?;
                s.lines.push(Line3 { a, b, color, width });
            }
            Ok(())
        });
        methods.add_method_mut("point", |_, s, (at, o): (Value, Option<Table>)| {
            s.check_items()?;
            let (at, o) = options_form(at, o)?;
            let (color, radius, hint) = match o {
                Some(o) => (
                    color_of(&o, Token::Accent)?,
                    o.raw_get::<Option<f32>>("radius")?.unwrap_or(3.0).clamp(0.5, 24.0),
                    o.raw_get::<Value>("hint").map(|v| (!v.is_nil()).then(|| value_text(&v)))?,
                ),
                None => (Color::token(Token::Accent), 3.0, None),
            };
            s.points.push(Point3 { at: v3(at, "point")?, radius, color, hint });
            Ok(())
        });
        methods.add_method_mut("label", |_, s, (at, text, o): (Value, Value, Option<Table>)| {
            s.check_items()?;
            let (at, text, o) = match (&at, &text) {
                (Value::Table(t), Value::Nil)
                    if t.raw_get::<Value>("at").is_ok_and(|v| !v.is_nil()) =>
                {
                    (t.raw_get("at")?, t.raw_get("text")?, Some(t.clone()))
                }
                _ => (at, text, o),
            };
            let (color, size) = match o {
                Some(o) => (
                    color_of(&o, Token::Muted)?,
                    match o.raw_get::<Option<String>>("size")? {
                        Some(size) => {
                            TextSize::parse(&size).map_err(|e| lua_error(e.to_string()))?
                        }
                        None => TextSize::Xs,
                    },
                ),
                None => (Color::token(Token::Muted), TextSize::Xs),
            };
            s.labels.push(Label3 { at: v3(at, "label")?, text: value_text(&text), color, size });
            Ok(())
        });
        // A glTF model from the visual's assets: placed at `at`, turned by
        // `rotate` (degrees), scaled by `scale`, or centered and sized so its
        // longest side is `fit`. Its own materials and textures apply; any
        // material option or `color` given here overrides them.
        methods.add_method_mut("model", |_, s, o: Table| {
            let name = o
                .raw_get::<Option<String>>("asset")?
                .ok_or_else(|| lua_error("s:model needs asset = \"name.glb\""))?;
            let model = s.model(&name)?;
            let at = match o.raw_get::<Value>("at")? {
                Value::Nil => [0.0; 3],
                other => v3(other, "model at")?,
            };
            let mut size = match o.raw_get::<Value>("scale")? {
                Value::Nil => [1.0; 3],
                Value::Integer(n) => [integer(n); 3],
                Value::Number(n) => [narrow(n); 3],
                other => v3(other, "model scale")?,
            };
            let (lo, hi) = model.bounds;
            let origin = if let Some(fit) = o.raw_get::<Option<f32>>("fit")? {
                let extent = hi.iter().zip(lo).map(|(h, l)| h - l).fold(1e-6_f32, f32::max);
                size = scale(size, fit.abs() / extent);
                scale(add(lo, hi), 0.5)
            } else {
                [0.0; 3]
            };
            let turn = rotation_of(o.raw_get("rotate")?)?;
            let color = scene_color(o.raw_get::<Value>("color")?)?;
            let texture = texture_of(s, &o)?;
            let place = |p: V3| {
                let p = sub(p, origin);
                add(at, rotate(&turn, [p[0] * size[0], p[1] * size[1], p[2] * size[2]]))
            };
            // Normals scale inversely, so a squashed model still shades right.
            let orient =
                |n: V3| rotate(&turn, normalize([n[0] / size[0], n[1] / size[1], n[2] / size[2]]));
            let mut images: Vec<Option<u32>> = vec![None; model.images.len()];
            for tri in &model.triangles {
                // Every triangle's material is the model's own or its default.
                let Some(source) = model.materials.get(tri.material) else {
                    continue;
                };
                s.material = material_over(source.material, &o)?;
                s.texture = texture.or_else(|| {
                    let image = source.texture?;
                    let (slot, data) = (images.get_mut(image)?, model.images.get(image)?);
                    Some(*slot.get_or_insert_with(|| {
                        s.texture_index(format!("model:{name}:{image}"), || data.clone())
                    }))
                });
                if tri.uvs.is_none() {
                    s.texture = None;
                }
                let color = color.unwrap_or(Color {
                    token: Token::Rgb(source.rgb),
                    mix: None,
                    alpha: source.alpha,
                });
                let corners = tri.positions.map(place);
                match tri.normals {
                    Some(normals) => s.smooth_face(&corners, &normals.map(orient), color)?,
                    None => s.face(&corners, color, None, source.double_sided)?,
                }
                if let Some(face) = s.faces.last_mut() {
                    face.two_sided = source.double_sided;
                }
                if let Some(uvs) = tri.uvs {
                    s.uv_last(&uvs);
                }
            }
            s.texture = None;
            Ok(())
        });
        // An environment map (an equirectangular image asset, ideally HDR):
        // what metal and glass reflect, and the light around the scene.
        methods.add_method_mut("environment", |_, s, o: Table| {
            let name = o
                .raw_get::<Option<String>>("asset")?
                .ok_or_else(|| lua_error("s:environment needs asset = \"name.hdr\""))?;
            let texture = s.asset_texture(&name)?;
            s.environment = Some(Environment {
                texture,
                intensity: o.raw_get::<Option<f32>>("intensity")?.unwrap_or(1.0).clamp(0.0, 16.0),
                background: o.raw_get::<Option<bool>>("background")?.unwrap_or(false),
            });
            Ok(())
        });
        // A camera-facing glow (a flare, a glint), `size` pixels across: an
        // image asset, or a soft round light without one.
        methods.add_method_mut("sprite", |_, s, (at, o): (Value, Option<Table>)| {
            s.check_items()?;
            let (at, o) = options_form(at, o)?;
            let at = v3(at, "sprite")?;
            let sprite = match o {
                Some(o) => Sprite {
                    at,
                    size: o.raw_get::<Option<f32>>("size")?.unwrap_or(48.0).clamp(1.0, 2048.0),
                    texture: texture_of(s, &o)?.or(match o.raw_get::<Option<String>>("asset")? {
                        Some(name) => Some(s.asset_texture(&name)?),
                        None => None,
                    }),
                    color: color_of(&o, Token::Accent)?,
                    glow: o.raw_get::<Option<f32>>("glow")?.unwrap_or(1.0).clamp(0.0, 16.0),
                },
                None => Sprite {
                    at,
                    size: 48.0,
                    texture: None,
                    color: Color::token(Token::Accent),
                    glow: 1.0,
                },
            };
            s.sprites.push(sprite);
            Ok(())
        });
        // A floor grid on y = 0 in hairlines.
        methods.add_method_mut("grid", |_, s, o: Option<Table>| {
            let (size, step, y) = match &o {
                Some(o) => (
                    o.raw_get::<Option<f32>>("size")?.unwrap_or(10.0).abs().max(0.1),
                    o.raw_get::<Option<f32>>("step")?.unwrap_or(1.0).abs().max(0.01),
                    o.raw_get::<Option<f32>>("y")?.unwrap_or(0.0),
                ),
                None => (10.0, 1.0, 0.0),
            };
            let half = size / 2.0;
            let n = whole((size / step).round()).clamp(1, 200);
            for i in 0..=n {
                s.check_items()?;
                let t = -half + size * count(i) / count(n);
                let color = Color::token(Token::Border);
                s.lines.push(Line3 { a: [t, y, -half], b: [t, y, half], color, width: 1.0 });
                s.lines.push(Line3 { a: [-half, y, t], b: [half, y, t], color, width: 1.0 });
            }
            Ok(())
        });
        // x/y/z axes from the origin, with optional names.
        methods.add_method_mut("axes", |_, s, o: Option<Table>| {
            let size = match &o {
                Some(o) => o.raw_get::<Option<f32>>("size")?.unwrap_or(1.0),
                None => 1.0,
            };
            let names: [String; 3] = match &o {
                Some(o) => match o.raw_get::<Option<Table>>("labels")? {
                    Some(t) => [
                        t.raw_get::<Option<String>>(1)?.unwrap_or_default(),
                        t.raw_get::<Option<String>>(2)?.unwrap_or_default(),
                        t.raw_get::<Option<String>>(3)?.unwrap_or_default(),
                    ],
                    None => ["x".into(), "y".into(), "z".into()],
                },
                None => ["x".into(), "y".into(), "z".into()],
            };
            for (axis, name) in names.into_iter().enumerate() {
                s.check_items()?;
                let along = |length: f32| -> V3 {
                    std::array::from_fn(|i| if i == axis { length } else { 0.0 })
                };
                s.lines.push(Line3 {
                    a: [0.0; 3],
                    b: along(size),
                    color: Color::token(Token::Muted),
                    width: 1.0,
                });
                if !name.is_empty() {
                    s.labels.push(Label3 {
                        at: along(size * 1.08),
                        text: name,
                        color: Color::token(Token::Muted),
                        size: TextSize::Xs,
                    });
                }
            }
            Ok(())
        });
    }
}

/// `s:point(at, options)` or `s:point { at = ..., ... }`.
fn options_form(at: Value, o: Option<Table>) -> mlua::Result<(Value, Option<Table>)> {
    if let Value::Table(t) = &at
        && o.is_none()
        && let at_field @ Value::Table(_) = t.raw_get::<Value>("at")?
    {
        return Ok((at_field, Some(t.clone())));
    }
    Ok((at, o))
}

fn line_style(o: Option<Table>) -> mlua::Result<(Color, f32)> {
    match o {
        Some(o) => Ok((
            color_of(&o, Token::Muted)?,
            o.raw_get::<Option<f32>>("width")?.unwrap_or(1.0).clamp(0.25, 12.0),
        )),
        None => Ok((Color::token(Token::Muted), 1.0)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cube() -> Scene {
        let mut s = Scene::default();
        let h = 0.5;
        let c = |x: f32, y: f32, z: f32| [x * h, y * h, z * h];
        s.face(
            &[c(-1., -1., 1.), c(1., -1., 1.), c(1., 1., 1.), c(-1., 1., 1.)],
            Color::token(Token::Accent),
            Some([0., 0., 1.]),
            false,
        )
        .unwrap();
        s.face(
            &[c(1., -1., -1.), c(-1., -1., -1.), c(-1., 1., -1.), c(1., 1., -1.)],
            Color::token(Token::Accent),
            Some([0., 0., -1.]),
            false,
        )
        .unwrap();
        s
    }

    #[test]
    fn the_camera_frames_the_geometry_and_culls_back_faces() {
        let projection = cube().project(Camera { yaw: 0.0, pitch: 0.0, zoom: 1.0 }, 400.0, 300.0);
        // Looking down -z from +z: only the +z face is drawn.
        assert_eq!(projection.prims.len(), 1);
        let Prim::Face { points, shade, .. } = &projection.prims[0] else { panic!() };
        for [x, y] in points {
            assert!((0.0..=400.0).contains(x) && (0.0..=300.0).contains(y), "{x},{y}");
        }
        assert!(*shade > 0.35);
        // From behind, the other face.
        let behind = cube().project(Camera { yaw: PI, pitch: 0.0, zoom: 1.0 }, 400.0, 300.0);
        assert_eq!(behind.prims.len(), 1);
    }

    #[test]
    fn faces_sort_back_to_front() {
        let mut s = Scene::default();
        let near = [[0., 0., 1.], [1., 0., 1.], [1., 1., 1.]];
        let far = [[0., 0., -1.], [1., 0., -1.], [1., 1., -1.]];
        s.face(&near, Color::token(Token::Chart(0)), None, true).unwrap();
        s.face(&far, Color::token(Token::Chart(1)), None, true).unwrap();
        let p = s.project(Camera { yaw: 0.0, pitch: 0.0, zoom: 1.0 }, 200.0, 200.0);
        let colors: Vec<Token> = p
            .prims
            .iter()
            .map(|p| match p {
                Prim::Face { color, .. } => color.token,
                other => panic!("not a face: {other:?}"),
            })
            .collect();
        assert_eq!(colors, [Token::Chart(1), Token::Chart(0)]);
    }

    #[test]
    fn ramps_mix_between_neighbouring_tokens() {
        let colors = [
            Color::token(Token::Chart(0)),
            Color::token(Token::Chart(1)),
            Color::token(Token::Chart(2)),
        ];
        let mid = ramp(&colors, 0.75);
        assert_eq!(mid.token, Token::Chart(1));
        assert_eq!(mid.mix.unwrap().0, Token::Chart(2));
        assert!((mid.mix.unwrap().1 - 0.5).abs() < 1e-5);
    }
}

// Derived from zeron (MIT, github.com/zeronsh/zeron @ 3b2f780), see THIRD_PARTY_NOTICES.md
//! glTF models for `s:model`.
//!
//! The default scene's meshes are flattened to world-space triangles with their normals, texture
//! coordinates, and PBR materials, plus the images those materials use.

use std::sync::Arc;

use gltf::accessor::{DataType, Dimensions};
use gltf::mesh::Semantic;

use crate::num::byte;
use crate::scene::{Material, TextureData, V3};

/// A parsed model, ready to place into a scene.
#[derive(Debug)]
pub(crate) struct Model {
    pub triangles: Vec<Triangle>,
    pub materials: Vec<ModelMaterial>,
    /// Encoded images (decoded, size-checked, only when drawn), indexed by
    /// `ModelMaterial::texture`.
    pub images: Vec<TextureData>,
    pub bounds: (V3, V3),
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Triangle {
    pub positions: [V3; 3],
    pub normals: Option<[V3; 3]>,
    pub uvs: Option<[[f32; 2]; 3]>,
    pub material: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct ModelMaterial {
    /// sRGB `0xRRGGBB` and alpha.
    pub rgb: u32,
    pub alpha: f32,
    pub texture: Option<usize>,
    pub material: Material,
    pub double_sided: bool,
}

/// Most triangles one model may bring.
const MAX_TRIANGLES: usize = 400_000;
/// Most node visits while walking the scene (instancing can repeat nodes;
/// a malformed file can loop).
const MAX_NODE_VISITS: usize = 100_000;

fn to_srgb(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.003_130_8 { c * 12.92 } else { 1.055_f32.mul_add(c.powf(1.0 / 2.4), -0.055) }
}

fn rgb_of([r, g, b]: [f32; 3]) -> u32 {
    let channel = |c: f32| u32::from(byte((to_srgb(c) * 255.0).round()));
    (channel(r) << 16) | (channel(g) << 8) | channel(b)
}

type Mat4 = [[f32; 4]; 4];

fn multiply(a: &Mat4, b: &Mat4) -> Mat4 {
    // Column-major, as glTF stores them: each column of `a * b` is `a`'s
    // columns weighted by that column of `b`.
    b.map(|b_col| {
        let mut out = [0.0; 4];
        for (a_col, weight) in a.iter().zip(b_col) {
            for (value, a_value) in out.iter_mut().zip(a_col) {
                *value = a_value.mul_add(weight, *value);
            }
        }
        out
    })
}

const fn transform_point(m: &Mat4, [x, y, z]: [f32; 3]) -> V3 {
    let [c0, c1, c2, c3] = m;
    [
        c0[0].mul_add(x, c1[0].mul_add(y, c2[0].mul_add(z, c3[0]))),
        c0[1].mul_add(x, c1[1].mul_add(y, c2[1].mul_add(z, c3[1]))),
        c0[2].mul_add(x, c1[2].mul_add(y, c2[2].mul_add(z, c3[2]))),
    ]
}

/// Normals transform by the matrix's linear part; uniform scales are by far
/// the common case, and the shading normal is renormalized anyway.
fn transform_normal(m: &Mat4, [x, y, z]: [f32; 3]) -> V3 {
    let [c0, c1, c2, _] = m;
    let [vx, vy, vz] = [
        c0[0].mul_add(x, c1[0].mul_add(y, c2[0] * z)),
        c0[1].mul_add(x, c1[1].mul_add(y, c2[1] * z)),
        c0[2].mul_add(x, c1[2].mul_add(y, c2[2] * z)),
    ];
    let len = vx.mul_add(vx, vy.mul_add(vy, vz * vz)).sqrt().max(1e-9);
    [vx / len, vy / len, vz / len]
}

/// The three entries of `list` a triangle's indices name, if all are in it.
fn pick<T: Copy>(list: &[T], [a, b, c]: [usize; 3]) -> Option<[T; 3]> {
    Some([*list.get(a)?, *list.get(b)?, *list.get(c)?])
}

impl Model {
    /// Parse a `.glb` or self-contained `.gltf` (buffers and images embedded
    /// or as data URIs; no external files).
    pub(crate) fn parse(bytes: &[u8]) -> Result<Self, String> {
        // The glTF reader asserts on some malformed files rather than
        // erroring; a model is untrusted, so that's an error here too.
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| Self::parse_unguarded(bytes)))
            .unwrap_or_else(|_| Err("not a usable glTF model: malformed accessors".into()))
    }

    fn parse_unguarded(bytes: &[u8]) -> Result<Self, String> {
        let unusable = |e: gltf::Error| format!("not a usable glTF model: {e}");
        let gltf = gltf::Gltf::from_slice(bytes).map_err(unusable)?;
        let document = gltf.document;
        // No base path: external files are refused, only embedded data loads.
        let buffers = gltf::import_buffers(&document, None, gltf.blob).map_err(unusable)?;
        // Images stay encoded: the renderer decodes them with the same size
        // limits as image assets (a small file can declare a huge image).
        let images = document
            .images()
            .map(|image| {
                let bytes: Vec<u8> = match image.source() {
                    gltf::image::Source::View { view, .. } => buffers
                        .get(view.buffer().index())
                        .and_then(|data| {
                            data.0.get(view.offset()..view.offset().checked_add(view.length())?)
                        })
                        .map(<[u8]>::to_vec)
                        .unwrap_or_default(),
                    gltf::image::Source::Uri { uri, .. } => uri
                        .strip_prefix("data:")
                        .and_then(|rest| rest.split_once(";base64,"))
                        .and_then(|(_, data)| {
                            use base64::Engine as _;
                            base64::engine::general_purpose::STANDARD.decode(data).ok()
                        })
                        .unwrap_or_default(),
                };
                TextureData::Encoded(bytes.into())
            })
            .collect();
        let materials: Vec<ModelMaterial> = document
            .materials()
            .map(|m| {
                let pbr = m.pbr_metallic_roughness();
                let base = pbr.base_color_factor();
                let emissive = m.emissive_factor();
                let strength = m.emissive_strength().unwrap_or(1.0);
                let glow = emissive.iter().copied().fold(0.0_f32, f32::max) * strength;
                // An emitter whose base is dark shines its emissive color.
                let rgb = if glow > 0.0 && base[0].max(base[1]).max(base[2]) < 0.05 {
                    rgb_of(emissive)
                } else {
                    rgb_of([base[0], base[1], base[2]])
                };
                let glass = m.transmission().map_or(0.0, |t| t.transmission_factor());
                // OPAQUE ignores alpha, and MASK cuts rather than blends:
                // only BLEND materials are see-through.
                let alpha = match m.alpha_mode() {
                    gltf::material::AlphaMode::Blend => base[3],
                    gltf::material::AlphaMode::Opaque | gltf::material::AlphaMode::Mask => 1.0,
                };
                ModelMaterial {
                    rgb,
                    alpha,
                    texture: pbr.base_color_texture().map(|info| info.texture().source().index()),
                    material: Material {
                        roughness: pbr.roughness_factor().clamp(0.02, 1.0),
                        metalness: pbr.metallic_factor().clamp(0.0, 1.0),
                        glow: (glow * 2.0).min(8.0),
                        glass,
                        ior: m.ior().unwrap_or(1.5),
                        dispersion: 0.0,
                    },
                    double_sided: m.double_sided(),
                }
            })
            .chain(std::iter::once(ModelMaterial {
                // Primitives without a material.
                rgb: 0x00cc_cccc,
                alpha: 1.0,
                texture: None,
                material: Material::default(),
                double_sided: false,
            }))
            .collect();
        let default_material = materials.len().saturating_sub(1);

        let mut model = Self {
            triangles: Vec::new(),
            materials,
            images,
            bounds: ([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]),
        };
        let scene = document
            .default_scene()
            .or_else(|| document.scenes().next())
            .ok_or("the model has no scene")?;
        let identity: Mat4 = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let mut stack: Vec<(gltf::Node, Mat4)> =
            scene.nodes().map(|node| (node, identity)).collect();
        let mut visits = 0_usize;
        while let Some((node, parent)) = stack.pop() {
            visits = visits.saturating_add(1);
            if visits > MAX_NODE_VISITS {
                return Err("the model's node tree is too large (or loops)".into());
            }
            let world = multiply(&parent, &node.transform().matrix());
            if let Some(mesh) = node.mesh() {
                for primitive in mesh.primitives() {
                    if primitive.mode() != gltf::mesh::Mode::Triangles {
                        continue;
                    }
                    // Only read attributes of the shape their meaning needs.
                    let shaped =
                        |semantic: Semantic, dimensions: Dimensions, types: &[DataType]| {
                            primitive.get(&semantic).is_some_and(|a| {
                                a.dimensions() == dimensions && types.contains(&a.data_type())
                            })
                        };
                    if !shaped(Semantic::Positions, Dimensions::Vec3, &[DataType::F32])
                        || primitive.indices().is_some_and(|a| {
                            a.dimensions() != Dimensions::Scalar
                                || ![DataType::U8, DataType::U16, DataType::U32]
                                    .contains(&a.data_type())
                        })
                    {
                        continue;
                    }
                    let normals_ok = shaped(Semantic::Normals, Dimensions::Vec3, &[DataType::F32]);
                    let uvs_ok = shaped(
                        Semantic::TexCoords(0),
                        Dimensions::Vec2,
                        &[DataType::F32, DataType::U8, DataType::U16],
                    );
                    let reader =
                        primitive.reader(|b| buffers.get(b.index()).map(|d| d.0.as_slice()));
                    let Some(positions) = reader.read_positions() else {
                        continue;
                    };
                    let positions: Vec<V3> =
                        positions.map(|p| transform_point(&world, p)).collect();
                    // Attributes must cover every vertex; a short one is dropped.
                    let normals: Option<Vec<V3>> = normals_ok
                        .then(|| reader.read_normals())
                        .flatten()
                        .map(|n| n.map(|n| transform_normal(&world, n)).collect::<Vec<_>>())
                        .filter(|n| n.len() >= positions.len());
                    let uvs: Option<Vec<[f32; 2]>> = uvs_ok
                        .then(|| reader.read_tex_coords(0))
                        .flatten()
                        .map(|t| t.into_f32().collect::<Vec<_>>())
                        .filter(|t| t.len() >= positions.len());
                    let indices: Vec<usize> = match reader.read_indices() {
                        Some(indices) => indices.into_u32().map(|i| i as usize).collect(),
                        None => (0..positions.len()).collect(),
                    };
                    let material = primitive.material().index().unwrap_or(default_material);
                    for &tri in indices.as_chunks::<3>().0 {
                        let Some(p) = pick(&positions, tri) else {
                            continue;
                        };
                        if model.triangles.len() >= MAX_TRIANGLES {
                            return Err(format!("the model has over {MAX_TRIANGLES} triangles"));
                        }
                        let (low, high) = &mut model.bounds;
                        for v in &p {
                            for ((low, high), value) in low.iter_mut().zip(high.iter_mut()).zip(v) {
                                *low = low.min(*value);
                                *high = high.max(*value);
                            }
                        }
                        // Both cover every vertex, so their picks are whole.
                        model.triangles.push(Triangle {
                            positions: p,
                            normals: normals.as_deref().and_then(|n| pick(n, tri)),
                            uvs: uvs.as_deref().and_then(|uv| pick(uv, tri)),
                            material,
                        });
                    }
                }
            }
            for child in node.children() {
                if stack.len() >= MAX_NODE_VISITS {
                    return Err("the model's node tree is too large (or loops)".into());
                }
                stack.push((child, world));
            }
        }
        if model.triangles.is_empty() {
            return Err("the model has no triangle meshes".into());
        }
        Ok(model)
    }
}

/// An `Arc`'d model for the scene's cache.
pub(crate) fn parse_shared(bytes: &[u8]) -> Result<Arc<Model>, String> {
    Model::parse(bytes).map(Arc::new)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A binary glTF of one red, double-sided triangle under a node that
    /// moves it up by 2 and doubles it.
    pub(crate) fn triangle_glb() -> Vec<u8> {
        let positions: [f32; 9] = [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
        let bin: Vec<u8> = positions.iter().flat_map(|f| f.to_le_bytes()).collect();
        let json = format!(
            r#"{{"asset":{{"version":"2.0"}},"scene":0,"scenes":[{{"nodes":[0]}}],
            "nodes":[{{"mesh":0,"translation":[0,2,0],"scale":[2,2,2]}}],
            "meshes":[{{"primitives":[{{"attributes":{{"POSITION":0}},"material":0}}]}}],
            "materials":[{{"doubleSided":true,"pbrMetallicRoughness":{{"baseColorFactor":[1,0,0,1],"metallicFactor":0.5,"roughnessFactor":0.25}}}}],
            "accessors":[{{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]}}],
            "bufferViews":[{{"buffer":0,"byteLength":{len}}}],
            "buffers":[{{"byteLength":{len}}}]}}"#,
            len = bin.len()
        );
        glb(&json, &bin)
    }

    /// A binary glTF of `json` over `bin`.
    fn glb(json: &str, bin: &[u8]) -> Vec<u8> {
        let mut json = json.as_bytes().to_vec();
        while !json.len().is_multiple_of(4) {
            json.push(b' ');
        }
        let total: usize = [12, 8, json.len(), 8, bin.len()].iter().sum();
        let length = |n: usize| u32::try_from(n).unwrap().to_le_bytes();
        let mut glb = Vec::with_capacity(total);
        glb.extend_from_slice(b"glTF");
        glb.extend_from_slice(&2_u32.to_le_bytes());
        glb.extend_from_slice(&length(total));
        glb.extend_from_slice(&length(json.len()));
        glb.extend_from_slice(b"JSON");
        glb.extend_from_slice(&json);
        glb.extend_from_slice(&length(bin.len()));
        glb.extend_from_slice(b"BIN\0");
        glb.extend_from_slice(bin);
        glb
    }

    /// One triangle (3 positions, then `extra` bytes) and `rest` of the JSON.
    fn triangle_with(extra: &[u8], rest: &str) -> Vec<u8> {
        let positions: [f32; 9] = [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
        let mut bin: Vec<u8> = positions.iter().flat_map(|f| f.to_le_bytes()).collect();
        bin.extend_from_slice(extra);
        while !bin.len().is_multiple_of(4) {
            bin.push(0);
        }
        let json = format!(
            r#"{{"asset":{{"version":"2.0"}},"scene":0,"scenes":[{{"nodes":[0]}}],
            "buffers":[{{"byteLength":{len}}}],{rest}}}"#,
            len = bin.len()
        );
        glb(&json, &bin)
    }

    #[test]
    fn looping_node_trees_are_refused_not_walked_forever() {
        for children in ["[0]", "[0,0,0,0]"] {
            let json = format!(
                r#"{{"asset":{{"version":"2.0"}},"scene":0,"scenes":[{{"nodes":[0]}}],"nodes":[{{"children":{children}}}]}}"#
            );
            let error = Model::parse(&glb(&json, &[])).unwrap_err();
            assert!(error.contains("loops"), "{error}");
        }
    }

    #[test]
    fn attributes_shorter_than_the_positions_are_dropped() {
        // POSITION has 3 vertices, NORMAL only 1.
        let normal: Vec<u8> = [0.0_f32, 0.0, 1.0].iter().flat_map(|f| f.to_le_bytes()).collect();
        let model = Model::parse(&triangle_with(
            &normal,
            r#""nodes":[{"mesh":0}],
            "meshes":[{"primitives":[{"attributes":{"POSITION":0,"NORMAL":1}}]}],
            "accessors":[{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]},
                         {"bufferView":1,"componentType":5126,"count":1,"type":"VEC3"}],
            "bufferViews":[{"buffer":0,"byteLength":36},{"buffer":0,"byteOffset":36,"byteLength":12}]"#,
        ))
        .unwrap();
        assert!(model.triangles[0].normals.is_none());
    }

    #[test]
    fn attributes_of_the_wrong_shape_are_skipped_not_read() {
        // TEXCOORD_0 pointing at the VEC3 positions (the glTF reader asserts).
        let model = Model::parse(&triangle_with(
            &[],
            r#""nodes":[{"mesh":0}],
            "meshes":[{"primitives":[{"attributes":{"POSITION":0,"TEXCOORD_0":0}}]}],
            "accessors":[{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]}],
            "bufferViews":[{"buffer":0,"byteLength":36}]"#,
        ))
        .unwrap();
        assert!(model.triangles[0].uvs.is_none());
    }

    #[test]
    fn embedded_images_stay_encoded_and_are_size_checked_when_drawn() {
        // A tiny PNG declaring an image wider than any texture may be.
        let mut png = Vec::new();
        ::image::RgbaImage::new(20_000, 1)
            .write_to(&mut std::io::Cursor::new(&mut png), ::image::ImageFormat::Png)
            .unwrap();
        let rest = format!(
            r#""nodes":[{{"mesh":0}}],
            "meshes":[{{"primitives":[{{"attributes":{{"POSITION":0}},"material":0}}]}}],
            "materials":[{{"pbrMetallicRoughness":{{"baseColorTexture":{{"index":0}}}}}}],
            "textures":[{{"source":0}}],
            "images":[{{"bufferView":1,"mimeType":"image/png"}}],
            "accessors":[{{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]}}],
            "bufferViews":[{{"buffer":0,"byteLength":36}},{{"buffer":0,"byteOffset":36,"byteLength":{len}}}]"#,
            len = png.len()
        );
        let model = Model::parse(&triangle_with(&png, &rest)).unwrap();
        let TextureData::Encoded(bytes) = &model.images[0] else { panic!("decoded eagerly") };
        assert_eq!(bytes.len(), png.len());
        let error = crate::image::decode_texture(&model.images[0]).unwrap_err();
        assert!(error.contains("20000x1"), "{error}");
    }

    #[test]
    fn a_glb_parses_into_world_space_triangles_with_materials() {
        let model = Model::parse(&triangle_glb()).unwrap();
        assert_eq!(model.triangles.len(), 1);
        let tri = model.triangles[0];
        assert_eq!(tri.positions, [[0.0, 2.0, 0.0], [2.0, 2.0, 0.0], [0.0, 4.0, 0.0]]);
        assert_eq!(model.bounds, ([0.0, 2.0, 0.0], [2.0, 4.0, 0.0]));
        let material = &model.materials[tri.material];
        assert_eq!(material.rgb, 0x00ff_0000);
        assert!(material.double_sided);
        assert_eq!(material.material.metalness, 0.5);
        assert_eq!(material.material.roughness, 0.25);
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        Model::parse(b"not a model").unwrap_err();
        Model::parse(b"glTF\x02\0\0\0\x0c\0\0\0").unwrap_err();
    }
}

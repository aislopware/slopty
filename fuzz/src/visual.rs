//! An agent's visual (`show_visual`) through the sandbox that runs it (`slopty_visual`).
//!
//! A visual is a Luau program an agent wrote, with asset files beside it, and the shaders its
//! nodes carry. The first byte picks what the rest is:
//! - `0`: a script, run as `show_visual` runs it before showing it ([`slopty_visual::check`]): its
//!   setup, the first render, every canvas and scene draw, then each control once;
//! - `1`: an image file, read as a texture or an environment is ([`image`]);
//! - `2`: a model file (glTF or GLB), placed in a scene as `s:model` places one;
//! - `3`: a shader's GLSL, checked as the shader node's code is ([`shader::validate`]).
//!
//! What must hold: no panic (but those of the glTF reader, which the model parse catches:
//! [`CONTAINED`]), the sandbox's memory cap and its time budget per call hold (a
//! script past them fails with an error, which libFuzzer's own limits would otherwise catch as
//! a leak, an out-of-memory or a timeout), and a decoded image is no larger than the limits
//! say and as large as its header says.

use std::collections::HashMap;

use slopty_visual::image::Pixels;
use slopty_visual::{Assets, image, shader};

/// Where the panics `slopty_visual` catches are raised: the glTF reader asserts on some malformed
/// files rather than failing, and a model is parsed under `catch_unwind` for it.
pub const CONTAINED: &[&str] = &["/gltf-1.", "/gltf-json-1."];

/// The seed every script runs with (`show_visual` derives one from the visual's id).
const SEED: u64 = 1;

/// The scene a model input is placed in, the asset under [`MODEL`].
const MODEL_SCENE: &str = r#"
return ui.scene { height = 200, draw = function(s)
  s:model { asset = "model.glb", fit = 1 }
end }
"#;

/// The asset name a model input is given.
const MODEL: &str = "model.glb";

/// Run one input.
pub fn run(data: &[u8]) {
    let Some((&what, rest)) = data.split_first() else { return };
    match what % 4 {
        0 => {
            let _checked = slopty_visual::check(&String::from_utf8_lossy(rest), SEED);
        }
        1 => decode(rest),
        2 => {
            let assets = Assets::new(HashMap::from([(MODEL.to_owned(), rest.to_vec().into())]));
            let _checked = slopty_visual::check_with_assets(MODEL_SCENE, SEED, assets);
        }
        _ => {
            let _checked = shader::validate(&String::from_utf8_lossy(rest), &[]);
        }
    }
}

/// An image decodes to no more pixels than the limits allow, and to the size its header says.
fn decode(bytes: &[u8]) {
    let header = image::dimensions(bytes);
    let Ok(decoded) = image::decode(bytes) else { return };
    let (width, height) = (decoded.width, decoded.height);
    assert!(width <= image::MAX_IMAGE_SIDE && height <= image::MAX_IMAGE_SIDE, "{width}×{height}");
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    assert!(pixels <= image::MAX_IMAGE_PIXELS, "{width}×{height}: {pixels} pixels");
    assert_eq!(header, Ok((width, height)), "the header and the decode disagree");
    let held = match &decoded.pixels {
        Pixels::Rgba8(bytes) => bytes.len(),
        Pixels::RgbaF32(floats) => floats.len(),
    };
    let wanted = usize::try_from(pixels.saturating_mul(4)).unwrap_or(usize::MAX);
    assert_eq!(held, wanted, "{width}×{height} decoded into {held} channel values");
}

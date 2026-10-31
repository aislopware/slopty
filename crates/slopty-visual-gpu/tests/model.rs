//! A glTF model drawn by `s:model`, on the software renderer and on the GPU, judged by
//! numbers: the image's size, and how many of its pixels the model covers.

#[cfg(test)]
mod model {
    use std::collections::HashMap;
    use std::sync::Arc;

    use slopty_visual::{Assets, Camera, Color, NodeKind, Quality, Token, Visual};

    /// A binary glTF of one grey, double-sided square of side 1 at the origin: two triangles.
    fn square_glb() -> Vec<u8> {
        let positions: [f32; 12] = [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 1.0, 0.0];
        let indices: [u16; 6] = [0, 1, 2, 0, 2, 3];
        let mut bin: Vec<u8> = positions.iter().flat_map(|f| f.to_le_bytes()).collect();
        let at = bin.len();
        bin.extend(indices.iter().flat_map(|i| i.to_le_bytes()));
        while !bin.len().is_multiple_of(4) {
            bin.push(0);
        }
        let json = format!(
            r#"{{"asset":{{"version":"2.0"}},"scene":0,"scenes":[{{"nodes":[0]}}],
        "nodes":[{{"mesh":0}}],
        "meshes":[{{"primitives":[{{"attributes":{{"POSITION":0}},"indices":1,"material":0}}]}}],
        "materials":[{{"doubleSided":true,"pbrMetallicRoughness":{{"baseColorFactor":[0.8,0.8,0.8,1]}}}}],
        "accessors":[
          {{"bufferView":0,"componentType":5126,"count":4,"type":"VEC3","min":[0,0,0],"max":[1,1,0]}},
          {{"bufferView":1,"componentType":5123,"count":6,"type":"SCALAR"}}],
        "bufferViews":[{{"buffer":0,"byteLength":{at}}},{{"buffer":0,"byteOffset":{at},"byteLength":12}}],
        "buffers":[{{"byteLength":{len}}}]}}"#,
            len = bin.len()
        );
        let mut json = json.into_bytes();
        while !json.len().is_multiple_of(4) {
            json.push(b' ');
        }
        // The header, then each chunk's length and type before it.
        let total =
            json.len().checked_add(bin.len()).and_then(|n| n.checked_add(12 + 8 + 8)).unwrap();
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
        glb.extend_from_slice(&bin);
        glb
    }

    /// Straight sRGB for a scene colour: its own hex, else a mid grey.
    fn rgba(color: Color) -> [f32; 4] {
        match color.token {
            Token::Rgb(hex) => {
                let [_, r, g, b] = hex.to_be_bytes().map(|byte| f32::from(byte) / 255.0);
                [r, g, b, color.alpha]
            }
            _ => [0.5, 0.5, 0.5, color.alpha],
        }
    }

    /// Pixels of a BGRA image that are not wholly transparent.
    fn covered(bgra: &[u8]) -> usize {
        bgra.as_chunks::<4>().0.iter().filter(|px| px[3] > 0).count()
    }

    /// The scene a script draws with the model, and its camera.
    fn model_scene() -> (slopty_visual::Scene, Camera) {
        let assets = Assets::new(HashMap::from([(
            "square.glb".to_owned(),
            Arc::<[u8]>::from(square_glb()),
        )]));
        let source = r#"
        return ui.scene { height = 200, draw = function(s)
          s:model { asset = "square.glb", fit = 2 }
        end }
    "#;
        slopty_visual::check_with_assets(source, 1, assets.clone()).expect("the check passes");
        let mut visual = Visual::load(source, 1).unwrap().with_assets(assets);
        let tree = visual.render().unwrap();
        let NodeKind::Scene(node) = &tree.kind else { panic!("a scene: {tree:?}") };
        let camera = Camera { yaw: node.yaw, pitch: node.pitch, zoom: node.zoom };
        (visual.draw_scene(node.draw, 0.0).unwrap(), camera)
    }

    #[test]
    fn a_model_renders_one_image_of_the_asked_size_in_software() {
        let (scene, camera) = model_scene();
        assert_eq!(scene.face_count(), 2, "the square's two triangles");
        let raster = scene.render(camera, 320.0, 200.0, 2.0, Quality::Final, &rgba);
        assert_eq!((raster.width, raster.height), (640, 400));
        assert_eq!(raster.bgra.len(), 640 * 400 * 4);
        let covered = covered(&raster.bgra);
        assert!(covered > 0, "the model covers some of the image");
        assert!(covered < 640 * 400, "and not all of it");
    }

    #[test]
    fn a_model_renders_on_the_gpu_when_there_is_one() {
        let (scene, camera) = model_scene();
        let mut state = None;
        let frame =
            slopty_visual_gpu::render_scene(&scene, camera, 320.0, 200.0, 2.0, &rgba, &mut state);
        match slopty_visual_gpu::unavailable_reason() {
            None => {
                let frame = frame.expect("a GPU renders the scene");
                assert_eq!((frame.width(), frame.height()), (640, 400));
                assert!(covered(&frame.read_pixels()) > 0, "the frame holds the model");
            }
            // A machine without a GPU draws in software (the test above); the GPU path says so.
            Some(reason) => {
                assert!(frame.is_none(), "no GPU, no frame");
                println!("no GPU here: {reason}");
            }
        }
    }
}

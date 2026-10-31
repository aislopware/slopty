// Derived from zeron (MIT, github.com/zeronsh/zeron @ 3b2f780), see THIRD_PARTY_NOTICES.md
//! Render a `ui.shader` pass headlessly, composited over a chat-like
//! background, to check how it will look:
//!
//! ```text
//! cargo run -p slopty-visual-gpu --example render_shader -- shader.glsl out.ppm \
//!    [--size 1440x840] [--blend light|alpha|opaque] [--bloom 1] [--time 2] \
//!    [--input name=value ...] [--background r,g,b]
//! ```
//!
//! Apple's platforms only, as the crate.

#[cfg(not(target_vendor = "apple"))]
fn main() {}

#[cfg(target_vendor = "apple")]
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "pixel sizes stay far below 2^24, exact in `f32`, and a blend of bytes stays in 0..=255"
)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use slopty_visual::shader::{Blend, ShaderUniforms};
    use slopty_visual_gpu::{ShaderSpec, render_shader};

    let mut args = std::env::args().skip(1);
    let (Some(source), Some(out)) = (args.next(), args.next()) else {
        return Err("usage: render_shader SHADER.glsl OUT.ppm [flags]".into());
    };
    let code = std::fs::read_to_string(&source)?;
    let (mut width, mut height, mut blend, mut bloom, mut time) =
        (1440_u32, 840_u32, Blend::Light, 0.0, 2.0);
    let mut inputs: Vec<(String, f32)> = Vec::new();
    let mut background = [0.80_f32, 0.83, 0.81];
    while let Some(flag) = args.next() {
        let value = args.next().unwrap_or_default();
        match flag.as_str() {
            "--size" => {
                let (across, down) = value.split_once('x').ok_or("--size takes WxH")?;
                (width, height) = (across.parse()?, down.parse()?);
            }
            "--blend" => {
                blend = Blend::parse(&value).ok_or("--blend takes light, alpha or opaque")?;
            }
            "--bloom" => bloom = value.parse()?,
            "--time" => time = value.parse()?,
            "--input" => {
                let (name, v) = value.split_once('=').ok_or("--input takes name=value")?;
                inputs.push((name.to_owned(), v.parse()?));
            }
            "--background" => {
                let parts = value.split(',').map(str::parse).collect::<Result<Vec<f32>, _>>()?;
                let &[red, green, blue, ..] = parts.as_slice() else {
                    return Err("--background takes r,g,b".into());
                };
                background = [red, green, blue];
            }
            other => return Err(format!("unknown flag {other}").into()),
        }
    }
    inputs.sort_by(|a, b| a.0.cmp(&b.0));
    let names: Vec<String> = inputs.iter().map(|(n, _)| n.clone()).collect();
    let mut slots = [[0.0_f32; 4]; 4];
    for (slot, (_, v)) in slots.as_flattened_mut().iter_mut().zip(&inputs) {
        *slot = *v;
    }
    let uniforms = ShaderUniforms {
        resolution: [width as f32, height as f32, 2.0, 0.0],
        mouse: [0.0, 0.0, -1.0, -1.0],
        camera: [0.3, 0.45, 1.0, 0.0],
        time: [time, 0.016, 60.0, 0.0],
        inputs: slots,
        ..ShaderUniforms::default()
    };
    let spec =
        ShaderSpec { image: &code, buffers: &[], inputs: &names, blend, bloom, textures: &[] };
    let frame = render_shader(&spec, &uniforms, width, height, &mut None)?;
    let mut ppm = format!("P6\n{width} {height}\n255\n").into_bytes();
    for &[blue, green, red, alpha] in frame.read_pixels().as_chunks().0 {
        let alpha = f32::from(alpha) / 255.0;
        for (ch, bg) in [red, green, blue].into_iter().zip(background) {
            ppm.push(f32::from(ch).mul_add(alpha, bg * 255.0 * (1.0 - alpha)) as u8);
        }
    }
    std::fs::write(out, ppm)?;
    Ok(())
}

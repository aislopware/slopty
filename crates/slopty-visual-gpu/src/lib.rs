// Derived from zeron (MIT, github.com/zeronsh/zeron @ 3b2f780), see THIRD_PARTY_NOTICES.md
//! The GPU behind agent visuals.
//!
//! GPUI composites its own primitives, not foreign GPU passes, so visuals
//! that need the GPU (fragment shaders, lit 3D scenes) render offscreen here
//! through wgpu on Metal. A frame's last pass draws straight into an
//! `IOSurface`-backed pixel buffer ([`Frame`]) that GPUI's surface samples
//! where it lies, clipped, faded and layered with the transcript: nothing is
//! read back to the CPU or uploaded again. One device serves every visual;
//! pipelines are cached by source, so a shader compiles once however many
//! frames it draws.
//!
//! Apple's platforms only, as the shell that shows the frames: Metal draws
//! them and CoreVideo pools them. Elsewhere the crate is empty.

#![cfg(target_vendor = "apple")]
#![warn(unreachable_pub)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` is on, so an item shared from a private module is `pub(crate)`"
)]

use std::sync::OnceLock;

mod scene;
mod shader;
mod surface;
mod textures;

pub use scene::{SceneState, render_scene};
pub use shader::{ShaderSpec, ShaderState, render_shader};
pub use surface::Frame;

/// The device every visual draws with. Its fields are private to the crate
/// root, which every module of the crate descends from, so all of them reach
/// the fields.
pub(crate) struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// The shader pipelines, made on the first shader.
    shaders: OnceLock<shader::Pipelines>,
    /// The scene pipelines, made on the first scene.
    scene: OnceLock<scene::Pipelines>,
    /// Images and environment maps, uploaded once.
    textures: textures::Textures,
}

/// The shared device, made on first use; `None` without a usable adapter.
pub(crate) fn gpu() -> Option<&'static Gpu> {
    device().as_ref().ok()
}

fn device() -> &'static Result<Gpu, String> {
    static GPU: OnceLock<Result<Gpu, String>> = OnceLock::new();
    GPU.get_or_init(|| {
        pollster::block_on(Gpu::new())
            .inspect_err(|error| tracing::warn!(%error, "visual GPU unavailable"))
    })
}

/// Run `f`, catching every error wgpu reports while it runs (validation,
/// internal, out of memory) as this call's error.
pub(crate) fn guarded<T>(device: &wgpu::Device, f: impl FnOnce() -> T) -> Result<T, String> {
    let scopes = [
        wgpu::ErrorFilter::OutOfMemory,
        wgpu::ErrorFilter::Internal,
        wgpu::ErrorFilter::Validation,
    ]
    .map(|filter| device.push_error_scope(filter));
    let out = f();
    let mut error = None;
    for scope in scopes.into_iter().rev() {
        if let Some(e) = pollster::block_on(scope.pop()) {
            error.get_or_insert_with(|| e.to_string());
        }
    }
    match error {
        Some(error) => Err(error),
        None => Ok(out),
    }
}

/// Why visuals can't use the GPU here, if they can't.
pub fn unavailable_reason() -> Option<String> {
    device().as_ref().err().cloned()
}

/// Whether visuals can render on the GPU here.
pub fn available() -> bool {
    gpu().is_some()
}

/// The format of every frame: GPUI's surfaces sample BGRA as it lies.
pub(crate) const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8Unorm;

/// WGSL for the final passes: how much of a pixel survives near the frame's
/// edges, so light running off a visual fades out instead of stopping at a
/// hard line. The ramp is 15% of the shorter side, eased at both ends
/// (smootherstep) so neither the start nor the end of the fade shows.
pub(crate) const EDGE_FADE: &str = "
fn edge_ease(d: f32, width: f32) -> f32 {
    let t = clamp(d / width, 0.0, 1.0);
    return t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
}

fn edge_fade(pixel: vec2<f32>, size: vec2<f32>) -> f32 {
    let width = max(0.15 * min(size.x, size.y), 1.0);
    let d = min(pixel, size - pixel);
    return edge_ease(d.x, width) * edge_ease(d.y, width);
}
";

impl Gpu {
    async fn new() -> Result<Self, String> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::METAL,
            flags: wgpu::InstanceFlags::default(),
            backend_options: wgpu::BackendOptions::default(),
            memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
            display: None,
        });
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                force_fallback_adapter: false,
                compatible_surface: None,
                apply_limit_buckets: false,
            })
            .await
            .map_err(|e| e.to_string())?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("slopty visuals"),
                // What this adapter has, not wgpu's defaults: some devices
                // (the iOS simulator) fall short of a default limit the
                // renderers never come near.
                required_limits: adapter.limits(),
                ..Default::default()
            })
            .await
            .map_err(|e| e.to_string())?;
        // wgpu panics on an error nothing catches; a visual's mistake (or a
        // lost device) must cost that visual, not the app.
        device.on_uncaptured_error(std::sync::Arc::new(|error| {
            tracing::warn!(%error, "visual GPU error");
        }));
        Ok(Self {
            device,
            queue,
            shaders: OnceLock::new(),
            scene: OnceLock::new(),
            textures: textures::Textures::default(),
        })
    }

    /// Run `encoder`'s passes and wait until the GPU finished them: a frame's buffer goes to
    /// GPUI only once it is drawn, and a wait that fails (the device lost) hands out none.
    pub(crate) fn finish(&self, encoder: wgpu::CommandEncoder) -> Result<(), String> {
        self.queue.submit([encoder.finish()]);
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map(|_finished| ())
            .map_err(|error| format!("the GPU never finished the frame: {error}"))
    }
}

#[cfg(test)]
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a blend of two bytes stays in 0..=255"
)]
pub(crate) fn dump_ppm(frame: &Frame, path: &str) {
    let mut out = format!("P6\n{} {}\n255\n", frame.width(), frame.height()).into_bytes();
    for &[b, g, r, a] in frame.read_pixels().as_chunks().0 {
        // Over a mid grey, so transparency reads.
        let a = f32::from(a) / 255.0;
        for ch in [r, g, b] {
            out.push(f32::from(ch).mul_add(a, 60.0 * (1.0 - a)) as u8);
        }
    }
    // A debugging aid: a dump that can't be written costs nothing.
    let _unwritten = std::fs::write(path, out);
}

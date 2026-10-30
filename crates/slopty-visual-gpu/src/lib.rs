// Derived from zeron (MIT, github.com/zeronsh/zeron @ 3b2f780), see THIRD_PARTY_NOTICES.md
//! The GPU behind agent visuals.
//!
//! GPUI composites images, not foreign GPU passes, so visuals that need the
//! GPU (fragment shaders, lit 3D scenes) render offscreen here through wgpu
//! (Metal, Vulkan, or Direct3D) into a texture that is read back as
//! straight-alpha BGRA and painted like any other image, clipped, faded, and
//! layered with the transcript. One device serves every visual; pipelines are
//! cached by source, so a shader compiles once however many frames it draws.

#![warn(unreachable_pub)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` is on, so an item shared from a private module is `pub(crate)`"
)]

use std::sync::OnceLock;

mod scene;
mod shader;
mod textures;

pub use scene::{SceneState, render_scene};
pub use shader::{ShaderSpec, ShaderState, render_shader};

/// One rendered frame: straight-alpha BGRA, top row first.
#[derive(Debug, Clone)]
pub struct Frame {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Four bytes a pixel, rows packed with no padding.
    pub bgra: Vec<u8>,
}

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

/// The target every visual renders into before readback.
pub(crate) const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

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
            backends: wgpu::Backends::PRIMARY,
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

    /// Read a rendered texture back as straight-alpha BGRA, through a
    /// staging buffer kept in `staging` between frames.
    pub(crate) fn read_back(
        &self,
        mut encoder: wgpu::CommandEncoder,
        texture: &wgpu::Texture,
        staging: &mut Option<wgpu::Buffer>,
    ) -> Frame {
        let (width, height) = (texture.width(), texture.height());
        // A texture is far narrower than `u32` overflows at: the device caps it.
        let row = width.saturating_mul(4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let size = u64::from(row).saturating_mul(u64::from(height));
        let buffer: &wgpu::Buffer = match staging {
            Some(buffer) if buffer.size() == size => buffer,
            _ => staging.insert(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("visual readback"),
                size,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            })),
        };
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: None,
                },
            },
            texture.size(),
        );
        self.queue.submit([encoder.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        buffer.slice(..).map_async(wgpu::MapMode::Read, move |result| {
            let _gone = tx.send(result);
        });
        // A wait that fails shows below, as a map that never succeeded.
        let _waited = self.device.poll(wgpu::PollType::wait_indefinitely());
        let width_bytes = (width as usize).saturating_mul(4);
        let mut bgra = vec![0_u8; width_bytes.saturating_mul(height as usize)];
        if rx.recv() == Ok(Ok(())) {
            if let Ok(mapped) = buffer.slice(..).get_mapped_range() {
                // `max(1)`: a zero-width frame has no rows to copy, and
                // `chunks` takes no zero.
                let rows = mapped.chunks((row as usize).max(1));
                for (src, dst) in rows.zip(bgra.chunks_exact_mut(width_bytes.max(1))) {
                    let src = src.get(..width_bytes).unwrap_or_default();
                    for (d, &[r, g, b, a]) in
                        dst.as_chunks_mut().0.iter_mut().zip(src.as_chunks().0)
                    {
                        *d = [b, g, r, a];
                    }
                }
                drop(mapped);
            }
            buffer.unmap();
        }
        Frame { width, height, bgra }
    }

    pub(crate) fn target(&self, width: u32, height: u32, label: &str) -> wgpu::Texture {
        self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    }
}

#[cfg(test)]
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a blend of two bytes stays in 0..=255"
)]
pub(crate) fn dump_ppm(frame: &Frame, path: &str) {
    let mut out = format!("P6\n{} {}\n255\n", frame.width, frame.height).into_bytes();
    for &[b, g, r, a] in frame.bgra.as_chunks().0 {
        // Over a mid grey, so transparency reads.
        let a = f32::from(a) / 255.0;
        for ch in [r, g, b] {
            out.push(f32::from(ch).mul_add(a, 60.0 * (1.0 - a)) as u8);
        }
    }
    // A debugging aid: a dump that can't be written costs nothing.
    let _unwritten = std::fs::write(path, out);
}

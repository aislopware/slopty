//! What an animating visual's frame costs on its way to the window, for `docs/MEASUREMENTS.md`
//! ("2026-10-31 — an animating visual renders into the surface GPUI shows"). A shader at
//! 1600 × 900 (an 800 × 450 pt visual at 2x) draws frame after frame into pixel buffers from its
//! pool; each is wrapped as a texture through a CoreVideo cache, as GPUI's renderer wraps a
//! surface to draw it, and the two newest are held, as the window holds the one it shows and the
//! one its last frame still samples.
//!
//! Per frame: the thread's CPU time, and of it the window's share (the wrap; the main thread's
//! in the app), the wall time (the GPU's included, since a frame is handed on only once drawn),
//! and the instructions the whole process retired, the Metal driver's threads with it. Over the
//! run: what the process's footprint grew by from before the first frame, at the end and at its
//! peak.
//!
//! ```sh
//! cargo test -p slopty-visual-gpu --release --test animated_frames -- --ignored --nocapture
//! ```

#[cfg(test)]
mod animated_frames {
    use std::collections::VecDeque;
    use std::ptr::NonNull;
    use std::time::{Duration, Instant};

    use objc2_core_foundation::CFRetained;
    use objc2_core_video::{
        CVMetalTexture, CVMetalTextureCache, CVMetalTextureGetTexture, CVPixelBuffer,
        CVPixelBufferGetHeight, CVPixelBufferGetWidth, kCVReturnSuccess,
    };
    use objc2_metal::{MTLCreateSystemDefaultDevice, MTLPixelFormat};
    use slopty_testkit::process;
    use slopty_testkit::stats::{Spread, us};
    use slopty_visual::shader::{Blend, ShaderUniforms};
    use slopty_visual_gpu::{ShaderSpec, render_shader};

    /// The frame's size in pixels.
    const SIZE: (u32, u32) = (1600, 900);
    /// Frames drawn before the timed ones, so pipelines, pools and caches are made.
    const WARM: u16 = 20;
    /// Frames timed.
    const FRAMES: u16 = 300;
    /// A plasma with bloom: a few transcendental calls a pixel, as the visuals agents draw.
    const CODE: &str = "void mainImage(out vec4 fragColor, in vec2 fragCoord) {
        vec2 uv = fragCoord / iResolution.xy;
        float t = iTime;
        float v = sin(uv.x * 10.0 + t) + sin((uv.y * 10.0 + t) / 2.0);
        v += sin((uv.x * 10.0 + uv.y * 10.0 + t) / 2.0);
        vec2 c = uv * 10.0 + vec2(sin(t / 3.0), cos(t / 2.0)) * 5.0;
        v += sin(sqrt(c.x * c.x + c.y * c.y + 1.0) + t);
        fragColor = vec4(0.5 + 0.5 * sin(v * 3.14159), 0.5 + 0.5 * sin(v * 3.14159 + 2.094),
            0.5 + 0.5 * sin(v * 3.14159 + 4.188), 1.0);
    }";

    /// The CPU time the calling thread has spent.
    fn thread_cpu() -> Duration {
        let mut now = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        // SAFETY: `now` is a live `timespec` the call fills; the clock id is one it knows.
        let status = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &raw mut now) };
        assert_eq!(status, 0, "the thread's CPU clock");
        Duration::new(
            u64::try_from(now.tv_sec).unwrap_or_default(),
            u32::try_from(now.tv_nsec).unwrap_or_default(),
        )
    }

    /// `buffer` as a Metal texture, as GPUI's renderer wraps a BGRA surface to draw it.
    fn wrap(cache: &CVMetalTextureCache, buffer: &CVPixelBuffer) -> CFRetained<CVMetalTexture> {
        let mut texture = std::ptr::null_mut();
        // SAFETY: CVMetalTextureCache.h's rule for `CVMetalTextureCacheCreateTextureFromImage`: a
        // live cache, an `IOSurface`-backed BGRA buffer viewed whole as its only plane, and a valid
        // out-pointer; the texture comes back owned (+1).
        let status = unsafe {
            CVMetalTextureCache::create_texture_from_image(
                None,
                cache,
                buffer,
                None,
                MTLPixelFormat::BGRA8Unorm,
                CVPixelBufferGetWidth(buffer),
                CVPixelBufferGetHeight(buffer),
                0,
                NonNull::from(&mut texture),
            )
        };
        assert_eq!(status, kCVReturnSuccess, "the surface's texture");
        // SAFETY: owned (+1), as above.
        let texture = unsafe { CFRetained::from_raw(NonNull::new(texture).expect("a texture")) };
        assert!(CVMetalTextureGetTexture(&texture).is_some(), "a Metal texture");
        texture
    }

    #[test]
    #[ignore = "a measurement, run by hand or by a probe"]
    #[expect(clippy::cast_precision_loss, reason = "sizes and counts far below 2^52")]
    fn what_an_animating_frame_costs() {
        if let Some(reason) = slopty_visual_gpu::unavailable_reason() {
            eprintln!("no GPU here: {reason}");
            return;
        }
        let device = MTLCreateSystemDefaultDevice().expect("a Metal device");
        let mut cache = std::ptr::null_mut();
        // SAFETY: CVMetalTextureCache.h's rule for `CVMetalTextureCacheCreate`: a live device and a
        // valid out-pointer; the cache comes back owned (+1).
        let status = unsafe {
            CVMetalTextureCache::create(None, None, &device, None, NonNull::from(&mut cache))
        };
        assert_eq!(status, kCVReturnSuccess, "a texture cache");
        // SAFETY: owned (+1), as above.
        let cache = unsafe { CFRetained::from_raw(NonNull::new(cache).expect("a cache")) };
        let spec = ShaderSpec {
            image: CODE,
            buffers: &[],
            inputs: &[],
            blend: Blend::Opaque,
            bloom: 0.6,
            textures: &[],
        };
        let mut uniforms = ShaderUniforms {
            resolution: [SIZE.0 as f32, SIZE.1 as f32, 2.0, 0.0],
            ..Default::default()
        };
        let (mut state, mut held) = (None, VecDeque::new());
        let (mut cpu, mut window, mut wall, mut instructions) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        // The GPU is up (`unavailable_reason` made it); the shader has drawn nothing yet.
        let base = process::own();
        for step in 0..WARM + FRAMES {
            uniforms.time[0] = f32::from(step) / 60.0;
            let (cpu_at, wall_at, counted_at) =
                (thread_cpu(), Instant::now(), process::instructions());
            let frame =
                render_shader(&spec, &uniforms, SIZE.0, SIZE.1, &mut state).expect("a frame");
            // The window's side: GPUI's renderer wraps the buffer as a texture to draw the surface.
            let window_at = thread_cpu();
            let texture = wrap(&cache, &frame.pixel_buffer());
            held.push_back((frame, texture));
            if held.len() > 2 {
                held.pop_front();
            }
            cache.flush(0);
            if step >= WARM {
                let now = thread_cpu();
                cpu.push(now.saturating_sub(cpu_at));
                window.push(now.saturating_sub(window_at));
                wall.push(wall_at.elapsed());
                if let (Some(at), Some(now)) = (counted_at, process::instructions()) {
                    instructions.push(now.saturating_sub(at));
                }
            }
        }
        let after = process::own();
        let mib = |bytes: u64| bytes as f64 / f64::from(1 << 20);
        let show = |what: &str, spread: Option<Spread>| {
            if let Some(s) = spread {
                eprintln!("MEASURE {what}: p50 {} p95 {} max {}", us(s.p50), us(s.p95), us(s.max));
            }
        };
        show("thread CPU per frame", Spread::of_durations(&cpu));
        show("of it the window's", Spread::of_durations(&window));
        show("wall per frame", Spread::of_durations(&wall));
        // A virtual machine's guest counts no instructions: every reading is 0 there.
        if let Some(s) = Spread::of(&mut instructions).filter(|s| s.max > 0) {
            eprintln!("MEASURE process instructions per frame: p50 {} p95 {}", s.p50, s.p95);
        }
        if let (Some(base), Some(after)) = (base, after) {
            eprintln!(
                "MEASURE footprint over the GPU's own: {:+.1} MiB after {} frames, peak {:+.1} MiB",
                mib(after.footprint) - mib(base.footprint),
                WARM + FRAMES,
                mib(after.peak_footprint) - mib(base.footprint),
            );
        }
        assert_eq!(cpu.len(), usize::from(FRAMES));
    }
}
